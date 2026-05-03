//! wlr-layer-shell-v1 handler.
//!
//! Panels, docks, wallpapers, overlays, notification popups, lock screens —
//! all use wlr-layer-shell to anchor surfaces to screen edges.
//!
//! Public API for the render side:
//!   `state.layer_surfaces()` — iterate all mapped layer surfaces (LayerInfo).
//!   `state.refresh_layer_layout(output_w, output_h)` — re-read each surface's
//!       cached anchor / margin / exclusive-zone state, recompute its
//!       compositor-space rect, and reserve area for top/bottom/left/right
//!       exclusive zones. Call once per frame from the render loop.
//!   `state.reserved_zones()` — per-edge sum of exclusive zones (used by the
//!       window manager so toplevels avoid panel/dock areas).
//!   `state.exclusive_keyboard_layer()` — the topmost layer surface (Top or
//!       Overlay) requesting `Exclusive` keyboard focus, if any.

use std::sync::{Arc, Mutex};

use smithay::{
    delegate_layer_shell,
    reexports::wayland_server::protocol::{wl_output, wl_surface::WlSurface},
    wayland::shell::wlr_layer::{
        Anchor, ExclusiveZone, KeyboardInteractivity, Layer, LayerSurface as WlrLayerSurface,
        LayerSurfaceCachedState, Margins, WlrLayerShellHandler, WlrLayerShellState,
    },
};
use tracing::{info, trace};

use crate::wayland_state::{ClientSurfaceData, SpikeState};

// ──────────────────────────────────────────────────────────────────────────────
// Public data exposed to the render side
// ──────────────────────────────────────────────────────────────────────────────

/// Per-edge exclusive-zone reservation summed across all mapped layer
/// surfaces of the Top + Bottom layers. Used by the WM to keep toplevel
/// windows out of panel / dock areas.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReservedZones {
    pub top: i32,
    pub bottom: i32,
    pub left: i32,
    pub right: i32,
}

/// Metadata the render side needs to position and z-order layer surfaces.
///
/// `anchor`, `exclusive_zone`, `margin`, `keyboard_interactivity` and
/// `desired_size` are mirrored from the surface's `LayerSurfaceCachedState`
/// on every call to [`SpikeState::refresh_layer_layout`]. `x/y/w/h` is the
/// resulting compositor-space rect (output-local, in logical pixels).
#[derive(Debug, Clone)]
pub struct LayerInfo {
    pub surface: WlrLayerSurface,
    pub layer: Layer,
    pub namespace: String,

    pub anchor: Anchor,
    pub exclusive_zone: ExclusiveZone,
    pub exclusive_edge: Option<Anchor>,
    pub margin: Margins,
    pub keyboard_interactivity: KeyboardInteractivity,
    pub desired_size: (i32, i32),

    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,

    /// Composited pixel buffer for the layer surface tree. Refreshed by
    /// the wl_surface commit handler (SHM + DMA-BUF), not on a per-frame
    /// schedule — `pixels.dirty` then drives whether the renderer needs
    /// to rebuild the Slint `layers` model.
    pub pixels: Arc<Mutex<ClientSurfaceData>>,
}

impl LayerInfo {
    /// Whether this layer surface participates in keyboard focus routing
    /// (None = never; OnDemand = via normal click-to-focus; Exclusive = takes
    /// focus from toplevels while mapped).
    pub fn can_receive_keyboard_focus(&self) -> bool {
        matches!(
            self.keyboard_interactivity,
            KeyboardInteractivity::Exclusive | KeyboardInteractivity::OnDemand
        )
    }

