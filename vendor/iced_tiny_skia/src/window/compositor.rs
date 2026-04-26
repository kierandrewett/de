use crate::core::{Color, Rectangle, Size};
use crate::graphics::compositor::{self, Information};
use crate::graphics::damage;
use crate::graphics::error::{self, Error};
use crate::graphics::{self, Shell, Viewport};
use crate::{Layer, Renderer, Settings};

use std::collections::VecDeque;
use std::num::NonZeroU32;

pub struct Compositor {
    context: softbuffer::Context<Box<dyn compositor::Display>>,
    settings: Settings,
}

pub struct Surface {
    window: softbuffer::Surface<
        Box<dyn compositor::Display>,
        Box<dyn compositor::Window>,
    >,
    clip_mask: tiny_skia::Mask,
    layer_stack: VecDeque<Vec<Layer>>,
    background_color: Color,
    max_age: u8,
}

impl crate::graphics::Compositor for Compositor {
    type Renderer = Renderer;
    type Surface = Surface;

    async fn with_backend(
        settings: graphics::Settings,
        display: impl compositor::Display,
        _compatible_window: impl compositor::Window,
        _shell: Shell,
        backend: Option<&str>,
    ) -> Result<Self, Error> {
        match backend {
            None | Some("tiny-skia") | Some("tiny_skia") => {
                Ok(new(settings.into(), display))
            }
            Some(backend) => Err(Error::GraphicsAdapterNotFound {
                backend: "tiny-skia",
                reason: error::Reason::DidNotMatch {
                    preferred_backend: backend.to_owned(),
                },
            }),
        }
    }

    fn create_renderer(&self) -> Self::Renderer {
        Renderer::new(
            self.settings.default_font,
            self.settings.default_text_size,
        )
    }

    fn create_surface<W: compositor::Window + Clone>(
        &mut self,
        window: W,
        width: u32,
        height: u32,
    ) -> Self::Surface {
        let window = softbuffer::Surface::new(
            &self.context,
            Box::new(window.clone()) as _,
        )
        .expect("Create softbuffer surface for window");

        let mut surface = Surface {
            window,
            clip_mask: tiny_skia::Mask::new(1, 1).expect("Create clip mask"),
            layer_stack: VecDeque::new(),
            background_color: Color::BLACK,
            max_age: 0,
        };

        if width > 0 && height > 0 {
            self.configure_surface(&mut surface, width, height);
        }

        surface
    }

    fn configure_surface(
        &mut self,
        surface: &mut Self::Surface,
        width: u32,
        height: u32,
    ) {
        surface
            .window
            .resize(
                NonZeroU32::new(width).expect("Non-zero width"),
                NonZeroU32::new(height).expect("Non-zero height"),
            )
            .expect("Resize surface");

        surface.clip_mask =
            tiny_skia::Mask::new(width, height).expect("Create clip mask");
        surface.layer_stack.clear();
    }

    fn information(&self) -> Information {
        Information {
            adapter: String::from("CPU"),
            backend: String::from("tiny-skia"),
        }
    }

    fn present(
        &mut self,
        renderer: &mut Self::Renderer,
        surface: &mut Self::Surface,
        viewport: &Viewport,
        background_color: Color,
        on_pre_present: impl FnOnce(),
    ) -> Result<(), compositor::SurfaceError> {
        present(
            renderer,
            surface,
            viewport,
            background_color,
            on_pre_present,
        )
    }

    fn screenshot(
        &mut self,
        renderer: &mut Self::Renderer,
        viewport: &Viewport,
        background_color: Color,
    ) -> Vec<u8> {
        screenshot(renderer, viewport, background_color)
    }
}

pub fn new(
    settings: Settings,
    display: impl compositor::Display,
) -> Compositor {
    #[allow(unsafe_code)]
    let context = softbuffer::Context::new(Box::new(display) as _)
        .expect("Create softbuffer context");

    Compositor { context, settings }
}

pub fn present(
    renderer: &mut Renderer,
    surface: &mut Surface,
    viewport: &Viewport,
    background_color: Color,
    on_pre_present: impl FnOnce(),
) -> Result<(), compositor::SurfaceError> {
    let physical_size = viewport.physical_size();

    let mut buffer = surface
        .window
        .buffer_mut()
        .map_err(|_| compositor::SurfaceError::Lost)?;

    // PATCH: On Wayland with wl_shm, softbuffer's `buffer.age()` reports a
    // non-zero age but the returned buffer is actually a fresh/zero shm
    // buffer. Trusting age leads iced to do a damage-only partial redraw
    // into an empty buffer, leaving everything outside the damage region
    // black — visible as panel content "fading to black" the moment the
    // mouse moves and a partial redraw is triggered.
    //
    // Forcing full damage every frame costs us a single full repaint per
    // present (cheap for shell surfaces at panel/dock size) and guarantees
    // the buffer always contains a complete render. This also makes the
    // softbuffer age-tracking and layer_stack history dead code, but we
    // keep the structure intact in case upstream fixes the wl_shm path
    // and we want to revert.
    let _ = (
        buffer.age(),
        &mut surface.max_age,
        &mut surface.layer_stack,
    );
    surface.layer_stack.clear();
    surface.layer_stack.push_front(renderer.layers().to_vec());
    surface.background_color = background_color;
    let damage = vec![Rectangle::with_size(viewport.logical_size())];

    let damage = damage::group(
        damage,
        Rectangle::with_size(viewport.logical_size()),
    );

    let mut pixels = tiny_skia::PixmapMut::from_bytes(
        bytemuck::cast_slice_mut(&mut buffer),
        physical_size.width,
        physical_size.height,
    )
    .expect("Create pixel map");

    renderer.draw(
        &mut pixels,
        &mut surface.clip_mask,
        viewport,
        &damage,
        background_color,
    );

    on_pre_present();
    buffer.present().map_err(|_| compositor::SurfaceError::Lost)
}

pub fn screenshot(
    renderer: &mut Renderer,
    viewport: &Viewport,
    background_color: Color,
) -> Vec<u8> {
    let size = viewport.physical_size();

    let mut offscreen_buffer: Vec<u32> =
        vec![0; size.width as usize * size.height as usize];

    let mut clip_mask = tiny_skia::Mask::new(size.width, size.height)
        .expect("Create clip mask");

    renderer.draw(
        &mut tiny_skia::PixmapMut::from_bytes(
            bytemuck::cast_slice_mut(&mut offscreen_buffer),
            size.width,
            size.height,
        )
        .expect("Create offscreen pixel map"),
        &mut clip_mask,
        viewport,
        &[Rectangle::with_size(Size::new(
            size.width as f32,
            size.height as f32,
        ))],
        background_color,
    );

    offscreen_buffer.iter().fold(
        Vec::with_capacity(offscreen_buffer.len() * 4),
        |mut acc, pixel| {
            const A_MASK: u32 = 0xFF_00_00_00;
            const R_MASK: u32 = 0x00_FF_00_00;
            const G_MASK: u32 = 0x00_00_FF_00;
            const B_MASK: u32 = 0x00_00_00_FF;

            let a = ((A_MASK & pixel) >> 24) as u8;
            let r = ((R_MASK & pixel) >> 16) as u8;
            let g = ((G_MASK & pixel) >> 8) as u8;
            let b = (B_MASK & pixel) as u8;

            acc.extend([r, g, b, a]);
            acc
        },
    )
}
