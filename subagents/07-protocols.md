# SUBAGENT: Wayland Protocol Wiring
# Crate: `crates/compositor` (module: `src/wayland/`)

> **Read `ARCHITECTURE.md` in the repo root before starting.** It explains why we use Smithay (not wlroots), how the compositor rendering pipeline works, the full Wayland protocol reference, smithay API patterns (delegate pattern, GlesRenderer, calloop), and the winit dev backend.

> **Also read `WINDOW_SPEC.md`** for the window decoration rendering spec (used by the xdg-decoration handler to decide SSD vs CSD treatment).
# Branch: `feat/compositor-protocols`

You are wiring up ALL Wayland protocol handlers in the compositor using smithay's delegate pattern. Smithay provides the protocol logic; you implement the `*Handler` traits on the compositor `State` struct and call `delegate_*!` macros.

## Compositor State Skeleton
```rust
pub struct State {
    pub backend: Backend,
    pub common: CommonState,
}

pub struct CommonState {
    pub display: Display<State>,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub dmabuf_state: DmabufState,
    pub seat_state: SeatState<State>,
    pub output_map: OutputMap,
    pub layer_shell_state: WlrLayerShellState,
    // ... all protocol states
    pub socket_name: String,
}

pub enum Backend {
    Winit(WinitData),
    Udev(UdevData),
}
```

## Protocols to wire up

Create a file per protocol group in `src/wayland/handlers/`:

### `compositor.rs` — Core
- `CompositorHandler`, `delegate_compositor!`
- `ShmHandler`, `delegate_shm!`
- `SeatHandler`, `delegate_seat!`
- `OutputHandler`

### `xdg_shell.rs` — Window management
- `XdgShellHandler`, `delegate_xdg_shell!`
- Handle toplevel map/unmap, popup management, configure events

### `decoration.rs` — Window decorations
- `XdgDecorationHandler`, `delegate_xdg_decoration!` — advertise ServerSide preference
- `KdeServerDecorationHandler` — for Qt apps

### `layer_shell.rs` — Shell surfaces
- `WlrLayerShellHandler`, `delegate_layer_shell!`

### `dmabuf.rs` — GPU buffers
- `DmabufHandler`, `delegate_dmabuf!`

### `input.rs` — Advanced input
- `RelativePointerHandler`, `PointerConstraintsHandler`, `PointerGesturesHandler`
- `KeyboardShortcutsInhibitHandler`
- `CursorShapeHandler`
- `TabletHandler`
- `TextInputHandler`, `InputMethodHandler`, `VirtualKeyboardHandler`

### `output.rs` — Output management
- `XdgOutputHandler`
- `FractionalScaleHandler`

### `clipboard.rs` — Clipboard and selections
- `DataDeviceHandler`
- `PrimarySelectionHandler`
- `DataControlHandler` (ext + wlr variants)

### `idle.rs` — Idle and power
- `IdleNotifyHandler`, `IdleInhibitHandler`

### `session_lock.rs`
- `SessionLockHandler`

### `screencopy.rs` — Screen capture
- `ImageCaptureSourceHandler`, `ImageCopyCaptureHandler`

### `misc.rs` — Everything else
- `XdgActivationHandler`
- `XdgForeignHandler`
- `ForeignToplevelListHandler`
- `SecurityContextHandler`
- `ContentTypeHandler`
- `FifoHandler`
- `CommitTimingHandler`
- `AlphaModifierHandler`
- `XdgDialogHandler`
- `TearingControlHandler`
- `SinglePixelBufferHandler`
- `PresentationTimeHandler`
- `ViewporterHandler`
- `DrmSyncobjHandler`, `DrmLeaseHandler`
- `BackgroundEffectHandler`
- `PointerWarpHandler`
- `XdgSystemBellHandler`
- `XdgToplevelIconHandler`, `XdgToplevelTagHandler`

### `xwayland.rs` — X11 compat
- `XwaylandShellHandler`
- `XwaylandKeyboardGrabHandler`

## Backend setup

### `src/winit.rs`
Set up the winit backend following smithay's anvil example:
- `winit::init()` → `WinitGraphicsBackend` + `WinitEventLoop`
- Create a virtual `Output` matching window size
- Pump winit events in the calloop event loop
- Handle resize events
- Import DMA buffers via EGL

### `src/udev.rs`
Set up the udev/DRM backend:
- Enumerate GPUs via udev
- Open DRM devices
- Set up KMS outputs
- libinput for input handling
- Session management via libseat

### `src/main.rs`
```rust
fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("--winit") => winit::run(),
        Some("--tty-udev") => udev::run(),
        _ => {
            // Auto-detect: if WAYLAND_DISPLAY or DISPLAY is set, use winit
            // Otherwise try udev
        }
    }
}
```

## Reference
- smithay anvil source: https://github.com/Smithay/smithay/tree/master/anvil
- smithay protocol tracking: https://github.com/Smithay/smithay/issues/781
- smithay docs: https://docs.rs/smithay

## Work iteratively
1. Set up the State struct and basic compositor/shm/seat
2. Add xdg-shell and get a window displaying
3. Add winit backend and verify a window opens
4. Wire up remaining protocols one file at a time
5. Add udev backend skeleton
6. Verify each protocol with a test client (e.g. `weston-terminal`, `alacritty`)
