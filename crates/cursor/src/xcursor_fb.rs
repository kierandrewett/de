//! Xcursor bitmap fallback loader.

use xcursor::parser::Image;

/// Load a cursor from an xcursor theme, picking the frame closest to `physical_size`.
///
/// Returns `Ok(None)` if the cursor name is not present in the theme.
/// Returns `Ok(Some(_))` on success.
/// Returns `Err(_)` on I/O or parse failure.
pub fn load_xcursor(
    theme: &xcursor::CursorTheme,
    name: &str,
    physical_size: u32,
) -> crate::Result<Option<crate::CachedCursor>> {
    // load_icon returns the path to the .cursor file, if found.
    let path = match theme.load_icon(name) {
        Some(p) => p,
        None => return Ok(None),
    };

    let raw = std::fs::read(&path)?;
    let images = match xcursor::parser::parse_xcursor(&raw) {
        Some(imgs) if !imgs.is_empty() => imgs,
        _ => return Ok(None),
    };

    let best = best_match(&images, physical_size);

    // pixels_rgba is already in R,G,B,A byte order — use it directly.
    let pixels = best.pixels_rgba.clone();

    Ok(Some(crate::CachedCursor {
        pixels,
        width: best.width,
        height: best.height,
        hotspot_x: best.xhot as i32,
        hotspot_y: best.yhot as i32,
    }))
}

/// Select the image whose nominal size is closest to `target`.
fn best_match(images: &[Image], target: u32) -> &Image {
    images
        .iter()
        .min_by_key(|img| {
            let diff = img.size as i64 - target as i64;
            diff.unsigned_abs()
        })
        .expect("images is non-empty (checked by caller)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_missing_cursor_returns_none() {
        let theme = xcursor::CursorTheme::load("__nonexistent_theme_xyz__");
        let result = load_xcursor(&theme, "left_ptr", 24).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_best_match_exact() {
        let images: Vec<Image> = vec![
            Image { size: 24, width: 24, height: 24, xhot: 0, yhot: 0, delay: 0,
                    pixels_rgba: vec![], pixels_argb: vec![] },
            Image { size: 48, width: 48, height: 48, xhot: 0, yhot: 0, delay: 0,
                    pixels_rgba: vec![], pixels_argb: vec![] },
        ];
        assert_eq!(best_match(&images, 24).size, 24);
        assert_eq!(best_match(&images, 48).size, 48);
        assert_eq!(best_match(&images, 32).size, 24); // closer to 24 than 48
        assert_eq!(best_match(&images, 40).size, 48); // closer to 48 than 24
    }
}
