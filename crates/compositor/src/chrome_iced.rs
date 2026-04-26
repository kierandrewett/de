//! Iced-rendered SSD title-bar chrome.
//!
//! Each window's title bar is laid out as an iced widget tree (a [`Row`] of
//! three control [`Button`]s plus a [`Text`] widget for the window title),
//! rendered offscreen with [`iced_tiny_skia::Renderer`] into a
//! [`tiny_skia::Pixmap`], and copied as BGRA into a [`MemoryRenderBuffer`].
//!
//! The result is cached per `(size, scale, focus, title)`; on a hit we hand
//! back the cached buffer with no allocation. Eviction is coarse — when the
//! cache exceeds [`MAX_CACHE_ENTRIES`] we clear it.
//!
//! There is no event loop here: we run [`UserInterface::build`] + `draw`
//! once per state change. Click handling is owned by the input layer, which
//! hit-tests the same logical button rects published by [`crate::shell`].

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::OnceLock;

use iced_core::{
    alignment, mouse,
    renderer::Style,
    Background, Border, Color, Element, Font, Length, Padding, Pixels, Rectangle, Size, Theme,
};
use iced_graphics::Viewport;
use iced_runtime::user_interface::{self, UserInterface};
use iced_tiny_skia::Renderer as IcedRenderer;
use iced_widget::image::Handle as ImageHandle;
use iced_widget::{button, container, image, row};

use smithay::backend::{
    allocator::Fourcc,
    renderer::element::memory::MemoryRenderBuffer,
};
use smithay::utils::Transform;

const CLOSE_SVG: &[u8] = include_bytes!("../assets/close.svg");
const MINIMIZE_SVG: &[u8] = include_bytes!("../assets/minimize.svg");
const MAXIMIZE_SVG: &[u8] = include_bytes!("../assets/maximize.svg");
/// Title text uses Inter Variable (per `WINDOW_SPEC.md`). All bundled
/// UI fonts are registered with the cosmic-text font system at chrome
/// init via [`text_render::BUNDLED_UI_FONTS`].
const TITLE_FONT_FAMILY: &str = text_render::INTER_FAMILY;

const ICON_PX: u32 = 17;
const BUTTON_PADDING: u16 = 2;
const BUTTON_SIZE_F32: f32 = 21.0;
const TITLE_FONT_SIZE: f32 = 14.0;
/// Pixel resolution used to pre-rasterize the icon SVGs. We render at
/// `ICON_PX × SUPERSAMPLE` so that — with `FilterMethod::Nearest` and the
/// chrome buffer composited 1:1 — every icon pixel hits exactly one
/// destination pixel without any bilinear smoothing.
const ICON_RASTER_PX: u32 = ICON_PX * SUPERSAMPLE;
/// Render the chrome at this multiplier above the output's physical
/// scale, then composite as a HiDPI buffer. 1× lets iced's grayscale
/// rasterizer hit the exact target pixels with no resampling either side.
const SUPERSAMPLE: u32 = 1;
const MAX_CACHE_ENTRIES: usize = 64;
/// Minimum logical bar width before the chrome will draw. Anything smaller
/// can't fit the three buttons + spacing + padding without iced's flex
/// layout producing zero-width quads (which trip a debug assert in
/// iced_tiny_skia).
const MIN_CHROME_WIDTH_LOGICAL: u32 = 200;

/// Iced widgets need a Message type; chrome buttons currently emit no events
/// at this layer (clicks are caught by the compositor's input handler before
/// reaching the offscreen UI).
#[derive(Debug, Clone, Copy)]
pub enum ChromeMessage {}

/// Light vs dark colour palette for the SSD chrome. Toggled at runtime via
/// [`IcedChrome::set_theme`]; the cache is keyed by theme so swapping
/// re-renders without discarding the entries for the unselected mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChromeTheme {
    Light,
    Dark,
}

