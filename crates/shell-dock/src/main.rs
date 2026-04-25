//! Wayland dock for myDE — iced layer-shell application.

mod config;
mod desktop;
mod ipc_sub;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use animation::spring::SpringAnimation;
use iced::widget::{column, container, mouse_area, row, text};
use iced::{Color, Element, Length, Task};
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::{LayerShellSettings, Settings};
use ipc::{ShellRequest, WindowInfo, WindowState};

use config::Config;
use desktop::AppInfo;

// ---------------------------------------------------------------------------
// Message
// ---------------------------------------------------------------------------

/// Actions available from the right-click context menu.
#[derive(Debug, Clone)]
pub enum ContextAction {
    /// Open a new window.
    NewWindow,
    /// Close all windows for the app.
    CloseAll,
    /// Pin the app to the dock.
    Pin,
    /// Unpin the app from the dock.
    Unpin,
    /// Send quit to all windows.
    Quit,
}

/// All messages the dock application can receive.
///
/// The `#[iced_layershell::to_layer_message]` attribute injects layer-shell
/// control variants (AnchorChange, SizeChange, etc.) and implements the
/// required `TryInto<LayershellCustomActions>` conversion automatically.
#[iced_layershell::to_layer_message(derives = "Debug Clone")]
pub enum Message {
    // ---- IPC window events ----
    WindowOpened(WindowInfo),
    WindowClosed(u64),
    WindowStateChanged { window_id: u64, state: WindowState },
    WindowAppIdChanged { window_id: u64, app_id: String },

    // ---- IPC connection ----
    IpcConnected,
    IpcDisconnected,

    // ---- User interactions ----
    /// Left-click on a dock icon.
    IconClicked(String),
    /// Right-click on a dock icon.
    ContextMenuRequested(String),
    /// Pointer entered a dock icon.
    HoverEnter(String),
    /// Pointer left a dock icon.
    HoverExit(String),

    // ---- Context menu ----
    ContextAction(ContextAction),
    ContextMenuDismiss,

    // ---- Hover preview ----
    PreviewWindowFocused(u64),
    PreviewDismiss,

    // ---- Trash ----
    CheckTrash,
    TrashClicked,

    // ---- Animation tick ----
    Tick(Instant),
}

// ---------------------------------------------------------------------------
// Supporting state types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ContextMenu {
    app_id: String,
}

#[derive(Debug, Clone)]
struct HoverPreview {
    app_id: String,
    windows: Vec<WindowInfo>,
}

/// Cached image/SVG handle for an application icon.
#[derive(Clone)]
enum IconHandle {
    Image(iced::widget::image::Handle),
    Svg(iced::widget::svg::Handle),
}

impl std::fmt::Debug for IconHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Image(_) => write!(f, "IconHandle::Image"),
            Self::Svg(_) => write!(f, "IconHandle::Svg"),
        }
    }
}

// ---------------------------------------------------------------------------
// DockApp state
// ---------------------------------------------------------------------------

/// Maximum icon scale during hover magnification.
const MAX_SCALE: f32 = 1.15;
/// Spring stiffness for hover and bounce animations.
const SPRING_K: f64 = 800.0;
/// Damping ratio: underdamped so bounce oscillates.
const SPRING_D: f64 = 0.5;
/// Settlement epsilon for scale springs.
const SCALE_EPS: f64 = 0.001;
/// Settlement epsilon for bounce springs (coarser — visually imperceptible).
const BOUNCE_EPS: f64 = 0.5;
/// Milliseconds of hover before the preview popup appears.
const PREVIEW_DELAY_MS: u128 = 500;
/// Exclusive zone height (dock surface height including bottom margin).
const SURFACE_HEIGHT: u32 = 80;

