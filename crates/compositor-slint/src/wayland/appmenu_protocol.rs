//! Generated server-side bindings for the vendored `org_kde_kwin_appmenu`
//! protocol (`protocols/appmenu.xml`).
//!
//! This is the Wayland-native side of KDE-style global menus: a client
//! creates an `org_kde_kwin_appmenu` for one of its surfaces and calls
//! `set_address(service_name, object_path)` to point at the
//! `com.canonical.dbusmenu` interface it exports over D-Bus. The panel then
//! fetches + renders that menu for whichever window has focus.
//!
//! wayland-scanner generates the interface tables + server dispatch glue;
//! the actual `GlobalDispatch` / `Dispatch` impls live in
//! `wayland/appmenu.rs`.
#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case)]
#![allow(unused_imports, dead_code, clippy::all)]

pub mod __interfaces {
    use wayland_server::protocol::__interfaces::*;
    wayland_scanner::generate_interfaces!("protocols/appmenu.xml");
}

use self::__interfaces::*;
use wayland_server;
use wayland_server::protocol::*;

wayland_scanner::generate_server_code!("protocols/appmenu.xml");
