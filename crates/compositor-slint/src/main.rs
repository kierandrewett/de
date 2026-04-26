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

// ── Compatibility shim ────────────────────────────────────────────────────────
//
// `renderer.rs` was written against the spike-era inline `CompositorUI`
// (clock-text / client-texture / client-x/y/w/h / quit-clicked /
// client-clicked). The production `Compositor` from `slint/Compositor.slint`
// has a richer contract that renderer.rs hasn't been migrated to yet. This
// shim keeps the spike API alive so wayland + GPU + UI work all build together
// as one coherent crate. Drop the shim once renderer.rs drives `Compositor`
// directly.
slint::slint! {
    export component CompositorUI inherits Window {
        in property <image>  client-texture;
        in property <string> clock-text: "00:00:00";
        in property <bool>   client-visible: false;
        in property <int>    client-x: 100;
        in property <int>    client-y: 100;
        in property <int>    client-w: 800;
        in property <int>    client-h: 600;

        callback quit-clicked();
        callback client-clicked(float, float);

        panel := Rectangle {
            x: 0px; y: 0px;
            width: parent.width; height: 34px;
            background: #111318;
            Text {
                x: 14px; y: 0px; height: parent.height;
                text: root.clock-text;
                color: rgba(255, 255, 255, 0.80);
                font-size: 13px;
                vertical-alignment: center;
            }
            quit-btn := Rectangle {
                x: parent.width - 80px; y: 5px;
                width: 70px; height: 24px;
                background: #cc3333; border-radius: 4px;
                Text {
                    width: parent.width; height: parent.height;
                    text: "Quit"; color: white; font-size: 12px;
                    horizontal-alignment: center; vertical-alignment: center;
                }
                touch := TouchArea { clicked => { root.quit-clicked(); } }
            }
        }

        if root.client-visible: client-area := Rectangle {
            x: root.client-x * 1px; y: root.client-y * 1px;
            width: root.client-w * 1px; height: root.client-h * 1px;
            background: #2a2a2a;
            Image {
                width: parent.width; height: parent.height;
                source: root.client-texture; image-fit: fill;
            }
            ta := TouchArea {
                pointer-event(pe) => {
                    root.client-clicked(self.mouse-x / 1px, self.mouse-y / 1px);
                }
            }
        }

        Rectangle {
            x: 0px; y: 0px;
            width: parent.width; height: parent.height;
            background: #1e1e2e; z: -1;
        }
    }
}

mod wayland_state;
mod wayland;
mod platform;
mod renderer;

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
