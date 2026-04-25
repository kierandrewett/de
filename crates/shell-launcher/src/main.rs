//! Spotlight-style application launcher for the Wayland desktop environment.
//!
//! Invoked by the compositor when the Super key is pressed. Presents a centred
//! floating search bar that searches installed apps, recent files, performs
//! inline calculator evaluation, and falls back to a web search.

mod app;
mod history;
mod launch;
mod search;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    tracing::info!("shell-launcher starting");

    iced::application(app::Launcher::title, app::Launcher::update, app::Launcher::view)
        .subscription(app::Launcher::subscription)
        .theme(app::Launcher::theme)
        .window(iced::window::Settings {
            size: iced::Size::new(640.0, 520.0),
            decorations: false,
            transparent: true,
            level: iced::window::Level::AlwaysOnTop,
            position: iced::window::Position::Centered,
            resizable: false,
            ..Default::default()
        })
        .run_with(app::Launcher::init)
        .map_err(|e| anyhow::anyhow!("iced error: {e}"))
}
