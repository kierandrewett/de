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
//!   - DMA-BUF client buffers are documented as a follow-up (wgpu-28 lacks a
//!     stable HAL DMA-BUF import path; tracked in SPIKE_LOG.md).
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
mod chrome_shader;

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
