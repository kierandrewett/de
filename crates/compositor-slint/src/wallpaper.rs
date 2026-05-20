//! Wallpaper loading — finds a wallpaper image on disk and converts it to a
//! Slint `Image` for the `Compositor.wallpaper` property.
//!
//! Search order (home-relative, in order):
//!   1. `~/Documents/wallpaper_light.{jpg,jpeg,png}` (light-mode default)
//!   2. `~/Documents/wallpaper_dark.{jpg,jpeg,png}`
//!   3. `~/Documents/wallpaper.{jpg,jpeg,png}`
//!   4. `~/Pictures/wallpaper.{jpg,jpeg,png}`
//!   5. `~/.config/myDE/wallpaper.{jpg,jpeg,png}`
//!   6. `/usr/share/backgrounds/...` (distro fallbacks)

use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

/// Find the first wallpaper image that exists on disk.
pub fn find_wallpaper_path() -> Option<PathBuf> {
    let home = home_dir();

    let candidates: &[(&str, &[&str])] = &[
        ("Documents/wallpaper_light", &["jpg", "jpeg", "png"]),
        ("Documents/wallpaper_dark", &["jpg", "jpeg", "png"]),
        ("Documents/wallpaper", &["jpg", "jpeg", "png"]),
        ("Pictures/wallpaper", &["jpg", "jpeg", "png"]),
        (".config/myDE/wallpaper", &["jpg", "jpeg", "png"]),
    ];

    for (stem, exts) in candidates {
        for ext in *exts {
            let p = home.join(format!("{stem}.{ext}"));
            if p.exists() {
                debug!("found wallpaper at {:?}", p);
                return Some(p);
            }
        }
    }

    let absolute: &[(&str, &[&str])] = &[
        ("/usr/share/backgrounds/default", &["jpg", "png"]),
        ("/usr/share/backgrounds/gnome/symbolic-d", &["png"]),
        (
            "/usr/share/backgrounds/cosmic/A_stormy_stellar_nursery_esa_379309",
            &["jpg"],
        ),
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

/// True if the wallpaper file looks like a "light" wallpaper (filename
/// contains `light`). Used to default the theme to Light when the file is
/// `wallpaper_light.*`.
pub fn looks_light(path: &Path) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase().contains("light"))
        .unwrap_or(false)
}

/// Pick a wallpaper for a given mode tag (`"light"` / `"dark"`). Looks for
/// `~/Documents/wallpaper_<tag>.{jpg,jpeg,png}` first, then falls back to the
/// generic `find_wallpaper_path()` candidate.
pub fn find_wallpaper_for_mode(tag: &str) -> Option<PathBuf> {
    let home = home_dir();
    for ext in ["jpg", "jpeg", "png"] {
        let p = home.join(format!("Documents/wallpaper_{}.{}", tag, ext));
        if p.exists() {
            return Some(p);
        }
    }
    find_wallpaper_path()
}

/// Load an image from an explicit path.
pub fn load_from_path(path: &Path) -> Option<slint::Image> {
    let img = image::open(path)
        .map_err(|e| warn!("failed to open wallpaper {:?}: {}", path, e))
        .ok()?;

    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();

    let pixel_buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        rgba.as_raw(),
        width,
        height,
    );

    let slint_image = slint::Image::from_rgba8(pixel_buf);
    info!("wallpaper loaded: {:?} ({}x{})", path, width, height);
    Some(slint_image)
}

/// Load + pre-blur a wallpaper for use as a panel/dock backdrop.
pub fn load_blurred_from_path(path: &Path, sigma: f32) -> Option<slint::Image> {
    let img = image::open(path)
        .map_err(|e| warn!("failed to open wallpaper for blur {:?}: {}", path, e))
        .ok()?;

    let (orig_w, orig_h) = (img.width(), img.height());
    let target_w: u32 = 640;
    let target_h: u32 = ((orig_h as f32 / orig_w as f32) * target_w as f32) as u32;
    let small = img.resize_exact(target_w, target_h, image::imageops::FilterType::Triangle);
    let blurred = image::imageops::blur(&small.to_rgba8(), sigma);

    let (w, h) = blurred.dimensions();
    let pixel_buf =
        slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(blurred.as_raw(), w, h);
    let slint_image = slint::Image::from_rgba8(pixel_buf);
    info!(
        "wallpaper blurred backdrop ready: {}x{} (sigma={})",
        w, h, sigma
    );
    Some(slint_image)
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/root"))
}