fn icon_handles() -> &'static (ImageHandle, ImageHandle, ImageHandle) {
    static H: OnceLock<(ImageHandle, ImageHandle, ImageHandle)> = OnceLock::new();
    H.get_or_init(|| {
        (
            rasterize_icon(CLOSE_SVG),
            rasterize_icon(MINIMIZE_SVG),
            rasterize_icon(MAXIMIZE_SVG),
        )
    })
}

fn rasterize_icon(svg: &[u8]) -> ImageHandle {
    let svg_str = std::str::from_utf8(svg).expect("icon svg is utf8");
    let pixels = cursor::render::render_svg(svg_str, ICON_RASTER_PX, None)
        .expect("rasterise icon svg");
    ImageHandle::from_rgba(ICON_RASTER_PX, ICON_RASTER_PX, pixels)
}

// Title-bar palettes ────────────────────────────────────────────────────
//
// Values lifted directly from `WINDOW_SPEC.md`. We always render the
// *active* bar here; the inactive 0.75-opacity variant is produced at
// composite time by lerping the alpha of the rendered title-bar buffer.
//
// Light Active: bg #FFFFFF, text rgba(0,0,0,0.75)
// Dark  Active: bg #111318, text rgba(255,255,255,0.8)

fn bar_style_light_focused(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::WHITE)),
        text_color: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.75)),
        ..Default::default()
    }
}
fn bar_style_light_unfocused(_theme: &Theme) -> container::Style {
    // Same content as focused — the visual "fade" comes from the 0.75
    // opacity applied to the whole bar element at composite time.
    bar_style_light_focused(_theme)
}

// Window controls — circular pill background. Spec calls for
// rgba(0,0,0,0.1) base and a brighter hover/pressed state. iced's
// `button::Status` carries the live interaction state, so we lerp the
// fill brightness off it.
fn button_style_light(_theme: &Theme, status: button::Status) -> button::Style {
    let base_alpha = match status {
        button::Status::Pressed => 0.22,
        button::Status::Hovered => 0.16,
        _ => 0.10,
    };
    button::Style {
        background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, base_alpha))),
        text_color: Color::from_rgba(0.0, 0.0, 0.0, 0.8),
        border: Border { color: Color::TRANSPARENT, width: 0.0, radius: 10.5.into() },
        shadow: Default::default(),
    }
}

fn bar_style_dark_focused(_theme: &Theme) -> container::Style {
    // #111318 ≈ rgb(0.067, 0.075, 0.094)
    container::Style {
        background: Some(Background::Color(Color::from_rgb(0.067, 0.075, 0.094))),
        text_color: Some(Color::from_rgba(1.0, 1.0, 1.0, 0.80)),
        ..Default::default()
    }
}
fn bar_style_dark_unfocused(_theme: &Theme) -> container::Style {
    bar_style_dark_focused(_theme)
}
fn button_style_dark(_theme: &Theme, status: button::Status) -> button::Style {
    let base_alpha = match status {
        button::Status::Pressed => 0.24,
        button::Status::Hovered => 0.18,
        _ => 0.10,
    };
    button::Style {
        background: Some(Background::Color(Color::from_rgba(1.0, 1.0, 1.0, base_alpha))),
        text_color: Color::from_rgba(1.0, 1.0, 1.0, 0.8),
        border: Border { color: Color::TRANSPARENT, width: 0.0, radius: 10.5.into() },
        shadow: Default::default(),
    }
}

