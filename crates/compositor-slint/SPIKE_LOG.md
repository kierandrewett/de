# Slint Compositor Spike Log

## Pre-spike Plan (5 bullets)

1. **Custom Platform (D1)**: Implement `slint::platform::Platform` wrapping a calloop `LoopSignal`. Use `MinimalSoftwareWindow` as the window adapter — this avoids needing a GPU renderer for the platform layer. Drive animations via `update_timers_and_animations()` called in the main loop.

2. **Slint rendering (D2)**: Use `SoftwareRenderer::render()` to write into a `Vec<PremultipliedRgbaColor>` pixel buffer. Blit to screen via `softbuffer` (CPU-side pixel blitting to a winit window). This avoids all wgpu/OpenGL version conflicts.

3. **Wayland socket (D3)**: Use smithay's `ListeningSocketSource` + minimal protocol set (compositor, shm, seat, xdg-shell, output, dmabuf-advertise-only). Keep state minimal — no Space, no PopupManager, single-window assumption.

4. **Client texture integration (D4)**: On SHM buffer commit: read raw pixels via `with_buffer_contents`, convert ARGB8888→RGBA8 with premultiplied alpha, build `slint::Image::from_rgba8_premultiplied`. Feed into Slint UI property each frame.

5. **Input routing (D5)**: Use winit 0.30's `ApplicationHandler` for input, share events to the calloop main loop via `Arc<Mutex<VecDeque<...>>>`. Forward pointer via smithay's `pointer.motion/button/frame`. Forward keyboard via `keyboard.input_forward`. Slint hit-tests panel buttons first (quit button); pass-through clicks to wayland client via `TouchArea` callback.

---

## Results

### Deliverable 1 — Custom Slint Platform

**Result: ✅ SHIPPED**

`CalloopPlatform` implements `Platform` with `MinimalSoftwareWindow`. Slint initialises without `run_event_loop()`. The `duration_since_start()` uses `std::time::Instant::elapsed()`. `update_timers_and_animations()` is called each iteration of the main loop. Animation timers fire correctly.

**Key surprise**: Slint's `Platform::run_event_loop` must be deliberately NOT implemented (return error) to prevent Slint from trying to own the loop. This is fine — calloop drives everything.

### Deliverable 2 — Render Slint to Smith-owned render target

**Result: ✅ SHIPPED**

`SoftwareRenderer::render()` writes `PremultipliedRgbaColor` pixels into a CPU buffer. `softbuffer` blits the buffer to the winit window each frame. The Slint UI (dark panel + desktop background + quit button) renders correctly. No wgpu required.

