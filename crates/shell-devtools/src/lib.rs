//! Shell DevTools — an iced-based widget inspector for the custom Wayland DE.
//!
//! This crate provides a developer-tools overlay that wraps any iced UI and
//! shows a live widget-tree inspector, bounds overlay, and performance counters.
//!
//! # Feature flags
//!
//! | Flag | Default | Description |
//! |------|---------|-------------|
//! | `devtools` | ✓ | Enables all devtools functionality. Disable for production builds with zero overhead. |
//!
//! # Quick start
//!
//! ```no_run
//! use shell_devtools::{DevTools, DevToolsState, DevToolsMessage};
//!
//! #[derive(Clone)]
//! enum MyMessage {
//!     DevTools(DevToolsMessage),
//! }
//!
//! impl From<DevToolsMessage> for MyMessage {
//!     fn from(m: DevToolsMessage) -> Self { Self::DevTools(m) }
//! }
//!
//! // In your application state:
//! struct MyApp {
//!     devtools: DevToolsState,
//! }
//!
//! // In your view function, wrap your root widget:
//! fn view(app: &mut MyApp) -> iced::Element<'_, MyMessage> {
//!     use iced::widget::text;
//!     let content: iced::Element<'_, MyMessage> = text("Hello, world!").into();
//!     DevTools::new(content, &mut app.devtools).into()
//! }
//! ```
#![deny(missing_docs)]

#[cfg(feature = "devtools")]
pub mod dump;
#[cfg(feature = "devtools")]
pub mod message;
#[cfg(feature = "devtools")]
pub mod overlay;
#[cfg(feature = "devtools")]
pub mod panel;
#[cfg(feature = "devtools")]
pub mod perf;
#[cfg(feature = "devtools")]
pub mod state;
#[cfg(feature = "devtools")]
pub mod widget;

#[cfg(feature = "devtools")]
pub use message::DevToolsMessage;
#[cfg(feature = "devtools")]
pub use state::{DevToolsState, WidgetNode};
#[cfg(feature = "devtools")]
pub use widget::DevTools;