struct DockApp {
    config: Config,
    /// Ordered pinned app_ids.
    pinned: Vec<String>,
    /// Running windows, keyed by app_id.
    running: HashMap<String, Vec<WindowInfo>>,
    /// Resolved app metadata, keyed by app_id.
    app_info: HashMap<String, AppInfo>,
    /// Cached icon handles, keyed by app_id (None = fallback text icon).
    icons: HashMap<String, Option<IconHandle>>,
    /// Per-icon hover scale springs (value range 1.0–MAX_SCALE).
    scales: HashMap<String, SpringAnimation>,
    /// Per-icon launch bounce y-offset springs.
    bounces: HashMap<String, SpringAnimation>,
    /// App_ids of apps currently launching (waiting for a window to appear).
    launching: HashSet<String>,
    /// Timestamp when hover began, per app_id (used for preview delay).
    hover_start: HashMap<String, Instant>,
    /// Active context menu.
    context_menu: Option<ContextMenu>,
    /// Active hover preview popup.
    preview: Option<HoverPreview>,
    /// Whether `~/.local/share/Trash/files` is non-empty.
    trash_full: bool,
    /// Whether we are connected to the compositor IPC socket.
    ipc_connected: bool,
    /// Previous tick timestamp for computing animation dt.
    last_tick: Instant,
}

impl DockApp {
    fn new() -> Self {
        let config = Config::load();
        let pinned = config.pinned.iter().map(|p| p.app_id.clone()).collect();

        let mut app = Self {
            config,
            pinned,
            running: HashMap::new(),
            app_info: HashMap::new(),
            icons: HashMap::new(),
            scales: HashMap::new(),
            bounces: HashMap::new(),
            launching: HashSet::new(),
            hover_start: HashMap::new(),
            context_menu: None,
            preview: None,
            trash_full: is_trash_full(),
            ipc_connected: false,
            last_tick: Instant::now(),
        };

        // Pre-resolve app info for pinned apps.
        let ids: Vec<String> = app.pinned.clone();
        for id in ids {
            app.ensure_app_info(&id);
        }

        app
    }

    /// Resolves and caches AppInfo + icon handle for `app_id` if not already done.
    fn ensure_app_info(&mut self, app_id: &str) {
        if self.app_info.contains_key(app_id) {
            return;
        }

        let info = desktop::resolve(app_id).unwrap_or_else(|| AppInfo {
            name: app_id.to_string(),
            exec: app_id.to_string(),
            icon: None,
        });

        let handle = info.icon.as_ref().map(|path| {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext.eq_ignore_ascii_case("svg") {
                IconHandle::Svg(iced::widget::svg::Handle::from_path(path))
            } else {
                IconHandle::Image(iced::widget::image::Handle::from_path(path))
            }
        });

        self.icons.insert(app_id.to_string(), handle);
        self.app_info.insert(app_id.to_string(), info);
    }

    fn ensure_scale_spring(&mut self, app_id: &str) {
        if self.scales.contains_key(app_id) {
            return;
        }
        let mut s = SpringAnimation::new(SPRING_K, SPRING_D, SCALE_EPS);
        s.set_position(1.0);
        self.scales.insert(app_id.to_string(), s);
    }

    fn ensure_bounce_spring(&mut self, app_id: &str) {
        if self.bounces.contains_key(app_id) {
            return;
        }
        let s = SpringAnimation::new(SPRING_K, SPRING_D, BOUNCE_EPS);
        self.bounces.insert(app_id.to_string(), s);
    }

    /// Spawns the app and starts the launch bounce animation.
    fn launch_app(&mut self, app_id: &str) {
        let exec = self
            .app_info
            .get(app_id)
            .map(|i| i.exec.clone())
            .unwrap_or_else(|| app_id.to_string());

        let exec = exec.trim().to_string();
        if exec.is_empty() {
            return;
        }

        tracing::info!("launching {app_id}: {exec}");
        self.launching.insert(app_id.to_string());
        self.ensure_bounce_spring(app_id);
        if let Some(spring) = self.bounces.get_mut(app_id) {
            // Upward kick: spring settles back to 0, but underdamped so it bounces.
            spring.set_target_with_velocity(0.0, -200.0);
        }

        let _ = std::process::Command::new("setsid")
            .args(["sh", "-c", &exec])
            .spawn();
    }

