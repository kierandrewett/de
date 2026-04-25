# SUBAGENT: Shell Panel + Control Centre + Date/Time
# Crate: `crates/shell-panel`

> **Read `ARCHITECTURE.md` in the repo root before starting.** It explains why we use Smithay (not wlroots), how the compositor rendering pipeline works, the full Wayland protocol reference, smithay API patterns (delegate pattern, GlesRenderer, calloop), and the winit dev backend.
# Branch: `feat/shell-panel`

You are building the top panel and control centre for a Wayland DE. This is a standalone iced application that runs as a Wayland client using `wlr-layer-shell`.

## Panel (layer-shell, Layer::Top, full width, top edge)
Layout: `[Focused App Name]  [———— Clock ————]  [Indicators] [Tray] [CC ▼]`

**Left:** Focused window app name (subscribe to `FocusedWindowChanged` via IPC)
**Centre:** Clock (HH:MM format). Click → date/time popout.
**Right indicators:** Screen sharing dot (pulsing), recording dot (red + timer), mic in use. Query compositor via IPC.
**Tray icons:** Consume `TrayEvent` from `status-notifier` crate. Render icons. Click=activate, right-click=DBusMenu dropdown.
**Control Centre button:** Toggle CC popout.

## Date/Time Popout (layer-shell overlay, below clock)
- Month calendar grid, today highlighted, week numbers
- World clocks (configurable city list)
- Squircle-rounded card with macOS border

## Control Centre (layer-shell overlay, top-right)
**Quick toggles:** Wi-Fi, Bluetooth, DND, Night Light, Dark Mode, Airplane Mode
**Sliders:** Volume (PipeWire via D-Bus), Brightness (`/sys/class/backlight/`)
**MPRIS Now Playing:** Album art, track/artist, play/pause/next/prev buttons. Subscribe to `org.mpris.MediaPlayer2.*` on D-Bus, handle `PropertiesChanged` signals for live updates.
**Battery:** UPower D-Bus
**Screen recording:** Start/stop button, elapsed time
**Bottom row:** Settings, Lock, Power menu (shutdown/reboot/suspend via logind)

## D-Bus integrations
- NetworkManager (`org.freedesktop.NetworkManager`): Wi-Fi state + SSID
- Bluez (`org.bluez`): BT on/off + connected devices
- UPower (`org.freedesktop.UPower`): battery level + charging
- MPRIS (`org.mpris.MediaPlayer2.*`): playback state, metadata, controls
- logind (`org.freedesktop.login1`): power off, reboot, suspend, lock

## Dependencies
`iced` with wayland/layer-shell support, `zbus`, `status-notifier` crate, `notification` crate, `ipc` crate, `theme` crate, `rounding` crate, `animation` crate

## Wayland protocols (as client)
`wlr-layer-shell-v1`, `cursor-shape-v1`, `ext-foreign-toplevel-list-v1`

Exclusive zone = panel height. Control centre and date popout have no exclusive zone (float over).

Work iteratively: clock + panel skeleton first, then tray integration, then control centre toggles, then MPRIS, then calendar, then indicators.
