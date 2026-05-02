//! Window Manager state for compositor-slint.
//!
//! Owns per-window geometry, focus stack, z-order, animation state, and the
//! smart-cascade placement logic.  The Rust render loop calls into this module
//! to translate `SpikeState::toplevels` into `WindowItem` lists for Slint.

use std::collections::HashMap;
use std::time::Instant;

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use tracing::{debug, info};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Panel height (reserved at top of screen).
pub const PANEL_HEIGHT: i32 = 34;
/// Dock height (reserved at bottom of screen). Matches Dock.slint:
/// `64 px (icons) + 2 * dock-padding + 2 * dock-outer-gap` with both = 8 px.
pub const DOCK_HEIGHT: i32 = 96;
/// Titlebar height that the SSD chrome adds above the client content.
pub const TITLEBAR_HEIGHT: f64 = 33.0;

/// Initial cascade base offset (top-left corner of first new window).
const CASCADE_BASE_X: i32 = 120;
const CASCADE_BASE_Y: i32 = 80;
/// Per-window cascade step.
const CASCADE_STEP: i32 = 30;

/// Default new-window content size sent via configure.
pub const DEFAULT_WINDOW_W: i32 = 800;
pub const DEFAULT_WINDOW_H: i32 = 600;

/// Spring stiffness for open/close/min/max animations.
/// Tuned for a macOS-feel: ~250 ms total animation with no overshoot.
pub const SPRING_STIFFNESS: f64 = 200.0;
/// Damping ratio. 1.0 = critically damped (no bounce, smooth ease-out feel).
pub const SPRING_DAMPING: f64 = 1.0;
/// Spring settle epsilon.
pub const SPRING_EPSILON: f64 = 0.001;

/// Open animation start scale (window scales from this to 1.0 as it opens).
pub const OPEN_SCALE_FROM: f64 = 0.85;
/// Close animation end scale.
pub const CLOSE_SCALE_TO: f64 = 0.85;
/// Minimize animation end scale.
pub const MINIMIZE_SCALE_TO: f64 = 0.40;

/// Close animation settle threshold: when opacity falls below this, remove the window.
pub const CLOSE_OPACITY_THRESHOLD: f32 = 0.01;

// ── Animation phases ──────────────────────────────────────────────────────────

/// Lifecycle animation phase for a window.
#[derive(Debug, Clone, PartialEq)]
pub enum AnimPhase {
    /// Opening: opacity 0→1, scale 0.92→1.0
    Opening,
    /// Fully open, no lifecycle animation in flight.
    Open,
    /// Closing: opacity 1→0, scale 1.0→0.92
    Closing,
    /// Minimized (hidden from scene).
    Minimized,
    /// Restoring from minimized.
    Restoring,
}

// ── Per-window spring state ────────────────────────────────────────────────────

/// A simple damped spring (semi-implicit Euler), self-contained so we don't
/// need the `animation` crate as a workspace dep for compositor-slint.
#[derive(Debug, Clone)]
pub struct Spring {
    stiffness: f64,
    damping_ratio: f64,
    epsilon: f64,
    pos: f64,
    vel: f64,
    target: f64,
    done: bool,
}

impl Spring {
    pub fn new(stiffness: f64, damping_ratio: f64, epsilon: f64) -> Self {
        Self {
            stiffness,
            damping_ratio,
            epsilon,
            pos: 0.0,
            vel: 0.0,
            target: 0.0,
            done: true,
        }
    }

    /// Instantaneous jump, no animation.
    pub fn set_instant(&mut self, value: f64) {
        self.pos = value;
        self.target = value;
        self.vel = 0.0;
        self.done = true;
    }

    /// Animate toward `target`.
    pub fn set_target(&mut self, target: f64) {
        self.target = target;
        if (self.pos - target).abs() >= self.epsilon {
            self.done = false;
        }
    }

    /// Advance by `dt` seconds (subdivided into 1 ms steps for stability).
    pub fn tick(&mut self, dt: f64) {
        if self.done || dt <= 0.0 {
            return;
        }
        const MAX_STEP: f64 = 0.001;
        let mass = 1.0_f64;
        let damping = self.damping_ratio * 2.0 * (self.stiffness * mass).sqrt();
        let mut remaining = dt;
        while remaining > 0.0 {
            let step = remaining.min(MAX_STEP);
            remaining -= step;
            let spring_force = -self.stiffness * (self.pos - self.target);
            let damp_force = -damping * self.vel;
            let accel = spring_force + damp_force; // mass = 1
            self.vel += accel * step;
            self.pos += self.vel * step;
        }
        if (self.pos - self.target).abs() < self.epsilon && self.vel.abs() < self.epsilon {
            self.pos = self.target;
            self.vel = 0.0;
            self.done = true;
        }
    }

    pub fn value(&self) -> f64 {
        self.pos
    }

