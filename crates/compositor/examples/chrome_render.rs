//! Standalone visual check for the iced-rendered title bar chrome.
//!
//! Renders a sample title bar to /tmp/chrome.png. Useful for iterating on
//! styling without restarting the full compositor + nested wayland session.
//!
//! Usage: `cargo run --example chrome_render -p compositor`

use std::borrow::Cow;
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

const CLOSE_SVG: &[u8] = include_bytes!("../assets/close.svg");
const MINIMIZE_SVG: &[u8] = include_bytes!("../assets/minimize.svg");
const MAXIMIZE_SVG: &[u8] = include_bytes!("../assets/maximize.svg");

const ICON_PX: u32 = 17;
const BUTTON_PADDING: u16 = 2;
const TITLE_FONT_SIZE: f32 = 13.0;

#[derive(Debug, Clone, Copy)]
enum Msg {}

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

fn bar_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::from_rgb(0.95, 0.95, 0.95))),
        text_color: Some(Color::from_rgb(0.10, 0.10, 0.10)),
        ..Default::default()
    }
}

fn view<'a>(title: &'a str) -> Element<'a, Msg, Theme, IcedRenderer> {
    let (close, min, max) = svg_handles();
    let icon = |h: &SvgHandle| svg(h.clone()).width(ICON_PX as f32).height(ICON_PX as f32);
    let btn = |h: &SvgHandle| button(icon(h)).padding(BUTTON_PADDING).style(button_style);

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

fn main() {
    let width: u32 = 640;
    let height: u32 = 28;
    let scale: f64 = 2.0; // upscale so the output is easier to inspect
    let phys_w = (width as f64 * scale) as u32;
    let phys_h = (height as f64 * scale) as u32;

    let mut renderer = IcedRenderer::new(Font::DEFAULT, Pixels(TITLE_FONT_SIZE));
    let viewport = Viewport::with_physical_size(Size::new(phys_w, phys_h), scale);
    let logical = viewport.logical_size();

    let cache = user_interface::Cache::new();
    let mut ui = UserInterface::build(
        view("Hello, world  —  iced chrome"),
        Size::new(logical.width, logical.height),
        cache,
        &mut renderer,
    );

    let _ = ui.draw(
        &mut renderer,
        &Theme::Light,
        &Style {
            text_color: Color::BLACK,
        },
        mouse::Cursor::Unavailable,
    );

    let mut pixmap = tiny_skia::Pixmap::new(phys_w, phys_h).unwrap();
    let mut clip_mask = tiny_skia::Mask::new(phys_w, phys_h).unwrap();
    let damage = [Rectangle {
        x: 0.0,
        y: 0.0,
        width: logical.width,
        height: logical.height,
    }];
    let overlay: [&str; 0] = [];
    renderer.draw(
        &mut pixmap.as_mut(),
        &mut clip_mask,
        &viewport,
        &damage,
        Color::WHITE,
        &overlay,
    );

    let out = "/tmp/chrome.png";
    pixmap.save_png(out).expect("save_png");
    eprintln!("wrote {out} ({phys_w}x{phys_h})");
}