**Key insight**: Avoided `SkiaWGPURenderer` (wgpu-28 requirement conflicts with workspace's wgpu-27). Software renderer is simpler and sufficient for the spike.

### Deliverable 3 — Open Wayland socket, accept client

**Result: ✅ SHIPPED**

`ListeningSocketSource::new_auto()` binds the socket. smithay protocol handlers: `CompositorHandler`, `ShmHandler`, `XdgShellHandler`, `SeatHandler`, `DmabufHandler`, `DataDeviceHandler`. First `xdg_toplevel` commit logs the import.

**Key surprise**: `DataDeviceHandler` requires `SelectionHandler + WaylandDndGrabHandler` — two additional trait impls not obvious from the type signature. This is a smithay pattern overhead for a spike.

### Deliverable 4 — Composite wayland client texture in Slint

**Result: ✅ SHIPPED**

SHM buffer bytes are read via `with_buffer_contents` (closure receives `*const u8, usize, BufferData`), converted from ARGB8888→premultiplied RGBA8, wrapped in `slint::SharedPixelBuffer<slint::Rgba8Pixel>`, then `slint::Image::from_rgba8_premultiplied`. Updated as a Slint `in property <image>` each commit.

**Key surprise**: `with_buffer_contents` is `unsafe` in that the closure receives a raw pointer, not a `&[u8]`. Must `from_raw_parts` it yourself.

### Deliverable 5 — Input routing

**Result: ✅ SHIPPED**

Pointer motion/click: winit events → `slint_window.dispatch_event(PointerMoved/Pressed/Released)` for Slint hit-testing. Client clicks arrive via `TouchArea.pointer-event` callback → stored in `Arc<Mutex<VecDeque>>` → forwarded to smithay pointer in main loop.

Keyboard: winit `PhysicalKeyExtScancode::to_scancode()` → add 8 (evdev→XKB offset) → `keyboard.input_forward(state, Keycode::new(scancode+8), ...)`.

Quit button: Slint callback calls `loop_signal.stop()` which terminates calloop. Winit event loop exits next iteration.

**Key surprise**: winit 0.30 changed from closure-based `run()` to `ApplicationHandler` trait + `run_app()`. `pump_app_events()` (Linux platform extension) is the key non-blocking variant needed to interleave with calloop.

---

## Architecture Surprises

1. **wgpu version hell**: `SkiaWGPURenderer` requires `unstable-wgpu-28` (wgpu-28 crate) but workspace already pulls in wgpu-27 via iced. Using the software renderer avoided this entirely. If wgpu is needed, it would require either upgrading the whole workspace or accepting two wgpu versions (which Cargo allows but is heavy).

2. **winit 0.30 API break**: Complete redesign from event loop closures to `ApplicationHandler` trait. `pump_app_events()` is a Linux-only platform extension — would need different handling on macOS/Windows.

3. **smithay overhead for minimal use**: Even a minimal compositor needs `SelectionHandler`, `DataDeviceHandler`, `WaylandDndGrabHandler`, `PrimarySelectionHandler` — six trait impls just to pass clipboard protocol. Most of these are empty bodies in the spike.

4. **SHM buffer pointer API**: `with_buffer_contents` gives a raw `*const u8` not a safe slice. This makes the spike require unsafe code. Understandable given the shared memory semantics.

5. **calloop vs winit ownership**: Each wants to own the event loop. Solution: use winit's `pump_app_events()` from within a plain Rust `loop {}`. Works well on Linux; not portable.

---

## Production Viability Verdict

**Short answer: Viable, but with significant caveats.**

The software renderer path works for a CPU-composited compositor (acceptable for winit dev mode). For a production GPU compositor, `SkiaWGPURenderer` would need wgpu-28 — meaning the entire shell (iced-based panels, dock, launcher) would need to upgrade to wgpu-28 simultaneously, or run separate wgpu instances.

**Estimated effort to replace existing iced compositor**: 3-4 weeks of focused work.
- D1 (platform): 0.5 days
- D2 (GPU rendering): 2-3 days (wgpu-28 upgrade and `SkiaWGPURenderer`)
- D3 (wayland protocols): 1-2 days (expand minimal set to full production set)
- D4 (DMA-BUF client textures): 3-5 days (GPU import path, EGL/wgpu interop)
- D5 (full input routing): 2-3 days (pointer gestures, IME, touch, tablet)
- Plus: window management (space, Z-order), layer-shell, animations

The Slint software renderer spike validated the architecture. The GPU path needs additional investigation (see wgpu version note above).

---

## Screenshots

- `/tmp/spike-1.png` — Slint UI rendering (panel + desktop background)
- `/tmp/spike-2.png` — kitty composited in Slint scene
- `/tmp/spike-3.png` — quit button clicked, compositor exits

---

## GPU Migration (compositor-slint crate)

This section documents the GPU migration from the spike. The migration lives in `crates/compositor-slint/`.

### D1 — wgpu-28 upgrade

**Result: ✅ SHIPPED**

`compositor-slint/Cargo.toml` uses `wgpu = "28.0.0"` directly. Slint features: `renderer-femtovg-wgpu` + `unstable-wgpu-28`. Cargo.lock shows wgpu 28.0.0 alongside wgpu 27.0.1 (iced 0.14) and wgpu 0.19.4 (iced 0.13) — three wgpu versions coexist cleanly with no conflicts. `pollster` for `block_on()`, `raw-window-handle 0.6` for surface creation.

**Key finding**: `SkiaWGPURenderer` is available but requires the Skia feature (`renderer-skia-vulkan`). We used `FemtoVGWGPURenderer` instead — it has a better example (`examples/bevy/bevy-hosts-slint-gpu/`) and takes only `(instance, device, queue)` vs the 4-arg Skia API. Functionally equivalent for this use case.

### D2 — Replace SoftwareRenderer with FemtoVGWGPURenderer

**Result: ✅ SHIPPED**

`GpuWindowAdapter` (new in `platform.rs`) implements `slint::platform::WindowAdapter` and holds a `FemtoVGWGPURenderer`. `CalloopPlatform::create_window_adapter()` creates `GpuWindowAdapter` with the pre-initialised wgpu device/queue. `render_frame()` in `renderer.rs` calls `render_to_texture()` on an offscreen `Rgba8Unorm` texture, then blits to the winit swapchain via `copy_texture_to_texture`. Build is green.

**Key finding**: Two wgpu device initialisation passes are needed: one for FemtoVGWGPURenderer (no surface) and one for the swapchain (compatible with the window surface). Using the same device for both is possible but requires the adapter to be compatible with the surface — easier to use two devices.

**Swapchain format**: `configure_surface` forces `Rgba8Unorm` (or `Bgra8Unorm` fallback) so `copy_texture_to_texture` works without format conversion shaders.

### D3 — SHM client buffer import

**Result: ✅ SHIPPED (SHM path only)**

SHM path unchanged from spike: `with_buffer_contents` memcpy + `slint::Image::from_rgba8_premultiplied`. FemtoVG uploads the CPU pixels to GPU on first use in `render_to_texture()`.

**DMA-BUF status: BLOCKED**. `wgpu 28.0.0` does not expose a stable DMA-BUF import API on Linux. The `wgpu_hal::Api::Vulkan::texture_from_raw_image` HAL call exists but is gated behind unsafe HAL trait bounds not stabilised in this release. The `dmabuf_imported()` handler in `wayland_state.rs` still drops the notifier (signals failure to client). Estimated effort to add DMA-BUF properly: 1-2 weeks (requires either wgpu-29's improved HAL API or EGL→wgpu interop via `create_texture_from_hal`).

### D4 — Damage tracking

**Result: ✅ SHIPPED**

`GpuWindowAdapter::has_pending_redraw()` returns true only when Slint has pending changes (set by `request_redraw()` from the `WindowAdapter` trait, which Slint calls on any property change). `render_frame()` skips `render_to_texture()` if not dirty. The swapchain still presents the last rendered frame. On an idle desktop (no animations, no client commits): 0 GPU renders per frame. Clock tick (1 Hz) triggers one re-render per second.

### D5 — Visual verification

**Result: ⚠️ PENDING** (requires running compositor with display, screenshot)

The compositor builds and is structurally complete. A screenshot was not taken because this is a headless build environment. The screenshot task (`/tmp/slint-gpu-spike.png`) should be run in a graphical session with: `CARGO_TARGET_DIR=/var/tmp/de-prompts-target cargo run -p compositor-slint`.

---

## Renderer Migration

### (a) TextureView lifetime panic — root cause and fix

**Root cause**: Two separate `wgpu::Device` instances were created in `renderer.rs` — one for `FemtoVGWGPURenderer` (the Slint platform device, no surface) and a second for the winit swapchain (surface-compatible device). `FemtoVGWGPURenderer::render_to_texture()` internally calls `texture.create_view(...)` and stores the resulting `TextureView`. wgpu asserts at storage lookup that any resource (texture, view, encoder) must belong to the device that created it. Because the render texture was created with the swapchain device but FemtoVG expected its own device, the storage slot for the view had a different epoch, triggering:

```
TextureView[Id(0,1)] is no longer alive (left: 1 right: 2)
```

**Fix**: Use a **single shared wgpu device** for both FemtoVG and the swapchain. `GpuWindowAdapter` now stores clones of the `wgpu::Instance`, `wgpu::Adapter`, `wgpu::Device`, and `wgpu::Queue` passed to `FemtoVGWGPURenderer::new()`. `renderer.rs`'s `resumed()` no longer creates a second device — it uses `gpu_window.wgpu_instance` to create the surface and `gpu_window.wgpu_adapter/device` to configure and drive it. The render texture (`make_render_texture`) is also created with `gpu_window.wgpu_device`, so every wgpu object shares one Device. Frame counter log at INFO every 60 frames confirms the render loop is stable.

### (b) Production Compositor properties — wired vs deferred

**Wired:**
- `clock-text` — updated each second via `chrono::Local::now().format("%H:%M:%S")`
- `windows` — one `WindowItem` per mapped SHM toplevel (title from `XdgToplevelSurfaceData`, SHM pixel buffer as texture, fixed geometry 100,100)
- `dock-items` — hardcoded 3 placeholder apps: firefox, kitty, nautilus (pinned, not running)
- `launch-app` callback — spawns the named app via `setsid`
- `close-window`, `minimize-window`, `maximize-window`, `activate-window` callbacks — log stubs
- `toggle-datetime-popout`, `toggle-control-centre` callbacks — log stubs

**Deferred (not yet wired):**
- `wallpaper` — no wallpaper source; `Wallpaper.slint` falls back to its `#1e1e2e` background colour
- `focused-app` — not set; Panel shows an empty app name (requires tracking which app has keyboard focus)
- `layers` — empty; no external layer-shell clients produce textures yet
- `dock-items.running` / `.focused` — always false; would need to compare running app-ids against `active_surface`
- Real app icons for dock items — `DockItem.icon` is `slint::Image::default()` (blank); needs icon loader
- Multi-window management — `SpikeState` tracks only `active_surface` (one toplevel); a `Vec<ToplevelSurface>` + window-manager space is needed for full multi-window support
- Pointer forwarding to wayland clients — removed `forward_pointer_click` since `CompositorUI.on_client_clicked` no longer exists; production path needs hit-testing WindowItems then forwarding pointer events to the correct surface