    pub fn value_f32(&self) -> f32 {
        self.pos as f32
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    pub fn at_target(&self) -> bool {
        self.done && (self.pos - self.target).abs() < self.epsilon
    }
}

// ── WindowAnimState ────────────────────────────────────────────────────────────

/// Per-window spring animation values fed to the Slint `WindowItem`.
#[derive(Debug, Clone)]
pub struct WindowAnimState {
    /// Opacity spring: 0.0 = invisible, 1.0 = fully visible.
    pub opacity: Spring,
    /// Scale spring: 0.92 = shrunk (open/close start), 1.0 = full size.
    pub scale: Spring,
    /// Geometry springs: x, y, w, h.
    pub geo_x: Spring,
    pub geo_y: Spring,
    pub geo_w: Spring,
    pub geo_h: Spring,
}

impl WindowAnimState {
    pub fn new_opening(x: i32, y: i32, w: i32, h: i32) -> Self {
        let mut opacity = Spring::new(SPRING_STIFFNESS, SPRING_DAMPING, SPRING_EPSILON);
        opacity.set_instant(0.0);
        opacity.set_target(1.0);

        let mut scale = Spring::new(SPRING_STIFFNESS, SPRING_DAMPING, SPRING_EPSILON);
        scale.set_instant(OPEN_SCALE_FROM);
        scale.set_target(1.0);

        let mut geo_x = Spring::new(SPRING_STIFFNESS, SPRING_DAMPING, SPRING_EPSILON);
        geo_x.set_instant(x as f64);
        let mut geo_y = Spring::new(SPRING_STIFFNESS, SPRING_DAMPING, SPRING_EPSILON);
        geo_y.set_instant(y as f64);
        let mut geo_w = Spring::new(SPRING_STIFFNESS, SPRING_DAMPING, SPRING_EPSILON);
        geo_w.set_instant(w as f64);
        let mut geo_h = Spring::new(SPRING_STIFFNESS, SPRING_DAMPING, SPRING_EPSILON);
        geo_h.set_instant(h as f64);

        Self { opacity, scale, geo_x, geo_y, geo_w, geo_h }
    }

    pub fn tick(&mut self, dt: f64) {
        self.opacity.tick(dt);
        self.scale.tick(dt);
        self.geo_x.tick(dt);
        self.geo_y.tick(dt);
        self.geo_w.tick(dt);
        self.geo_h.tick(dt);
    }

    pub fn is_settled(&self) -> bool {
        self.opacity.is_done()
            && self.scale.is_done()
            && self.geo_x.is_done()
            && self.geo_y.is_done()
            && self.geo_w.is_done()
            && self.geo_h.is_done()
    }

    /// Set geometry instantly (no animation).
    pub fn set_geometry_instant(&mut self, x: i32, y: i32, w: i32, h: i32) {
        self.geo_x.set_instant(x as f64);
        self.geo_y.set_instant(y as f64);
        self.geo_w.set_instant(w as f64);
        self.geo_h.set_instant(h as f64);
    }

    /// Animate geometry toward target.
    pub fn set_geometry_target(&mut self, x: i32, y: i32, w: i32, h: i32) {
        self.geo_x.set_target(x as f64);
        self.geo_y.set_target(y as f64);
        self.geo_w.set_target(w as f64);
        self.geo_h.set_target(h as f64);
    }

