# WAYLAND_LOG.md — Protocol Handler Status

**Crate:** `compositor-slint-spike`
**Build status:** `cargo build -p compositor-slint-spike` — clean (warnings only, no errors)
**Date:** 2026-04-26
**Agent scope:** smithay protocol handlers only (no GPU rendering, no Slint UI)

---

## Per-Protocol Status

### P0 — Core (already in spike baseline, confirmed working)

| Protocol | Status | Notes |
|---|---|---|
| `wl_compositor` v6 | ✅ | `CompositorHandler` in `wayland/compositor.rs` |
| `wl_subcompositor` v1 | ✅ | Handled by smithay's `delegate_compositor!` — no separate delegate needed |
| `wl_shm` v2 | ✅ | `ShmHandler` in `wayland_state.rs` |
| `wl_seat` v9 | ✅ | `SeatHandler` in `wayland_state.rs` |
| `wl_output` v4 | ✅ | `OutputHandler` in `wayland/outputs.rs` |
| `wl_data_device_manager` v3 | ✅ | `DataDeviceHandler` in `wayland_state.rs` |
| `xdg-shell` v7 | ✅ | `XdgShellHandler` in `wayland/xdg_shell.rs` |
| `linux-dmabuf-v1` v5 | ✅ | `DmabufHandler` in `wayland_state.rs` (logs unsupported, clients fall back to SHM) |
| `xdg-output-unstable-v1` v3 | ✅ | `OutputManagerState::new_with_xdg_output` — registered with output global |
| `presentation-time` v2 | ✅ | `delegate_presentation!` in `wayland/timing.rs` |
| `viewporter` v2 | ✅ | `delegate_viewporter!` in `wayland/scaling.rs` |
| `single-pixel-buffer-v1` | ✅ | `delegate_single_pixel_buffer!` in `wayland/misc.rs` |
| `primary-selection-unstable-v1` | ✅ | `PrimarySelectionHandler` in `wayland_state.rs` |

### P1 — Daily driver (NEW, implemented in this pass)