    fn focus_windows(&self, app_id: &str) {
        if let Some(windows) = self.running.get(app_id) {
            if let Some(w) = windows.first() {
                ipc_sub::send_request(ShellRequest::ActivateWindow { window_id: w.id });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Application impl
// ---------------------------------------------------------------------------

impl iced_layershell::Application for DockApp {
    type Executor = iced::executor::Default;
    type Message = Message;
    type Theme = iced::Theme;
    type Flags = ();

    fn new(_flags: ()) -> (Self, Task<Message>) {
        (DockApp::new(), Task::none())
    }

    fn namespace(&self) -> String {
        "shell-dock".to_string()
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            // ---- IPC window events ----
            Message::WindowOpened(window) => {
                let app_id = window.app_id.clone();
                if !app_id.is_empty() {
                    self.ensure_app_info(&app_id);
                    self.running.entry(app_id.clone()).or_default().push(window);
                    if self.launching.remove(&app_id) {
                        if let Some(s) = self.bounces.get_mut(&app_id) {
                            s.set_target(0.0);
                        }
                    }
                }
            }
            Message::WindowClosed(window_id) => {
                for ws in self.running.values_mut() {
                    ws.retain(|w| w.id != window_id);
                }
                self.running.retain(|_, ws| !ws.is_empty());
            }
            Message::WindowStateChanged { window_id, state } => {
                for ws in self.running.values_mut() {
                    if let Some(w) = ws.iter_mut().find(|w| w.id == window_id) {
                        w.state = state;
                        break;
                    }
                }
            }
            Message::WindowAppIdChanged { window_id, app_id } => {
                let mut old_app_id = None::<String>;
                let mut window = None::<WindowInfo>;
                for (oid, ws) in self.running.iter_mut() {
                    if let Some(pos) = ws.iter().position(|w| w.id == window_id) {
                        let mut w = ws.remove(pos);
                        old_app_id = Some(oid.clone());
                        w.app_id = app_id.clone();
                        window = Some(w);
                        break;
                    }
                }
                if let (Some(old), Some(w)) = (old_app_id, window) {
                    self.running.retain(|k, ws| k != &old || !ws.is_empty());
                    if !app_id.is_empty() {
                        self.ensure_app_info(&app_id);
                        self.running.entry(app_id).or_default().push(w);
                    }
                }
            }

            // ---- IPC connection ----
            Message::IpcConnected => {
                self.ipc_connected = true;
                tracing::info!("IPC: connected");
            }
            Message::IpcDisconnected => {
                self.ipc_connected = false;
                tracing::warn!("IPC: disconnected, will reconnect");
            }

            // ---- User interactions ----
            Message::IconClicked(app_id) => {
                let running = self.running.get(&app_id).map(|ws| !ws.is_empty()).unwrap_or(false);
                if running {
                    self.focus_windows(&app_id);
                } else {
                    self.launch_app(&app_id);
                }
            }
            Message::ContextMenuRequested(app_id) => {
                self.context_menu = Some(ContextMenu { app_id });
                self.preview = None;
            }
            Message::HoverEnter(app_id) => {
                self.hover_start.insert(app_id.clone(), Instant::now());
                self.ensure_scale_spring(&app_id);
                if let Some(s) = self.scales.get_mut(&app_id) {
                    s.set_target(MAX_SCALE as f64);
                }
            }
            Message::HoverExit(app_id) => {
                self.hover_start.remove(&app_id);
                if let Some(s) = self.scales.get_mut(&app_id) {
                    s.set_target(1.0);
                }
                if let Some(ref p) = self.preview {
                    if p.app_id == app_id {
                        self.preview = None;
                    }
                }
            }

            // ---- Context menu ----
            Message::ContextAction(action) => {
                let app_id = self.context_menu.take().map(|m| m.app_id);
                if let Some(app_id) = app_id {
                    match action {
                        ContextAction::NewWindow => self.launch_app(&app_id),
                        ContextAction::CloseAll | ContextAction::Quit => {
                            if let Some(windows) = self.running.get(&app_id).cloned() {
                                for w in &windows {
                                    ipc_sub::send_request(ShellRequest::CloseWindow {
                                        window_id: w.id,
                                    });
                                }
                            }
                        }
                        ContextAction::Pin => {
                            if !self.pinned.contains(&app_id) {
                                self.pinned.push(app_id);
                            }
                        }
                        ContextAction::Unpin => {
                            self.pinned.retain(|id| id != &app_id);
                        }
                    }
                }
            }
            Message::ContextMenuDismiss => {
                self.context_menu = None;
            }

            // ---- Hover preview ----
            Message::PreviewWindowFocused(window_id) => {
                self.preview = None;
                ipc_sub::send_request(ShellRequest::ActivateWindow { window_id });
            }
            Message::PreviewDismiss => {
                self.preview = None;
            }

            // ---- Trash ----
            Message::CheckTrash => {
                self.trash_full = is_trash_full();
            }
            Message::TrashClicked => {
                let _ = std::process::Command::new("xdg-open")
                    .arg("trash:///")
                    .spawn();
            }

            // ---- Animation tick ----
            Message::Tick(now) => {
                let dt = now.duration_since(self.last_tick).as_secs_f64().min(0.1);
                self.last_tick = now;

                for s in self.scales.values_mut() {
                    s.tick(dt);
                }
                for s in self.bounces.values_mut() {
                    s.tick(dt);
                }

                // Show preview after PREVIEW_DELAY_MS of continuous hover.
                let ready: Vec<String> = self
                    .hover_start
                    .iter()
                    .filter(|(_, t)| t.elapsed().as_millis() >= PREVIEW_DELAY_MS)
                    .map(|(id, _)| id.clone())
                    .collect();

                for app_id in ready {
                    self.hover_start.remove(&app_id);
                    if self.preview.is_none() {
                        if let Some(windows) = self.running.get(&app_id) {
                            if !windows.is_empty() {
                                self.preview = Some(HoverPreview {
                                    app_id,
                                    windows: windows.clone(),
                                });
                            }
                        }
                    }
                }
            }

            // Layer-shell control variants are intercepted by the runtime
            // before reaching update(); the catch-all silences exhaustiveness.
            _ => {}
        }

        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let dock = self.render_dock();

        if let Some(ref menu) = self.context_menu {
            return self.render_with_context_menu(dock, menu);
        }

        dock
    }

    fn subscription(&self) -> iced::Subscription<Message> {
        iced::Subscription::batch([
            ipc_sub::subscription(),
            iced::time::every(Duration::from_millis(16)).map(Message::Tick),
            iced::time::every(Duration::from_secs(5)).map(|_| Message::CheckTrash),
        ])
    }

    fn theme(&self) -> iced::Theme {
        iced::Theme::Dark
    }
}

// ---------------------------------------------------------------------------
// View helpers
// ---------------------------------------------------------------------------

impl DockApp {
    fn render_dock(&self) -> Element<'_, Message> {
        let mut items: Vec<Element<Message>> = Vec::new();

        for app_id in &self.pinned {
            items.push(self.render_icon(app_id));
        }

        for app_id in self.running.keys() {
            if !self.pinned.contains(app_id) {
                items.push(self.render_icon(app_id));
            }
        }

        if !items.is_empty() {
            items.push(render_separator());
        }
        items.push(self.render_trash());

        let dock_row = row(items)
            .spacing(4)
            .align_y(iced::alignment::Vertical::Bottom);

        let inner = container(dock_row).padding(iced::Padding {
            top: 8.0,
            right: 12.0,
            bottom: 8.0,
            left: 12.0,
        });

        let dock_bg = container(inner)
            .style(dock_background_style)
            .padding(iced::Padding {
                top: 0.0,
                right: 0.0,
                bottom: 8.0,
                left: 0.0,
            });

        if let Some(ref preview) = self.preview {
            self.render_with_preview(dock_bg, preview)
        } else {
            dock_bg.into()
        }
    }

    fn render_icon<'a>(&'a self, app_id: &'a str) -> Element<'a, Message> {
        let icon_size = self.config.icon_size as f32;
        let max_size = icon_size * MAX_SCALE;

        let windows = self.running.get(app_id);
        let running_count = windows.map(|ws| ws.len()).unwrap_or(0);
        let minimized_count = windows
            .map(|ws| ws.iter().filter(|w| w.state.is_minimized).count())
            .unwrap_or(0);
        let is_launching = self.launching.contains(app_id);

        let scale = self
            .scales
            .get(app_id)
            .map(|s| s.position_f32())
            .unwrap_or(1.0)
            .clamp(1.0, MAX_SCALE);

        let bounce_y = self
            .bounces
            .get(app_id)
            .map(|s| s.position_f32())
            .unwrap_or(0.0);

        let name = self
            .app_info
            .get(app_id)
            .map(|i| i.name.as_str())
            .unwrap_or(app_id);

        let icon_handle = self.icons.get(app_id).and_then(|h| h.as_ref());
        let scaled = icon_size * scale;

        // Icon image or text fallback.
        let icon_widget: Element<Message> = match icon_handle {
            Some(IconHandle::Image(h)) => iced::widget::image(h.clone())
                .width(scaled)
                .height(scaled)
                .into(),
            Some(IconHandle::Svg(h)) => iced::widget::svg(h.clone())
                .width(scaled)
                .height(scaled)
                .into(),
            None => {
                let first = name.chars().next().unwrap_or('?');
                container(
                    text(first.to_uppercase().to_string())
                        .size(scaled * 0.45)
                        .color(Color::WHITE),
                )
                .width(scaled)
                .height(scaled)
                .style(move |_: &iced::Theme| iced::widget::container::Style {
                    background: Some(iced::Background::Color(app_color(app_id))),
                    border: iced::Border {
                        radius: (scaled * 0.22).into(),
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .align_x(iced::alignment::Horizontal::Center)
                .align_y(iced::alignment::Vertical::Center)
                .into()
            }
        };

        // Fixed bounding box so dock width stays stable during scale animation.
        let icon_box = container(icon_widget)
            .width(max_size)
            .height(max_size)
            .align_x(iced::alignment::Horizontal::Center)
            .align_y(iced::alignment::Vertical::Center);

        // Apply bounce offset by adjusting padding.
        let offset_top = (-bounce_y).max(0.0);
        let offset_bottom = bounce_y.max(0.0);
        let bounced = container(icon_box).padding(iced::Padding {
            top: offset_top,
            right: 0.0,
            bottom: offset_bottom,
            left: 0.0,
        });

        let dots = render_dots(running_count, minimized_count, is_launching);

        let col = column![bounced, dots]
            .align_x(iced::alignment::Horizontal::Center)
            .spacing(3);

        mouse_area(col)
            .on_press(Message::IconClicked(app_id.to_string()))
            .on_right_press(Message::ContextMenuRequested(app_id.to_string()))
            .on_enter(Message::HoverEnter(app_id.to_string()))
            .on_exit(Message::HoverExit(app_id.to_string()))
            .into()
    }

    fn render_trash(&self) -> Element<'_, Message> {
        let sz = self.config.icon_size as f32;
        let label = if self.trash_full { "▣" } else { "□" };

        let icon = container(
            text(label)
                .size(sz * 0.6)
                .color(Color::from_rgba(0.8, 0.8, 0.8, 0.9)),
        )
        .width(sz)
        .height(sz)
        .align_x(iced::alignment::Horizontal::Center)
        .align_y(iced::alignment::Vertical::Center);

        let col = column![
            icon,
            container(iced::widget::Space::with_height(8.0))
        ]
        .align_x(iced::alignment::Horizontal::Center);

        mouse_area(col).on_press(Message::TrashClicked).into()
    }

    fn render_with_context_menu<'a>(
        &'a self,
        dock: Element<'a, Message>,
        menu: &'a ContextMenu,
    ) -> Element<'a, Message> {
        let app_id = &menu.app_id;
        let is_pinned = self.pinned.contains(app_id);
        let is_running = self.running.get(app_id).map(|ws| !ws.is_empty()).unwrap_or(false);

        let mut items: Vec<Element<Message>> = Vec::new();

        if is_running {
            items.push(menu_item("New Window", Message::ContextAction(ContextAction::NewWindow)));
            items.push(menu_item("Close All", Message::ContextAction(ContextAction::CloseAll)));
        } else {
            items.push(menu_item("Open", Message::IconClicked(app_id.clone())));
        }
        if is_pinned {
            items.push(menu_item("Unpin from Dock", Message::ContextAction(ContextAction::Unpin)));
        } else {
            items.push(menu_item("Pin to Dock", Message::ContextAction(ContextAction::Pin)));
        }
        if is_running {
            items.push(menu_item("Quit", Message::ContextAction(ContextAction::Quit)));
        }

        let menu_widget = container(column(items).spacing(2))
            .style(popup_background_style)
            .padding(iced::Padding {
                top: 6.0,
                right: 4.0,
                bottom: 6.0,
                left: 4.0,
            });

        column![
            mouse_area(
                container(iced::widget::Space::with_height(Length::Fill)).width(Length::Fill)
            )
            .on_press(Message::ContextMenuDismiss),
            container(menu_widget)
                .align_x(iced::alignment::Horizontal::Center)
                .width(Length::Fill),
            dock,
        ]
        .into()
    }

    fn render_with_preview<'a>(
        &'a self,
        dock: impl Into<Element<'a, Message>>,
        preview: &'a HoverPreview,
    ) -> Element<'a, Message> {
        let window_items: Vec<Element<Message>> = preview
            .windows
            .iter()
            .map(|w| {
                let title = if w.title.is_empty() {
                    preview.app_id.as_str()
                } else {
                    w.title.as_str()
                };
                let wid = w.id;
                mouse_area(
                    container(text(title).size(12.0).color(Color::WHITE))
                        .padding(iced::Padding {
                            top: 6.0,
                            right: 10.0,
                            bottom: 6.0,
                            left: 10.0,
                        })
                        .style(preview_item_style),
                )
                .on_press(Message::PreviewWindowFocused(wid))
                .into()
            })
            .collect();

        let popup = container(column(window_items).spacing(4))
            .style(popup_background_style)
            .padding(8);

        column![
            container(popup)
                .align_x(iced::alignment::Horizontal::Center)
                .width(Length::Fill),
            mouse_area(dock.into()).on_press(Message::PreviewDismiss),
        ]
        .into()
    }
}