    /// Current interpolated geometry.
    pub fn current_x(&self) -> i32 { self.geo_x.value() as i32 }
    pub fn current_y(&self) -> i32 { self.geo_y.value() as i32 }
    pub fn current_w(&self) -> i32 { self.geo_w.value().max(1.0) as i32 }
    pub fn current_h(&self) -> i32 { self.geo_h.value().max(1.0) as i32 }
}

// ── WindowState ────────────────────────────────────────────────────────────────

/// All compositor-side state for one managed toplevel.
#[derive(Debug)]
pub struct WindowState {
    /// Stable ID (1-based, matching `WindowItem.id` in Slint).
    pub id: i32,
    /// The wayland surface.
    pub surface: WlSurface,
    /// Committed logical geometry (content area, not including titlebar).
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// Geometry before maximize (for restore).
    pub pre_maximize: Option<(i32, i32, i32, i32)>,
    /// Geometry before minimize (for restore).
    pub pre_minimize: Option<(i32, i32, i32, i32)>,
    /// Geometry before the most recent snap (left/right half, quarter,
    /// maximize-via-edge-snap). When the user starts a Move drag on a
    /// window with this set, we restore to that pre-snap rect so dragging
    /// out of a snap feels like macOS/Windows.
    pub pre_snap: Option<(i32, i32, i32, i32)>,
    /// Whether this window currently has keyboard focus.
    pub focused: bool,
    pub minimized: bool,
    pub maximized: bool,
    /// When true the window is playing its close animation; removed after settle.
    pub closing: bool,
    /// Z-order index (higher = on top).
    pub z_order: usize,
    /// Animation springs.
    pub anim: WindowAnimState,
    /// Current lifecycle phase.
    pub phase: AnimPhase,
    /// `app_id` from the xdg-toplevel, used for dock integration.
    pub app_id: String,
    /// Window title.
    pub title: String,
    /// When true, the alt-tab ring is currently selecting this window.
    pub alt_tab_selected: bool,
    /// True until the client commits its first buffer. The open spring stays
    /// parked while this is true; we kick it off in the renderer when the
    /// first buffer arrives so the open animation is actually visible.
    pub awaiting_first_render: bool,
    /// Mirror of `ToplevelInfo.csd`. When true the client paints its own
    /// titlebar/border, so hit-test should not add `TITLEBAR_HEIGHT`.
    pub csd: bool,
    /// Visible-rect dims from `xdg_surface.set_window_geometry` (or our
    /// pixel-driven auto-detect), in logical pixels. CSD clients pad their
    /// buffer with shadow + corner pixels; the geom rect is what we actually
    /// display, and is therefore the size hit-tests / chrome layout should
    /// use. Falls back to `(w, h)` when the client never set a
    /// window-geometry and the buffer is fully opaque.
    pub geom_w: i32,
    pub geom_h: i32,
    /// Visible-rect offset within the buffer. Non-zero for CSD apps whose
    /// shadow/border padding we cropped out — pointer events need to add
    /// `(geom_x, geom_y)` to chrome-space coords to address the right pixel
    /// in the buffer (and therefore the right subsurface).
    pub geom_x: i32,
    pub geom_y: i32,
}

impl WindowState {
    pub fn new(id: i32, surface: WlSurface, x: i32, y: i32, w: i32, h: i32, z_order: usize) -> Self {
        // Spring at PARKED start values (target = current, done = true).
        // start_open() is fired from the renderer when the first buffer
        // commits so the user sees the animation play.
        let mut opacity = Spring::new(SPRING_STIFFNESS, SPRING_DAMPING, SPRING_EPSILON);
        opacity.set_instant(0.0);
        let mut scale = Spring::new(SPRING_STIFFNESS, SPRING_DAMPING, SPRING_EPSILON);
        scale.set_instant(OPEN_SCALE_FROM);
        let mut geo_x = Spring::new(SPRING_STIFFNESS, SPRING_DAMPING, SPRING_EPSILON);
        geo_x.set_instant(x as f64);
        let mut geo_y = Spring::new(SPRING_STIFFNESS, SPRING_DAMPING, SPRING_EPSILON);
        geo_y.set_instant(y as f64);
        let mut geo_w = Spring::new(SPRING_STIFFNESS, SPRING_DAMPING, SPRING_EPSILON);
        geo_w.set_instant(w as f64);
        let mut geo_h = Spring::new(SPRING_STIFFNESS, SPRING_DAMPING, SPRING_EPSILON);
        geo_h.set_instant(h as f64);
        let anim = WindowAnimState { opacity, scale, geo_x, geo_y, geo_w, geo_h };

        Self {
            id,
            surface,
            x,
            y,
            w,
            h,
            pre_maximize: None,
            pre_minimize: None,
            pre_snap: None,
            focused: false,
            minimized: false,
            maximized: false,
            closing: false,
            z_order,
            anim,
            phase: AnimPhase::Opening,
            app_id: String::new(),
            title: String::new(),
            alt_tab_selected: false,
            awaiting_first_render: true,
            csd: false,
            geom_w: w,
            geom_h: h,
            geom_x: 0,
            geom_y: 0,
        }
    }

    /// Start the open animation (opacity 0→1, scale OPEN_SCALE_FROM→1.0).
    pub fn start_open(&mut self) {
        self.anim.opacity.set_instant(0.0);
        self.anim.opacity.set_target(1.0);
        self.anim.scale.set_instant(OPEN_SCALE_FROM);
        self.anim.scale.set_target(1.0);
        self.phase = AnimPhase::Opening;
    }

    /// Start the close animation.
    pub fn start_close(&mut self) {
        self.closing = true;
        self.anim.opacity.set_target(0.0);
        self.anim.scale.set_target(CLOSE_SCALE_TO);
        self.phase = AnimPhase::Closing;
    }

    /// Start the minimize animation: fade + shrink toward dock area.
    pub fn start_minimize(&mut self, dock_target_y: i32) {
        self.pre_minimize = Some((self.x, self.y, self.w, self.h));
        self.anim.opacity.set_target(0.0);
        self.anim.scale.set_target(MINIMIZE_SCALE_TO);
        self.anim.set_geometry_target(self.x + self.w / 4, dock_target_y, self.w / 2, self.h / 2);
        self.minimized = true;
        self.phase = AnimPhase::Minimized;
    }

    /// Restore a minimized window.
    pub fn start_restore(&mut self) {
        if let Some((rx, ry, rw, rh)) = self.pre_minimize.take() {
            self.anim.set_geometry_target(rx, ry, rw, rh);
            self.anim.opacity.set_target(1.0);
            self.anim.scale.set_target(1.0);
            self.x = rx;
            self.y = ry;
            self.w = rw;
            self.h = rh;
        }
        self.minimized = false;
        self.phase = AnimPhase::Restoring;
    }

    /// Start maximize: animate geometry to output size minus reserved
    /// (panel/dock) areas. `top/bottom/left/right` are the per-edge work-area
    /// reservations from the layer-shell exclusive-zone arrange pass and
    /// already include the built-in chrome floors.
    pub fn start_maximize_in(
        &mut self,
        output_w: i32,
        output_h: i32,
        top: i32,
        bottom: i32,
        left: i32,
        right: i32,
    ) {
        self.pre_maximize = Some((self.x, self.y, self.w, self.h));
        let max_x = left;
        let max_y = top;
        let max_w = (output_w - left - right).max(0);
        let max_h = (output_h - top - bottom).max(0);
        self.anim.set_geometry_target(max_x, max_y, max_w, max_h);
        self.anim.opacity.set_target(1.0);
        self.anim.scale.set_target(1.0);
        self.maximized = true;
        // Update committed geometry immediately so configure is correct.
        self.x = max_x;
        self.y = max_y;
        self.w = max_w;
        self.h = max_h;
    }

