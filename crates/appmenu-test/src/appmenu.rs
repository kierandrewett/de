//! Client-side bindings for the vendored `org_kde_kwin_appmenu` protocol
//! (shares `compositor-slint/protocols/appmenu.xml`).
#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case)]
#![allow(unused_imports, dead_code, clippy::all)]

pub mod __interfaces {
    use wayland_client::protocol::__interfaces::*;
    wayland_scanner::generate_interfaces!("../compositor-slint/protocols/appmenu.xml");
}

use self::__interfaces::*;
use wayland_client;
use wayland_client::protocol::*;

wayland_scanner::generate_client_code!("../compositor-slint/protocols/appmenu.xml");
