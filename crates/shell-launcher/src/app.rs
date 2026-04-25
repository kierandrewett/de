//! Root iced application for the shell launcher.

use std::time::Duration;

use iced::keyboard;
use iced::widget::{
    button, column, container, row, scrollable, text, text_input,
};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, Subscription, Task, Theme};

use crate::history::LaunchHistory;
use crate::search::{self, AppEntry, RecentFile, SearchResult};

/// Text-input widget ID so we can programmatically focus it.
const SEARCH_INPUT_ID: &str = "launcher-search";

/// Application state.
pub struct Launcher {
    /// Current query string.
    query: String,
    /// Ranked search results for the current query.
    results: Vec<SearchResult>,
    /// Index of the keyboard-selected result.
    selected: usize,
    /// All installed apps, loaded at startup.
    apps: Vec<AppEntry>,
    /// Recent files, loaded at startup.
    recent: Vec<RecentFile>,
    /// Per-app launch frequency.
    history: LaunchHistory,
    /// Monotonic counter incremented on every query change for debouncing.
    debounce_seq: u32,
}

/// Application messages.
#[derive(Debug, Clone)]
pub enum Message {
    /// Text input changed.
    QueryChanged(String),
    /// Debounce timer fired; carry the sequence number to discard stale fires.
    SearchDebounced(u32, String),
    /// Move keyboard selection up.
    SelectionUp,
    /// Move keyboard selection down.
    SelectionDown,
    /// Launch the selected result (Enter key or click).
    Launch,
    /// Launch a specific result by index.
    LaunchIndex(usize),
    /// Close the launcher (Escape key).
    Close,
    /// Cycle through result categories with Tab.
    TabCycle,
    /// App list has been loaded from disk.
    AppsLoaded(Vec<AppEntry>),
    /// Recent files have been loaded from disk.
    RecentLoaded(Vec<RecentFile>),
}

impl Launcher {
    /// Initial state + startup task.
    pub fn init() -> (Self, Task<Message>) {
        let history = LaunchHistory::load();

        let state = Self {
            query: String::new(),
            results: Vec::new(),
            selected: 0,
            apps: Vec::new(),
            recent: Vec::new(),
            history,
            debounce_seq: 0,
        };

        let focus_task = text_input::focus(text_input::Id::new(SEARCH_INPUT_ID));

        let load_task = Task::batch([
            Task::perform(
                async { search::desktop::load_apps() },
                Message::AppsLoaded,
            ),
            Task::perform(
                async { search::recent::load_recent_files() },
                Message::RecentLoaded,
            ),
        ]);

        (state, Task::batch([focus_task, load_task]))
    }

    /// Update handler (iced 0.13 function-style API).
    pub fn update(&mut self, msg: Message) -> Task<Message> {
        match msg {
            Message::QueryChanged(q) => {
                self.query = q;
                self.debounce_seq = self.debounce_seq.wrapping_add(1);
                let seq = self.debounce_seq;
                let query = self.query.clone();

                return Task::perform(
                    async move {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        (seq, query)
                    },
                    |(seq, q)| Message::SearchDebounced(seq, q),
                );
            }

            Message::SearchDebounced(seq, query) => {
                if seq == self.debounce_seq {
                    self.results = search::search(
                        &query,
                        &self.apps,
                        &self.recent,
                        self.history.counts(),
                    );
                    self.selected = 0;
                }
            }

            Message::SelectionUp => {
                if !self.results.is_empty() && self.selected > 0 {
                    self.selected -= 1;
                }
            }

            Message::SelectionDown => {
                if self.selected + 1 < self.results.len() {
                    self.selected += 1;
                }
            }

            Message::Launch => {
                if let Some(result) = self.results.get(self.selected) {
                    self.record_and_launch(result.clone());
                    return iced::exit();
                }
            }

            Message::LaunchIndex(i) => {
                if let Some(result) = self.results.get(i).cloned() {
                    self.record_and_launch(result);
                    return iced::exit();
                }
            }

            Message::Close => {
                return iced::exit();
            }

            Message::TabCycle => {
                if !self.results.is_empty() {
                    self.selected = (self.selected + 1) % self.results.len();
                }
            }

            Message::AppsLoaded(apps) => {
                tracing::info!("loaded {} applications", apps.len());
                self.apps = apps;
                // Re-run search in case there is already a query.
                if !self.query.is_empty() {
                    self.results = search::search(
                        &self.query,
                        &self.apps,
                        &self.recent,
                        self.history.counts(),
                    );
                }
            }

            Message::RecentLoaded(recent) => {
                tracing::info!("loaded {} recent files", recent.len());
                self.recent = recent;
            }
        }

        Task::none()
    }

    fn record_and_launch(&mut self, result: SearchResult) {
        if let SearchResult::Application(ref app) = result {
            self.history.record(&app.name);
        }
        crate::launch::launch(&result);
    }

    /// View function (iced 0.13).
    pub fn view(&self) -> Element<'_, Message> {
        let search_bar = self.search_bar_view();
        let results = self.results_view();