    /// Back-compat shim that uses the built-in panel/dock constants only —
    /// callers that have access to the WM's reserved-zone state should
    /// prefer `start_maximize_in`.
    pub fn start_maximize(&mut self, output_w: i32, output_h: i32) {
        self.start_maximize_in(output_w, output_h, PANEL_HEIGHT, DOCK_HEIGHT, 0, 0);
    }

    /// Restore from maximize.
    pub fn start_unmaximize(&mut self) {
        if let Some((rx, ry, rw, rh)) = self.pre_maximize.take() {
            self.anim.set_geometry_target(rx, ry, rw, rh);
            self.x = rx;
            self.y = ry;
            self.w = rw;
            self.h = rh;
        }
        self.maximized = false;
    }

    /// Advance springs by `dt` seconds; returns true when the open phase settles.
    pub fn tick(&mut self, dt: f64) {
        self.anim.tick(dt);
        if self.phase == AnimPhase::Opening && self.anim.opacity.is_done() && self.anim.scale.is_done() {
            self.phase = AnimPhase::Open;
        }
        if self.phase == AnimPhase::Restoring && self.anim.is_settled() {
            self.phase = AnimPhase::Open;
        }
    }

    /// Whether the window should be visible in the Slint scene.
    /// A minimized window is hidden; a closing window stays until opacity ≈ 0.
    pub fn is_visible(&self) -> bool {
        if self.minimized && self.phase == AnimPhase::Minimized {
            // Keep visible until animation settles.
            return !self.anim.is_settled();
        }
        true
    }

    /// Whether the close animation has settled and the window can be dropped.
    pub fn close_done(&self) -> bool {
        self.closing && self.anim.opacity.value_f32() <= CLOSE_OPACITY_THRESHOLD
    }
}

// ── WindowManager ──────────────────────────────────────────────────────────────

/// Top-level window manager, owned by `CompositorApp`.
///
/// Maps `wl_surface` identity (via raw pointer) to per-window state.  Also
/// holds the focus stack (most-recently-focused at the back) and a cascade
/// counter for smart placement.
pub struct WindowManager {
    /// All managed windows, keyed by surface id (raw pointer as usize).
    pub windows: HashMap<usize, WindowState>,
    /// Focus stack — surface ids, most-recently-focused last.
    pub focus_stack: Vec<usize>,
    /// Next stable ID counter.
    next_id: i32,
    /// Next z-order counter.
    next_z: usize,
    /// Cascade slot counter (wraps at screen edges).
    cascade_slot: i32,
    /// Output size (set from renderer, default 1280×960).
    pub output_w: i32,
    pub output_h: i32,
    /// Per-edge area reserved by mapped wlr-layer-shell surfaces with an
    /// exclusive zone (refreshed each frame from `SpikeState::reserved_zones`).
    /// Toplevel placement / maximize uses `top.max(PANEL_HEIGHT)` etc. so the
    /// built-in shell chrome still claims its slot when no third-party panel
    /// is mapped, but a real panel/dock pushes windows out of the way.
    pub reserved_top: i32,
    pub reserved_bottom: i32,
    pub reserved_left: i32,
    pub reserved_right: i32,
    /// Alt-tab state: current index into focus_stack (None = inactive).
    pub alt_tab_idx: Option<usize>,
    /// Instant of last animation tick (for dt computation).
    last_tick: Option<Instant>,
}

impl WindowManager {
    pub fn new(output_w: i32, output_h: i32) -> Self {
        Self {
            windows: HashMap::new(),
            focus_stack: Vec::new(),
            next_id: 1,
            next_z: 0,
            cascade_slot: 0,
            output_w,
            output_h,
            reserved_top: 0,
            reserved_bottom: 0,
            reserved_left: 0,
            reserved_right: 0,
            alt_tab_idx: None,
            last_tick: None,
        }
    }

    /// Forward per-edge exclusive-zone reservations from the layer-shell
    /// arrange pass. Called once per frame; cheap (4 i32 writes).
    pub fn set_reserved_zones(&mut self, top: i32, bottom: i32, left: i32, right: i32) {
        self.reserved_top = top.max(0);
        self.reserved_bottom = bottom.max(0);
        self.reserved_left = left.max(0);
        self.reserved_right = right.max(0);
    }

    /// Effective top/bottom/left/right reserved area for toplevel placement.
    /// Built-in shell chrome (PANEL_HEIGHT / DOCK_HEIGHT) acts as a floor —
    /// a third-party panel with a larger exclusive zone wins.
    pub fn effective_top(&self) -> i32 {
        self.reserved_top.max(PANEL_HEIGHT)
    }
    pub fn effective_bottom(&self) -> i32 {
        self.reserved_bottom.max(DOCK_HEIGHT)
    }
    pub fn effective_left(&self) -> i32 {
        self.reserved_left
    }
    pub fn effective_right(&self) -> i32 {
        self.reserved_right
    }

    /// Surface key: raw pointer cast to usize (stable for the lifetime of the surface).
    fn key(surface: &WlSurface) -> usize {
        use smithay::reexports::wayland_server::Resource;
        surface.id().protocol_id() as usize
    }