// ---------------------------------------------------------------------------
// Widget helpers
// ---------------------------------------------------------------------------

fn render_separator() -> Element<'static, Message> {
    container(
        container(iced::widget::Space::with_height(24.0))
            .width(1.0)
            .style(|_: &iced::Theme| iced::widget::container::Style {
                background: Some(iced::Background::Color(Color::from_rgba(
                    1.0, 1.0, 1.0, 0.15,
                ))),
                ..Default::default()
            }),
    )
    .padding(iced::Padding {
        top: 0.0,
        right: 8.0,
        bottom: 0.0,
        left: 8.0,
    })
    .align_y(iced::alignment::Vertical::Center)
    .into()
}

fn render_dots(
    normal: usize,
    minimized: usize,
    launching: bool,
) -> Element<'static, Message> {
    let total = normal + minimized;
    if total == 0 && !launching {
        return container(iced::widget::Space::with_height(8.0)).into();
    }

    let count = total.min(3).max(if launching { 1 } else { 0 });
    let dots: Vec<Element<Message>> = (0..count)
        .map(|i| {
            let is_minimized = i >= normal;
            let color = if launching {
                Color::from_rgba(0.0, 0.478, 1.0, 0.9)
            } else if is_minimized {
                Color::from_rgba(1.0, 1.0, 1.0, 0.35)
            } else {
                Color::from_rgba(1.0, 1.0, 1.0, 0.85)
            };
            container(iced::widget::Space::new(5.0, 5.0))
                .style(move |_: &iced::Theme| iced::widget::container::Style {
                    background: Some(iced::Background::Color(color)),
                    border: iced::Border {
                        radius: 3.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .into()
        })
        .collect();

    row(dots).spacing(3).into()
}

fn menu_item(label: &str, msg: Message) -> Element<'_, Message> {
    mouse_area(
        container(text(label).size(13.0).color(Color::WHITE)).padding(iced::Padding {
            top: 6.0,
            right: 14.0,
            bottom: 6.0,
            left: 14.0,
        }),
    )
    .on_press(msg)
    .into()
}

// ---------------------------------------------------------------------------
// Style functions
// ---------------------------------------------------------------------------

fn dock_background_style(theme: &iced::Theme) -> iced::widget::container::Style {
    let _ = theme;
    iced::widget::container::Style {
        background: Some(iced::Background::Color(Color::from_rgba(
            0.07, 0.07, 0.09, 0.88,
        ))),
        border: iced::Border {
            radius: 16.0.into(),
            width: 0.5,
            color: Color::from_rgba(1.0, 1.0, 1.0, 0.15),
        },
        shadow: iced::Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.35),
            offset: iced::Vector::new(0.0, 4.0),
            blur_radius: 20.0,
        },
        text_color: None,
    }
}

