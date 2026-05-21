//! appmenu-test — manual verification client for the compositor's
//! KDE-style global menu.
//!
//! It opens a plain wayland window, exports a `com.canonical.dbusmenu`
//! over the session bus (File / Edit / View with submenus), and links the
//! two with `org_kde_kwin_appmenu::set_address`. When this window has
//! keyboard focus the compositor's panel should show its menu bar.
//!
//! Run it against a live compositor:
//!   WAYLAND_DISPLAY=wayland-N cargo run -p appmenu-test

mod appmenu;

use std::collections::HashMap;
use std::io::Write;
use std::os::fd::AsFd;
use std::sync::mpsc;

use wayland_client::{
    protocol::{
        wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
        wl_registry::WlRegistry, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
    },
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel, xdg_wm_base::XdgWmBase,
};

use appmenu::org_kde_kwin_appmenu::OrgKdeKwinAppmenu;
use appmenu::org_kde_kwin_appmenu_manager::OrgKdeKwinAppmenuManager;

const DBUS_NAME: &str = "org.deprompts.AppmenuTest";
const MENU_PATH: &str = "/MenuBar";
const WIN_W: i32 = 480;
const WIN_H: i32 = 320;

// ── com.canonical.dbusmenu server ───────────────────────────────────────────

use zbus::zvariant::{OwnedValue, Value};

/// One static menu row.
struct Row {
    id: i32,
    label: &'static str,
    separator: bool,
    submenu: bool,
}

/// Returns `(parent_props, children)` for a GetLayout(`parent_id`) call.
fn menu_for(parent_id: i32) -> Option<(&'static str, Vec<Row>)> {
    let r = |id, label, separator, submenu| Row {
        id,
        label,
        separator,
        submenu,
    };
    match parent_id {
        0 => Some((
            "",
            vec![
                r(1, "File", false, true),
                r(2, "Edit", false, true),
                r(3, "View", false, true),
            ],
        )),
        1 => Some((
            "File",
            vec![
                r(11, "New Window", false, false),
                r(12, "Open…", false, false),
                r(13, "", true, false),
                r(14, "Quit", false, false),
            ],
        )),
        2 => Some((
            "Edit",
            vec![
                r(21, "Undo", false, false),
                r(22, "Redo", false, false),
                r(23, "", true, false),
                r(24, "Copy", false, false),
                r(25, "Paste", false, false),
            ],
        )),
        3 => Some((
            "View",
            vec![
                r(31, "Zoom In", false, false),
                r(32, "Zoom Out", false, false),
                r(33, "Fullscreen", false, false),
            ],
        )),
        _ => None,
    }
}

/// Build a child layout node `(id, a{sv} props, av children)` as an
/// OwnedValue (depth-1 — grandchildren omitted).
fn child_node(row: &Row) -> OwnedValue {
    let mut props: HashMap<String, OwnedValue> = HashMap::new();
    if row.separator {
        props.insert(
            "type".into(),
            OwnedValue::try_from(Value::from("separator".to_string())).unwrap(),
        );
    } else {
        props.insert(
            "label".into(),
            OwnedValue::try_from(Value::from(row.label.to_string())).unwrap(),
        );
        props.insert(
            "enabled".into(),
            OwnedValue::try_from(Value::from(true)).unwrap(),
        );
        if row.submenu {
            props.insert(
                "children-display".into(),
                OwnedValue::try_from(Value::from("submenu".to_string())).unwrap(),
            );
        }
    }
    let empty: Vec<OwnedValue> = Vec::new();
    OwnedValue::try_from(Value::from((row.id, props, empty))).unwrap()
}

