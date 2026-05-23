//! Chrome render pipeline — composed of small, single-purpose passes.
//!
//! Architecture:
//!
//!   ChromeRenderer
//!     ├── Shared            — bind-group layout, sampler, dynamic uniform buffer
//!     ├── ShadowPass        — 3-layer drop shadow (silhouette → blur-h → blur-v → composite)
//!     ├── SquircleClipPass  — alpha-mask the scene with a squircle (off by default)
//!     ├── BorderPass        — 0.5 px outer stroke
//!     └── HighlightPass     — 1 px inner top-edge gradient
//!
//! Per-frame entry point: `ChromeRenderer::render(...)`. Caller supplies the
//! list of `WindowChromeParams` (one per visible window) plus the swapchain
//! frame view + the (already-blitted) Slint scene view.
//!
//! Pass order:
//!   1. Shadow layers, back-to-front per window
//!   2. (optional) Squircle clip — replaces the scene's window-region pixels
//!      with squircle-clipped alpha. Currently OFF; enable once Slint's
//!      WindowChrome drops its circular `border-radius`.
//!   3. Border (0.5 px stroke) per window
//!   4. Highlight (1 px inner top gradient) per window

pub mod border;
pub mod common;
pub mod dnd_icon;
pub mod highlight;
pub mod shadow;
pub mod squircle;

use std::sync::Arc;


use common::{ChromeUniforms, Shared, MAX_DRAWS, UNIFORM_STRIDE, UNIFORM_STRUCT_SIZE};
use shadow::{active_layers, inactive_layers, uniforms_for_layer};

/// Per-window data the coordinator needs to schedule passes.
#[derive(Debug, Clone, Copy)]
pub struct WindowChromeParams {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Discrete active-vs-blur flag. Superseded by the smooth `focus_t`
    /// for shading; kept here so callers that just want a bool (debug
    /// overlays, theme tracking) don't have to threshold the float.
    #[allow(dead_code)]
    pub active: bool,
    pub focus_t: f32,
    pub mode_t: f32,
    /// True for CSD apps. CSD windows skip the GPU drop-shadow (the client
    /// paints its own shadow into the buffer; ours would double-stack and
    /// — worse — extends visually ~24 px above the WM hit-rect, which made
    /// the user click on shadow pixels expecting them to belong to the
    /// window). Border + highlight passes are also skipped because they
    /// would draw a thin inset line over the client's own chrome.
    pub csd: bool,
    /// Outer corner radius in PHYSICAL pixels for this window's GPU chrome
    /// (shadow / border / highlight squircle). 0 when the window is
    /// maximized so the chrome squares off against the screen edges;
    /// otherwise the themed window radius. Must track WindowChrome.slint's
    /// `window-corner-radius-outer` or the Slint clip and the GPU chrome
    /// disagree at the corners.
    pub radius: f32,
}

pub struct ChromeRenderer {
    pub shared: Shared,
    pub shadow: shadow::ShadowPass,
    pub squircle_clip: squircle::SquircleClipPass,
    pub border: border::BorderPass,
    pub highlight: highlight::HighlightPass,

    /// Whether to run the squircle-clip pass on the scene each frame. Off
    /// while WindowChrome.slint still uses circular `border-radius`.
    pub enable_squircle_clip: bool,
}

impl ChromeRenderer {
    pub fn new(device: Arc<wgpu::Device>, format: wgpu::TextureFormat) -> Self {
        let shared = Shared::new(device, format);
        let shadow = shadow::ShadowPass::new(&shared);
        let squircle_clip = squircle::SquircleClipPass::new(&shared);
        let border = border::BorderPass::new(&shared);
        let highlight = highlight::HighlightPass::new(&shared);
        Self {
            shared,
            shadow,
            squircle_clip,
            border,
            highlight,
            enable_squircle_clip: false,
        }
    }

    /// Drop intermediates (shadow textures) on output resize.
    pub fn invalidate(&mut self) {
        self.shadow.invalidate();
    }

