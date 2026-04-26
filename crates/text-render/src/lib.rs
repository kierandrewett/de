//! GTK4/Figma-quality grayscale text renderer for first-party apps and
//! the compositor's SSD chrome.
//!
//! Why this lives in its own crate: the panel, dock, launcher, and any
//! future native shell apps want exactly the same crisp-but-soft text
//! the compositor draws on title bars. Pulling the renderer out of the
//! compositor lets every shell app share a single tuned implementation
//! (and a single bundled font) instead of inheriting iced's defaults
//! and re-discovering the same gamma/hint trade-offs.
//!
//! Approach (see also the GTK Development Blog "On fractional scales,
//! fonts and hinting", March 2024):
//!
//!   - **Grayscale alpha mask** (`Format::Alpha`). NOT subpixel. GTK4
//!     explicitly says *"Our antialiasing for fonts is always
//!     grayscale"* because compositing pipelines without component
//!     alpha can't carry per-channel coverage through transforms.
//!   - **Unhinted outlines** (`hint(false)`). GTK4 calls this the
//!     "optimize for uniform spacing" mode. With hinting on, stems
//!     snap to integer pixels and the text reads as "stamped" at small
//!     sizes — which is the harsh look design tools avoid.
//!   - **Subpixel-X glyph positioning** (carried by cosmic-text's
//!     `glyph.physical()` `x_bin`). Keeps inter-glyph spacing uniform
//!     without snapping each glyph to whole pixels.
//!   - **Gamma-correct blend** (sRGB → linear → blend → sRGB) via a
//!     256-entry LUT.
//!   - **Size-adaptive stem darkening** as a coverage curve. Same
//!     intent as FreeType's stem widening; ramped down to zero above
//!     ~24 px. See [`stem_darken_for_size`].
//!
//! The destination pixmap is assumed opaque-where-text-lands so the
//! blend is straight `dst = fg*α + dst*(1-α)` per channel without
//! unwinding premultiplied alpha. Re-export of `cosmic_text::Weight`
//! and `cosmic_text::FontSystem` is provided so callers don't need a
//! second `cosmic-text` dep just to spell those types.

use std::sync::OnceLock;

use cosmic_text::{
    Attrs, Buffer, CacheKeyFlags, Family, FontSystem, Metrics, Shaping, Weight, Wrap,
};
use swash::scale::{image::Content, Render, ScaleContext, Source};
use swash::zeno::{Format, Vector};

pub use cosmic_text;

/// Bundled font bytes for GNOME's Cantarell variable. Apps can register
/// this once into the global cosmic-text font system to guarantee
/// consistent text rendering across processes.
///
/// ```ignore
/// use std::borrow::Cow;
/// font_system.write().unwrap().load_font(Cow::Borrowed(text_render::CANTARELL_VF));
/// ```
pub const CANTARELL_VF: &[u8] = include_bytes!("../assets/Cantarell-VF.otf");
/// Family name to pass as the `family` argument when using the bundled
/// font from [`CANTARELL_VF`].
pub const CANTARELL_FAMILY: &str = "Cantarell";

/// Bundled font bytes for the Inter Variable typeface — the macOS-style
/// system UI font used for window-chrome titles and any other DE chrome
/// per `WINDOW_SPEC.md`. Variable across the full 100–900 weight axis.
pub const INTER_VF: &[u8] = include_bytes!("../assets/Inter-VF.ttf");
/// Family name to pass as the `family` argument when using the bundled
/// font from [`INTER_VF`]. The .ttf advertises itself as
/// `Inter Variable`, not `Inter`.
pub const INTER_FAMILY: &str = "Inter Variable";

