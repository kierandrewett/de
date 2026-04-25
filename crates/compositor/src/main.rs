//! Compositor binary entry point. Backend (winit dev / udev prod) is selected at runtime.
//!
//! Subagent ownership:
//! - `wayland/` — protocol handlers (subagent 07)
//! - `render/`  — per-window rendering pipeline (subagent 08)
//! - `shell/`   — window management, snapping, alt-tab (subagent 09)
//! - `state.rs` — shared `State` struct (touched by all three; merge carefully)

use clap::Parser;

mod focus;
mod input;
mod ipc;
mod render;
mod shell;
mod state;
mod udev;
mod wayland;
mod winit;

#[derive(Parser, Debug)]
#[command(name = "compositor")]
struct Cli {
    /// Run as a nested compositor inside a winit window (development mode).
    #[arg(long, conflicts_with = "tty_udev")]
    winit: bool,

    /// Run as a real compositor on a TTY using DRM/KMS + libinput (production).
    #[arg(long = "tty-udev", conflicts_with = "winit")]
    tty_udev: bool,

    /// Exit after this many seconds (used by integration tests).
    #[arg(long)]
    test_timeout: Option<u64>,
}

fn main() -> anyhow::Result<()> {
    // Default to `info` so the wayland socket name is visible without
    // RUST_LOG being set. Override with RUST_LOG=… as usual.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let cli = Cli::parse();

    // Auto-detect backend when no flag is given: prefer winit if a display is
    // available (WAYLAND_DISPLAY or DISPLAY), otherwise fall back to udev.
    let use_winit = if cli.winit {
        true
    } else if cli.tty_udev {
        false
    } else {
        std::env::var("WAYLAND_DISPLAY").is_ok() || std::env::var("DISPLAY").is_ok()
    };

    if use_winit {
        tracing::info!("Starting compositor (winit dev mode)");
        crate::winit::run()?;
    } else {
        tracing::info!("Starting compositor (tty-udev production mode)");
        crate::udev::run()?;
    }

    Ok(())
}
