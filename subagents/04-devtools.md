# SUBAGENT: Iced DevTools / UI Inspector
# Crate: `crates/shell-devtools`

> **Read `ARCHITECTURE.md` in the repo root before starting.** It explains why we use Smithay (not wlroots), how the compositor rendering pipeline works, the full Wayland protocol reference, smithay API patterns (delegate pattern, GlesRenderer, calloop), and the winit dev backend.
# Branch: `feat/devtools`

You are building debug/inspection tools for iced applications in a Wayland DE. Since iced has no built-in devtools (unlike browser DevTools), this crate provides a debug overlay and widget tree inspector that can be toggled with F12 in any of our shell apps (panel, dock, launcher).

## Purpose
1. Visual debugging during development — see widget bounds, padding, alignment
2. Headless inspection for Claude Code subagents — dump widget tree to JSON so an AI agent can understand the current UI state without seeing the screen

## Architecture

A wrapper widget `DevTools<Message>` wraps any iced application's root element:

```rust
pub struct DevTools<'a, Message> {
    content: Element<'a, Message>,
    enabled: bool,
    state: &'a mut DevToolsState,
}

pub struct DevToolsState {
    selected_widget: Option<usize>,
    tree_expanded: HashSet<usize>,
    show_bounds: bool,
    show_padding: bool,
    frame_times: VecDeque<Duration>,  // last 120 frames
}

impl<'a, Message: Clone + 'a> DevTools<'a, Message> {
    pub fn new(content: impl Into<Element<'a, Message>>, state: &'a mut DevToolsState) -> Self;
    pub fn toggle(&mut self);
}
```

## Features

### Widget tree inspector (side panel when enabled)
- Expandable tree view of the widget hierarchy
- For each widget node: type name, bounds rect (x,y,w,h), computed size
- Click node in tree → highlight widget in the actual UI with a coloured overlay
- Click element in UI → select it in the tree (reverse lookup via bounds hit-testing)

### Layout visualiser overlay
- Toggle showing all widget bounds as semi-transparent coloured rectangles
- Different colours for different widget types (Container=blue, Row=green, Column=red, Text=yellow, etc.)
- Show padding regions as hatched/shaded areas

### Performance overlay
- Frame time graph (last 120 frames) in top-right corner
- Current FPS counter
- Widget count in current tree

### Headless JSON dump
For Claude Code to inspect the UI without a screen:

```rust
pub fn dump_widget_tree(root: &Element<'_, impl Any>) -> serde_json::Value;
```

Writes to `$XDG_RUNTIME_DIR/myDE-devtools-{app_name}.json`:
```json
{
  "type": "Column",
  "bounds": {"x": 0, "y": 0, "w": 1920, "h": 30},
  "children": [
    {"type": "Text", "content": "14:32", "bounds": {"x": 900, "y": 5, "w": 60, "h": 20}},
    ...
  ]
}
```

Also expose via IPC: compositor can forward a `DumpWidgetTree` request to any shell process.

## Conditional compilation
Everything behind `#[cfg(feature = "devtools")]`. Not compiled in release:
```toml
[features]
default = ["devtools"]
devtools = []
```

## Cargo.toml
```toml
[package]
name = "shell-devtools"
version = "0.1.0"
edition = "2021"

[dependencies]
iced = { version = "0.13", features = ["advanced"] }
serde = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }
```

## Implementation Notes
- The tree inspection relies on iced's widget::Tree and layout::Node structures
- For bounds overlay: implement as a custom iced Canvas widget that draws rectangles over the content
- The side panel takes ~300px from the right edge when enabled
- F12 toggles devtools on/off via a subscription or keyboard event handler
- Frame timing: measure time between `view()` calls using `std::time::Instant`

## Work iteratively
1. Create the DevTools wrapper widget skeleton
2. Implement bounds overlay (coloured rectangles for all widgets)
3. Implement tree view side panel
4. Implement element picking (click UI → select in tree)
5. Add performance overlay (FPS counter, frame graph)
6. Add JSON dump for headless inspection
7. Tests + docs