/// All UI fonts bundled by this crate, in the order chrome should prefer
/// them. Callers register these once at startup with their cosmic-text
/// font system so text shaping doesn't fall back to whatever fontconfig
/// happens to resolve on the host.
///
/// ```ignore
/// let mut fs = font_system.write().unwrap();
/// for &bytes in text_render::BUNDLED_UI_FONTS {
///     fs.load_font(std::borrow::Cow::Borrowed(bytes));
/// }
/// ```
///
/// Without this, asking cosmic-text for `"Inter Variable"` at e.g.
/// `Weight::MEDIUM` silently picks an unrelated fallback face and the
/// glyphs come out looking nothing like Inter.
pub const BUNDLED_UI_FONTS: &[&[u8]] = &[INTER_VF, CANTARELL_VF];

/// Maximum stem-darkening exponent at small sizes. Applied as a
/// coverage curve `coverage' = coverage^(1/k)`: `k > 1` thickens
/// partial-coverage edges. Same idea as FreeType's stem darkening
/// (which adjusts the outline pre-raster) but done in coverage space
/// where Skia ("contrast") and DirectWrite ("ClearType gamma") apply
/// theirs. The actual exponent ramps with size — see
/// [`stem_darken_for_size`].
pub const STEM_DARKEN_MAX: f32 = 1.15;
/// Font size (physical px) at which stem darkening reaches its
/// maximum. Below this, exponent stays at `STEM_DARKEN_MAX`.
pub const STEM_DARKEN_FULL_BELOW_PX: f32 = 9.0;
/// Font size (physical px) at which stem darkening drops to 1.0
/// (off). Mirrors FreeType's curve — stem widening tapers to zero
/// around 23 px because the glyph itself has enough body for shape
/// fidelity to carry the perceived weight.
pub const STEM_DARKEN_NONE_ABOVE_PX: f32 = 24.0;

/// Compute the size-adaptive coverage-curve exponent. Heaviest at very
/// small sizes (where each AA edge pixel matters a lot for perceived
/// weight), tapering to none at chunky sizes (where the rendered glyph
/// already has enough body).
pub fn stem_darken_for_size(size_px: f32) -> f32 {
    if size_px >= STEM_DARKEN_NONE_ABOVE_PX {
        1.0
    } else if size_px <= STEM_DARKEN_FULL_BELOW_PX {
        STEM_DARKEN_MAX
    } else {
        let t = (STEM_DARKEN_NONE_ABOVE_PX - size_px)
            / (STEM_DARKEN_NONE_ABOVE_PX - STEM_DARKEN_FULL_BELOW_PX);
        1.0 + (STEM_DARKEN_MAX - 1.0) * t
    }
}

/// Lay out `text` and draw it centered both axes inside the supplied
/// physical pixmap region.
///
/// `bar_width_phys` / `bar_height_phys` are the physical pixel dimensions
/// of the area to centre into. `font_size_phys` is the physical-pixel
/// font size (logical_size * scale). `color_rgba` is the foreground
/// colour as straight (non-premultiplied) RGBA.
pub fn render_centered(
    pixmap: &mut tiny_skia::PixmapMut<'_>,
    font_system: &mut FontSystem,
    text: &str,
    family: &str,
    weight: Weight,
    font_size_phys: f32,
    color_rgba: [u8; 4],
    bar_width_phys: u32,
    bar_height_phys: u32,
) {
    if text.is_empty() || bar_width_phys == 0 || bar_height_phys == 0 {
        return;
    }

    let line_height = font_size_phys * 1.2;
    let metrics = Metrics::new(font_size_phys, line_height);
    let mut buffer = Buffer::new(font_system, metrics);
    buffer.set_size(font_system, Some(bar_width_phys as f32), Some(line_height));
    buffer.set_wrap(font_system, Wrap::None);

    let attrs = Attrs::new()
        .family(Family::Name(family))
        .weight(weight);
    buffer.set_text(font_system, text, attrs, Shaping::Advanced);
    buffer.shape_until_scroll(font_system, false);

    let Some(run) = buffer.layout_runs().next() else {
        return;
    };

    let text_w = run.line_w;
    let offset_x = (bar_width_phys as f32 - text_w) * 0.5;
    let offset_y = (bar_height_phys as f32 - line_height) * 0.5;

    draw_run(
        pixmap,
        font_system,
        &buffer,
        offset_x,
        offset_y,
        font_size_phys,
        color_rgba,
    );
}