    /// True when this layer surface is on Top/Overlay and asked for exclusive
    /// keyboard focus (lock screens, password prompts, app launchers).
    pub fn wants_exclusive_keyboard(&self) -> bool {
        matches!(
            self.keyboard_interactivity,
            KeyboardInteractivity::Exclusive
        ) && matches!(self.layer, Layer::Top | Layer::Overlay)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Handler impl
// ──────────────────────────────────────────────────────────────────────────────

impl WlrLayerShellHandler for SpikeState {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        _wl_output: Option<wl_output::WlOutput>,
        layer: Layer,
        namespace: String,
    ) {
        info!(ns = %namespace, layer = ?layer, "layer_shell: new surface");

        // Send the initial configure so the client knows output geometry.
        // (0, 0) size hint → client chooses its own size.
        // The cached state (anchor / margin / exclusive_zone / size) is
        // populated by the client in subsequent commits and re-read each
        // frame in `refresh_layer_layout`.
        surface.send_configure();

        self.layer_surfaces.push(LayerInfo {
            surface,
            layer,
            namespace,
            anchor: Anchor::empty(),
            exclusive_zone: ExclusiveZone::Neutral,
            exclusive_edge: None,
            margin: Margins::default(),
            keyboard_interactivity: KeyboardInteractivity::None,
            desired_size: (0, 0),
            x: 0,
            y: 0,
            w: 0,
            h: 0,
            pixels: Arc::new(Mutex::new(ClientSurfaceData::default())),
        });
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        info!("layer_shell: surface destroyed");
        self.layer_surfaces.retain(|li| li.surface != surface);
    }
}

delegate_layer_shell!(SpikeState);

// ──────────────────────────────────────────────────────────────────────────────
// Layout: read cached state + compute compositor-space rect for each surface
// ──────────────────────────────────────────────────────────────────────────────

/// Anchor edge that an `Exclusive` zone reservation applies to.
///
/// Spec: `exclusive_edge` (added in v5) is honoured if set; otherwise the
/// edge is implied from the anchor when the surface is anchored to exactly
/// one edge or to three edges (the missing one). Two parallel / two
/// perpendicular / four-edge anchors have no implied exclusive edge.
fn effective_exclusive_edge(anchor: Anchor, explicit: Option<Anchor>) -> Option<Anchor> {
    if let Some(e) = explicit {
        return Some(e);
    }
    match anchor.bits().count_ones() {
        1 => Some(anchor),
        3 => Some(
            match anchor.complement()
                & (Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT)
            {
                Anchor::TOP => Anchor::BOTTOM,
                Anchor::BOTTOM => Anchor::TOP,
                Anchor::LEFT => Anchor::RIGHT,
                Anchor::RIGHT => Anchor::LEFT,
                _ => return None,
            },
        ),
        _ => None,
    }
}

impl SpikeState {
    /// Return all currently-mapped layer surfaces, ordered as received.
    /// Render code should iterate and z-sort by `LayerInfo::layer`.
    pub fn layer_surfaces(&self) -> &[LayerInfo] {
        &self.layer_surfaces
    }

    /// Per-edge sum of exclusive zones across all currently mapped Top +
    /// Bottom layer surfaces. Background / Overlay layers are not subtracted
    /// from the toplevel work area (Background sits beneath windows, and
    /// Overlay-with-exclusive-zone is rare and tends to be modal anyway).
    pub fn reserved_zones(&self) -> ReservedZones {
        let mut z = ReservedZones::default();
        for li in &self.layer_surfaces {
            if !matches!(li.layer, Layer::Top | Layer::Bottom) {
                continue;
            }
            let amount = match li.exclusive_zone {
                ExclusiveZone::Exclusive(v) => v as i32,
                _ => continue,
            };
            match effective_exclusive_edge(li.anchor, li.exclusive_edge) {
                Some(Anchor::TOP) => z.top += amount + li.margin.top,
                Some(Anchor::BOTTOM) => z.bottom += amount + li.margin.bottom,
                Some(Anchor::LEFT) => z.left += amount + li.margin.left,
                Some(Anchor::RIGHT) => z.right += amount + li.margin.right,
                _ => {}
            }
        }
        z
    }

    /// The topmost layer surface (Top or Overlay) requesting `Exclusive`
    /// keyboard focus, if any. Lock screens and launchers map here.
    pub fn exclusive_keyboard_layer(&self) -> Option<&WlSurface> {
        // Overlay outranks Top.
        let overlay = self
            .layer_surfaces
            .iter()
            .filter(|li| li.wants_exclusive_keyboard() && matches!(li.layer, Layer::Overlay))
            .next_back();
        if let Some(l) = overlay {
            return Some(l.surface.wl_surface());
        }
        let top = self
            .layer_surfaces
            .iter()
            .filter(|li| li.wants_exclusive_keyboard() && matches!(li.layer, Layer::Top))
            .next_back();
        top.map(|l| l.surface.wl_surface())
    }

