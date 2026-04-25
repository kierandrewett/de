//! Root iced application for the file chooser dialog.

use std::collections::HashMap;
use std::path::PathBuf;

use iced::widget::{
    button, column, container, horizontal_rule, row, scrollable, text, text_input,
};
use iced::{Alignment, Element, Length, Task};
use serde::{Deserialize, Serialize};

use crate::args::{FileFilter, Options};
use crate::breadcrumb;
use crate::file_list::{self, FileEntry};
use crate::sidebar::{self, SidebarEntry};

/// JSON result written to stdout when the dialog closes.
#[derive(Debug, Serialize, Deserialize)]
pub struct DialogResult {
    /// 0 = success, 1 = cancelled.
    pub response: u32,
    /// Selected `file://` URIs.
    #[serde(default)]
    pub uris: Vec<String>,
    /// User choices for extra combo-boxes / checkboxes.
    #[serde(default)]
    pub choices: HashMap<String, String>,
    /// The filter the user selected last.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_filter: Option<FileFilter>,
}

/// Application messages.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum Message {
    /// Navigate into a new directory.
    Navigate(PathBuf),
    /// User clicked a file entry.
    EntryClicked(usize),
    /// User double-clicked a file entry.
    EntryActivated(usize),
    /// Sidebar bookmark clicked.
    SidebarClicked(usize),
    /// Breadcrumb segment clicked.
    BreadcrumbClicked(usize),
    /// Search / filename field changed.
    SearchChanged(String),
    /// File filter selection changed.
    FilterChanged(usize),
    /// Accept button pressed.
    Accept,
    /// Cancel button pressed.
    Cancel,
}

/// Root application state.
pub struct App {
    opts: Options,
    current_dir: PathBuf,
    entries: Vec<FileEntry>,
    selected: Vec<usize>,
    sidebar: Vec<SidebarEntry>,
    search: String,
    active_filter: usize,
    breadcrumb: Vec<breadcrumb::Segment>,
    done: bool,
}

impl App {
    /// Construct initial state from parsed options.
    pub fn new(opts: Options) -> (Self, Task<Message>) {
        let start = opts
            .current_folder
            .clone()
            .unwrap_or_else(|| {
                std::env::var("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from("/"))
            });

        let filters = if opts.filters.is_empty() {
            None
        } else {
            Some(opts.filters.as_slice())
        };

        let entries = file_list::list_dir(&start, filters);
        let sidebar = sidebar::entries();
        let breadcrumb = breadcrumb::segments(&start);

        let app = Self {
            opts,
            current_dir: start,
            entries,
            selected: Vec::new(),
            sidebar,
            search: String::new(),
            active_filter: 0,
            breadcrumb,
            done: false,
        };

        (app, Task::none())
    }

    fn navigate(&mut self, path: PathBuf) {
        let filter = self.current_filter_slice();
        self.entries = file_list::list_dir(&path, filter);
        self.breadcrumb = breadcrumb::segments(&path);
        self.current_dir = path;
        self.selected.clear();
        self.search.clear();
    }

    fn current_filter_slice(&self) -> Option<&[FileFilter]> {
        if self.opts.filters.is_empty() || self.active_filter == 0 {
            None
        } else {
            self.opts.filters.get(self.active_filter - 1).map(std::slice::from_ref)
        }
    }

    fn accept_entry(&self) -> Option<Vec<PathBuf>> {
        if self.selected.is_empty() {
            return None;
        }
        let paths: Vec<PathBuf> = self
            .selected
            .iter()
            .filter_map(|&i| self.entries.get(i))
            .map(|e| e.path.clone())
            .collect();
        if paths.is_empty() { None } else { Some(paths) }
    }

    /// Emit the result JSON to stdout and signal that we're done.
    fn emit_result(&mut self, response: u32, paths: Vec<PathBuf>) {
        let uris = paths
            .iter()
            .map(|p| format!("file://{}", p.display()))
            .collect();
        let result = DialogResult {
            response,
            uris,
            choices: HashMap::new(),
            current_filter: self.opts.filters.get(
                self.active_filter.saturating_sub(1)
            ).cloned(),
        };
        println!("{}", serde_json::to_string(&result).unwrap_or_default());
        self.done = true;
    }

    /// Update the file list with the current search filter applied.
    fn refresh_entries(&mut self) {
        let filter = self.current_filter_slice();
        let mut entries = file_list::list_dir(&self.current_dir, filter);
        if !self.search.is_empty() {
            let q = self.search.to_lowercase();
            entries.retain(|e| e.name.to_lowercase().contains(&q));
        }
        self.entries = entries;
        self.selected.clear();
    }

    pub fn update(&mut self, msg: Message) -> Task<Message> {
        match msg {
            Message::Navigate(path) => {
                self.navigate(path);
            }
            Message::EntryClicked(i) => {
                if self.opts.multiple {
                    if self.selected.contains(&i) {
                        self.selected.retain(|&x| x != i);
                    } else {
                        self.selected.push(i);
                    }
                } else {
                    self.selected = vec![i];
                }
            }
            Message::EntryActivated(i) => {
                if let Some(entry) = self.entries.get(i) {
                    if entry.is_dir {
                        self.navigate(entry.path.clone());
                    } else {
                        self.selected = vec![i];
                        let paths = self.accept_entry().unwrap_or_default();
                        self.emit_result(0, paths);
                    }
                }
            }
            Message::SidebarClicked(i) => {
                if let Some(entry) = self.sidebar.get(i) {
                    self.navigate(entry.path.clone());
                }
            }
            Message::BreadcrumbClicked(i) => {
                if let Some(seg) = self.breadcrumb.get(i) {
                    self.navigate(seg.path.clone());
                }
            }
            Message::SearchChanged(s) => {
                self.search = s;
                self.refresh_entries();
            }
            Message::FilterChanged(i) => {
                self.active_filter = i;
                self.refresh_entries();
            }
            Message::Accept => {
                let paths = self.accept_entry().unwrap_or_default();
                let response = if paths.is_empty() { 1 } else { 0 };
                self.emit_result(response, paths);
            }
            Message::Cancel => {
                self.emit_result(1, Vec::new());
            }
        }
        Task::none()
    }

    pub fn view(&self) -> Element<'_, Message> {
        // ── Sidebar ──
        let sidebar_items = self.sidebar.iter().enumerate().map(|(i, entry)| {
            button(text(entry.name.clone()))
                .on_press(Message::SidebarClicked(i))
                .width(Length::Fill)
                .into()
        });
        let sidebar_col: Element<'_, Message> = container(
            scrollable(column(sidebar_items).spacing(2).padding(8))
                .height(Length::Fill),
        )
        .width(200)
        .height(Length::Fill)
        .into();