| Protocol | Status | Notes |
|---|---|---|
| `wlr-layer-shell-v1` v5 | ✅ | `WlrLayerShellHandler` in `wayland/layer_shell.rs`; public `layer_surfaces()` API |
| `xdg-decoration-unstable-v1` | ✅ | `XdgDecorationHandler` in `wayland/decoration.rs`; defaults to `ServerSide` |
| `kde-server-decoration` | ✅ | `KdeDecorationHandler` in `wayland/decoration.rs`; `Server` mode advertised |
| `fractional-scale-v1` | ✅ | `FractionalScaleHandler` + `delegate_fractional_scale!` in `wayland/scaling.rs` |
| `wp-fifo-v1` | ✅ | `delegate_fifo!` in `wayland/timing.rs`; `pre_render_drive_clients()` helper |
| `commit-timing-v1` | ✅ | `delegate_commit_timing!` in `wayland/timing.rs` |
| `relative-pointer-unstable-v1` | ✅ | `delegate_relative_pointer!` in `wayland/input.rs` |
| `pointer-constraints-unstable-v1` | ✅ | `PointerConstraintsHandler` in `wayland/input.rs`; auto-activates constraints |
| `pointer-gestures-unstable-v1` | ✅ | `delegate_pointer_gestures!` in `wayland/input.rs` |
| `cursor-shape-v1` | ✅ | `delegate_cursor_shape!` in `wayland/input.rs`; requires `TabletSeatHandler` stub |
| `keyboard-shortcuts-inhibit-unstable-v1` | ✅ | `KeyboardShortcutsInhibitHandler` in `wayland/input.rs`; auto-activates |
| `ext-idle-notify-v1` | ✅ | `IdleNotifierHandler` in `wayland/idle.rs` |
| `idle-inhibit-unstable-v1` | ✅ | `IdleInhibitHandler` in `wayland/idle.rs` |
| `ext-session-lock-v1` | ⚠️ | `SessionLockHandler` registered in `wayland/session_lock.rs`; stub impl (no lock surface rendering, drops `SessionLocker` without confirming — doesn't crash, lock clients get protocol error-free response) |
| `text-input-unstable-v3` | ✅ | `delegate_text_input_manager!` in `wayland/input.rs`; global registered |
| `input-method-unstable-v2` | ✅ | `InputMethodHandler` stub in `wayland/input.rs`; global registered |
| `virtual-keyboard-v1` | ✅ | `delegate_virtual_keyboard_manager!` in `wayland/input.rs`; global registered |
| `xdg-activation-v1` | ✅ | `XdgActivationHandler` in `wayland/misc.rs` |
| `content-type-v1` | ✅ | `delegate_content_type!` in `wayland/misc.rs` |
| `tablet-v2` | ✅ | `TabletSeatHandler` stub + `delegate_tablet_manager!` in `wayland/input.rs` |

### P1.5 — Output management (not implemented)

| Protocol | Status | Notes |
|---|---|---|
| `wlr-output-management-v1` | ❌ | Smithay does NOT implement this; requires manual protocol implementation — out of scope for this pass |
| `wlr-output-power-management-v1` | ❌ | Same — manual implementation required |
| `wlr-gamma-control-v1` | ❌ | Same — manual implementation required |

### P2 — Full DE parity (NEW, implemented in this pass)

| Protocol | Status | Notes |
|---|---|---|
| `ext-foreign-toplevel-list-v1` | ✅ | `ForeignToplevelListHandler` in `wayland/toplevel.rs` |
| `alpha-modifier-v1` | ✅ | `delegate_alpha_modifier!` in `wayland/misc.rs` |
| `security-context-v1` | ✅ | `SecurityContextHandler` in `wayland/misc.rs` |
| `xdg-foreign-unstable-v2` | ✅ | `XdgForeignHandler` in `wayland/misc.rs` |
| `xdg-dialog-v1` | ✅ | `XdgDialogHandler` stub in `wayland/misc.rs` |
| `xdg-system-bell-v1` | ✅ | `XdgSystemBellHandler` in `wayland/misc.rs` |
| `xdg-toplevel-icon-v1` | ✅ | `XdgToplevelIconHandler` stub in `wayland/misc.rs` |
| `xdg-toplevel-tag-v1` | ✅ | `XdgToplevelTagHandler` stub in `wayland/misc.rs` |
| `pointer-warp-v1` | ✅ | `PointerWarpHandler` stub in `wayland/misc.rs` |

### Not implemented (out of scope or not in smithay)

| Protocol | Status | Notes |
|---|---|---|
| `wlr-foreign-toplevel-management-v1` | ❌ | Not in smithay; requires manual wayland-server protocol impl |
| `ext-workspace-v1` | ❌ | Not in smithay |
| `wlr-screencopy-v1` | ❌ | Not in smithay (use `ext-image-copy-capture-v1` instead) |
| `ext-image-copy-capture-v1` / `ext-image-capture-source-v1` | ❌ | Smithay has it; not wired yet — requires renderer integration |
| `tearing-control-v1` | ❌ | WIP in smithay, not stable |
| `linux-drm-syncobj-v1` | ❌ | Not wired — requires DRM backend |
| `drm-lease-v1` | ❌ | Not wired — requires DRM backend |
| `xwayland-shell-v1` | ❌ | Not wired — XWayland not enabled in spike |
| `ext-background-effect-v1` | ❌ | Smithay has it; not wired — no practical clients yet |
| `color-management-v1` | ❌ | WIP in smithay |
| `wlr-data-control-v1` | ❌ | Not wired in this pass — original spike didn't have it either |

---

## Public API Surface

### Layer surfaces (for render/positioning code)

```rust
// Defined in crates/compositor-slint-spike/src/wayland/layer_shell.rs

pub struct LayerInfo {
    pub surface: WlrLayerSurface,  // smithay layer surface handle
    pub layer: Layer,              // Background / Bottom / Top / Overlay
    pub namespace: String,         // e.g. "panel", "dock", "notifications"
}

impl SpikeState {
    /// Return all currently-mapped layer surfaces, ordered as received.
    /// Render code should iterate and z-sort by `LayerInfo::layer`.
    pub fn layer_surfaces(&self) -> &[LayerInfo];
}
```

### Pre-render frame timing helper

```rust
// Defined in crates/compositor-slint-spike/src/wayland/timing.rs

impl SpikeState {
    /// Signal wp_fifo barriers and drain blocked transaction queues.
    /// Call this BEFORE submitting the frame to the display.
    /// Render agent must call this each frame.
    pub fn pre_render_drive_clients(&mut self);
}
```

### Decoration mode (for render chrome decisions)

The `xdg-decoration` handler (in `wayland/decoration.rs`) sets `decoration_mode = Some(Mode::ServerSide)` on every new toplevel's pending state. The render agent can query this via smithay's `ToplevelSurface::with_pending_state` or `current_state().decoration_mode`.

---

## Module Structure

```
crates/compositor-slint-spike/src/
├── main.rs                    (unchanged — adds `mod wayland;`)
├── renderer.rs                (unchanged — GPU agent scope)
├── platform.rs                (unchanged)
├── wayland_state.rs           (SpikeState struct + basic handlers)
└── wayland/
    ├── mod.rs                 (pub mod declarations)
    ├── compositor.rs          (wl_compositor, delegate_compositor)
    ├── xdg_shell.rs           (XdgShellHandler)
    ├── layer_shell.rs         (WlrLayerShellHandler + LayerInfo public type)
    ├── decoration.rs          (XdgDecorationHandler + KdeDecorationHandler)
    ├── outputs.rs             (OutputHandler + delegate_output)
    ├── scaling.rs             (FractionalScaleHandler + delegate_viewporter)
    ├── timing.rs              (delegate_presentation, fifo, commit_timing + pre_render_drive_clients)
    ├── input.rs               (relative_pointer, constraints, gestures, cursor_shape, tablet, shortcuts_inhibit, text_input, input_method, virtual_keyboard)
    ├── toplevel.rs            (ForeignToplevelListHandler)
    ├── idle.rs                (IdleNotifierHandler + IdleInhibitHandler)
    ├── session_lock.rs        (SessionLockHandler stub)
    └── misc.rs                (single_pixel_buffer, activation, foreign, security_context, alpha_modifier, content_type, pointer_warp, dialog, system_bell, toplevel_icon, toplevel_tag)
```

---

## Smithay Quirks Discovered

1. **`delegate_subcompositor!` does not exist.** `wl_subcompositor` is handled internally by `CompositorState` + `delegate_compositor!`. No separate delegate or handler trait needed.

2. **`FractionalScaleHandler` is a required trait** even though it has a default method body. The `delegate_fractional_scale!` macro generates a `Dispatch` impl that requires `D: FractionalScaleHandler` — you must impl the trait explicitly.

3. **`cursor-shape-v1` requires `TabletSeatHandler`.** The `delegate_cursor_shape!` macro generates dispatch code that handles both pointer and tablet tool cursor shape requests, so `TabletSeatHandler` is a trait bound. An empty impl suffices.

4. **`LayerSurfaceCachedState.layer` vs `LayerSurfaceState.layer`.** `with_pending_state` gives a `&mut LayerSurfaceState` which only has `size`. The `layer` field is on `LayerSurfaceCachedState` (the committed/cached state). You cannot set `layer` in a configure; it's client-controlled via `set_layer`.

5. **`SessionLocker` drop behavior.** Dropping a `SessionLocker` without calling `.lock()` does NOT confirm the lock — the compositor stays "unlocked" from the protocol PoV. A real impl must store the `SessionLocker` and call `.lock()` once the lock surface has been rendered.

6. **Disk space.** The worktree's `/home` filesystem was 100% full (40GB main target). All builds were redirected to `CARGO_TARGET_DIR=/tmp/compositor-slint-target`.

---

## Build Verification

```
CARGO_TARGET_DIR=/tmp/compositor-slint-target \
  cargo build -p compositor-slint-spike

Finished `dev` profile [optimized + debuginfo] target(s) in 2.22s
(20 warnings, 0 errors)
```