    /// Re-read cached state for every layer surface and recompute its
    /// compositor-space rect against the given output size (logical pixels).
    ///
    /// Mirrors the algorithm in `smithay::desktop::LayerMap::arrange`:
    ///   1. Pass 1 — exclusive-zone surfaces consume their edge from the
    ///      shared work area (`zone`), in declaration order.
    ///   2. Pass 2 — non-exclusive surfaces (Neutral/DontCare) lay out
    ///      against the (possibly shrunken) work area.
    ///
    /// We don't drive smithay's `LayerMap` itself because we never call
    /// `map_layer` (the renderer composites layer surfaces directly from
    /// `LayerInfo` rects rather than going through a `Space`). The math is
    /// the same.
    pub fn refresh_layer_layout(&mut self, output_w: i32, output_h: i32) {
        // Snapshot cached state for every layer surface before doing layout.
        // We only mutate `self.layer_surfaces` here — the arrange algorithm
        // walks the same vec twice (exclusive then non-exclusive) so we
        // collect indices into a working list to avoid double-borrows.
        for li in self.layer_surfaces.iter_mut() {
            let cached: LayerSurfaceCachedState = li.surface.with_cached_state(|s| *s);
            li.anchor = cached.anchor;
            li.exclusive_zone = cached.exclusive_zone;
            li.exclusive_edge = cached.exclusive_edge;
            li.margin = cached.margin;
            li.keyboard_interactivity = cached.keyboard_interactivity;
            li.desired_size = (cached.size.w, cached.size.h);
            // Cached `layer` may have been updated by a v2 set_layer request;
            // mirror that so we route z-order and reserved-zone math correctly.
            li.layer = cached.layer;
        }

        // Two-pass arrange.
        let output_rect = (0i32, 0i32, output_w.max(0), output_h.max(0));
        let mut zone = output_rect; // shared work-area; shrunken by exclusive zones.

        // Order: all exclusive-zone surfaces first, then the rest. Indexing
        // by usize so the second pass can re-borrow `&mut self.layer_surfaces`.
        let mut order: Vec<usize> = (0..self.layer_surfaces.len()).collect();
        order.sort_by_key(|&i| match self.layer_surfaces[i].exclusive_zone {
            ExclusiveZone::Exclusive(_) => 0,
            _ => 1,
        });

        for i in order {
            let (rect, consume) = compute_layer_rect(&self.layer_surfaces[i], output_rect, zone);
            let li = &mut self.layer_surfaces[i];
            li.x = rect.0;
            li.y = rect.1;
            li.w = rect.2;
            li.h = rect.3;
            trace!(
                ns = %li.namespace, layer = ?li.layer, anchor = ?li.anchor,
                ez = ?li.exclusive_zone,
                rect = ?(li.x, li.y, li.w, li.h),
                "layer_shell: arranged"
            );
            // Apply the consumed exclusive edge to the shared work area for
            // the next surface in the iteration.
            if let Some((edge, amount)) = consume {
                match edge {
                    Anchor::TOP => {
                        zone.1 = zone.1.saturating_add(amount);
                        zone.3 = zone.3.saturating_sub(amount);
                    }
                    Anchor::BOTTOM => {
                        zone.3 = zone.3.saturating_sub(amount);
                    }
                    Anchor::LEFT => {
                        zone.0 = zone.0.saturating_add(amount);
                        zone.2 = zone.2.saturating_sub(amount);
                    }
                    Anchor::RIGHT => {
                        zone.2 = zone.2.saturating_sub(amount);
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Compute a single layer surface's compositor-space rect.
///
/// Returns `(rect, exclusive_consume)` where `exclusive_consume` is
/// `Some((edge, amount))` if the surface has an effective exclusive zone
/// that should be subtracted from the shared work area for subsequent
/// surfaces (margin already included in `amount`).
fn compute_layer_rect(
    li: &LayerInfo,
    output_rect: (i32, i32, i32, i32),
    zone: (i32, i32, i32, i32),
) -> ((i32, i32, i32, i32), Option<(Anchor, i32)>) {
    // Source rect: zone (work area) for Exclusive/Neutral, full output for
    // DontCare. DontCare also uses the full output as its outer bound but
    // doesn't consume any exclusive zone.
    let mut source = match li.exclusive_zone {
        ExclusiveZone::Exclusive(_) | ExclusiveZone::Neutral => zone,
        ExclusiveZone::DontCare => output_rect,
    };

    // Margins shrink the source by the corresponding edge.
    if li.anchor.contains(Anchor::LEFT) {
        source.2 = source.2.saturating_sub(li.margin.left);
    }
    if li.anchor.contains(Anchor::RIGHT) {
        source.2 = source.2.saturating_sub(li.margin.right);
    }
    if li.anchor.contains(Anchor::TOP) {
        source.3 = source.3.saturating_sub(li.margin.top);
    }
    if li.anchor.contains(Anchor::BOTTOM) {
        source.3 = source.3.saturating_sub(li.margin.bottom);
    }

    // Determine final size: clamped to source, falling back to half-source
    // when the client gave 0; stretched along anchored axes.
    let mut sw = li.desired_size.0.min(source.2).max(0);
    let mut sh = li.desired_size.1.min(source.3).max(0);
    if sw == 0 {
        sw = source.2 / 2;
    }
    if sh == 0 {
        sh = source.3 / 2;
    }
    if li.anchor.anchored_horizontally() {
        sw = source.2;
    }
    if li.anchor.anchored_vertically() {
        sh = source.3;
    }
    sw = sw.max(0);
    sh = sh.max(0);

    let x = if li.anchor.contains(Anchor::LEFT) {
        source.0 + li.margin.left
    } else if li.anchor.contains(Anchor::RIGHT) {
        source.0 + (source.2 - sw)
    } else {
        source.0 + (source.2 / 2 - sw / 2)
    };

    let y = if li.anchor.contains(Anchor::TOP) {
        source.1 + li.margin.top
    } else if li.anchor.contains(Anchor::BOTTOM) {
        source.1 + (source.3 - sh)
    } else {
        source.1 + (source.3 / 2 - sh / 2)
    };

    let consume = match li.exclusive_zone {
        ExclusiveZone::Exclusive(amount) => effective_exclusive_edge(li.anchor, li.exclusive_edge)
            .map(|edge| {
                let m = match edge {
                    Anchor::TOP => li.margin.top,
                    Anchor::BOTTOM => li.margin.bottom,
                    Anchor::LEFT => li.margin.left,
                    Anchor::RIGHT => li.margin.right,
                    _ => 0,
                };
                (edge, amount as i32 + m)
            }),
        _ => None,
    };

    ((x, y, sw, sh), consume)
}