/// Lay out `text` and draw it left-aligned at `(x, y)` (physical pixels,
/// `y` is the top of the line box). Useful for panels / status bars that
/// want a specific anchor instead of full centering.
pub fn render_at(
    pixmap: &mut tiny_skia::PixmapMut<'_>,
    font_system: &mut FontSystem,
    text: &str,
    family: &str,
    weight: Weight,
    font_size_phys: f32,
    color_rgba: [u8; 4],
    x: f32,
    y: f32,
    max_width_phys: u32,
) {
    if text.is_empty() || max_width_phys == 0 {
        return;
    }

    let line_height = font_size_phys * 1.2;
    let metrics = Metrics::new(font_size_phys, line_height);
    let mut buffer = Buffer::new(font_system, metrics);
    buffer.set_size(font_system, Some(max_width_phys as f32), Some(line_height));
    buffer.set_wrap(font_system, Wrap::None);

    let attrs = Attrs::new()
        .family(Family::Name(family))
        .weight(weight);
    buffer.set_text(font_system, text, attrs, Shaping::Advanced);
    buffer.shape_until_scroll(font_system, false);

    draw_run(
        pixmap,
        font_system,
        &buffer,
        x,
        y,
        font_size_phys,
        color_rgba,
    );
}

/// Measure the width (physical px) `text` would occupy at the given
/// font / size / weight. Useful for laying out panel widgets that need
/// to size containers around their text without rendering twice.
pub fn measure_width(
    font_system: &mut FontSystem,
    text: &str,
    family: &str,
    weight: Weight,
    font_size_phys: f32,
) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    let line_height = font_size_phys * 1.2;
    let metrics = Metrics::new(font_size_phys, line_height);
    let mut buffer = Buffer::new(font_system, metrics);
    buffer.set_size(font_system, Some(f32::MAX), Some(line_height));
    buffer.set_wrap(font_system, Wrap::None);
    let attrs = Attrs::new()
        .family(Family::Name(family))
        .weight(weight);
    buffer.set_text(font_system, text, attrs, Shaping::Advanced);
    buffer.shape_until_scroll(font_system, false);
    buffer.layout_runs().next().map(|r| r.line_w).unwrap_or(0.0)
}

/// Shared raster path used by [`render_centered`] and [`render_at`].
/// `offset_x` / `offset_y` are the line-box origin in physical pixels.
fn draw_run(
    pixmap: &mut tiny_skia::PixmapMut<'_>,
    font_system: &mut FontSystem,
    buffer: &Buffer,
    offset_x: f32,
    offset_y: f32,
    font_size_phys: f32,
    color_rgba: [u8; 4],
) {
    let Some(run) = buffer.layout_runs().next() else {
        return;
    };

    let mut scale_ctx = ScaleContext::new();
    let line_y_phys = (run.line_y + offset_y).round() as i32;

    let lut = srgb_to_linear_lut();
    let fg_lin = [
        lut[color_rgba[0] as usize],
        lut[color_rgba[1] as usize],
        lut[color_rgba[2] as usize],
    ];
    let darken = stem_darken_for_size(font_size_phys);

    for glyph in run.glyphs.iter() {
        let physical = glyph.physical((offset_x, 0.0), 1.0);

        let Some(font) = font_system.get_font(physical.cache_key.font_id) else {
            continue;
        };

        let mut scaler = scale_ctx
            .builder(font.as_swash())
            .size(f32::from_bits(physical.cache_key.font_size_bits))
            // Unhinted: outlines rasterized at design metrics with no
            // stem snapping. Pairs with grayscale AA + subpixel-X
            // positioning to match the GTK4 / Figma look.
            .hint(false)
            .build();

        let subpx_offset = Vector::new(
            physical.cache_key.x_bin.as_float(),
            physical.cache_key.y_bin.as_float(),
        );

        let italic = physical
            .cache_key
            .flags
            .contains(CacheKeyFlags::FAKE_ITALIC);

        let mut render = Render::new(&[Source::Outline]);
        render
            .format(Format::Alpha)
            .offset(subpx_offset);
        if italic {
            render.transform(Some(swash::zeno::Transform::skew(
                swash::zeno::Angle::from_degrees(14.0),
                swash::zeno::Angle::from_degrees(0.0),
            )));
        }
        let Some(image) = render.render(&mut scaler, physical.cache_key.glyph_id) else {
            continue;
        };

        if image.content != Content::Mask {
            continue;
        }

        let dst_x = physical.x + image.placement.left;
        let dst_y = physical.y - image.placement.top + line_y_phys;
        blend_alpha_gamma(
            pixmap,
            dst_x,
            dst_y,
            image.placement.width,
            image.placement.height,
            &image.data,
            fg_lin,
            darken,
        );
    }
}

