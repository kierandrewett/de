//! Slint-as-compositor-renderer spike.
//!
//! Architecture:
//!   - Smithay handles all Wayland protocol plumbing + calloop event loop
//!   - Slint SoftwareRenderer owns the scene graph / UI layout
//!   - Each frame: Slint renders to a CPU pixel buffer → blitted to screen via softbuffer
//!   - Client SHM buffers are copied into a slint::Image each commit
//!
//! SPIKE: This is a throwaway proof-of-concept. Hardcoded sizes, minimal error handling.

use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use tracing::{debug, error, info, warn};

// ──────────────────────────────────────────────────────────────────────────────
// Slint UI definition
// ──────────────────────────────────────────────────────────────────────────────

slint::slint! {
    // SPIKE: Inline .slint definition avoids a build.rs / file I/O dependency.
    import { VerticalBox } from "std-widgets.slint";

    export component CompositorUI inherits Window {
        // Properties updated from Rust each frame
        in property <image> client-texture;
        in property <string> clock-text: "00:00:00";
        in property <bool> client-visible: false;
        in property <int> client-x: 100;
        in property <int> client-y: 100;
        in property <int> client-w: 800;
        in property <int> client-h: 600;

        callback quit-clicked();
        callback client-clicked(float, float);

        // Top panel bar (36 px)
        panel := Rectangle {
            x: 0px; y: 0px;
            width: parent.width; height: 36px;
            background: #1d1d1d;

            // Clock label on left
            clock := Text {
                x: 12px; y: 0px;
                height: parent.height;
                text: root.clock-text;
                color: #e0e0e0;
                font-size: 13px;
                vertical-alignment: center;
            }

            // Quit button on right
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

        // Wayland client surface (shown below panel)
        if root.client-visible: client-area := Rectangle {
            x: root.client-x * 1px;
            y: root.client-y * 1px;
            width: root.client-w * 1px;
            height: root.client-h * 1px;
            background: #2a2a2a; // placeholder if no texture yet

            // Client texture image
            client-img := Image {
                x: 0px; y: 0px;
                width: parent.width;
                height: parent.height;
                source: root.client-texture;
                image-fit: fill;
            }

            // TouchArea to intercept pointer events for forwarding to wayland client
            ta := TouchArea {
                pointer-event(pe) => {
                    root.client-clicked(self.mouse-x / 1px, self.mouse-y / 1px);
                }
            }
        }

        // Desktop background
        background := Rectangle {
            x: 0px; y: 0px;
            width: parent.width; height: parent.height;
            background: #1e1e2e; // dark purple-ish desktop
            z: -1;
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Wayland compositor state
// ──────────────────────────────────────────────────────────────────────────────

mod wayland_state;
mod platform;
mod renderer;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "compositor_slint_spike=debug,smithay=warn".parse().unwrap()),
        )
        .init();

    info!("Starting Slint-compositor spike");

    // Run the compositor
    renderer::run()
}