    /// Register a newly-mapped toplevel. Returns the assigned stable ID.
    pub fn add_window(&mut self, surface: WlSurface) -> i32 {
        let id = self.next_id;
        self.next_id += 1;
        let z = self.next_z;
        self.next_z += 1;

        // Smart cascade placement: try to avoid overlap.
        let (x, y) = self.smart_cascade_position();

        let key = Self::key(&surface);
        // Spring stays parked until the renderer sees the first buffer commit
        // and calls start_open() — keeps the open animation visible.
        let win = WindowState::new(id, surface.clone(), x, y, DEFAULT_WINDOW_W, DEFAULT_WINDOW_H, z);

        info!("WM: add window id={} key={} at ({},{}) z={}", id, key, x, y, z);
        self.windows.insert(key, win);
        self.focus_stack.push(key);
        id
    }

    /// Compute smart cascade position for the next new window.
    /// First window is centred on the safe area (panel + dock excluded);
    /// subsequent windows cascade by 30 px down/right, wrapping at edges.
    fn smart_cascade_position(&mut self) -> (i32, i32) {
        let slot = self.cascade_slot;
        self.cascade_slot += 1;

        let top = self.effective_top();
        let bottom = self.effective_bottom();
        let left = self.effective_left();
        let right = self.effective_right();

        // Centre the very first spawned window on the visible safe area —
        // matches macOS / GNOME "open in middle of screen" intuition.
        if slot == 0 && self.windows.is_empty() {
            let safe_h = (self.output_h - top - bottom).max(DEFAULT_WINDOW_H);
            let safe_w = (self.output_w - left - right).max(DEFAULT_WINDOW_W);
            let cx = left + (safe_w - DEFAULT_WINDOW_W) / 2;
            let cy = top + (safe_h - DEFAULT_WINDOW_H).max(0) / 2;
            return (cx.max(left + 20), cy.max(top));
        }

        let x = CASCADE_BASE_X.max(left) + slot * CASCADE_STEP;
        let y = CASCADE_BASE_Y + top + slot * CASCADE_STEP;

        // Wrap when reaching edges (leave room for the default window size).
        let max_x = self.output_w - DEFAULT_WINDOW_W - right - 20;
        let max_y = self.output_h - DEFAULT_WINDOW_H - bottom - 20;

        // If wrapped past edges, reset cascade.
        if x > max_x.max(CASCADE_BASE_X) || y > max_y.max(CASCADE_BASE_Y + top) {
            self.cascade_slot = 0;
            return (CASCADE_BASE_X.max(left), CASCADE_BASE_Y + top);
        }

        (x, y)
    }

    /// Remove a toplevel when the wayland surface is destroyed.
    pub fn remove_window(&mut self, surface: &WlSurface) {
        let key = Self::key(surface);
        self.windows.remove(&key);
        self.focus_stack.retain(|&k| k != key);
        debug!("WM: removed window key={}", key);
    }

    /// Begin the close animation for a window. The actual removal happens after
    /// the animation settles (via `sweep_closed`).
    pub fn begin_close(&mut self, surface: &WlSurface) {
        let key = Self::key(surface);
        if let Some(win) = self.windows.get_mut(&key) {
            win.start_close();
        }
        self.focus_stack.retain(|&k| k != key);
        // Re-focus the next window if any.
        self.focus_top();
    }

    /// Begin close by window ID.
    pub fn begin_close_by_id(&mut self, id: i32) {
        let key = self.windows.iter()
            .find(|(_, w)| w.id == id)
            .map(|(&k, _)| k);
        if let Some(key) = key {
            if let Some(win) = self.windows.get_mut(&key) {
                win.start_close();
            }
            self.focus_stack.retain(|&k| k != key);
            self.focus_top();
        }
    }

    /// Sweep windows whose close animation has settled; returns their surfaces
    /// for `xdg_toplevel.close` dispatch.
    pub fn sweep_closed(&mut self) -> Vec<WlSurface> {
        let mut done_keys: Vec<usize> = Vec::new();
        for (&key, win) in &self.windows {
            if win.close_done() {
                done_keys.push(key);
            }
        }
        let mut surfaces = Vec::new();
        for key in done_keys {
            if let Some(win) = self.windows.remove(&key) {
                surfaces.push(win.surface);
            }
            self.focus_stack.retain(|&k| k != key);
        }
        surfaces
    }

    /// Focus a window by surface, raise it to the top of z-order.
    pub fn focus_surface(&mut self, surface: &WlSurface) {
        let key = Self::key(surface);
        if !self.windows.contains_key(&key) {
            return;
        }

        // Raise z-order.
        let new_z = self.next_z;
        self.next_z += 1;
        if let Some(win) = self.windows.get_mut(&key) {
            win.z_order = new_z;
            win.focused = true;
        }

        // Update focus state on all windows.
        for (&k, win) in &mut self.windows {
            win.focused = k == key;
        }

        // Update focus stack (remove and re-push to make it MRU-last).
        self.focus_stack.retain(|&k| k != key);
        self.focus_stack.push(key);

        debug!("WM: focus surface key={}", key);
    }

    /// Focus a window by its stable ID.
    pub fn focus_by_id(&mut self, id: i32) {
        let key = self.windows.iter()
            .find(|(_, w)| w.id == id)
            .map(|(&k, _)| k);
        if let Some(key) = key {
            // Raise z.
            let new_z = self.next_z;
            self.next_z += 1;
            for (&k, win) in &mut self.windows {
                win.focused = k == key;
                if k == key {
                    win.z_order = new_z;
                }
            }
            self.focus_stack.retain(|&k| k != key);
            self.focus_stack.push(key);
        }
    }

