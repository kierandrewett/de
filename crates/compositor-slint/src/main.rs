//! compositor-slint: GPU-migrated Slint compositor.
//!
//! Architecture (GPU migration):
//!   - Smithay: Wayland protocol plumbing + calloop event loop (unchanged)
//!   - Slint FemtoVGWGPURenderer: GPU rasterisation of the Slint scene
//!   - Each frame: render_to_texture() into Rgba8Unorm offscreen tex → blit to swapchain
//!   - Client SHM buffers: CPU memcpy into Slint Image (FemtoVG uploads on render)
//!   - DMA-BUF: blocked on wgpu-28 not having stable dmabuf import API (see renderer.rs)

use anyhow::Result;
use tracing::info;

// ──────────────────────────────────────────────────────────────────────────────
// Slint UI definition (UNCHANGED from spike — not in scope for this agent)
// ──────────────────────────────────────────────────────────────────────────────

slint::slint! {
    import { VerticalBox } from "std-widgets.slint";

    export component CompositorUI inherits Window {
        in property <image> client-texture;
        in property <string> clock-text: "00:00:00";
        in property <bool> client-visible: false;
        in property <int> client-x: 100;
        in property <int> client-y: 100;
        in property <int> client-w: 800;
        in property <int> client-h: 600;

        callback quit-clicked();
        callback client-clicked(float, float);

        panel := Rectangle {
            x: 0px; y: 0px;
            width: parent.width; height: 36px;
            background: #1d1d1d;

            clock := Text {
                x: 12px; y: 0px;
                height: parent.height;
                text: root.clock-text;
                color: #e0e0e0;
                font-size: 13px;
                vertical-alignment: center;
            }

            quit-btn := Rectangle {
                x: parent.width - 80px; y: 6px;
                width: 70px; height: 24px;
                background: #cc3333;
                border-radius: 4px;

                Text {
                    width: parent.width; height: parent.height;
                    text: "Quit";
                    color: white;
                    font-size: 12px;
                    horizontal-alignment: center;
                    vertical-alignment: center;
                }

                touch := TouchArea {
                    clicked => { root.quit-clicked(); }
                }
            }
        }

        if root.client-visible: client-area := Rectangle {
            x: root.client-x * 1px;
            y: root.client-y * 1px;
            width: root.client-w * 1px;
            height: root.client-h * 1px;
            background: #2a2a2a;

            client-img := Image {
                x: 0px; y: 0px;
                width: parent.width;
                height: parent.height;
                source: root.client-texture;
                image-fit: fill;
            }

            ta := TouchArea {
                pointer-event(pe) => {
                    root.client-clicked(self.mouse-x / 1px, self.mouse-y / 1px);
                }
            }
        }

        background := Rectangle {
            x: 0px; y: 0px;
            width: parent.width; height: parent.height;
            background: #1e1e2e;
            z: -1;
        }
    }
}

mod wayland_state;
mod platform;
mod renderer;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "compositor_slint=debug,smithay=warn".parse().unwrap()),
        )
        .init();

    info!("Starting compositor-slint (GPU)");
    renderer::run()
}
