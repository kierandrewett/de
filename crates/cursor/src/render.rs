//! SVG rasterisation using `resvg` / `usvg` / `tiny-skia`.

use crate::ACCENT_PLACEHOLDER;

/// Rasterise an SVG string to premultiplied RGBA bytes at the given square size.
///
/// If `accent_color` is set the placeholder colour [`ACCENT_PLACEHOLDER`] is
/// substituted before parsing, enabling live theme recolouring without touching
/// the cached bytes.
///
/// Returns raw RGBA pixel bytes (4 bytes per pixel, row-major, premultiplied alpha).
pub fn render_svg(
    svg_data: &str,
    target_size: u32,
    accent_color: Option<[u8; 3]>,
) -> crate::Result<Vec<u8>> {
    if target_size == 0 {
        return Err(crate::CursorError::RenderFailed(0));
    }

    // Apply accent colour substitution before parsing (cheap string replace).
    let owned;
    let data: &str = match accent_color {
        Some(c) => {
            let hex = format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
            owned = svg_data.replace(ACCENT_PLACEHOLDER, &hex);
            &owned
        }
        None => svg_data,
    };

    let opt = usvg::Options::default();
    let tree = usvg::Tree::from_str(data, &opt)
        .map_err(|e| crate::CursorError::Svg(e.to_string()))?;

    let svg_size = tree.size();
    let sx = target_size as f32 / svg_size.width();
    let sy = target_size as f32 / svg_size.height();
    let transform = tiny_skia::Transform::from_scale(sx, sy);

    let mut pixmap = tiny_skia::Pixmap::new(target_size, target_size)
        .ok_or(crate::CursorError::RenderFailed(target_size))?;

    resvg::render(&tree, transform, &mut pixmap.as_mut());

    Ok(pixmap.data().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal valid SVG for rendering tests.
    const TEST_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24">
  <circle cx="12" cy="12" r="10" fill="#1d99f3"/>
</svg>"##;

    #[test]
    fn test_render_basic() {
        let pixels = render_svg(TEST_SVG, 24, None).unwrap();
        assert_eq!(pixels.len(), 24 * 24 * 4);
        // Should have at least some non-zero pixels (circle is drawn)
        assert!(pixels.iter().any(|&b| b > 0));
    }

    #[test]
    fn test_render_at_48() {
        let pixels = render_svg(TEST_SVG, 48, None).unwrap();
        assert_eq!(pixels.len(), 48 * 48 * 4);
    }

    #[test]
    fn test_render_at_96() {
        let pixels = render_svg(TEST_SVG, 96, None).unwrap();
        assert_eq!(pixels.len(), 96 * 96 * 4);
    }

    #[test]
    fn test_render_never_panics_1_to_512() {
        for size in 1u32..=512 {
            let _ = render_svg(TEST_SVG, size, None);
        }
    }

    #[test]
    fn test_render_size_zero_is_err() {
        assert!(render_svg(TEST_SVG, 0, None).is_err());
    }

    #[test]
    fn test_accent_color_substitution() {
        let red: [u8; 3] = [0xff, 0x00, 0x00];
        let pixels_default = render_svg(TEST_SVG, 24, None).unwrap();
        let pixels_accent = render_svg(TEST_SVG, 24, Some(red)).unwrap();
        // Different colour means different pixel data
        assert_ne!(pixels_default, pixels_accent);
    }

    #[test]
    fn test_invalid_svg_is_err() {
        let result = render_svg("not valid svg at all <<<", 24, None);
        assert!(result.is_err());
    }
}
