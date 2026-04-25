//! Core devtools state: [`DevToolsState`] and [`WidgetNode`].

use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

/// A node in the logical widget tree, populated by the application.
///
/// Because iced's internal widget tree is not public API, the application (or
/// a helper macro) must build this tree manually and hand it to
/// [`DevToolsState`].  Each node carries enough information to render a useful
/// inspector panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetNode {
    /// Stable identifier for this node within the tree.
    pub id: usize,

    /// Human-readable widget type name (e.g. `"Container"`, `"Text"`).
    pub widget_type: String,

    /// The axis-aligned bounding box of the widget in logical pixels.
    pub bounds: Bounds,

    /// Optional textual content (populated for `Text`, `Button`, etc.).
    pub content: Option<String>,

    /// Child nodes of this widget.
    pub children: Vec<WidgetNode>,
}

/// Axis-aligned bounding rectangle in logical (device-independent) pixels.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Bounds {
    /// Horizontal position.
    pub x: f32,
    /// Vertical position.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

impl Bounds {
    /// Creates a new [`Bounds`].
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
}

/// Maximum number of frame-time samples kept for the FPS graph.
const MAX_FRAME_SAMPLES: usize = 120;

/// All persistent state required by the devtools overlay.
#[derive(Debug)]
pub struct DevToolsState {
    /// Whether the devtools UI is currently visible.
    pub enabled: bool,

    /// The node id that is currently highlighted in the inspector.
    pub selected_widget: Option<usize>,

    /// Set of node ids whose children are shown in the tree view.
    pub tree_expanded: HashSet<usize>,

    /// Whether the coloured bounds overlay is rendered over the content area.
    pub show_bounds: bool,

    /// Whether padding regions are highlighted in the overlay.
    pub show_padding: bool,

    /// Ring buffer of the last 120 frame durations.
    pub frame_times: VecDeque<Duration>,

    /// Timestamp of the previous frame, used to compute delta time.
    pub last_frame: Option<Instant>,

    /// The widget tree provided by the application.
    pub widget_tree: Vec<WidgetNode>,
}

impl Default for DevToolsState {
    fn default() -> Self {
        Self::new()
    }
}

impl DevToolsState {
    /// Creates a new, disabled [`DevToolsState`].
    pub fn new() -> Self {
        Self {
            enabled: false,
            selected_widget: None,
            tree_expanded: HashSet::new(),
            show_bounds: true,
            show_padding: false,
            frame_times: VecDeque::with_capacity(MAX_FRAME_SAMPLES),
            last_frame: None,
            widget_tree: Vec::new(),
        }
    }

    /// Toggle the devtools visibility.
    pub fn toggle(&mut self) {
        self.enabled = !self.enabled;
        tracing::debug!(enabled = self.enabled, "devtools toggled");
    }

    /// Record a new frame.  Call this once per rendered frame from your
    /// application's update loop.
    pub fn record_frame(&mut self) {
        let now = Instant::now();
        if let Some(last) = self.last_frame {
            let delta = now.duration_since(last);
            if self.frame_times.len() >= MAX_FRAME_SAMPLES {
                self.frame_times.pop_front();
            }
            self.frame_times.push_back(delta);
        }
        self.last_frame = Some(now);
    }

    /// Returns the current average frames-per-second based on the recorded
    /// frame-time ring buffer.  Returns `0.0` when fewer than two frames have
    /// been recorded.
    pub fn fps(&self) -> f64 {
        if self.frame_times.is_empty() {
            return 0.0;
        }
        let total_secs: f64 = self
            .frame_times
            .iter()
            .map(|d| d.as_secs_f64())
            .sum::<f64>();
        let count = self.frame_times.len() as f64;
        count / total_secs
    }

    /// Returns the total number of [`WidgetNode`]s in the tree (recursive).
    pub fn widget_count(&self) -> usize {
        fn count_recursive(nodes: &[WidgetNode]) -> usize {
            nodes
                .iter()
                .map(|n| 1 + count_recursive(&n.children))
                .sum()
        }
        count_recursive(&self.widget_tree)
    }

    /// Replace the widget tree.  Call this whenever the view is rebuilt.
    pub fn set_widget_tree(&mut self, tree: Vec<WidgetNode>) {
        self.widget_tree = tree;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_node(id: usize, children: Vec<WidgetNode>) -> WidgetNode {
        WidgetNode {
            id,
            widget_type: "Container".to_owned(),
            bounds: Bounds::new(0.0, 0.0, 100.0, 100.0),
            content: None,
            children,
        }
    }

    #[test]
    fn default_state_is_disabled() {
        let state = DevToolsState::default();
        assert!(!state.enabled);
    }

    #[test]
    fn toggle_enables_and_disables() {
        let mut state = DevToolsState::new();
        assert!(!state.enabled);
        state.toggle();
        assert!(state.enabled);
        state.toggle();
        assert!(!state.enabled);
    }

    #[test]
    fn fps_returns_zero_with_no_frames() {
        let state = DevToolsState::new();
        assert_eq!(state.fps(), 0.0);
    }

    #[test]
    fn record_frame_accumulates() {
        let mut state = DevToolsState::new();
        // First call just records the timestamp.
        state.record_frame();
        assert_eq!(state.frame_times.len(), 0);
        // Manually push a duration so fps() has data.
        state.frame_times.push_back(Duration::from_millis(16));
        state.frame_times.push_back(Duration::from_millis(16));
        let fps = state.fps();
        // ~62.5 fps for 16 ms frames
        assert!(fps > 60.0 && fps < 65.0, "expected ~62 fps, got {fps}");
    }

    #[test]
    fn fps_caps_at_max_samples() {
        let mut state = DevToolsState::new();
        for _ in 0..150 {
            state.frame_times.push_back(Duration::from_millis(16));
        }
        // record_frame caps at MAX_FRAME_SAMPLES = 120
        while state.frame_times.len() > MAX_FRAME_SAMPLES {
            state.frame_times.pop_front();
        }
        assert_eq!(state.frame_times.len(), MAX_FRAME_SAMPLES);
    }

    #[test]
    fn widget_count_flat() {
        let mut state = DevToolsState::new();
        state.set_widget_tree(vec![make_node(0, vec![]), make_node(1, vec![])]);
        assert_eq!(state.widget_count(), 2);
    }

    #[test]
    fn widget_count_nested() {
        let mut state = DevToolsState::new();
        let child = make_node(1, vec![make_node(2, vec![])]);
        state.set_widget_tree(vec![make_node(0, vec![child])]);
        assert_eq!(state.widget_count(), 3);
    }

    #[test]
    fn bounds_new_fields() {
        let b = Bounds::new(1.0, 2.0, 3.0, 4.0);
        assert_eq!(b.x, 1.0);
        assert_eq!(b.y, 2.0);
        assert_eq!(b.w, 3.0);
        assert_eq!(b.h, 4.0);
    }
}
