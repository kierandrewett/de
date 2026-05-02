//! compositor-slint: Slint as the compositor render layer.
//!
//! Architecture:
//!   - Smithay handles all Wayland protocol plumbing + the calloop event loop
//!     (see `src/wayland/` for the per-protocol handler split — wlr-layer-shell,
//!     xdg-decoration, presentation-time/wp-fifo/commit-timing, scaling, etc.)
//!   - Slint owns the scene graph + render path:
//!       * `slint/Compositor.slint` is the production UI (panel/dock/wallpaper/chrome)
//!       * GPU rendering via `FemtoVGWGPURenderer` (wgpu-28) — see `renderer.rs`
//!   - Each frame: Slint renders to a wgpu texture, blitted to the winit swapchain
//!   - Client SHM buffers are uploaded as wgpu textures each commit
//!   - DMA-BUF client buffers: Option B two-stage import (EGL/GLES → CPU → wgpu).
//!     Enables Firefox, GTK4, kitty GPU, and wgpu-based clients.
//!     See SPIKE_LOG.md "Wave 1D" for the full option comparison.
//!
//! Wave 1D additions:
//!   - DMA-BUF Option B import (wayland_state.rs)
//!   - True separable Gaussian shadow blur (chrome.wgsl, chrome_shader.rs)
//!   - Border-overlaid chrome contract (chrome.wgsl)
//!   - SVG icon decode via resvg (desktop.rs)
//!
//! This is the seed of the production compositor; the iced-based
//! `crates/compositor` is being phased out.

use anyhow::Result;
use tracing::info;

// ──────────────────────────────────────────────────────────────────────────────
// Slint UI — generated from slint/Compositor.slint via build.rs
// ──────────────────────────────────────────────────────────────────────────────
//
// Pulls in the production UI types (`Compositor`, `WindowItem`, `LayerItem`,
// `DockItem`, etc.) defined under `slint/`. The contract is in
// `slint/Compositor.slint`.
slint::include_modules!();

mod wayland_state;
mod wayland;
mod platform;
mod renderer;
// chrome_shader retired in favour of `mod render` (separate single-purpose
// passes). The old src/chrome_shader.rs + shaders/chrome.wgsl files remain on
// disk for reference but are no longer compiled.
mod theme;
mod wallpaper;
mod desktop;
mod cursor;
mod cursor_render;
mod resize;
mod snap;
mod backdrop;
mod ipc_server;
mod tray;
mod dbusmenu;
mod render;
mod screencopy;
pub mod wm;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "compositor_slint=debug,smithay=warn".parse().unwrap()),
        )
        .init();

    info!("starting compositor-slint");
    renderer::run()
}