    /// Focus the topmost (last focus_stack entry) window.
    fn focus_top(&mut self) {
        if let Some(&key) = self.focus_stack.last() {
            let new_z = self.next_z;
            self.next_z += 1;
            for (&k, win) in &mut self.windows {
                win.focused = k == key;
                if k == key {
                    win.z_order = new_z;
                }
            }
        } else {
            for win in self.windows.values_mut() {
                win.focused = false;
            }
        }
    }

    /// Look up the currently focused surface.
    pub fn focused_surface(&self) -> Option<WlSurface> {
        self.focus_stack.last().and_then(|k| self.windows.get(k)).map(|w| w.surface.clone())
    }

    /// Look up the currently focused window's stable ID.
    pub fn focused_id(&self) -> Option<i32> {
        self.windows.values().find(|w| w.focused).map(|w| w.id)
    }

    /// Return the stable ids of every mapped (non-closing) window whose
    /// `app_id` matches `target` exactly. Used by the dock context menu's
    /// "Show All Windows" / "Quit" actions.
    pub fn ids_for_app(&self, target: &str) -> Vec<i32> {
        self.windows.values()
            .filter(|w| !w.closing && w.app_id == target)
            .map(|w| w.id)
            .collect()
    }

    /// Drop focus from every window (e.g. after a click on the desktop).
    /// Returns true if at least one window changed focus state.
    pub fn unfocus_all(&mut self) -> bool {
        let mut changed = false;
        for win in self.windows.values_mut() {
            if win.focused {
                win.focused = false;
                changed = true;
            }
        }
        self.focus_stack.clear();
        changed
    }

    /// Handle pointer click at compositor-space (x, y): focus the topmost window under cursor.
    /// Returns the surface that was focused (if any).
    pub fn pointer_click_focus(&mut self, x: f64, y: f64) -> Option<WlSurface> {
        // Find the topmost window under (x,y) — highest z_order wins.
        let mut best: Option<(usize, usize)> = None; // (key, z_order)
        for (&key, win) in &self.windows {
            if win.closing || (win.minimized && win.anim.is_settled()) {
                continue;
            }
            let wx = win.anim.current_x() as f64;
            let wy = win.anim.current_y() as f64;
            // Visual chrome footprint = client's geom rect (excludes CSD
            // shadow padding) + our titlebar for SSD.
            let total_w = win.geom_w.max(1) as f64;
            let total_h = win.geom_h.max(1) as f64
                + if win.csd { 0.0 } else { TITLEBAR_HEIGHT };
            if x >= wx && x < wx + total_w && y >= wy && y < wy + total_h {
                if best.map_or(true, |(_, z)| win.z_order > z) {
                    best = Some((key, win.z_order));
                }
            }
        }
        if let Some((key, _)) = best {
            let new_z = self.next_z;
            self.next_z += 1;
            for (&k, win) in &mut self.windows {
                win.focused = k == key;
                if k == key {
                    win.z_order = new_z;
                }
            }
            self.focus_stack.retain(|&k| k != key);
            self.focus_stack.push(key);
            self.windows.get(&key).map(|w| w.surface.clone())
        } else {
            None
        }
    }

    /// Begin minimize.
    pub fn minimize_by_id(&mut self, id: i32) {
        let dock_target_y = self.output_h - DOCK_HEIGHT;
        let key = self.windows.iter()
            .find(|(_, w)| w.id == id)
            .map(|(&k, _)| k);
        if let Some(key) = key {
            if let Some(win) = self.windows.get_mut(&key) {
                win.start_minimize(dock_target_y);
            }
            self.focus_stack.retain(|&k| k != key);
            self.focus_top();
        }
    }

    /// Restore (un-minimize) by ID.
    pub fn restore_by_id(&mut self, id: i32) {
        let key = self.windows.iter()
            .find(|(_, w)| w.id == id)
            .map(|(&k, _)| k);
        if let Some(key) = key {
            if let Some(win) = self.windows.get_mut(&key) {
                win.start_restore();
            }
            // Raise and focus.
            let new_z = self.next_z;
            self.next_z += 1;
            if let Some(win) = self.windows.get_mut(&key) {
                win.z_order = new_z;
                win.focused = true;
            }
            for (&k, win) in &mut self.windows {
                if k != key {
                    win.focused = false;
                }
            }
            self.focus_stack.retain(|&k| k != key);
            self.focus_stack.push(key);
        }
    }

    /// Toggle maximize/restore for a window by ID.
    pub fn toggle_maximize_by_id(&mut self, id: i32) {
        let key = self.windows.iter()
            .find(|(_, w)| w.id == id)
            .map(|(&k, _)| k);
        if let Some(key) = key {
            let (ow, oh) = (self.output_w, self.output_h);
            let (t, b, l, r) = (
                self.effective_top(),
                self.effective_bottom(),
                self.effective_left(),
                self.effective_right(),
            );
            if let Some(win) = self.windows.get_mut(&key) {
                if win.maximized {
                    win.start_unmaximize();
                } else {
                    win.start_maximize_in(ow, oh, t, b, l, r);
                }
            }
        }
    }

