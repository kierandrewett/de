//! ext-image-copy-capture-v1 framebuffer readback.
//!
//! Synchronous wgpu → wl_shm path: copy `final_tex` to a staging buffer,
//! `device.poll(Wait)` until the copy completes, map the buffer, swap RGBA
//! channels into the BGRA byte order wl_shm Argb8888 / Xrgb8888 require,
//! write into the client's wl_buffer, then signal `Frame::success`.
//!
//! Sync was chosen over async for v1 — the readback adds ~1 frame of latency
//! to the capture path but doesn't stall the swapchain (the encoder for the
//! capture is a separate submission from the main render encoder, which has
//! already presented). A non-blocking variant would need a `MapMode::Read`
//! callback to thread back into the calloop event loop.

use std::sync::Arc;

use smithay::{
    reexports::wayland_server::protocol::wl_shm,
    utils::{Rectangle, Transform},
    wayland::{
        image_copy_capture::{CaptureFailureReason, Frame},
        shm::{with_buffer_contents_mut, BufferAccessError},
    },
};
use tracing::{debug, warn};

/// Minimal wgpu handles needed by the readback path. Plumbed in from the
/// renderer (which owns the device/queue/final_tex) so this module stays
/// independent of `CompositorApp`.
pub struct CaptureContext<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub final_tex: &'a wgpu::Texture,
    /// Physical pixel dimensions of `final_tex`.
    pub width: u32,
    pub height: u32,
}

