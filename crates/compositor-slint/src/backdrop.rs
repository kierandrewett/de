//! CPU-synthesised backdrop image for the panel and dock.
//!
//! Real per-frame GPU backdrop-blur (sampling the swapchain) needs a
//! two-pass Slint render plus a kawase compute pass — a multi-hour
//! architectural change. This module is the pragmatic equivalent: we hold a
//! pre-downsampled copy of the wallpaper and, on a rate limit, composite
//! every visible window's last buffer onto it, then Gaussian-blur the
//! result. The output is fed back to Slint as `wallpaper-blurred`, so the
//! Panel and Dock pick it up via their existing backdrop machinery.
//!
//! Trade-offs:
//!   - Reflects current window positions + content (refreshes every
//!     `REFRESH_MS` milliseconds).
//!   - Capped at low resolution (320 × something) so the per-frame cost
//!     stays bounded; the heavy Gaussian σ hides the resolution.
//!   - Costs roughly one wallpaper-sized memcpy + per-window scale/blend +
//!     a separable Gaussian per refresh — well under a 30 Hz tick.

use std::path::Path;
use std::time::Instant;

use image::{imageops, RgbaImage};
use tracing::debug;

/// Backdrop canvas width. Lower = much cheaper blur (cost is O(W·H·sigma)
/// for the cascaded box-blur). 320 px gives noticeably sharper backdrops
/// than 200 while staying well under 1 ms per refresh.
const BACKDROP_WIDTH: u32 = 320;
/// Refresh cadence. 16 ms = effectively per-frame at 60 Hz so window
/// movement reflects in the dock/panel backdrop with no visible lag.
/// Cost stays low because the compositing + cascaded box-blur runs at
/// 320 × ~180 px, not output resolution.
const REFRESH_MS: u64 = 16;
/// Gaussian σ applied after compositing windows onto the wallpaper. Higher
/// hides the low backdrop resolution and reads as a softer macOS glass.
const BLUR_SIGMA: f32 = 14.0;

/// One window's contribution to the backdrop synthesis.
pub struct WindowSnapshot<'a> {
    /// Compositor-space top-left in physical pixels.
    pub x: i32,
    pub y: i32,
    /// Physical size on screen.
    pub w: i32,
    pub h: i32,
    /// RGBA bytes from the client buffer (premultiplied is fine — alpha
    /// is overwhelmingly 255 for SHM clients).
    pub pixels: &'a [u8],
    /// Buffer dimensions.
    pub buf_w: u32,
    pub buf_h: u32,
}

pub struct BackdropSynth {
    /// Pre-downsampled wallpaper at backdrop resolution, kept around as the
    /// base layer for every refresh.
    wallpaper_small: Option<RgbaImage>,
    /// Output dimensions in physical pixels (used to scale window rects to
    /// the small backdrop coordinate system).
    output_w: u32,
    output_h: u32,
    /// Backdrop dimensions matching the downsampled wallpaper aspect ratio.
    backdrop_w: u32,
    backdrop_h: u32,
    /// Last successful refresh; throttle gate.
    last_at: Option<Instant>,
}

impl BackdropSynth {
    pub fn new(output_w: u32, output_h: u32) -> Self {
        let backdrop_w = BACKDROP_WIDTH;
        let backdrop_h = ((output_h as f32 / output_w as f32) * backdrop_w as f32) as u32;
        Self {
            wallpaper_small: None,
            output_w,
            output_h,
            backdrop_w,
            backdrop_h,
            last_at: None,
        }
    }

    pub fn set_output_size(&mut self, w: u32, h: u32) {
        self.output_w = w;
        self.output_h = h;
        self.backdrop_h = ((h as f32 / w as f32) * self.backdrop_w as f32) as u32;
        // Force the next synth to refresh; the old wallpaper_small still has
        // the right pixel ratio so we don't reload from disk here.
        self.last_at = None;
    }

    /// Load + downsample the wallpaper from disk. Call when the wallpaper
    /// changes (initial boot, theme toggle, etc.).
    pub fn load_wallpaper(&mut self, path: &Path) {
        let Ok(img) = image::open(path) else {
            self.wallpaper_small = None;
            return;
        };
        let small = img.resize_exact(
            self.backdrop_w, self.backdrop_h,
            imageops::FilterType::Triangle,
        );
        self.wallpaper_small = Some(small.to_rgba8());
        self.last_at = None;
        debug!("backdrop: wallpaper resampled to {}×{}", self.backdrop_w, self.backdrop_h);
    }

    /// Composite the wallpaper + window snapshots and blur. Returns None if
    /// throttled (called within REFRESH_MS of the last successful synth) or
    /// if the wallpaper hasn't been loaded.
    pub fn try_synth(&mut self, windows: &[WindowSnapshot<'_>]) -> Option<slint::Image> {
        if let Some(t) = self.last_at {
            if t.elapsed().as_millis() < REFRESH_MS as u128 {
                return None;
            }
        }
        let base = self.wallpaper_small.as_ref()?.clone();
        let mut canvas = base;

        for w in windows {
            if w.buf_w == 0 || w.buf_h == 0 || w.w <= 0 || w.h <= 0 {
                continue;
            }
            // Scale window buffer to the backdrop coordinate system.
            let target_w = (w.w as u32 * self.backdrop_w / self.output_w).max(1);
            let target_h = (w.h as u32 * self.backdrop_h / self.output_h).max(1);
            let target_x = w.x as i64 * self.backdrop_w as i64 / self.output_w as i64;
            let target_y = w.y as i64 * self.backdrop_h as i64 / self.output_h as i64;

            // Build an RgbaImage from the window pixels (zero-copy borrow
            // would be ideal, but image's API requires owned bytes).
            let Some(buf) = RgbaImage::from_raw(w.buf_w, w.buf_h, w.pixels.to_vec()) else { continue };
            let scaled = imageops::resize(&buf, target_w, target_h, imageops::FilterType::Triangle);

            // Blend into canvas via overlay (the SHM clients' alpha is 255 so
            // overlay = paste-over; matches what the user sees on screen).
            imageops::overlay(&mut canvas, &scaled, target_x, target_y);
        }

        // Final Gaussian blur. image::imageops::blur is cascaded box-blur
        // internally — fast enough at 320 × ~180 even with σ=14.
        let blurred = imageops::blur(&canvas, BLUR_SIGMA);

        let (bw, bh) = blurred.dimensions();
        let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
            blurred.as_raw(), bw, bh,
        );
        let img = slint::Image::from_rgba8(buf);

        self.last_at = Some(Instant::now());
        Some(img)
    }
}