    /// Snap a window's position to (x, y) immediately (no animation).
    /// Called during a title-bar drag so the visual follows the pointer 1:1.
    pub fn set_position_by_id(&mut self, id: i32, x: i32, y: i32) {
        let key = self.windows.iter().find(|(_, w)| w.id == id).map(|(&k, _)| k);
        if let Some(key) = key {
            if let Some(win) = self.windows.get_mut(&key) {
                win.x = x;
                win.y = y;
                win.anim.geo_x.set_instant(x as f64);
                win.anim.geo_y.set_instant(y as f64);
            }
        }
    }

    /// Snap a window's full geometry to (x, y, w, h) immediately. Used during
    /// edge/corner resize drags.
    pub fn set_geometry_by_id(&mut self, id: i32, x: i32, y: i32, w: i32, h: i32) {
        let key = self.windows.iter().find(|(_, w)| w.id == id).map(|(&k, _)| k);
        if let Some(key) = key {
            if let Some(win) = self.windows.get_mut(&key) {
                win.x = x;
                win.y = y;
                win.w = w;
                win.h = h;
                win.anim.set_geometry_instant(x, y, w, h);
            }
        }
    }

    /// Look up the WM window id from a wayland surface.
    pub fn id_for_surface(&self, surface: &WlSurface) -> Option<i32> {
        let key = Self::key(surface);
        self.windows.get(&key).map(|w| w.id)
    }

    /// Update committed geometry for a surface (called when client commits with
    /// a new buffer size, or after configure is acked).
    pub fn update_geometry(&mut self, surface: &WlSurface, w: i32, h: i32) {
        let key = Self::key(surface);
        if let Some(win) = self.windows.get_mut(&key) {
            if !win.maximized {
                win.w = w;
                win.h = h;
                // Snap geometry springs to the committed size ONLY when the
                // springs have already settled. If they're mid-flight (e.g.
                // an unmaximize geometry animation in progress), snapping
                // here would cut the animation short — we let the spring run
                // and trust the next commit to land at the same target.
                if win.phase == AnimPhase::Open
                    && win.anim.geo_w.is_done()
                    && win.anim.geo_h.is_done()
                {
                    win.anim.geo_w.set_instant(w as f64);
                    win.anim.geo_h.set_instant(h as f64);
                }
            }
        }
    }

    /// Advance all window animations by `dt` seconds.
    pub fn tick_all(&mut self, dt: f64) {
        for win in self.windows.values_mut() {
            win.tick(dt);
        }
    }

    /// Tick with auto-computed dt from last call.
    pub fn tick_auto(&mut self) -> f64 {
        let now = Instant::now();
        let dt = if let Some(last) = self.last_tick {
            now.duration_since(last).as_secs_f64().min(0.1) // cap at 100 ms
        } else {
            0.016 // first frame ~60fps assumed
        };
        self.last_tick = Some(now);
        self.tick_all(dt);
        dt
    }

