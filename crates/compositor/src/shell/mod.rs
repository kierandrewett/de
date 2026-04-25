//! Window management — snapping, maximize, minimize, alt-tab. Owned by subagent 09.
//!
//! The [`Shell`] struct is the top-level container for all window state.  Each
//! sub-module handles one concern; the compositor's main loop calls into here on
//! input events and commit signals.
// Shell module is scaffolded ahead of wiring; silence until subagent 07 connects it.
#![allow(dead_code)]

pub mod alt_tab;
pub mod floating;
pub mod grab;
pub mod maximize;
pub mod minimize;
pub mod output_switch;
pub mod snapping;

use smithay::utils::{Logical, Point, Rectangle, Size};

use animation::animated::{AnimatedFloat, AnimatedRect, AnimatedValue};
use ipc::{Rect, SnapLayout, WindowInfo, WindowState};

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// Whether a window draws its own chrome (CSD) or the compositor does (SSD).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecorationMode {
    /// Client renders its own title bar and borders.
    ClientSide,
    /// Compositor renders the title bar (iced) and border stroke.
    ServerSide,
}

/// Opaque cached frame used for minimize-to-dock animation.
///
/// The render subagent (08) fills `data` with a GPU-exportable handle.
/// The shell module only needs size metadata to scale the animation.
// TODO: replace _data with an actual GPU texture handle once render crate is available
#[derive(Debug, Clone)]
pub struct CachedTexture {
    /// Logical width of the captured frame.
    pub width: u32,
    /// Logical height of the captured frame.
    pub height: u32,
}

/// Per-window spring animation state.
///
/// All geometry transitions (maximize, unmaximize, snap, move) drive the
/// `geometry` spring.  Minimize-to-dock additionally drives `opacity` and
/// `scale` so the texture shrinks toward the dock icon.
pub struct WindowAnimState {
    /// Animated logical geometry `[x, y, w, h]`.
    pub geometry: AnimatedRect,
    /// Animated opacity in `[0.0, 1.0]`.
    pub opacity: AnimatedFloat,
    /// Animated uniform scale factor (1.0 = original size).
    pub scale: AnimatedFloat,
}

impl WindowAnimState {
    /// Create state snapped to `rect` with no in-flight animation.
    pub fn new(rect: Rectangle<i32, Logical>) -> Self {
        let mut geometry: AnimatedRect = AnimatedValue::new_spring(800.0, 1.0, 0.5);
        geometry.set_position([
            rect.loc.x as f64,
            rect.loc.y as f64,
            rect.size.w as f64,
            rect.size.h as f64,
        ]);

        let mut opacity: AnimatedFloat = AnimatedValue::new_spring(600.0, 1.0, 0.001);
        opacity.set_position([1.0]);

        let mut scale: AnimatedFloat = AnimatedValue::new_spring(600.0, 1.0, 0.001);
        scale.set_position([1.0]);

        Self { geometry, opacity, scale }
    }

    /// Advance all components by `dt` seconds.
    pub fn tick(&mut self, dt: f64) {
        self.geometry.tick(dt);
        self.opacity.tick(dt);
        self.scale.tick(dt);
    }

    /// Returns `true` when all animations have settled.
    pub fn is_complete(&self) -> bool {
        self.geometry.is_complete() && self.opacity.is_complete() && self.scale.is_complete()
    }

    /// Set the geometry target; animation will spring toward it.
    pub fn set_geometry_target(&mut self, rect: Rectangle<i32, Logical>) {
        self.geometry.set_target([
            rect.loc.x as f64,
            rect.loc.y as f64,
            rect.size.w as f64,
            rect.size.h as f64,
        ]);
    }

    /// Jump geometry instantly without animation.
    pub fn set_geometry_instant(&mut self, rect: Rectangle<i32, Logical>) {
        self.geometry.set_position([
            rect.loc.x as f64,
            rect.loc.y as f64,
            rect.size.w as f64,
            rect.size.h as f64,
        ]);
        self.geometry.set_target([
            rect.loc.x as f64,
            rect.loc.y as f64,
            rect.size.w as f64,
            rect.size.h as f64,
        ]);
    }

    /// Current interpolated logical geometry, clamped to minimum 1×1.
    pub fn current_geometry(&self) -> Rectangle<i32, Logical> {
        let p = self.geometry.position_f32();
        Rectangle::new(
            Point::from((p[0] as i32, p[1] as i32)),
            Size::from((p[2].max(1.0) as i32, p[3].max(1.0) as i32)),
        )
    }