struct DbusMenu;

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl DbusMenu {
    /// The compositor calls this before fetching a submenu.
    async fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    #[zbus(property)]
    async fn version(&self) -> u32 {
        3
    }

    #[zbus(property)]
    async fn status(&self) -> String {
        "normal".into()
    }

    /// Return the layout rooted at `parent_id`, one level deep.
    async fn get_layout(
        &self,
        parent_id: i32,
        _recursion_depth: i32,
        _property_names: Vec<String>,
    ) -> (u32, (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>)) {
        let (label, rows) = menu_for(parent_id).unwrap_or(("", Vec::new()));
        let mut props: HashMap<String, OwnedValue> = HashMap::new();
        if !label.is_empty() {
            props.insert(
                "label".into(),
                OwnedValue::try_from(Value::from(label.to_string())).unwrap(),
            );
        }
        let children: Vec<OwnedValue> = rows.iter().map(child_node).collect();
        (1, (parent_id, props, children))
    }

    /// Row activated — print it so the test is observable from the terminal.
    async fn event(
        &self,
        id: i32,
        event_id: String,
        _data: Value<'_>,
        _timestamp: u32,
    ) {
        if event_id == "clicked" {
            let label = (0..4)
                .filter_map(|p| menu_for(p))
                .flat_map(|(_, rows)| rows)
                .find(|r| r.id == id)
                .map(|r| r.label)
                .unwrap_or("?");
            println!("[appmenu-test] menu item activated: id={id} ({label})");
        }
    }
}

/// Bring up the D-Bus service on a background tokio runtime. Signals
/// `ready` once the well-known name is owned.
fn spawn_dbus(ready: mpsc::Sender<()>) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(async move {
            let conn = zbus::connection::Builder::session()
                .expect("session bus")
                .name(DBUS_NAME)
                .expect("request name")
                .serve_at(MENU_PATH, DbusMenu)
                .expect("serve dbusmenu")
                .build()
                .await
                .expect("build connection");
            println!("[appmenu-test] dbusmenu live at {DBUS_NAME} {MENU_PATH}");
            let _ = ready.send(());
            // Keep the connection (and runtime) alive forever.
            std::future::pending::<()>().await;
            drop(conn);
        });
    });
}

// ── wayland client ──────────────────────────────────────────────────────────

#[derive(Default)]
struct App {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    appmenu_mgr: Option<OrgKdeKwinAppmenuManager>,
    surface: Option<WlSurface>,
    buffer: Option<WlBuffer>,
    configured: bool,
    closed: bool,
}

fn main() {
    // 1. D-Bus menu service first, so set_address points at a live name.
    let (tx, rx) = mpsc::channel();
    spawn_dbus(tx);
    let _ = rx.recv_timeout(std::time::Duration::from_secs(5));

    // 2. Wayland window.
    let conn = Connection::connect_to_env().expect("connect to wayland (set WAYLAND_DISPLAY)");
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let display = conn.display();
    display.get_registry(&qh, ());

    let mut app = App::default();
    // Roundtrip until the globals we need are bound.
    queue.roundtrip(&mut app).expect("registry roundtrip");
    queue.roundtrip(&mut app).expect("globals roundtrip");

    let compositor = app.compositor.clone().expect("no wl_compositor");
    let shm = app.shm.clone().expect("no wl_shm");
    let wm_base = app.wm_base.clone().expect("no xdg_wm_base");
    let appmenu_mgr = app
        .appmenu_mgr
        .clone()
        .expect("compositor does not advertise org_kde_kwin_appmenu_manager");

    // 3. SHM buffer — a solid dark fill is enough to map the window.
    let stride = WIN_W * 4;
    let size = (stride * WIN_H) as usize;
    let mut file = tempfile().expect("shm tempfile");
    {
        let px = [0x2e_u8, 0x1e, 0x1e, 0xff]; // BGRA dark
        let mut buf = Vec::with_capacity(size);
        for _ in 0..(WIN_W * WIN_H) {
            buf.extend_from_slice(&px);
        }
        file.write_all(&buf).expect("write shm");
        file.flush().ok();
    }
    let pool = shm.create_pool(file.as_fd(), size as i32, &qh, ());
    let buffer = pool.create_buffer(
        0,
        WIN_W,
        WIN_H,
        stride,
        wayland_client::protocol::wl_shm::Format::Argb8888,
        &qh,
        (),
    );
    app.buffer = Some(buffer.clone());

    // 4. xdg-shell surface + toplevel.
    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title("Appmenu Test".into());
    toplevel.set_app_id("org.deprompts.appmenu-test".into());
    surface.commit();
    app.surface = Some(surface.clone());

    // 5. Link the window to the exported menu.
    let appmenu = appmenu_mgr.create(&surface, &qh, ());
    appmenu.set_address(DBUS_NAME.into(), MENU_PATH.into());
    println!("[appmenu-test] set_address({DBUS_NAME}, {MENU_PATH}) — window should show its menu");

    // 6. Event loop.
    while !app.closed {
        queue.blocking_dispatch(&mut app).expect("dispatch");
    }
    println!("[appmenu-test] window closed — exiting");
}