fn build_view<'a>(
    focused: bool,
    theme: ChromeTheme,
    bar_height_logical: f32,
) -> Element<'a, ChromeMessage, Theme, IcedRenderer> {
    // Pad the button box symmetrically so the buttons end up vertically
    // centred and inset by an equal amount from the right edge. This means
    // the visual gap auto-tracks the bar height — a 33 px bar yields a
    // 6 px inset, a 28 px bar would yield 3.5 px, etc.
    let button_inset = ((bar_height_logical - BUTTON_SIZE_F32) / 2.0).max(0.0);
    let (close, min, max) = icon_handles();
    let icon = |h: &ImageHandle| {
        image(h.clone())
            .width(ICON_PX as f32)
            .height(ICON_PX as f32)
            // Nearest avoids smoothing — the source pixmap is already at
            // the exact size iced will draw it (ICON_RASTER_PX matches
            // the supersampled target), so nearest = pixel-perfect.
            .filter_method(image::FilterMethod::Nearest)
    };
    let btn_style: fn(&Theme, button::Status) -> button::Style = match theme {
        ChromeTheme::Light => button_style_light,
        ChromeTheme::Dark => button_style_dark,
    };
    let btn = |h: &ImageHandle| button(icon(h)).padding(BUTTON_PADDING).style(btn_style);

    let bar_style: fn(&Theme) -> container::Style = match (theme, focused) {
        (ChromeTheme::Light, true) => bar_style_light_focused,
        (ChromeTheme::Light, false) => bar_style_light_unfocused,
        (ChromeTheme::Dark, true) => bar_style_dark_focused,
        (ChromeTheme::Dark, false) => bar_style_dark_unfocused,
    };

    // The title is drawn as a separate text pass after iced finishes
    // — see `text_render::render_centered`. So this view is just the
    // background fill + the right-aligned button block.
    container(
        container(
            row![btn(min), btn(max), btn(close)]
                .spacing(6)
                .align_y(alignment::Vertical::Center),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(alignment::Horizontal::Right)
        .align_y(alignment::Vertical::Center)
        .padding(Padding {
            top: button_inset,
            bottom: button_inset,
            left: button_inset,
            right: button_inset,
        }),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(bar_style)
    .into()
}

/// Foreground colour for the title text — pre-multiplied with the
/// active-state alpha from the spec. The unfocused fade is reproduced
/// by lowering the title-bar element's alpha at composite time, which
/// also fades the bar's background uniformly so the bar reads as a
/// single dimmer surface rather than re-tinting just the text.
fn title_color(theme: ChromeTheme, _focused: bool) -> [u8; 4] {
    let f = |c: f32| (c * 255.0).round() as u8;
    let (r, g, b, a) = match theme {
        // Light: rgba(0, 0, 0, 0.75)
        ChromeTheme::Light => (0.0, 0.0, 0.0, 0.75),
        // Dark: rgba(255, 255, 255, 0.8)
        ChromeTheme::Dark => (1.0, 1.0, 1.0, 0.80),
    };
    [f(r), f(g), f(b), f(a)]
}

#[derive(Hash, Eq, PartialEq, Clone)]
struct CacheKey {
    width_px: u32,
    height_px: u32,
    scale_milli: u32,
    focused: bool,
    theme: ChromeTheme,
    title: String,
}

/// Holds the persistent iced renderer and a per-window cache of rendered
/// title bars. The renderer keeps glyph + svg caches alive across frames so
/// only state changes pay the layout/raster cost.
pub struct IcedChrome {
    renderer: IcedRenderer,
    cache: HashMap<CacheKey, MemoryRenderBuffer>,
    theme: ChromeTheme,
}

impl IcedChrome {
    /// Switch the active palette. Cached buffers for the previous theme
    /// stay until evicted, so toggling back is cheap.
    pub fn set_theme(&mut self, theme: ChromeTheme) {
        self.theme = theme;
    }

    pub fn theme(&self) -> ChromeTheme {
        self.theme
    }
}

impl Default for IcedChrome {
    fn default() -> Self {
        // Bundle the title font so the chrome looks identical
        // regardless of what fontconfig resolves on the host. The font
        // system is process-global, so loading once at chrome init
        // applies to every Renderer instance.
        {
            let mut fs = iced_graphics::text::font_system()
                .write()
                .expect("font system rwlock");
            for &bytes in text_render::BUNDLED_UI_FONTS {
                fs.load_font(Cow::Borrowed(bytes));
            }
        }
        Self {
            renderer: IcedRenderer::new(Font::DEFAULT, Pixels(TITLE_FONT_SIZE)),
            cache: HashMap::new(),
            theme: ChromeTheme::Light,
        }
    }
}

impl IcedChrome {
    /// Render (or fetch from cache) the title bar for one window.
    /// `width_px` / `height_px` are physical pixels; `scale` is the output
    /// fractional scale (e.g. 1.0, 1.5, 2.0).
    pub fn render_title_bar(
        &mut self,
        width_px: u32,
        height_px: u32,
        scale: f64,
        title: &str,
        focused: bool,
    ) -> Option<&MemoryRenderBuffer> {
        if width_px == 0 || height_px == 0 {
            return None;
        }
        // Defensive: winit may report a 0 / non-finite scale before the
        // first surface configure. Iced's logical_size = physical/scale,
        // which becomes ±inf and trips its `is_normal` quad assertions.
        let scale = if !scale.is_finite() || scale <= 0.0 { 1.0 } else { scale };
        // The chrome can't lay out below the width of the controls block.
        // Below this, iced's flex layout would try to allocate negative
        // space to children and produce zero-width quads, which trips
        // iced_tiny_skia's debug assertion. Skip the chrome until the
        // window is wide enough for the controls + a sliver of title room.
        let min_chrome_width_logical = MIN_CHROME_WIDTH_LOGICAL;
        if (width_px as f64 / scale) < min_chrome_width_logical as f64 {
            return None;
        }
        let key = CacheKey {
            width_px,
            height_px,
            scale_milli: (scale * 1000.0).round() as u32,
            focused,
            theme: self.theme,
            title: title.to_owned(),
        };

        if !self.cache.contains_key(&key) {
            let buffer = self.render_uncached(width_px, height_px, scale, title, focused)?;
            if self.cache.len() >= MAX_CACHE_ENTRIES {
                self.cache.clear();
            }
            self.cache.insert(key.clone(), buffer);
        }
        self.cache.get(&key)
    }

    /// Render the chrome to a freshly-allocated tiny_skia pixmap. Exposed
    /// for the standalone visual-test example; the compositor uses
    /// [`render_title_bar`] which wraps this in a `MemoryRenderBuffer`.
    pub fn render_pixmap(
        &mut self,
        width_px: u32,
        height_px: u32,
        scale: f64,
        title: &str,
        focused: bool,
    ) -> Option<tiny_skia::Pixmap> {
        // Supersample so iced's grayscale rasterizer has more pixels to
        // work with. Smithay treats the resulting buffer as HiDPI via the
        // `buffer_scale` arg below and downsamples on composite.
        let pmw = width_px.saturating_mul(SUPERSAMPLE);
        let pmh = height_px.saturating_mul(SUPERSAMPLE);
        let render_scale = scale * SUPERSAMPLE as f64;

        let viewport = Viewport::with_physical_size(Size::new(pmw, pmh), render_scale);
        let logical = viewport.logical_size();
        let bounds = Size::new(logical.width, logical.height);

        let view = build_view(focused, self.theme, logical.height);
        let cache = user_interface::Cache::new();
        let mut ui = UserInterface::build(view, bounds, cache, &mut self.renderer);

        let _interaction = ui.draw(
            &mut self.renderer,
            &Theme::Light,
            &Style {
                text_color: Color::BLACK,
            },
            mouse::Cursor::Unavailable,
        );

        let mut pixmap = tiny_skia::Pixmap::new(pmw, pmh)?;
        let mut clip_mask = tiny_skia::Mask::new(pmw, pmh)?;
        let damage = [Rectangle {
            x: 0.0,
            y: 0.0,
            width: logical.width,
            height: logical.height,
        }];
        let overlay: [&str; 0] = [];
        self.renderer.draw(
            &mut pixmap.as_mut(),
            &mut clip_mask,
            &viewport,
            &damage,
            Color::TRANSPARENT,
            &overlay,
        );

        // Title text pass — draws directly onto the pixmap iced just
        // filled. Goes through the shared `text-render` crate so the
        // chrome and any first-party shell apps share a single tuned
        // glyph rasteriser (grayscale + unhinted + gamma + size-adaptive
        // stem darkening). See `text-render`'s lib.rs header for the
        // rationale.
        {
            let mut font_system = iced_graphics::text::font_system()
                .write()
                .expect("font system rwlock");
            text_render::render_centered(
                &mut pixmap.as_mut(),
                font_system.raw(),
                title,
                TITLE_FONT_FAMILY,
                // WINDOW_SPEC calls for Semi-Bold (600), but at 13 px on a
                // dark/light bar that reads as visually heavy. Using Medium
                // (500) keeps the title legible while letting it sit back
                // a little instead of competing with the window controls.
                cosmic_text::Weight::MEDIUM,
                TITLE_FONT_SIZE * render_scale as f32,
                title_color(self.theme, focused),
                pmw,
                pmh,
            );
        }

        Some(pixmap)
    }

    fn render_uncached(
        &mut self,
        width_px: u32,
        height_px: u32,
        scale: f64,
        title: &str,
        focused: bool,
    ) -> Option<MemoryRenderBuffer> {
        let pmw = width_px.saturating_mul(SUPERSAMPLE);
        let pmh = height_px.saturating_mul(SUPERSAMPLE);
        let mut pixmap = self.render_pixmap(width_px, height_px, scale, title, focused)?;
        // Bottom divider (per WINDOW_SPEC.md): 0.5 px solid
        //   - Light: #BBBBBB
        //   - Dark : rgba(0, 0, 0, 0.8)
        // Drawn into the bottom-most row of the pixmap so it appears as a
        // hairline between the title bar and the surface beneath.
        draw_bottom_divider(&mut pixmap, self.theme, pmw, pmh);
        // Clip the top corners only.
        clip_title_bar_top_corners(&mut pixmap, pmw, pmh);
        if std::env::var("DUMP_TITLE_BAR_MASK").is_ok() {
            let _ = pixmap.save_png("/tmp/title_bar_clipped.png");
            tracing::info!("dumped /tmp/title_bar_clipped.png");
        }

        // EXPERIMENT: skip the R↔B swap. tiny_skia outputs RGBA premul;
        // if smithay's Argb8888-on-LE actually means RGBA in memory, the
        // historic swap inverts the colours.
        let bytes = pixmap.take();

        Some(MemoryRenderBuffer::from_slice(
            &bytes,
            Fourcc::Argb8888,
            (pmw as i32, pmh as i32),
            SUPERSAMPLE as i32,
            Transform::Normal,
            None,
        ))
    }
}

/// Paint the spec-mandated 0.5 px bottom divider directly onto the
/// title-bar pixmap. We render at SUPERSAMPLE×, so the actual stroke
/// height is `SUPERSAMPLE.max(1)` rows — enough for the line to read
/// crisply at any scale.
fn draw_bottom_divider(
    pixmap: &mut tiny_skia::Pixmap,
    theme: ChromeTheme,
    pmw: u32,
    pmh: u32,
) {
    use tiny_skia::{Paint, Rect as SkRect, Transform};
    if pmw == 0 || pmh == 0 {
        return;
    }
    let stroke_h = (SUPERSAMPLE as f32 * 0.5).max(1.0);
    let y_top = (pmh as f32 - stroke_h).max(0.0);
    let Some(rect) = SkRect::from_xywh(0.0, y_top, pmw as f32, stroke_h) else {
        return;
    };
    let mut paint = Paint::default();
    let (r, g, b, a) = match theme {
        // Light: #BBBBBB ≈ rgb(0.733, 0.733, 0.733)
        ChromeTheme::Light => (0xBB, 0xBB, 0xBB, 0xFF),
        // Dark: rgba(0, 0, 0, 0.8)
        ChromeTheme::Dark => (0, 0, 0, (0.8 * 255.0) as u8),
    };
    paint.set_color_rgba8(r, g, b, a);
    paint.anti_alias = true;
    pixmap.fill_rect(rect, &paint, Transform::identity(), None);
}

/// Clip the top-left and top-right corners of a title-bar pixmap to the
/// shared squircle radius so the rendered bar matches the surface's
/// SDF-clipped corners.
///
/// `pmw`/`pmh` are the pixmap dimensions in physical (super-sampled) pixels.
///
/// The mask is built directly via `tiny_skia::PathBuilder` rather than via
/// the `rounding::SquirclePath` because the latter currently emits a
/// malformed clockwise traversal at the top-left corner (its TL exit
/// lands on the LEFT edge instead of the TOP edge, then a diagonal
/// `LineTo` cuts across the rectangle to the TR entry on the TOP edge).
/// That self-intersection makes FillRule::Winding stamp two overlapping
/// rounded shapes — visible as a "second" rounded rect inside the bar.
fn clip_title_bar_top_corners(pixmap: &mut tiny_skia::Pixmap, pmw: u32, pmh: u32) {
    use tiny_skia::{FillRule, Paint, PathBuilder, Transform};

    // Match `WindowTheme::default()`. Sourced from `WINDOW_SPEC.md`.
    const CORNER_RADIUS_LOGICAL: f32 = 14.0;

    if pmw == 0 || pmh == 0 {
        return;
    }
    let r = (CORNER_RADIUS_LOGICAL * SUPERSAMPLE as f32).min(pmw as f32 / 2.0).min(pmh as f32);
    let w = pmw as f32;
    let h = pmh as f32;

    // Standard clockwise rounded-rect path with the BOTTOM corners squared
    // off — the title bar sits flush against the SDF-clipped surface
    // below it, so its bottom edge must be a straight line. We use cubic
    // Bézier corners with the standard 0.5523 control-point coefficient
    // (matches a circular quarter arc to ~0.02 % error).
    const K: f32 = 0.5522847; // 4 * (sqrt(2) - 1) / 3
    let mut pb = PathBuilder::new();
    // Start at top-left, just past the corner curve
    pb.move_to(r, 0.0);
    // Top edge to top-right corner start
    pb.line_to(w - r, 0.0);
    // Top-right corner: from (w-r, 0) to (w, r)
    pb.cubic_to(w - r + r * K, 0.0, w, r - r * K, w, r);
    // Right edge straight down to bottom-right
    pb.line_to(w, h);
    // Bottom edge straight to bottom-left
    pb.line_to(0.0, h);
    // Left edge up to top-left corner end
    pb.line_to(0.0, r);
    // Top-left corner: from (0, r) to (r, 0)
    pb.cubic_to(0.0, r - r * K, r - r * K, 0.0, r, 0.0);
    pb.close();
    let Some(path) = pb.finish() else {
        return;
    };

    let Some(mut mask) = tiny_skia::Pixmap::new(pmw, pmh) else {
        return;
    };
    let mut mask_paint = Paint::default();
    mask_paint.set_color_rgba8(255, 255, 255, 255);
    mask_paint.anti_alias = true;
    mask.fill_path(
        &path,
        &mask_paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    if std::env::var("DUMP_TITLE_BAR_MASK").is_ok() {
        let _ = mask.save_png("/tmp/title_bar_mask.png");
        tracing::info!(pmw = pmw, pmh = pmh, r = r, "title bar mask params");
    }

    // Multiply the bar's pre-multiplied RGBA by the mask alpha, in place.
    let bar_data = pixmap.data_mut();
    let mask_data = mask.data();
    for (b, m) in bar_data.chunks_exact_mut(4).zip(mask_data.chunks_exact(4)) {
        let mask_alpha = m[3] as u32;
        b[0] = ((b[0] as u32 * mask_alpha + 127) / 255) as u8;
        b[1] = ((b[1] as u32 * mask_alpha + 127) / 255) as u8;
        b[2] = ((b[2] as u32 * mask_alpha + 127) / 255) as u8;
        b[3] = ((b[3] as u32 * mask_alpha + 127) / 255) as u8;
    }
}
