# SUBAGENT: Dock
# Crate: `crates/shell-dock`

> **Read `ARCHITECTURE.md` in the repo root before starting.** It explains why we use Smithay (not wlroots), how the compositor rendering pipeline works, the full Wayland protocol reference, smithay API patterns (delegate pattern, GlesRenderer, calloop), and the winit dev backend.
# Branch: `feat/shell-dock`

You are building a macOS/GNOME-style dock for a Wayland DE. Standalone iced app, layer-shell.

## Layer-shell config
Layer::Top, anchor bottom-centre, auto-width based on icon count, exclusive zone = dock height + margin.

## Features
**Pinned apps:** User-configured list of `.desktop` app_ids (stored in `~/.config/myDE/dock.json`). Always shown.
**Running apps:** Shown with dot indicator. Source: subscribe to `ext-foreign-toplevel-list-v1` via compositor IPC (`WindowOpened`/`WindowClosed` events).
**Minimized indicator:** Different dot for apps with minimized windows.
**Launch:** Click pinned icon → look up `.desktop` Exec field, spawn with setsid. Click running icon → focus window (send `ActivateWindow` via IPC).
**Hover magnification:** Icons scale 1.0→1.15 on hover, spring animation.
**Launch bounce:** When app is launching, icon bounces (spring damping=0.5, stiffness=800).
**Right-click menu:** New Window, Close All Windows, Pin/Unpin, Quit.
**Hover preview:** After 500ms hover on running app, show popover above dock icon with window thumbnails (request `GetWindowsForApp` via IPC, receive window info + cached thumbnail textures). Click thumbnail → focus that window.
**Trash icon:** Rightmost, shows full/empty state based on `~/.local/share/Trash/files/`.
**Auto-hide (optional):** Dock slides down when not in use, slides up on mouse near bottom edge.

## Visual
- Background: frosted glass / semi-transparent with blur (`ext-background-effect-v1`)
- Squircle-rounded container (whole dock is one rounded shape)
- Icon size: configurable, default 48px
- Separator line between pinned and running sections
- macOS border treatment on the container

## Dependencies
`iced`, `ipc` crate, `theme` crate, `rounding` crate, `animation` crate, `freedesktop-desktop-entry` crate for parsing .desktop files

Work iteratively: static pinned icons first, then running app detection, then click-to-launch/focus, then hover magnification, then right-click menu, then hover previews, then auto-hide.
