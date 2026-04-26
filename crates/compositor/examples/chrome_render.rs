//! Standalone visual check for the iced-rendered title bar chrome.
//!
//! Calls the same iced pipeline that the compositor uses (via
//! `IcedChrome::render_pixmap`), so the output PNGs are exactly what would
//! land on screen — handy for iterating on icon SVGs / styling without
//! restarting a nested wayland session.
//!
//! Usage: `cargo run --example chrome_render -p compositor`

#[path = "../src/chrome_iced.rs"]
mod chrome_iced;

use chrome_iced::{ChromeTheme, IcedChrome};

fn main() {
    let mut chrome = IcedChrome::default();

    let cases = [
        ("/tmp/chrome-light-1x.png", 600u32, 33u32, 1.0f64, ChromeTheme::Light, true),
        ("/tmp/chrome-dark-1x.png", 600, 33, 1.0, ChromeTheme::Dark, true),
        ("/tmp/chrome-light-2x.png", 1200, 66, 2.0, ChromeTheme::Light, true),
        ("/tmp/chrome-light-unfocused-1x.png", 600, 33, 1.0, ChromeTheme::Light, false),
    ];

    for (path, w, h, scale, theme, focused) in cases {
        chrome.set_theme(theme);
        let pixmap = chrome
            .render_pixmap(w, h, scale, "Hello, world  —  iced chrome", focused)
            .expect("render_pixmap returned None");
        pixmap.save_png(path).expect("save_png");
        eprintln!("wrote {path} ({}x{})", pixmap.width(), pixmap.height());
    }
}