    /// Run chrome passes for every window. After EACH window's full chrome
    /// (shadow + border + highlight) is drawn, `after_each_window` is called
    /// with the index of the window just rendered. The callback is meant to
    /// re-blit higher-z window rects from the Slint scene texture, so that
    /// chrome of a back window which fell inside a front window's footprint
    /// gets cleanly overwritten by the front window's actual content.
    ///
    /// `windows` MUST be in z-order back-to-front (lowest z first).
    ///
    /// `scene_view` — Slint render-target view (used by the squircle clip
    /// pass; bound as a dummy view for borders/highlights).
    /// `target_view` — composite target view; chrome passes draw onto this.
    pub fn render(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        scene_view: &wgpu::TextureView,
        target_view: &wgpu::TextureView,
        surface_w: u32,
        surface_h: u32,
        windows: &[WindowChromeParams],
        mut after_each_window: impl FnMut(&mut wgpu::CommandEncoder, usize),
    ) {
        if windows.is_empty() {
            return;
        }

        let sw = surface_w as f32;
        let sh = surface_h as f32;

        // ── Stage A. Lay out uniform slots PER WINDOW so the pass loop
        //              can iterate per-window and re-blit higher-z rects
        //              between iterations (preserves z-order across borders
        //              and highlights, not just shadows).
        struct PerWin {
            shadow_slots: Vec<u32>,
            clip_slot: Option<u32>,
            border_slot: u32,
            hl_slot: u32,
        }
        let mut raw_uniforms: Vec<ChromeUniforms> = Vec::new();
        let mut per_win: Vec<PerWin> = Vec::with_capacity(windows.len());

        for win in windows.iter() {
            let active = active_layers();
            let inactive = inactive_layers();

            let mut shadow_slots = Vec::new();

            // GPU drop-shadow — drawn for SSD AND CSD windows. CSD clients
            // do paint their own shadow into the buffer, but we crop that
            // off via the `set_window_geometry` rect, so without our shadow
            // a CSD window has none at all. The GPU shadow is laid around
            // the visible window rect with the window's corner radius, so
            // it matches whatever the client drew. (It's not hidden when
            // maximized — see below — because the radius going to 0 already
            // squares it off; a maximized window's shadow is harmless,
            // mostly occluded by the panel/dock.)
            for li in 0..2 {
                let a = inactive[li].alpha + win.focus_t * (active[li].alpha - inactive[li].alpha);
                let b = inactive[li].blur_sigma
                    + win.focus_t * (active[li].blur_sigma - inactive[li].blur_sigma);
                let oy = inactive[li].offset_y
                    + win.focus_t * (active[li].offset_y - inactive[li].offset_y);
                let layer = shadow::ShadowLayer {
                    offset_x: active[li].offset_x,
                    offset_y: oy,
                    blur_sigma: b,
                    alpha: a,
                };
                let mut u = uniforms_for_layer(win.x, win.y, win.w, win.h, sw, sh, layer, 1.0);
                u.mode_t = win.mode_t;
                u.focus_t = win.focus_t;
                u.radius_px = win.radius;
                let slot = raw_uniforms.len() as u32;
                raw_uniforms.push(u);
                shadow_slots.push(slot);
            }

            if win.focus_t > 0.001 {
                let layer3 = active[2];
                let mut u =
                    uniforms_for_layer(win.x, win.y, win.w, win.h, sw, sh, layer3, win.focus_t);
                u.mode_t = win.mode_t;
                u.focus_t = win.focus_t;
                u.radius_px = win.radius;
                let slot = raw_uniforms.len() as u32;
                raw_uniforms.push(u);
                shadow_slots.push(slot);
            }

            let clip_slot = if self.enable_squircle_clip {
                let mut u = ChromeUniforms::default();
                u.win_x = win.x;
                u.win_y = win.y;
                u.win_w = win.w;
                u.win_h = win.h;
                u.surface_w = sw;
                u.surface_h = sh;
                u.mode_t = win.mode_t;
                u.focus_t = win.focus_t;
                u.radius_px = win.radius;
                let slot = raw_uniforms.len() as u32;
                raw_uniforms.push(u);
                Some(slot)
            } else {
                None
            };

            let border_slot = {
                let mut u = ChromeUniforms::default();
                u.win_x = win.x;
                u.win_y = win.y;
                u.win_w = win.w;
                u.win_h = win.h;
                u.surface_w = sw;
                u.surface_h = sh;
                u.mode_t = win.mode_t;
                u.focus_t = win.focus_t;
                u.radius_px = win.radius;
                let slot = raw_uniforms.len() as u32;
                raw_uniforms.push(u);
                slot
            };

            let hl_slot = {
                let mut u = ChromeUniforms::default();
                u.win_x = win.x;
                u.win_y = win.y;
                u.win_w = win.w;
                u.win_h = win.h;
                u.surface_w = sw;
                u.surface_h = sh;
                u.mode_t = win.mode_t;
                u.focus_t = win.focus_t;
                u.radius_px = win.radius;
                let slot = raw_uniforms.len() as u32;
                raw_uniforms.push(u);
                slot
            };

            per_win.push(PerWin {
                shadow_slots,
                clip_slot,
                border_slot,
                hl_slot,
            });
        }

        if raw_uniforms.is_empty() || raw_uniforms.len() as u64 > MAX_DRAWS {
            return;
        }

        // ── Stage B. Upload uniforms in one go ──────────────────────────────
        let total = raw_uniforms.len();
        let mut packed = vec![0u8; (UNIFORM_STRIDE * total as u64) as usize];
        for (i, u) in raw_uniforms.iter().enumerate() {
            let off = i * UNIFORM_STRIDE as usize;
            packed[off..off + UNIFORM_STRUCT_SIZE].copy_from_slice(bytemuck::bytes_of(u));
        }
        queue.write_buffer(&self.shared.dynamic_uniform_buf, 0, &packed);

        // ── Stage C. Per-window: shadow → border → highlight → cleanup. ─────
        // Iteration order is the caller's order (back-to-front). After each
        // window, the callback re-blits any HIGHER-Z window rects from the
        // Slint scene so this window's chrome that fell inside a front
        // window gets covered by the front window's actual content.
        let dummy = self.shared.dummy_view.clone();
        for (wi, pw) in per_win.iter().enumerate() {
            // 1. Shadow layers for this window.
            for slot in &pw.shadow_slots {
                let dyn_offset = slot_offset(*slot);
                self.shadow.render_layer(
                    &self.shared,
                    encoder,
                    target_view,
                    surface_w,
                    surface_h,
                    dyn_offset,
                );
            }

            // 2. Optional squircle clip.
            if let Some(slot) = pw.clip_slot {
                self.squircle_clip.render(
                    &self.shared,
                    encoder,
                    scene_view,
                    target_view,
                    slot_offset(slot),
                );
            }

            // 3. Border + 4. Highlight — SSD only. CSD apps paint their own
            // chrome inside the cropped surface; adding our 0.5 px outer
            // stroke + 1 px inner highlight on top of that shows up as a
            // visible "inset" line at the window edge.
            if !windows[wi].csd {
                self.border.render(
                    &self.shared,
                    encoder,
                    &dummy,
                    target_view,
                    slot_offset(pw.border_slot),
                );
                self.highlight.render(
                    &self.shared,
                    encoder,
                    &dummy,
                    target_view,
                    slot_offset(pw.hl_slot),
                );
            }

            // 5. Caller cleanup — typically re-blits higher-z window rects
            //    so this window's chrome that fell inside a front window
            //    gets cleanly overwritten by render_tex's z-stacked content.
            after_each_window(encoder, wi);
        }
    }
}

fn slot_offset(slot: u32) -> u32 {
    (slot as u64 * UNIFORM_STRIDE) as u32
}
