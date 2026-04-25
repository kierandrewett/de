# SUBAGENT: XDG Desktop Portal Backend
# Crate: `crates/portal` + `crates/portal-ui`

> **Read `ARCHITECTURE.md` in the repo root before starting.** It explains why we use Smithay (not wlroots), how the compositor rendering pipeline works, the full Wayland protocol reference, smithay API patterns (delegate pattern, GlesRenderer, calloop), and the winit dev backend.
# Branch: `feat/portal`

You are building the xdg-desktop-portal backend for our DE. This is a D-Bus service that handles sandboxed app requests: file picking, screenshots, screen sharing, settings, notifications.

## Service: `org.freedesktop.impl.portal.desktop.myDE`

### FileChooser (`org.freedesktop.impl.portal.FileChooser`)
Methods: `OpenFile`, `SaveFile`, `SaveFiles`
- Options: `accept_label`, `modal`, `multiple`, `directory`, `filters` (array of (name, [(type, pattern)])), `choices` (array of (id, label, [(id,label)], initial)), `current_folder`
- Response: 0=success, 1=cancelled, 2=error. Results: `uris`, `choices`, `current_filter`
- Spawn `crates/portal-ui` (iced file picker) as child process. Communicate params via CLI args, results via stdout JSON.

**portal-ui** is a standalone iced app showing: sidebar (places/bookmarks), file list (icon+list views), breadcrumb bar, filter dropdown, search, filename entry (save mode). Uses our theme + squircle rounding.

### Settings (`org.freedesktop.impl.portal.Settings`)
Methods: `ReadAll(namespaces: as) → a{sa{sv}}`, `Read(namespace: s, key: s) → v`
Signal: `SettingChanged(namespace: s, key: s, value: v)`
Namespaces:
- `org.freedesktop.appearance`: `color-scheme` (u32: 0=none,1=dark,2=light), `accent-color` ((ddd)), `contrast` (u32)
- `org.gnome.desktop.interface`: `color-scheme` (s), `gtk-theme` (s), `icon-theme` (s), `cursor-theme` (s), `cursor-size` (u32), `font-name` (s), `text-scaling-factor` (d)

### Screenshot (`org.freedesktop.impl.portal.Screenshot`)
Methods: `Screenshot(options → interactive)`, `PickColor`
Communicate with compositor via IPC for capture. Save to temp file, return `file://` URI.

### ScreenCast (`org.freedesktop.impl.portal.ScreenCast`)
Methods: `CreateSession`, `SelectSources` (source_type: monitor/window), `Start`
Feed compositor frames into PipeWire stream. This enables Discord/browser/OBS screen sharing.
Requires PipeWire client integration (`pipewire-rs` crate).

### Notification (`org.freedesktop.impl.portal.Notification`)
Forward to our `org.freedesktop.Notifications` server.

### GlobalShortcuts (`org.freedesktop.impl.portal.GlobalShortcuts`)
Let apps register global keybindings via compositor IPC.

## portal file: `/usr/share/xdg-desktop-portal/portals/myDE.portal`
```ini
[portal]
DBusName=org.freedesktop.impl.portal.desktop.myDE
Interfaces=org.freedesktop.impl.portal.FileChooser;org.freedesktop.impl.portal.Settings;org.freedesktop.impl.portal.Screenshot;org.freedesktop.impl.portal.ScreenCast;org.freedesktop.impl.portal.Notification;org.freedesktop.impl.portal.GlobalShortcuts;
UseIn=myDE;
```

Use `zbus` for D-Bus. Each request gets a unique handle path. Implement `Request.Close` for cancellation.

Work iteratively: Settings first (simplest, immediate value for dark mode), then FileChooser (most visible), then Screenshot, then ScreenCast (most complex, PipeWire).