/// A regular file in XDG_RUNTIME_DIR / tmp used as the SHM backing store.
fn tempfile() -> std::io::Result<std::fs::File> {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let path = format!("{dir}/appmenu-test-{}.buf", std::process::id());
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)?;
    // Unlink immediately — the fd keeps it alive, nothing else needs the path.
    let _ = std::fs::remove_file(&path);
    Ok(file)
}

// ── Dispatch glue ────────────────────────────────────────────────────────────

impl Dispatch<WlRegistry, ()> for App {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wayland_client::protocol::wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_registry::Event;
        if let Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor =
                        Some(registry.bind::<WlCompositor, _, _>(name, version.min(4), qh, ()));
                }
                "wl_shm" => {
                    state.shm = Some(registry.bind::<WlShm, _, _>(name, 1, qh, ()));
                }
                "xdg_wm_base" => {
                    state.wm_base =
                        Some(registry.bind::<XdgWmBase, _, _>(name, version.min(3), qh, ()));
                }
                "org_kde_kwin_appmenu_manager" => {
                    state.appmenu_mgr = Some(registry.bind::<OrgKdeKwinAppmenuManager, _, _>(
                        name,
                        version.min(2),
                        qh,
                        (),
                    ));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<XdgWmBase, ()> for App {
    fn event(
        _: &mut Self,
        wm_base: &XdgWmBase,
        event: wayland_protocols::xdg::shell::client::xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_protocols::xdg::shell::client::xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for App {
    fn event(
        state: &mut Self,
        xdg_surface: &XdgSurface,
        event: wayland_protocols::xdg::shell::client::xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_protocols::xdg::shell::client::xdg_surface::Event::Configure { serial } =
            event
        {
            xdg_surface.ack_configure(serial);
            if let (Some(surface), Some(buffer)) = (&state.surface, &state.buffer) {
                if !state.configured {
                    surface.attach(Some(buffer), 0, 0);
                    surface.damage(0, 0, WIN_W, WIN_H);
                    surface.commit();
                    state.configured = true;
                }
            }
        }
    }
}

impl Dispatch<XdgToplevel, ()> for App {
    fn event(
        state: &mut Self,
        _: &XdgToplevel,
        event: wayland_protocols::xdg::shell::client::xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_protocols::xdg::shell::client::xdg_toplevel::Event::Close = event {
            state.closed = true;
        }
    }
}

// No-op dispatches for proxies that emit events we don't act on.
macro_rules! noop_dispatch {
    ($($ty:ty),* $(,)?) => {
        $(impl Dispatch<$ty, ()> for App {
            fn event(
                _: &mut Self, _: &$ty,
                _: <$ty as wayland_client::Proxy>::Event,
                _: &(), _: &Connection, _: &QueueHandle<Self>,
            ) {}
        })*
    };
}
noop_dispatch!(
    WlCompositor,
    WlShm,
    WlShmPool,
    WlBuffer,
    WlSurface,
    WlCallback,
    OrgKdeKwinAppmenuManager,
    OrgKdeKwinAppmenu,
);