        // ── Breadcrumb ──
        let crumb_items: Vec<Element<'_, Message>> = self
            .breadcrumb
            .iter()
            .enumerate()
            .flat_map(|(i, seg)| {
                let btn = button(text(seg.label.clone()))
                    .on_press(Message::BreadcrumbClicked(i));
                if i + 1 < self.breadcrumb.len() {
                    vec![btn.into(), text(" / ").into()]
                } else {
                    vec![btn.into()]
                }
            })
            .collect();
        let breadcrumb_row = container(
            row(crumb_items).spacing(0).align_y(Alignment::Center),
        )
        .padding([4, 8]);

        // ── Search ──
        let search_bar = text_input(
            "Search…",
            &self.search,
        )
        .on_input(Message::SearchChanged)
        .padding(6)
        .width(Length::Fill);

        // ── File list ──
        let file_items: Vec<Element<'_, Message>> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let is_selected = self.selected.contains(&i);
                let label = if entry.is_dir {
                    format!("📁 {}", entry.name)
                } else {
                    entry.name.clone()
                };
                let size_str = if entry.is_dir {
                    String::new()
                } else {
                    file_list::format_size(entry.size)
                };
                let row_el = row![
                    text(label).width(Length::Fill),
                    text(size_str).width(80),
                ]
                .spacing(8)
                .padding([4, 8])
                .align_y(Alignment::Center);

                let btn = button(row_el)
                    .on_press(Message::EntryClicked(i))
                    .width(Length::Fill)
                    .style(if is_selected {
                        iced::widget::button::primary
                    } else {
                        iced::widget::button::text
                    });
                btn.into()
            })
            .collect();
        let file_pane: Element<'_, Message> = container(
            scrollable(column(file_items).spacing(1)).height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .into();

        // ── Bottom bar (filter + filename + action buttons) ──
        let mut bottom_items: Vec<Element<'_, Message>> = Vec::new();

        // Filter pick-list (only when filters are specified)
        if !self.opts.filters.is_empty() {
            let filter_labels: Vec<String> = std::iter::once("All Files".to_owned())
                .chain(self.opts.filters.iter().map(|f| f.name.clone()))
                .collect();
            let _selected_label = filter_labels
                .get(self.active_filter)
                .cloned()
                .unwrap_or_default();
            // Render as simple text (pick_list requires Clone + PartialEq on items)
            bottom_items.push(text(format!("Filter: {_selected_label}")).into());
        }

        // Accept / Cancel
        bottom_items.push(
            row![
                button(text(self.opts.accept_label.clone()))
                    .on_press(Message::Accept),
                button(text("Cancel"))
                    .on_press(Message::Cancel),
            ]
            .spacing(8)
            .into(),
        );

        let bottom_bar = container(
            row(bottom_items)
                .spacing(16)
                .align_y(Alignment::Center)
                .padding([8, 12]),
        )
        .width(Length::Fill);

        // ── Root layout ──
        let top_bar = container(
            column![
                breadcrumb_row,
                search_bar,
            ]
            .spacing(4),
        )
        .width(Length::Fill);

        let content_row = row![sidebar_col, file_pane].spacing(0).height(Length::Fill);

        let root = column![
            top_bar,
            horizontal_rule(1),
            content_row,
            horizontal_rule(1),
            bottom_bar,
        ]
        .width(Length::Fill)
        .height(Length::Fill);

        container(root)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// Window title string.
    pub fn title(&self) -> String {
        self.opts.title.clone()
    }
}
