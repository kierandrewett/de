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
use iced_widget::svg::Handle as SvgHandle;
use iced_widget::{button, container, row, svg, text};

use smithay::backend::{
    allocator::Fourcc,
    renderer::element::memory::MemoryRenderBuffer,
};
use smithay::utils::Transform;

const CLOSE_SVG: &[u8] = include_bytes!("../assets/close.svg");
const MINIMIZE_SVG: &[u8] = include_bytes!("../assets/minimize.svg");
const MAXIMIZE_SVG: &[u8] = include_bytes!("../assets/maximize.svg");

const ICON_PX: u32 = 17;
const BUTTON_PADDING: u16 = 2;
const TITLE_FONT_SIZE: f32 = 13.0;
const MAX_CACHE_ENTRIES: usize = 64;
/// Minimum logical bar width before the chrome will draw. Anything smaller
/// can't fit the three buttons + spacing + padding without iced's flex
/// layout producing zero-width quads (which trip a debug assert in
/// iced_tiny_skia).
const MIN_CHROME_WIDTH_LOGICAL: u32 = 100;

/// Iced widgets need a Message type; chrome buttons currently emit no events
/// at this layer (clicks are caught by the compositor's input handler before
/// reaching the offscreen UI).
#[derive(Debug, Clone, Copy)]
pub enum ChromeMessage {}

fn svg_handles() -> &'static (SvgHandle, SvgHandle, SvgHandle) {
    static H: OnceLock<(SvgHandle, SvgHandle, SvgHandle)> = OnceLock::new();
    H.get_or_init(|| {
        (
            SvgHandle::from_memory(Cow::Borrowed(CLOSE_SVG)),
            SvgHandle::from_memory(Cow::Borrowed(MINIMIZE_SVG)),
            SvgHandle::from_memory(Cow::Borrowed(MAXIMIZE_SVG)),
        )
    })
}

fn button_style(_theme: &Theme, _status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.08))),
        text_color: Color::BLACK,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: 10.5.into(),
        },
        shadow: Default::default(),
    }
}

fn bar_style_focused(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::from_rgb(0.95, 0.95, 0.95))),
        text_color: Some(Color::from_rgb(0.10, 0.10, 0.10)),
        ..Default::default()
    }
}

fn bar_style_unfocused(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::from_rgb(0.88, 0.88, 0.88))),
        text_color: Some(Color::from_rgb(0.45, 0.45, 0.45)),
        ..Default::default()
    }
}

fn build_view<'a>(
    title: &'a str,
    focused: bool,
) -> Element<'a, ChromeMessage, Theme, IcedRenderer> {
    let (close, min, max) = svg_handles();
    let icon = |h: &SvgHandle| svg(h.clone()).width(ICON_PX as f32).height(ICON_PX as f32);
    let btn = |h: &SvgHandle| button(icon(h)).padding(BUTTON_PADDING).style(button_style);

    let bar_style: fn(&Theme) -> container::Style = if focused {
        bar_style_focused
    } else {
        bar_style_unfocused
    };

    container(
        row![
            btn(close),
            btn(min),
            btn(max),
            text(title).size(TITLE_FONT_SIZE).font(Font::DEFAULT),
        ]
        .spacing(6)
        .align_y(alignment::Vertical::Center)
        .padding(Padding {
            top: 0.0,
            bottom: 0.0,
            left: 10.0,
            right: 10.0,
        }),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_y(alignment::Vertical::Center)
    .style(bar_style)
    .into()
}

#[derive(Hash, Eq, PartialEq, Clone)]
struct CacheKey {
    width_px: u32,
    height_px: u32,
    scale_milli: u32,
    focused: bool,
    title: String,
}

/// Holds the persistent iced renderer and a per-window cache of rendered
/// title bars. The renderer keeps glyph + svg caches alive across frames so
/// only state changes pay the layout/raster cost.
pub struct IcedChrome {
    renderer: IcedRenderer,
    cache: HashMap<CacheKey, MemoryRenderBuffer>,
}

impl Default for IcedChrome {
    fn default() -> Self {
        Self {
            renderer: IcedRenderer::new(Font::DEFAULT, Pixels(TITLE_FONT_SIZE)),
            cache: HashMap::new(),
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

    fn render_uncached(
        &mut self,
        width_px: u32,
        height_px: u32,
        scale: f64,
        title: &str,
        focused: bool,
    ) -> Option<MemoryRenderBuffer> {
        let viewport = Viewport::with_physical_size(Size::new(width_px, height_px), scale);
        let logical = viewport.logical_size();
        let bounds = Size::new(logical.width, logical.height);

        let view = build_view(title, focused);
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

        let mut pixmap = tiny_skia::Pixmap::new(width_px, height_px)?;
        let mut clip_mask = tiny_skia::Mask::new(width_px, height_px)?;
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

        // tiny_skia::Pixmap is premultiplied RGBA; smithay's Argb8888 reads
        // memory as little-endian BGRA. Swap channels in place.
        let mut bytes = pixmap.take();
        for px in bytes.chunks_exact_mut(4) {
            px.swap(0, 2);
        }

        Some(MemoryRenderBuffer::from_slice(
            &bytes,
            Fourcc::Argb8888,
            (width_px as i32, height_px as i32),
            1,
            Transform::Normal,
            None,
        ))
    }
}