    /// Current interpolated opacity.
    pub fn current_opacity(&self) -> f32 {
        self.opacity.position_f32()[0].clamp(0.0, 1.0)
    }

    /// Current interpolated scale.
    pub fn current_scale(&self) -> f32 {
        self.scale.position_f32()[0].max(0.0)
    }
}

impl std::fmt::Debug for WindowAnimState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowAnimState")
            .field("geometry", &self.geometry.position())
            .field("opacity", &self.opacity.position())
            .field("scale", &self.scale.position())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Window surface placeholder
// ---------------------------------------------------------------------------

/// Opaque handle to the Wayland surface for a managed window.
///
/// Subagent 07 (wayland protocols) will replace this with the real
/// `smithay::desktop::Window` once xdg-shell handling is wired up.
// TODO: replace with smithay::desktop::Window once subagent 07 is available
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WindowSurface {
    /// Stable token identifying the surface; subagent 07 populates this.
    pub token: u64,
}

// ---------------------------------------------------------------------------
// MappedWindow
// ---------------------------------------------------------------------------

/// A compositor-managed window that is currently mapped (potentially minimized).
#[derive(Debug)]
pub struct MappedWindow {
    /// Compositor-assigned stable identifier.
    pub id: u64,
    /// Wayland surface handle (placeholder until subagent 07 is live).
    pub surface: WindowSurface,
    /// Committed logical geometry (what we last told the client).
    pub geometry: Rectangle<i32, Logical>,
    /// Saved geometry for unmaximize restore.
    pub pre_maximize_rect: Option<Rectangle<i32, Logical>>,
    /// Saved geometry for unminimize restore.
    pub pre_minimize_rect: Option<Rectangle<i32, Logical>>,
    /// SSD or CSD.
    pub decoration_mode: DecorationMode,
    /// Window is currently maximized.
    pub is_maximized: bool,
    /// Window is currently hidden in the dock.
    pub is_minimized: bool,
    /// Window is in exclusive fullscreen.
    pub is_fullscreen: bool,
    /// Active snap zone, if snapped.
    pub snap_state: Option<snapping::SnapTarget>,
    /// Live animation state driven by the shell.
    pub animation: WindowAnimState,
    /// Wayland `app_id` (may be empty).
    pub app_id: String,
    /// Window title (may be empty).
    pub title: String,
    /// Last composited frame, used for minimize animation.
    pub last_frame_texture: Option<CachedTexture>,
}