        let card = container(
            column![search_bar, results].spacing(0),
        )
        .width(Length::Fixed(640.0))
        .style(|_theme: &Theme| container::Style {
            background: Some(Background::Color(Color::from_rgba(0.12, 0.12, 0.14, 0.95))),
            border: Border {
                color: Color::from_rgba(1.0, 1.0, 1.0, 0.12),
                width: 1.0,
                radius: 14.0.into(),
            },
            ..Default::default()
        });

        container(card)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
            .style(|_theme: &Theme| container::Style {
                background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.4))),
                ..Default::default()
            })
            .into()
    }

    fn search_bar_view(&self) -> Element<'_, Message> {
        let icon = text("🔍").size(18);

        let input = text_input("Search apps, run commands…", &self.query)
            .id(text_input::Id::new(SEARCH_INPUT_ID))
            .on_input(Message::QueryChanged)
            .on_submit(Message::Launch)
            .padding(0)
            .size(20)
            .width(Length::Fill)
            .style(|_theme: &Theme, _status| text_input::Style {
                background: Background::Color(Color::TRANSPARENT),
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 0.0.into(),
                },
                icon: Color::from_rgba(1.0, 1.0, 1.0, 0.5),
                placeholder: Color::from_rgba(1.0, 1.0, 1.0, 0.35),
                value: Color::WHITE,
                selection: Color::from_rgba(0.4, 0.6, 1.0, 0.4),
            });

        container(
            row![icon, input]
                .spacing(12)
                .align_y(Alignment::Center)
                .padding([0, 4]),
        )
        .padding([16, 20])
        .width(Length::Fill)
        .into()
    }

    fn results_view(&self) -> Element<'_, Message> {
        if self.results.is_empty() {
            return column![].into();
        }

        let mut category_state: Option<&str> = None;
        let mut items: Vec<Element<'_, Message>> = Vec::new();

        // Horizontal separator between search bar and results.
        items.push(
            container(iced::widget::horizontal_rule(1))
                .padding([0, 20])
                .width(Length::Fill)
                .style(|_: &Theme| container::Style {
                    ..Default::default()
                })
                .into(),
        );

        for (i, result) in self.results.iter().enumerate() {
            let cat = result.category();
            if category_state != Some(cat) {
                category_state = Some(cat);
                items.push(self.category_header(cat));
            }
            items.push(self.result_row(i, result));
        }

        container(
            scrollable(column(items).spacing(0))
                .height(Length::Shrink),
        )
        .padding(Padding { top: 0.0, right: 0.0, bottom: 8.0, left: 0.0 })
        .width(Length::Fill)
        .into()
    }

    fn category_header<'a>(&self, label: &'a str) -> Element<'a, Message> {
        container(
            text(label)
                .size(11)
                .color(Color::from_rgba(1.0, 1.0, 1.0, 0.45)),
        )
        .padding(Padding { top: 8.0, right: 20.0, bottom: 2.0, left: 20.0 })
        .width(Length::Fill)
        .into()
    }

    fn result_row(&self, index: usize, result: &SearchResult) -> Element<'_, Message> {
        let is_selected = index == self.selected;

        let name_color = if is_selected {
            Color::WHITE
        } else {
            Color::from_rgba(1.0, 1.0, 1.0, 0.87)
        };

        let name_widget = text(result.display_name())
            .size(15)
            .color(name_color)
            .width(Length::Fill);

        let row_content: Element<'_, Message> = if let Some(sub) = result.subtitle() {
            column![
                name_widget,
                text(sub)
                    .size(12)
                    .color(Color::from_rgba(1.0, 1.0, 1.0, 0.45)),
            ]
            .spacing(1)
            .into()
        } else {
            name_widget.into()
        };

        let row_padded = container(row_content).padding([6, 20]);

        let btn = button(row_padded)
            .on_press(Message::LaunchIndex(index))
            .width(Length::Fill)
            .style(move |_theme: &Theme, _status| {
                let bg_color = if is_selected {
                    Color::from_rgba(0.4, 0.5, 1.0, 0.25)
                } else {
                    Color::TRANSPARENT
                };
                button::Style {
                    background: Some(Background::Color(bg_color)),
                    border: Border {
                        color: Color::TRANSPARENT,
                        width: 0.0,
                        radius: 8.0.into(),
                    },
                    text_color: Color::WHITE,
                    ..Default::default()
                }
            });

        container(btn).padding([0, 4]).width(Length::Fill).into()
    }

    /// Keyboard subscription.
    pub fn subscription(&self) -> Subscription<Message> {
        keyboard::on_key_press(|k, _mods| {
            use keyboard::key::Named;
            match k.as_ref() {
                keyboard::Key::Named(Named::Escape) => Some(Message::Close),
                keyboard::Key::Named(Named::ArrowUp) => Some(Message::SelectionUp),
                keyboard::Key::Named(Named::ArrowDown) => Some(Message::SelectionDown),
                keyboard::Key::Named(Named::Tab) => Some(Message::TabCycle),
                _ => None,
            }
        })
    }

    /// Window title.
    pub fn title(&self) -> String {
        "Launcher".to_owned()
    }

    /// Dark theme.
    pub fn theme(&self) -> Theme {
        Theme::Dark
    }
}