/// Grayscale alpha blend in linear-light space. `src` is 1 byte per
/// pixel (coverage from `Format::Alpha`). `fg_lin` is the foreground
/// in linear-light, precomputed once by the caller. `darken` is the
/// stem-darkening exponent — applied as `coverage^(1/darken)` before
/// the blend mix.
fn blend_alpha_gamma(
    pixmap: &mut tiny_skia::PixmapMut<'_>,
    dst_x: i32,
    dst_y: i32,
    src_w: u32,
    src_h: u32,
    src: &[u8],
    fg_lin: [f32; 3],
    darken: f32,
) {
    // Precomputed so the per-pixel powf doesn't redo the division.
    // When darken == 1.0 this is also 1.0 and the powf is identity, so
    // no need to special-case it out.
    let inv_darken = 1.0 / darken;
    let lut = srgb_to_linear_lut();
    let pm_w = pixmap.width() as i32;
    let pm_h = pixmap.height() as i32;
    let stride = pm_w as usize * 4;
    let dst = pixmap.data_mut();

    for sy in 0..src_h as i32 {
        let py = dst_y + sy;
        if py < 0 || py >= pm_h {
            continue;
        }
        let row_off = (sy as usize) * src_w as usize;
        for sx in 0..src_w as i32 {
            let px = dst_x + sx;
            if px < 0 || px >= pm_w {
                continue;
            }
            let cov = src[row_off + sx as usize];
            if cov == 0 {
                continue;
            }
            let a = (cov as f32 / 255.0).powf(inv_darken);
            let inv_a = 1.0 - a;
            let di = (py as usize) * stride + (px as usize) * 4;
            let dr_lin = lut[dst[di] as usize];
            let dg_lin = lut[dst[di + 1] as usize];
            let db_lin = lut[dst[di + 2] as usize];
            let or = fg_lin[0] * a + dr_lin * inv_a;
            let og = fg_lin[1] * a + dg_lin * inv_a;
            let ob = fg_lin[2] * a + db_lin * inv_a;
            dst[di] = linear_to_srgb_u8(or);
            dst[di + 1] = linear_to_srgb_u8(og);
            dst[di + 2] = linear_to_srgb_u8(ob);
        }
    }
}

fn srgb_to_linear_lut() -> &'static [f32; 256] {
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut a = [0.0f32; 256];
        for (i, slot) in a.iter_mut().enumerate() {
            let s = i as f32 / 255.0;
            *slot = if s <= 0.04045 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            };
        }
        a
    })
}

fn linear_to_srgb_u8(lin: f32) -> u8 {
    let l = lin.clamp(0.0, 1.0);
    let s = if l <= 0.0031308 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}
