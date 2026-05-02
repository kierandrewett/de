# ARCHITECTURE CONTEXT — Required Reading for All Subagents

This document provides the architectural context for the desktop environment. Every subagent MUST read and internalise this before writing any code. It covers why we chose smithay over wlroots, how the compositor rendering pipeline works at a fundamental level, and what Wayland protocols we need.

---

## Why Smithay, Not wlroots

We evaluated two compositor library options:

### wlroots (C, by sway's author)
- The most battle-tested Wayland compositor library
- Used by: Sway, Wayfire, LabWC, River, and dozens of others
- Provides: DRM/KMS, input handling, protocol plumbing, and a scene-graph API (`wlr_scene`)
- Has `wlr-layer-shell`, `wlr-screencopy`, `wlr-output-management`, `wlr-gamma-control`, `wlr-data-control`, `wlr-foreign-toplevel-management` — a suite of compositor-specific protocols that have become de facto standards
- **Downside for us:** C library, requires FFI from Rust, and Hyprland recently forked away from it entirely to build their own renderer — the `wlr_scene` API is convenient but constrains your rendering pipeline

### Smithay (Rust, used by COSMIC)
- Rust-native compositor library — no FFI boundary
- Used by: COSMIC (System76's full DE), Niri, Jay, and others
- Provides the same fundamentals: DRM/KMS, libinput, session management, EGL/GL, calloop event loop
- Implements all the same protocols (core Wayland, xdg-shell, linux-dmabuf, etc.) plus most wlr-* protocols
- **Does NOT** constrain your rendering pipeline — you own the render loop entirely
- COSMIC proves it scales to a full production desktop environment
- Protocol coverage is comprehensive (see tracking issue: https://github.com/Smithay/smithay/issues/781)
- Xfce recently chose smithay for their Wayland compositor (xfwl4), further validating it

### Our choice: Smithay
We chose smithay because:
1. **Rust-native** — no FFI, no unsafe boundary with C code, full ownership of memory
2. **Rendering freedom** — we own the entire render pipeline (critical for our squircle clipping, macOS borders, SDF shaders, and Slint scene rendering)
3. **COSMIC precedent** — System76 built a full DE on smithay, proving the architecture works at scale
4. **Protocol parity** — smithay implements virtually every protocol wlroots does, plus smithay's delegate pattern makes adding new protocols mechanical

### wlroots protocols we still need
Even though we're on smithay, we implement the wlr-* protocol extensions because the ecosystem depends on them:
- `wlr-layer-shell-v1` — our panel, dock, launcher, lock screen, notifications all use this. Smithay has it. ✅
- `wlr-output-management-v1` — runtime monitor config (resolution, position, scale). Used by `wdisplays`, `kanshi`, `wlr-randr`. **Smithay does NOT have this — we implement it ourselves.**
- `wlr-gamma-control-v1` — night light / blue light filter (`wlsunset`, `gammastep`). **We implement ourselves.**
- `wlr-output-power-management-v1` — DPMS control. **We implement ourselves.**
- `wlr-data-control-v1` — clipboard manager access (`wl-clipboard`, `cliphist`). Smithay has it. ✅
- `wlr-foreign-toplevel-management-v1` — full toplevel control for taskbars/scripting. **We implement ourselves.**
- `wlr-screencopy-v1` — legacy screen capture. Mostly superseded by `ext-image-copy-capture` which smithay has, but some tools still use wlr-screencopy.
- `wlr-virtual-pointer-v1` — virtual pointer injection. **We implement ourselves if needed.**

The `ext-*` staging protocols in wayland-protocols are the "official" replacements for many wlr-* protocols. We implement both for maximum compatibility.

---

## How Wayland Compositing Works (for subagents who need this)

### The fundamental model
A Wayland client (e.g. Firefox, Alacritty) renders its window content into a buffer (GPU texture via DMA-BUF, or CPU pixels via shared memory). It sends this buffer to the compositor via `wl_surface.commit()`. The compositor's job is to take all these client buffers and composite them into a final framebuffer that gets displayed on the monitor.

**The compositor owns the rendering.** The client has no idea how its buffer is displayed. The compositor can clip it, scale it, rotate it, apply shaders to it, draw things on top of it — whatever it wants. This is the fundamental insight that makes our custom decorations, squircle clipping, and macOS borders possible.

### Surface → Texture → Composite
```
Client submits wl_surface buffer
    ↓
Smithay imports it as a GPU texture (EGL/DMA-BUF import)
    ↓
Our render loop iterates all windows back-to-front
    ↓
For each window:
    1. Draw shadow (pre-computed gaussian blur texture)
    2. Draw outer border stroke (squircle path)
    3. Set up squircle clip mask (stencil buffer or SDF shader)
    4. Draw client texture through clip mask
    5. Draw inner highlight (inset squircle stroke)
    6. If SSD: composite Slint WindowChrome titlebar above client
    ↓
Submit final framebuffer to DRM/KMS for display
```

### Server-side vs Client-side Decorations
- **CSD (Client-Side Decorations):** The client draws its own title bar and window chrome. GTK4/libadwaita apps insist on this. We still clip the whole window with our squircle mask and add our shadow/border — the client never knows.
- **SSD (Server-Side Decorations):** We draw the title bar, window controls (close/max/min), and frame. The client just renders content. We negotiate this via `xdg-decoration-unstable-v1` — we advertise "ServerSide" preference, and apps that support it will give us a clean content rect.
- **Our approach:** For SSD windows, the titlebar is part of the Slint scene (`slint/WindowChrome.slint`) — Slint rasterises it via FemtoVG into the same render texture as the windows themselves, no per-window iced trees. For CSD windows we just clip the buffer to the xdg geom rect and run our own GPU shadow/border passes.

### Layer Shell (how our shell UI works)
`wlr-layer-shell-v1` lets special surfaces anchor to screen edges with an "exclusive zone" (area that windows avoid). The original architecture ran the panel, dock, and launcher as separate iced layer-shell client processes; the current architecture folds all of those into the compositor's own Slint scene (`slint/Compositor.slint`) — single render pipeline, no IPC roundtrip. Layer-shell support is still implemented and exposed for THIRD-PARTY clients (waybar replacements, swaync, mako, etc.) — those connect to our wayland socket like any other client and are composited via `wayland/layer_shell.rs`.

Notification popups remain a separate IPC-driven path: `crates/notification` runs the freedesktop notification D-Bus service in a sibling process, the compositor consumes its events over the unix-socket IPC, and the popout is rendered inside the Slint scene.

---

## Smithay Backend Architecture

### Winit backend (development)
```rust
// smithay provides this:
let (mut backend, mut winit_event_loop) = winit::init::<GlesRenderer>()?;
// backend: WinitGraphicsBackend<GlesRenderer> — gives you a renderer + window
// winit_event_loop: WinitEventLoop — gives you input events

// Create a virtual output matching the winit window:
let output = Output::new("winit-0", PhysicalProperties { ... });
let mode = Mode { size: (1920, 1080).into(), refresh: 60_000 };
output.change_current_state(Some(mode), None, None, Some((0,0).into()));

// Main loop (calloop):
loop {
    // Pump winit events (input, resize, close)
    winit_event_loop.dispatch_new_events(|event| match event {
        WinitEvent::Input(input) => handle_input(input),
        WinitEvent::Resized { size, .. } => handle_resize(size),
        _ => {}
    });

    // Render frame
    backend.bind()?;
    let renderer = backend.renderer();
    render_frame(renderer, &output, &state);
    backend.submit(Some(&[damage_rect]))?;

    // Dispatch Wayland clients
    display.dispatch_clients(&mut state)?;
    display.flush_clients()?;
}
```

### udev/DRM backend (production)
```rust
// Session management (libseat/logind):
let (session, session_notifier) = LibSeatSession::new()?;

// Discover GPUs and monitors via udev:
let udev_backend = UdevBackend::new(&seat_name)?;
for (device_id, path) in udev_backend.device_list() {
    let drm = DrmDevice::open(path)?;
    let gbm = GbmDevice::new(drm)?;
    // For each connected output (monitor):
    for connector in drm.connectors() {
        let output = Output::new(connector.name(), ...);
        // Set up DRM scanout, create GBM surfaces, etc.
    }
}

// Input via libinput:
let mut libinput = Libinput::new_with_udev(session);
// Calloop event source for libinput events

// Render loop per-output:
for output in outputs {
    let renderer = gpu.renderer();
    render_frame(renderer, &output, &state);
    drm.submit_frame(&output)?;
}
```

### Both backends share the same State struct
The `State` struct contains all compositor logic (window management, protocol handlers, shell state). The backend is just how we get a renderer and input events. This means ALL compositor features work identically in both modes.

```rust
pub struct State {
    pub backend: Backend,
    pub common: CommonState, // everything else
}

pub enum Backend {
    Winit(WinitData),
    Udev(UdevData),
}

// All protocol handlers and shell logic operate on CommonState,
// never touching Backend directly except for rendering.
```

---

## Complete Wayland Protocol Reference

### P0 — Without these, nothing renders
| Protocol | Purpose | Smithay Status |
|---|---|---|
| `wl_compositor` v6 | Surface creation | ✅ |
| `wl_subcompositor` v1 | Subsurface hierarchy | ✅ |
| `wl_shm` v2 | Shared memory buffers (software rendering) | ✅ |
| `wl_seat` v9 | Input devices | ✅ |
| `wl_output` v4 | Monitor info | ✅ |
| `wl_data_device_manager` v3 | Drag-and-drop + clipboard | ✅ |
| `xdg-shell` v7 | Window management (toplevels, popups, configure) | ✅ |
| `linux-dmabuf-v1` v5 | GPU buffer import | ✅ |
| `presentation-time` v2 | Frame timing | ✅ |
| `viewporter` v2 | Surface crop/scale | ✅ |
| `single-pixel-buffer-v1` | Efficient solid colours | ✅ |
| `xdg-output-unstable-v1` v3 | Logical output geometry | ✅ |

### P1 — Daily driver essentials
| Protocol | Purpose | Smithay |
|---|---|---|
| `wlr-layer-shell-v1` v5 | Panels, docks, overlays, lock screens | ✅ |
| `xdg-decoration-unstable-v1` | SSD/CSD negotiation | ✅ |
| `kde-server-decoration` | Qt SSD compat | ✅ |
| `primary-selection-unstable-v1` | Middle-click paste | ✅ |
| `ext-data-control-v1` + `wlr-data-control-v1` | Clipboard managers | ✅ |
| `relative-pointer-unstable-v1` | FPS games, 3D apps | ✅ |
| `pointer-constraints-unstable-v1` | Pointer lock (games) | ✅ |
| `pointer-gestures-unstable-v1` v3 | Trackpad gestures | ✅ |
| `cursor-shape-v1` v2 | Named cursor shapes | ✅ |
| `keyboard-shortcuts-inhibit-unstable-v1` | Apps grabbing all keys | ✅ |
| `ext-idle-notify-v1` v2 | Screen locker idle detection | ✅ |
| `idle-inhibit-unstable-v1` | Prevent idle during video | ✅ |
| `ext-session-lock-v1` | Secure screen lock | ✅ |
| `fractional-scale-v1` | HiDPI (125%, 150%, etc) | ✅ |
| `ext-image-capture-source-v1` + `ext-image-copy-capture-v1` | Screenshots, screen capture | ✅ |
| `text-input-unstable-v3` + `input-method-unstable-v2` | IME (CJK input) | ✅ |
| `virtual-keyboard-v1` | On-screen keyboard | ✅ |
| `xdg-activation-v1` | App focus requests | ✅ |
| `content-type-v1` | VRR content hints | ✅ |
| `fifo-v1` | Proper vsync | ✅ |
| `commit-timing-v1` | Frame pacing | ✅ |
| `tearing-control-v1` | Low-latency gaming | 🚧 |
| `pointer-warp-v1` | Cursor repositioning | ✅ |
| `tablet-v2` | Graphics tablet support | 🚧 |

### P2 — Full DE features
| Protocol | Purpose | Smithay |
|---|---|---|
| `ext-foreign-toplevel-list-v1` | Window list for taskbar/alt-tab | ✅ |
| `xdg-foreign-unstable-v2` | Cross-app parent-child windows | ✅ |
| `xdg-dialog-v1` | Dialog window hints | ✅ |
| `alpha-modifier-v1` | Per-surface alpha | ✅ |
| `linux-drm-syncobj-v1` | Explicit GPU sync | ✅ |
| `drm-lease-v1` | VR headsets | ✅ |
| `security-context-v1` | Flatpak sandbox identity | ✅ |
| `ext-background-effect-v1` | Background blur | ✅ |
| `xdg-toplevel-icon-v1` + `xdg-toplevel-tag-v1` | Window icons + tags | ✅ |
| `xdg-system-bell-v1` | System bell | ✅ |
| `xwayland-shell-v1` + `xwayland-keyboard-grab-v1` | X11 compat | ✅ |

### P2 — We implement ourselves (smithay doesn't have these)
| Protocol | Purpose | Notes |
|---|---|---|
| `wlr-output-management-v1` v4 | Runtime monitor config | Used by wdisplays, kanshi, wlr-randr |
| `wlr-gamma-control-v1` | Night light | Used by wlsunset, gammastep |
| `wlr-output-power-management-v1` | DPMS on/off | |
| `wlr-foreign-toplevel-management-v1` | Full toplevel control | Advanced taskbars, scripting |
| `ext-workspace-v1` | Workspace management | External workspace indicators |
| `color-management-v1` | HDR, ICC profiles | 🚧 WIP in smithay |

### Non-Wayland but essential
| System | Purpose | Implementation |
|---|---|---|
| **xdg-desktop-portal** | File picker, screenshots, screen sharing, settings, notifications (for sandboxed apps) | Our own D-Bus backend (crates/portal) |
| **PipeWire** | Screen sharing/recording streams | Portal ScreenCast feeds compositor frames into PipeWire |
| **StatusNotifierItem** | System tray icons | D-Bus watcher/host (crates/status-notifier) |
| **org.freedesktop.Notifications** | Desktop notifications | D-Bus server (crates/notification) |
| **MPRIS** | Media player controls | D-Bus client in panel |
| **NetworkManager / Bluez / UPower** | Control centre status | D-Bus clients in panel |
| **logind** | Power management, session control | D-Bus client for shutdown/lock/suspend |

---

## COSMIC as Reference Architecture

COSMIC (System76's DE) was our original architectural reference and many patterns still apply:

- **cosmic-comp** — their compositor, uses smithay, implements `IcedElement` for rendering iced widget trees as GPU textures inside the compositor's render loop for SSD decorations. Our equivalent is the Slint WindowChrome composited into the same render texture.
- **libcosmic** — their fork of iced with additional widgets, theming, and wayland integration. We use Slint instead.
- **cosmic-panel** — their panel, uses `wlr-layer-shell`. Our panel is in-process inside the Slint scene.
- **cosmic-applet-status-area** — their system tray, implements StatusNotifierWatcher + DBusMenu. We do the same in `crates/compositor-slint/src/tray.rs`.
- **cosmic-applets** — panel applets for audio, bluetooth, battery, etc.
- **cosmic-launcher** — their app launcher. Ours is a Slint overlay (Super+Space).
- **cosmic-notifications** — their notification daemon. Ours is `crates/notification`.

Key COSMIC source repos to study:
- https://github.com/pop-os/cosmic-comp (compositor)
- https://github.com/pop-os/libcosmic (toolkit)
- https://github.com/pop-os/cosmic-applets (panel applets)

We are NOT forking COSMIC. We're building our own DE with our own design language (macOS-inspired squircle aesthetics, Spotlight launcher instead of GNOME overview, etc.). But COSMIC's architecture validates our approach and provides concrete implementation patterns.

---

## Development Workflow

### Running in dev mode
```bash
# All-in-one — builds and launches compositor + notification + portal,
# then exports WAYLAND_DISPLAY for any clients you want to point at it.
./dev.sh

# Then in another terminal:
WAYLAND_DISPLAY=wayland-1 alacritty       # or any other wayland client
```

The compositor runs as a wayland client inside the host session
(winit-on-wayland). A bare-TTY DRM/KMS path is not yet wired.

### Why winit backend matters
Without the winit backend, you'd need to switch TTYs or use a nested Wayland compositor to test. With it, you just `cargo run` and your compositor opens as a window on your existing desktop. This is how smithay's own "anvil" sample compositor works — it supports `--winit`, `--x11`, and `--tty-udev` backends.

The winit backend has limitations (single output, no DRM lease, no hardware cursors), but for developing the shell UI, window management, animations, decorations, and protocol handlers, it's perfect.

---

## Key Smithay API Patterns

### Delegate pattern for protocols
```rust
// 1. Store protocol state in your State struct
struct CommonState {
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    // ...
}

// 2. Implement the handler trait
impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.common.compositor_state
    }
    fn commit(&mut self, surface: &WlSurface) {
        // Handle surface commit
    }
}

// 3. Call the delegate macro
delegate_compositor!(State);
// This generates the wayland-server Dispatch impl
```

### Rendering with GlesRenderer
```rust
let renderer: &mut GlesRenderer = backend.renderer();

// Import client surface as texture
let texture = renderer.import_buffer(&buffer, &surface_data)?;

// Draw textured quad
renderer.render(output_size, Transform::Normal, |renderer, frame| {
    frame.clear([0.1, 0.1, 0.1, 1.0])?; // background
    frame.render_texture_at(texture, location, scale, alpha, Transform::Normal)?;
    Ok(())
})?;
```

### calloop event loop
```rust
let mut event_loop = EventLoop::<State>::try_new()?;
let loop_handle = event_loop.handle();

// Register Wayland display as event source
loop_handle.insert_source(wayland_source, |event, _, state| {
    state.common.display.dispatch_clients(state)?;
});

// Register timer for animations
loop_handle.insert_source(Timer::immediate(), |_, _, state| {
    state.render_frame();
    TimeoutAction::ToDuration(Duration::from_millis(16)) // ~60fps
});

// Run
event_loop.run(None, &mut state, |state| {
    state.common.display.flush_clients()?;
})?;
```
