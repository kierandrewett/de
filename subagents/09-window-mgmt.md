# SUBAGENT: Window Management — Snapping, Maximize, Minimize, Alt-Tab
# Crate: `crates/compositor` (module: `src/shell/`)

> **Read `ARCHITECTURE.md` in the repo root before starting.** It explains why we use Smithay (not wlroots), how the compositor rendering pipeline works, the full Wayland protocol reference, smithay API patterns (delegate pattern, GlesRenderer, calloop), and the winit dev backend.
# Branch: `feat/compositor-shell`

You are building the window management logic inside the compositor. This handles all interactive windowing features: floating layout, edge/zone snapping, macOS-style maximize, minimize-to-dock, alt-tab, and dock previews.

## Module Structure
```
src/shell/
├── mod.rs            # Shell struct, workspace management
├── floating.rs       # Floating window layout + positioning
├── snapping.rs       # Edge snapping + FancyZones-style zones
├── maximize.rs       # macOS maximize (grow to work area, keep decorations)
├── minimize.rs       # Minimize-to-dock with frozen texture animation
├── alt_tab.rs        # Alt-Tab window switcher overlay
├── output_switch.rs  # Super+P monitor config overlay
└── grab.rs           # Move/resize grabs with snap detection
```

## Dependencies
- `animation` crate — WindowAnimState, spring/easing
- `theme` crate — spacing, corner radius
- `ipc` crate — ShellRequest/ShellEvent types

## Window State
```rust
pub struct MappedWindow {
    pub id: u64,
    pub surface: SmithayWindow,         // smithay Window wrapper
    pub geometry: Rectangle<i32>,        // current logical rect
    pub pre_maximize_rect: Option<Rectangle<i32>>,
    pub pre_minimize_rect: Option<Rectangle<i32>>,
    pub decoration_mode: DecorationMode,
    pub is_maximized: bool,
    pub is_minimized: bool,
    pub is_fullscreen: bool,
    pub snap_state: Option<SnapTarget>,
    pub animation: WindowAnimState,      // from animation crate
    pub app_id: String,
    pub title: String,
    pub last_frame_texture: Option<CachedTexture>,
}
```

## macOS-style Maximize
- Work area = monitor rect - panel height (top) - dock height (bottom)
- Window grows from current rect to work area rect via spring animation (damping=1.0, stiffness=800)
- Window KEEPS title bar, rounded corners, and decorations (NOT like GNOME which hides them)
- Save `pre_maximize_rect` for unmaximize
- Unmaximize: spring back to saved rect

## Minimize to Dock
1. Capture window's last composited frame as frozen texture
2. Query dock for icon position via IPC: `GetDockIconPosition(app_id)` → `Rect`
3. Animate frozen texture: position+scale from window rect → dock icon rect (spring)
4. After animation: hide window, mark minimized
5. Unminimize (dock click): reverse animation, then show window + focus

## Edge Snapping
```rust
pub struct SnapDetector {
    edge_threshold: i32,    // 10px from screen edge
    corner_threshold: i32,  // 20px from corner
}

pub enum SnapTarget {
    LeftHalf, RightHalf, TopHalf, BottomHalf,
    TopLeft, TopRight, BottomLeft, BottomRight,
    Maximize,
    Zone { layout: usize, zone: usize },
}

impl SnapDetector {
    pub fn detect(&self, cursor: Point, monitor: &MonitorInfo, layouts: &[SnapLayout]) -> Option<SnapTarget>;
}
```

When dragging a window, show a semi-transparent snap preview ghost (squircle-rounded rectangle) at the target zone with spring animation. On drop: animate window to snap rect.

## Snap Zones (FancyZones)
User-defined layouts (halves, thirds, quarters, 2/3+1/3). Hold modifier key during drag to show zone overlay. Layout stored per-monitor in config.

## Alt-Tab
Layer-shell overlay showing window thumbnails. Uses cached `last_frame_texture` for each window. Tab/Shift-Tab cycles, release Alt confirms, Escape cancels. Squircle-rounded preview cards. Selected card: highlighted border + scale 1.05.

## IPC
The compositor runs a unix socket server. On each shell event (focus change, window open/close, etc), broadcast to all connected clients. Handle incoming requests (GetWindowsForApp, GetDockIconPosition, etc).

Work iteratively: floating layout first, then maximize, then minimize, then snapping, then alt-tab, then IPC server.