    /// Find the topmost surface under (x, y) for pointer routing.
    /// Returns (surface, local_x, local_y) where local is relative to the
    /// surface under the pointer. For CSD clients with subsurfaces (Firefox,
    /// GTK header-bar apps) the topmost subsurface under the pointer is
    /// returned so wl_pointer events route to the right surface; the
    /// toplevel itself only wins when nothing else covers the point.
    pub fn surface_under(&self, x: f64, y: f64) -> Option<(WlSurface, f64, f64)> {
        use smithay::wayland::compositor::{with_surface_tree_downward, SubsurfaceCachedState, TraversalAction};
        use smithay::backend::renderer::utils::with_renderer_surface_state;

        let mut best: Option<(usize, usize)> = None;
        for (&key, win) in &self.windows {
            if win.closing || (win.minimized && win.anim.is_settled()) {
                continue;
            }
            let wx = win.anim.current_x() as f64;
            let wy = win.anim.current_y() as f64;
            let ww = win.geom_w.max(1) as f64;
            let titlebar = if win.csd { 0.0 } else { TITLEBAR_HEIGHT };
            let wh = win.geom_h.max(1) as f64 + titlebar;
            if x >= wx && x < wx + ww && y >= wy && y < wy + wh {
                if best.map_or(true, |(_, z)| win.z_order > z) {
                    best = Some((key, win.z_order));
                }
            }
        }
        let (key, _) = best?;
        let win = self.windows.get(&key)?;
        let wx = win.anim.current_x() as f64;
        let titlebar = if win.csd { 0.0 } else { TITLEBAR_HEIGHT };
        let wy = win.anim.current_y() as f64 + titlebar;
        let chrome_local_x = x - wx;
        let chrome_local_y = y - wy;
        // Pointer is in the SSD titlebar zone — handled by the chrome, not
        // forwarded to the client.
        if chrome_local_y < 0.0 {
            return None;
        }
        // Buffer-space position the user is pointing at.
        let buffer_x = chrome_local_x + win.geom_x as f64;
        let buffer_y = chrome_local_y + win.geom_y as f64;

        // Walk the toplevel's subsurface tree and find the topmost wl_surface
        // whose buffer rect contains the pointer. wayland clients expect
        // pointer events at the surface ACTUALLY under the cursor (toplevel
        // OR subsurface), with surface-local coords.
        //
        // Two-pass like import_shm_buffer: collect (surface, offset) inside
        // the traversal, then resolve each surface's buffer dims OUTSIDE.
        // Calling with_renderer_surface_state INSIDE the tree walk holds
        // surface-state locks on top of the locks the walk itself already
        // owns — that deadlocks the wayland thread.
        let mut surfaces_and_offsets: Vec<(WlSurface, (i32, i32), u32)> = Vec::new();
        let mut depth: u32 = 0;
        with_surface_tree_downward(
            &win.surface,
            (0i32, 0i32),
            |sub, states, parent_offset| {
                let mut my_offset = *parent_offset;
                if sub != &win.surface {
                    let mut sub_state = states.cached_state.get::<SubsurfaceCachedState>();
                    let loc = sub_state.current().location;
                    my_offset.0 += loc.x;
                    my_offset.1 += loc.y;
                }
                TraversalAction::DoChildren(my_offset)
            },
            |sub, _, parent_offset| {
                depth += 1;
                surfaces_and_offsets.push((sub.clone(), *parent_offset, depth));
            },
            |_, _, _| true,
        );

        // Pass 2 — find topmost surface whose buffer rect contains the point.
        let mut deepest: Option<(WlSurface, f64, f64, u32)> = None;
        for (sub, off, d) in &surfaces_and_offsets {
            let (sw, sh) = with_renderer_surface_state(sub, |st| {
                st.buffer_size().map(|sz| (sz.w, sz.h)).unwrap_or((0, 0))
            }).unwrap_or((0, 0));
            if sw == 0 || sh == 0 { continue; }
            let sx = off.0 as f64;
            let sy = off.1 as f64;
            let lx = buffer_x - sx;
            let ly = buffer_y - sy;
            if lx >= 0.0 && lx < sw as f64 && ly >= 0.0 && ly < sh as f64 {
                deepest = Some((sub.clone(), lx, ly, *d));
            }
        }

        let (focus_surface, local_x, local_y, target_depth) = match deepest {
            Some(h) => h,
            // No subsurface contained the point — fall back to the toplevel.
            None => (win.surface.clone(), buffer_x, buffer_y, 0),
        };

        debug!(
            "surface_under: ptr=({:.1},{:.1}) win={} chrome=({},{}) chrome_local=({:.1},{:.1}) geom=({},{},{},{}) csd={} buffer=({:.1},{:.1}) target_depth={} -> local=({:.1},{:.1})",
            x, y, win.id,
            win.anim.current_x(), win.anim.current_y(),
            chrome_local_x, chrome_local_y,
            win.geom_x, win.geom_y, win.geom_w, win.geom_h,
            win.csd,
            buffer_x, buffer_y,
            target_depth,
            local_x, local_y,
        );
        Some((focus_surface, local_x, local_y))
    }

    /// Return windows sorted by z_order (ascending = back to front) for rendering.
    pub fn windows_sorted(&self) -> Vec<&WindowState> {
        let mut wins: Vec<&WindowState> = self.windows.values().collect();
        wins.sort_by_key(|w| w.z_order);
        wins
    }

    // ── Alt-Tab ──────────────────────────────────────────────────────────────

    /// Start alt-tab cycling. Returns the ID of the next window to preview.
    pub fn alt_tab_start(&mut self) -> Option<i32> {
        let visible_count = self.focus_stack.iter()
            .filter(|&&k| self.windows.get(&k).map_or(false, |w| !w.minimized && !w.closing))
            .count();
        if visible_count < 2 {
            return None;
        }
        // Start from second-to-last (skip current focus).
        let idx = self.focus_stack.len().saturating_sub(2);
        self.alt_tab_idx = Some(idx);
        self.update_alt_tab_selection();
        self.focus_stack.get(idx).and_then(|k| self.windows.get(k)).map(|w| w.id)
    }

    /// Step to the next window in alt-tab order.
    pub fn alt_tab_next(&mut self) -> Option<i32> {
        let len = self.focus_stack.len();
        if len == 0 {
            return None;
        }
        let idx = self.alt_tab_idx.map_or(0, |i| {
            if i == 0 { len - 1 } else { i - 1 }
        });
        self.alt_tab_idx = Some(idx);
        self.update_alt_tab_selection();
        self.focus_stack.get(idx).and_then(|k| self.windows.get(k)).map(|w| w.id)
    }

    /// Commit alt-tab: focus the currently selected window.
    pub fn alt_tab_commit(&mut self) {
        if let Some(idx) = self.alt_tab_idx.take() {
            // Clear all alt_tab_selected flags.
            for win in self.windows.values_mut() {
                win.alt_tab_selected = false;
            }
            if let Some(&key) = self.focus_stack.get(idx) {
                // Re-arrange focus stack to put selected window at top.
                self.focus_stack.retain(|&k| k != key);
                self.focus_stack.push(key);
                self.focus_top();
            }
        }
    }

    /// Cancel alt-tab without changing focus.
    pub fn alt_tab_cancel(&mut self) {
        self.alt_tab_idx = None;
        for win in self.windows.values_mut() {
            win.alt_tab_selected = false;
        }
    }

    fn update_alt_tab_selection(&mut self) {
        let selected_key = self.alt_tab_idx
            .and_then(|i| self.focus_stack.get(i))
            .copied();
        for (&k, win) in &mut self.windows {
            win.alt_tab_selected = Some(k) == selected_key;
        }
    }
}
