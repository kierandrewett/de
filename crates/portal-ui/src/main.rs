//! Entry point for the `portal-ui` file chooser dialog.
//!
//! Reads a JSON options object from stdin (one line), shows the dialog, and
//! writes a JSON result object to stdout when the user closes it.

mod app;
mod args;
mod breadcrumb;
mod file_list;
mod sidebar;

use app::App;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let opts = args::read_from_stdin()?;

    iced::application(App::title, App::update, App::view)
        .window(iced::window::Settings {
            size: iced::Size::new(900.0, 600.0),
            resizable: true,
            decorations: true,
            ..Default::default()
        })
        .run_with(move || App::new(opts))
        .map_err(|e| anyhow::anyhow!("iced error: {e}"))?;

    Ok(())
}