/// Service one capture frame: read final_tex, format-convert into the
/// client's wl_buffer, and drive the frame to success/fail. Consumes
/// `frame` so dropping it (failure path included) auto-fails the client
/// per smithay's contract.
pub fn process_frame(ctx: &CaptureContext<'_>, frame: Frame, presented: std::time::Duration) {
    let buffer = frame.buffer();

    // Pull the wl_shm parameters we need to size the staging copy. We don't
    // hold the SHM map across the readback — we only need width/height/
    // format/stride to know how to write the converted bytes back later.
    let (buf_w, buf_h, buf_format) = {
        // with_buffer_contents is a peek-only borrow that returns whatever
        // we hand back from the closure.
        match smithay::wayland::shm::with_buffer_contents(&buffer, |_, _, spec| {
            (spec.width, spec.height, spec.format)
        }) {
            Ok(v) => v,
            Err(BufferAccessError::NotManaged) => {
                // dmabuf or other non-SHM buffer — we only advertise SHM in
                // capture_constraints so this would be a client bug, but
                // fail gracefully rather than panicking.
                warn!("screencopy: client buffer is not wl_shm (dmabuf?), failing frame");
                frame.fail(CaptureFailureReason::BufferConstraints);
                return;
            }
            Err(e) => {
                warn!("screencopy: with_buffer_contents failed: {:?}", e);
                frame.fail(CaptureFailureReason::Unknown);
                return;
            }
        }
    };
    if buf_w <= 0 || buf_h <= 0 {
        frame.fail(CaptureFailureReason::BufferConstraints);
        return;
    }
    let dst_w = buf_w as u32;
    let dst_h = buf_h as u32;
    if dst_w != ctx.width || dst_h != ctx.height {
        // We advertised a single size in BufferConstraints; if the client
        // attached a buffer with a different size we can't satisfy them
        // without scaling. Bail rather than copy partial pixels.
        warn!(
            "screencopy: client buffer {}x{} doesn't match output {}x{}, failing",
            dst_w, dst_h, ctx.width, ctx.height
        );
        frame.fail(CaptureFailureReason::BufferConstraints);
        return;
    }

    // wgpu requires the staging buffer's `bytes_per_row` to be a multiple
    // of COPY_BYTES_PER_ROW_ALIGNMENT (256). Pad each row in the GPU copy,
    // then strip padding when writing to the client buffer.
    const ALIGN: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let unpadded_bpr: u32 = ctx.width * 4;
    let padded_bpr: u32 = unpadded_bpr.div_ceil(ALIGN) * ALIGN;
    let staging_size = (padded_bpr as u64) * (ctx.height as u64);

    let staging = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("screencopy-staging"),
        size: staging_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("screencopy-readback"),
        });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: ctx.final_tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bpr),
                rows_per_image: Some(ctx.height),
            },
        },
        wgpu::Extent3d {
            width: ctx.width,
            height: ctx.height,
            depth_or_array_layers: 1,
        },
    );
    ctx.queue.submit(std::iter::once(encoder.finish()));

    // map_async + poll(Wait): wgpu requires a callback even for synchronous
    // mapping. We use an Arc<Mutex<Option<Result>>> as a sync-mailbox.
    let slot: Arc<std::sync::Mutex<Option<Result<(), wgpu::BufferAsyncError>>>> =
        Arc::new(std::sync::Mutex::new(None));
    let slot_cb = slot.clone();
    staging
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |res| {
            *slot_cb.lock().unwrap() = Some(res);
        });
    if let Err(e) = ctx.device.poll(wgpu::PollType::wait_indefinitely()) {
        warn!("screencopy: device.poll failed: {:?}", e);
        frame.fail(CaptureFailureReason::Unknown);
        return;
    }
    match slot.lock().unwrap().take() {
        Some(Ok(())) => {}
        Some(Err(e)) => {
            warn!("screencopy: map_async failed: {:?}", e);
            frame.fail(CaptureFailureReason::Unknown);
            return;
        }
        None => {
            warn!("screencopy: map_async never completed despite Wait poll");
            frame.fail(CaptureFailureReason::Unknown);
            return;
        }
    }

    let mapped = staging.slice(..).get_mapped_range();
    // Source: ctx.width * 4 RGBA bytes per row, padded to padded_bpr.
    // Destination: client wl_shm buffer, buf_stride bytes per row, format
    // determines channel order. Both Argb8888 and Xrgb8888 are stored in
    // little-endian native — i.e. byte order B, G, R, A on disk.
    let src_bpr = padded_bpr as usize;
    let row_bytes = (ctx.width as usize) * 4;
    let want_argb = matches!(buf_format, wl_shm::Format::Argb8888);

    let write_result = with_buffer_contents_mut(&buffer, |dst_ptr, dst_len, spec| {
        if spec.width != buf_w || spec.height != buf_h {
            // Buffer geometry shouldn't change between the peek above and
            // here — but if it did (client raced a destroy), bail.
            return Err(());
        }
        let dst_stride = spec.stride as usize;
        let dst_offset = spec.offset as usize;
        let needed = dst_offset + dst_stride * (buf_h as usize);
        if needed > dst_len {
            return Err(());
        }
        for y in 0..(ctx.height as usize) {
            let src_off = y * src_bpr;
            let src_row = &mapped[src_off..src_off + row_bytes];
            // SAFETY: dst_ptr is valid for `dst_len` bytes per the SHM
            // contract; we just bounds-checked `needed <= dst_len` above
            // and stride * y stays inside `needed`.
            let dst_row_off = dst_offset + y * dst_stride;
            unsafe {
                let dst = dst_ptr.add(dst_row_off);
                // Per-pixel BGRA swap. wl_shm Argb8888/Xrgb8888 are
                // little-endian u32 with high byte alpha → byte order
                // B, G, R, A in memory. Our final_tex is Rgba8Unorm →
                // byte order R, G, B, A.
                for x in 0..(ctx.width as usize) {
                    let s = x * 4;
                    let r = src_row[s];
                    let g = src_row[s + 1];
                    let b = src_row[s + 2];
                    let a = if want_argb { src_row[s + 3] } else { 0xff };
                    let p = dst.add(x * 4);
                    *p.add(0) = b;
                    *p.add(1) = g;
                    *p.add(2) = r;
                    *p.add(3) = a;
                }
            }
        }
        Ok(())
    });

    drop(mapped);
    staging.unmap();

    match write_result {
        Ok(Ok(())) => {
            debug!("screencopy: delivered {}x{} frame", ctx.width, ctx.height);
            // Full-frame damage: we don't track per-region damage on the
            // capture side yet, so report the whole output as damaged.
            let damage = vec![Rectangle::from_size((buf_w, buf_h).into())];
            frame.success(Transform::Normal, damage, presented);
        }
        Ok(Err(())) => {
            warn!("screencopy: client buffer geometry mismatch on write-back");
            frame.fail(CaptureFailureReason::BufferConstraints);
        }
        Err(e) => {
            warn!("screencopy: with_buffer_contents_mut failed: {:?}", e);
            frame.fail(CaptureFailureReason::Unknown);
        }
    }
}