fn popup_background_style(theme: &iced::Theme) -> iced::widget::container::Style {
    let _ = theme;
    iced::widget::container::Style {
        background: Some(iced::Background::Color(Color::from_rgba(
            0.10, 0.10, 0.12, 0.95,
        ))),
        border: iced::Border {
            radius: 10.0.into(),
            width: 0.5,
            color: Color::from_rgba(1.0, 1.0, 1.0, 0.12),
        },
        shadow: iced::Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.30),
            offset: iced::Vector::new(0.0, 4.0),
            blur_radius: 16.0,
        },
        text_color: None,
    }
}

fn preview_item_style(theme: &iced::Theme) -> iced::widget::container::Style {
    let _ = theme;
    iced::widget::container::Style {
        background: Some(iced::Background::Color(Color::from_rgba(
            1.0, 1.0, 1.0, 0.05,
        ))),
        border: iced::Border {
            radius: 6.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

/// Derives a stable accent colour from an `app_id` for fallback text icons.
fn app_color(app_id: &str) -> Color {
    let hash = app_id
        .bytes()
        .fold(5381u32, |h, b| h.wrapping_mul(33).wrapping_add(u32::from(b)));
    let hue = (hash % 360) as f32;
    let (r, g, b) = hsl_to_rgb(hue, 0.55, 0.38);
    Color::from_rgb(r, g, b)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r1, g1, b1) = if h < 60.0 {
        (c, x, 0.0)
    } else if h < 120.0 {
        (x, c, 0.0)
    } else if h < 180.0 {
        (0.0, c, x)
    } else if h < 240.0 {
        (0.0, x, c)
    } else if h < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    (r1 + m, g1 + m, b1 + m)
}

fn is_trash_full() -> bool {
    let home = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/root"));
    let trash = home.join(".local/share/Trash/files");
    std::fs::read_dir(trash)
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    tracing::info!("shell-dock starting");

    if let Err(e) = <DockApp as iced_layershell::Application>::run(Settings {
        layer_settings: LayerShellSettings {
            // Span full screen width; compositor centres the surface vertically.
            anchor: Anchor::Bottom | Anchor::Left | Anchor::Right,
            layer: Layer::Top,
            // Reserve dock height so windows don't slide under it.
            exclusive_zone: SURFACE_HEIGHT as i32,
            // height = SURFACE_HEIGHT; width = 0 (compositor stretches to fill anchored axes)
            size: Some((0, SURFACE_HEIGHT)),
            // 8 px gap from bottom edge
            margin: (0, 0, 8, 0),
            keyboard_interactivity: KeyboardInteractivity::None,
            binded_output_name: None,
        },
        ..Settings::default()
    }) {
        tracing::error!("shell-dock exited with error: {e}");
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_color_is_deterministic() {
        let c1 = app_color("firefox");
        let c2 = app_color("firefox");
        assert_eq!(c1.r, c2.r);
        assert_eq!(c1.g, c2.g);
        assert_eq!(c1.b, c2.b);
    }

    #[test]
    fn app_color_differs_for_different_ids() {
        let c1 = app_color("firefox");
        let c2 = app_color("org.gnome.Terminal");
        assert!(c1.r != c2.r || c1.g != c2.g || c1.b != c2.b);
    }

    #[test]
    fn hsl_to_rgb_red_hue() {
        let (r, g, b) = hsl_to_rgb(0.0, 1.0, 0.5);
        assert!(r > 0.9, "red channel should be high for hue=0");
        assert!(g < 0.1, "green channel should be low for hue=0, s=1.0");
        assert!(b < 0.1, "blue channel should be low for hue=0, s=1.0");
    }

    #[test]
    fn is_trash_full_does_not_panic() {
        // Just ensure no panic regardless of actual trash state.
        let _ = is_trash_full();
    }
}