impl MappedWindow {
    /// Create a new window with the given geometry and no active animations.
    pub fn new(
        id: u64,
        surface: WindowSurface,
        geometry: Rectangle<i32, Logical>,
        decoration_mode: DecorationMode,
        app_id: String,
        title: String,
    ) -> Self {
        Self {
            id,
            surface,
            geometry,
            pre_maximize_rect: None,
            pre_minimize_rect: None,
            decoration_mode,
            is_maximized: false,
            is_minimized: false,
            is_fullscreen: false,
            snap_state: None,
            animation: WindowAnimState::new(geometry),
            app_id,
            title,
            last_frame_texture: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Monitor / workspace helpers
// ---------------------------------------------------------------------------

/// Logical-space information about a connected monitor, as seen by the shell.
#[derive(Debug, Clone)]
pub struct MonitorInfo {
    /// Connector name (e.g. `"HDMI-A-1"`).
    pub name: String,
    /// Full logical rect of this output in compositor space.
    pub logical_rect: Rectangle<i32, Logical>,
    /// Height of the panel exclusive zone at the top.
    pub panel_height: i32,
    /// Height of the dock exclusive zone at the bottom.
    pub dock_height: i32,
}

impl MonitorInfo {
    /// The area available to normal windows (excludes panel and dock).
    pub fn work_area(&self) -> Rectangle<i32, Logical> {
        let r = self.logical_rect;
        let top = self.panel_height;
        let bottom = self.dock_height;
        let h = (r.size.h - top - bottom).max(0);
        Rectangle::new(
            Point::from((r.loc.x, r.loc.y + top)),
            Size::from((r.size.w, h)),
        )
    }
}

/// A virtual workspace (desktop) containing a set of window IDs.
#[derive(Debug, Default, Clone)]
pub struct Workspace {
    /// Human-readable label (e.g. `"1"`, `"Work"`).
    pub name: String,
    /// Window IDs present on this workspace.
    pub window_ids: Vec<u64>,
}

// ---------------------------------------------------------------------------
// Shell
// ---------------------------------------------------------------------------

/// Top-level window-management state, threaded through the calloop event loop.
#[derive(Debug)]
pub struct Shell {
    /// All mapped windows in paint order (back = index 0, front = last).
    pub windows: Vec<MappedWindow>,
    /// Focus stack — window IDs, most-recently-focused last.
    pub focus_stack: Vec<u64>,
    /// Index of the currently visible workspace.
    pub workspace_index: usize,
    /// All virtual workspaces.
    pub workspaces: Vec<Workspace>,
    /// User snap layouts, keyed by monitor connector name.
    pub snap_layouts: std::collections::HashMap<String, Vec<SnapLayout>>,
    /// Connected monitors, updated on hotplug / output change.
    pub monitors: Vec<MonitorInfo>,
    /// Monotonically increasing counter for window ID allocation.
    next_id: u64,
    /// Some(state) when the alt-tab overlay is active.
    pub alt_tab: Option<alt_tab::AltTabState>,
    /// Some(state) when the Super+P output-switch overlay is active.
    pub output_switch: Option<output_switch::OutputSwitchState>,
}

impl Shell {
    /// Create an empty shell with one default workspace.
    pub fn new() -> Self {
        Self {
            windows: Vec::new(),
            focus_stack: Vec::new(),
            workspace_index: 0,
            workspaces: vec![Workspace {
                name: "1".to_string(),
                window_ids: Vec::new(),
            }],
            snap_layouts: std::collections::HashMap::new(),
            monitors: Vec::new(),
            next_id: 1,
            alt_tab: None,
            output_switch: None,
        }
    }

    /// Allocate the next unique window ID.
    pub fn alloc_window_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Look up a window by ID.
    pub fn window(&self, id: u64) -> Option<&MappedWindow> {
        self.windows.iter().find(|w| w.id == id)
    }

    /// Look up a window by ID (mutable).
    pub fn window_mut(&mut self, id: u64) -> Option<&mut MappedWindow> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    /// Remove a window by ID, also cleaning up focus and workspace membership.
    ///
    /// Returns the removed window if it existed.
    pub fn remove_window(&mut self, id: u64) -> Option<MappedWindow> {
        let pos = self.windows.iter().position(|w| w.id == id)?;
        let win = self.windows.remove(pos);
        self.focus_stack.retain(|&fid| fid != id);
        for ws in &mut self.workspaces {
            ws.window_ids.retain(|&wid| wid != id);
        }
        Some(win)
    }

    /// Push `id` to the top of the focus stack (most recently focused).
    pub fn focus_window(&mut self, id: u64) {
        if self.window(id).is_some() {
            self.focus_stack.retain(|&fid| fid != id);
            self.focus_stack.push(id);
        }
    }

    /// The window that currently holds keyboard focus, if any.
    pub fn focused_window_id(&self) -> Option<u64> {
        self.focus_stack.last().copied()
    }

    /// Borrow the currently focused window.
    pub fn focused_window(&self) -> Option<&MappedWindow> {
        self.focused_window_id().and_then(|id| self.window(id))
    }

    /// Windows in stacking order (back to front), skipping minimized ones.
    pub fn visible_windows(&self) -> impl Iterator<Item = &MappedWindow> {
        self.windows.iter().filter(|w| !w.is_minimized)
    }

    /// Advance all window animations by `dt` seconds.
    pub fn tick_animations(&mut self, dt: f64) {
        for win in &mut self.windows {
            win.animation.tick(dt);
        }
    }

    /// First monitor (primary), if any.
    pub fn primary_monitor(&self) -> Option<&MonitorInfo> {
        self.monitors.first()
    }

    /// Find the monitor that contains the given point.
    pub fn monitor_at(&self, point: Point<i32, Logical>) -> Option<&MonitorInfo> {
        self.monitors
            .iter()
            .find(|m| m.logical_rect.contains(point))
    }

    /// Build an IPC [`WindowInfo`] snapshot for the given window.
    pub fn window_info(&self, id: u64) -> Option<WindowInfo> {
        let win = self.window(id)?;
        let g = win.geometry;
        Some(WindowInfo {
            id: win.id,
            app_id: win.app_id.clone(),
            title: win.title.clone(),
            state: WindowState {
                is_focused: self.focused_window_id() == Some(id),
                is_maximized: win.is_maximized,
                is_minimized: win.is_minimized,
                is_fullscreen: win.is_fullscreen,
            },
            geometry: Rect { x: g.loc.x, y: g.loc.y, w: g.size.w, h: g.size.h },
        })
    }

    /// Collect IPC snapshots for all managed windows.
    pub fn all_window_infos(&self) -> Vec<WindowInfo> {
        let ids: Vec<u64> = self.windows.iter().map(|w| w.id).collect();
        ids.iter().filter_map(|&id| self.window_info(id)).collect()
    }
}

impl Default for Shell {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    fn make_monitor() -> MonitorInfo {
        MonitorInfo {
            name: "test-0".to_string(),
            logical_rect: make_rect(0, 0, 1920, 1080),
            panel_height: 30,
            dock_height: 70,
        }
    }

    #[test]
    fn monitor_work_area_excludes_panel_and_dock() {
        let m = make_monitor();
        let wa = m.work_area();
        assert_eq!(wa.loc.y, 30);
        assert_eq!(wa.size.h, 1080 - 30 - 70);
        assert_eq!(wa.size.w, 1920);
    }

    #[test]
    fn shell_alloc_ids_are_unique() {
        let mut shell = Shell::new();
        let ids: Vec<u64> = (0..10).map(|_| shell.alloc_window_id()).collect();
        let unique: std::collections::HashSet<u64> = ids.iter().copied().collect();
        assert_eq!(unique.len(), 10);
    }

    #[test]
    fn shell_focus_stack_tracks_most_recent() {
        let mut shell = Shell::new();
        let id1 = shell.alloc_window_id();
        let id2 = shell.alloc_window_id();
        let surface1 = WindowSurface { token: id1 };
        let surface2 = WindowSurface { token: id2 };
        let rect = make_rect(0, 0, 800, 600);

        shell.windows.push(MappedWindow::new(
            id1, surface1, rect, DecorationMode::ServerSide, "app".into(), "Win 1".into(),
        ));
        shell.windows.push(MappedWindow::new(
            id2, surface2, rect, DecorationMode::ServerSide, "app".into(), "Win 2".into(),
        ));

        shell.focus_window(id1);
        assert_eq!(shell.focused_window_id(), Some(id1));

        shell.focus_window(id2);
        assert_eq!(shell.focused_window_id(), Some(id2));

        // Re-focusing id1 must bring it back to top.
        shell.focus_window(id1);
        assert_eq!(shell.focused_window_id(), Some(id1));
    }

    #[test]
    fn remove_window_cleans_up_focus_and_workspace() {
        let mut shell = Shell::new();
        let id = shell.alloc_window_id();
        let surface = WindowSurface { token: id };
        let rect = make_rect(0, 0, 800, 600);

        shell.windows.push(MappedWindow::new(
            id, surface, rect, DecorationMode::ServerSide, "app".into(), "Win".into(),
        ));
        shell.workspaces[0].window_ids.push(id);
        shell.focus_window(id);

        let removed = shell.remove_window(id);
        assert!(removed.is_some());
        assert!(shell.windows.is_empty());
        assert!(shell.focus_stack.is_empty());
        assert!(shell.workspaces[0].window_ids.is_empty());
    }

    #[test]
    fn window_anim_state_geometry_clamps_to_minimum() {
        let rect = make_rect(100, 200, 800, 600);
        let state = WindowAnimState::new(rect);
        let geo = state.current_geometry();
        assert!(geo.size.w >= 1);
        assert!(geo.size.h >= 1);
    }

    #[test]
    fn window_info_reflects_focus_state() {
        let mut shell = Shell::new();
        let id = shell.alloc_window_id();
        let surface = WindowSurface { token: id };
        let rect = make_rect(0, 0, 800, 600);

        shell.windows.push(MappedWindow::new(
            id, surface, rect, DecorationMode::ServerSide, "myapp".into(), "Title".into(),
        ));
        shell.focus_window(id);

        let info = shell.window_info(id).unwrap();
        assert!(info.state.is_focused);
        assert_eq!(info.app_id, "myapp");
        assert_eq!(info.geometry.w, 800);
    }
}
