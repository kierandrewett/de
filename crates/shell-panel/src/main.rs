mod app;
mod control_centre;
mod datetime_popout;
mod dbus;
mod panel;

fn main() -> iced_layershell::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    app::run()
}
