//! Wallpaper loading — finds a wallpaper image on disk and converts it to a
//! Slint `Image` for the `Compositor.wallpaper` property.
//!
//! Search order:
//!   1. `~/Pictures/wallpaper.{jpg,jpeg,png}`
//!   2. `~/.config/myDE/wallpaper.{jpg,jpeg,png}`
//!   3. `/usr/share/backgrounds/default.{jpg,png}`
//!
//! Returns `None` if no wallpaper is found; the caller should log a warning
//! and leave the default `#1e1e2e` background colour.

use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

/// Find the first wallpaper image that exists on disk.
fn find_wallpaper_path() -> Option<PathBuf> {
    let home = home_dir();

    let candidates: &[(&str, &[&str])] = &[
        ("Pictures/wallpaper", &["jpg", "jpeg", "png"]),
        (".config/myDE/wallpaper", &["jpg", "jpeg", "png"]),
    ];

    // Home-relative candidates.
    for (stem, exts) in candidates {
        for ext in *exts {
            let p = home.join(format!("{stem}.{ext}"));
            if p.exists() {
                debug!("found wallpaper at {:?}", p);
                return Some(p);
            }
        }
    }

    // Absolute fallbacks.
    let absolute: &[(&str, &[&str])] = &[
        ("/usr/share/backgrounds/default", &["jpg", "png"]),
        // Common distro paths.
        ("/usr/share/backgrounds/gnome/symbolic-d", &["png"]),
        ("/usr/share/backgrounds/cosmic/A_stormy_stellar_nursery_esa_379309", &["jpg"]),
    ];
    for (stem, exts) in absolute {
        for ext in *exts {
            let p = PathBuf::from(format!("{stem}.{ext}"));
            if p.exists() {
                debug!("found wallpaper at {:?}", p);
                return Some(p);
            }
        }
    }

    None
}

/// Load a wallpaper image from disk and return a Slint `Image`.
///
/// Returns `None` if no suitable file is found or decoding fails.
pub fn load() -> Option<slint::Image> {
    let path = find_wallpaper_path()?;
    load_from_path(&path)
}

/// Load an image from an explicit path.
pub fn load_from_path(path: &Path) -> Option<slint::Image> {
    let img = image::open(path)
        .map_err(|e| warn!("failed to open wallpaper {:?}: {}", path, e))
        .ok()?;

    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();

    // Slint expects RGBA8 (non-premultiplied) for `from_rgba8`.
    let pixel_buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        rgba.as_raw(),
        width,
        height,
    );

    let slint_image = slint::Image::from_rgba8(pixel_buf);
    info!("wallpaper loaded: {:?} ({}x{})", path, width, height);
    Some(slint_image)
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/root"))
}
