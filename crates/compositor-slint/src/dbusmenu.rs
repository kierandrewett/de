//! com.canonical.dbusmenu fetch + event dispatch.
//!
//! StatusNotifierItem (SNI) entries expose a `Menu` property — an object
//! path on the same DBus service implementing `com.canonical.dbusmenu`.
//! We fetch the layout via `GetLayout`, render it inside our existing
//! Slint `ContextMenu` widget, and send `Event(id, "clicked", ...)` back
//! when the user picks a menu entry.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::runtime::Builder;
use zbus::{
    names::BusName,
    zvariant::{ObjectPath, OwnedValue, Value},
    Connection, Proxy,
};

/// A single rendered menu entry. Submenus are flattened so v1 just shows
/// the top-level list — nested submenus would need an inline-expand UI
/// or a popup-on-hover delay we don't implement yet.
#[derive(Debug, Clone)]
pub struct DbusMenuItem {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    /// Visibility flag from the DBusMenu protocol. Currently read only at
    /// parse time (we drop invisible items before exposing the model to
    /// Slint), but the field is kept so a future "hidden items toggle" or
    /// remote-UI consumer doesn't have to re-introduce it.
    #[allow(dead_code)]
    pub visible: bool,
    pub separator: bool,
}

/// Read the `Menu` object-path property from a StatusNotifierItem. Some
/// SNI clients don't ship a menu — return None and the caller falls back
/// to the legacy `ContextMenu(x, y)` interaction.
async fn read_menu_property(bus: &str, sni_path: &str) -> Option<String> {
    let conn = Connection::session().await.ok()?;
    let bus_name = BusName::try_from(bus.to_string()).ok()?;
    let obj_path = ObjectPath::try_from(sni_path).ok()?;
    let proxy = Proxy::new(&conn, bus_name, obj_path, "org.kde.StatusNotifierItem")
        .await
        .ok()?;
    let v: OwnedValue = proxy.get_property("Menu").await.ok()?;
    let val: Value<'_> = v.into();
    match val {
        Value::ObjectPath(p) => Some(p.to_string()),
        _ => None,
    }
}

/// Pull a string-keyed `OwnedValue` and convert it to T. Returns None on
/// type mismatch or missing key — the dbusmenu spec is loose enough that
/// every property must be treated as optional.
fn prop<T: TryFrom<OwnedValue>>(props: &HashMap<String, OwnedValue>, key: &str) -> Option<T> {
    let v = props.get(key)?;
    let cloned = v.try_clone().ok()?;
    T::try_from(cloned).ok()
}

/// Recursive walk of a `(i32, a{sv}, av)` GetLayout response, flattening
/// to a single list of renderable entries. The synthetic root (id == 0)
/// is skipped; everything else with a non-empty label or `type=separator`
/// is appended in document order.
fn flatten_node(node_value: &Value<'_>, out: &mut Vec<DbusMenuItem>) {
    let Value::Structure(s) = node_value else {
        return;
    };
    let fields = s.fields();
    if fields.len() < 3 {
        return;
    }

    let id: i32 = match &fields[0] {
        Value::I32(i) => *i,
        _ => return,
    };

    // properties dict
    let mut props: HashMap<String, OwnedValue> = HashMap::new();
    if let Value::Dict(dict) = &fields[1] {
        for (k, v) in dict.iter() {
            if let Value::Str(ks) = k {
                if let Ok(owned) = v.try_clone().and_then(OwnedValue::try_from) {
                    props.insert(ks.to_string(), owned);
                }
            }
        }
    }

    let label_raw: String = prop(&props, "label").unwrap_or_default();
    let label = label_raw.replace('_', "");
    let item_type: String = prop(&props, "type").unwrap_or_default();
    let separator = item_type == "separator";
    let enabled: bool = prop(&props, "enabled").unwrap_or(true);
    let visible: bool = prop(&props, "visible").unwrap_or(true);

    if id != 0 && visible && (separator || !label.is_empty()) {
        out.push(DbusMenuItem {
            id,
            label,
            enabled,
            visible,
            separator,
        });
    }

    // children: array of variants, each variant wraps a recursive
    // (i32, a{sv}, av) tuple
    if let Value::Array(arr) = &fields[2] {
        for child_v in arr.iter() {
            if let Value::Value(boxed) = child_v {
                flatten_node(boxed, out);
            } else {
                flatten_node(child_v, out);
            }
        }
    }
}

/// Spawn a worker that fetches the DBusMenu layout and invokes `done`
/// with the flattened item list. Runs entirely off the wayland thread.
pub fn fetch_layout<F>(bus: String, sni_path: String, done: F)
where
    F: FnOnce(Vec<DbusMenuItem>) + Send + 'static,
{
    std::thread::spawn(move || {
        let rt = match Builder::new_current_thread().enable_all().build() {
            Ok(r) => r,
            Err(_) => {
                done(Vec::new());
                return;
            }
        };
        let items = rt.block_on(async move {
            let menu_path = match read_menu_property(&bus, &sni_path).await {
                Some(p) => p,
                None => return Vec::new(),
            };
            let conn = match Connection::session().await {
                Ok(c) => c,
                Err(_) => return Vec::new(),
            };
            let bus_name = match BusName::try_from(bus) {
                Ok(b) => b,
                Err(_) => return Vec::new(),
            };
            let obj_path = match ObjectPath::try_from(menu_path.as_str()) {
                Ok(p) => p,
                Err(_) => return Vec::new(),
            };
            let proxy = match Proxy::new(&conn, bus_name, obj_path, "com.canonical.dbusmenu").await
            {
                Ok(p) => p,
                Err(_) => return Vec::new(),
            };
            // Tell the client we're about to show the menu — some apps
            // populate state lazily in response.
            let _ = proxy.call::<_, _, bool>("AboutToShow", &(0i32)).await;
            let response: (u32, OwnedValue) = match proxy
                .call(
                    "GetLayout",
                    &(0i32, 1i32, vec!["label", "type", "enabled", "visible"]),
                )
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::debug!("dbusmenu GetLayout failed: {e}");
                    return Vec::new();
                }
            };
            let mut items = Vec::new();
            let root_val: Value<'_> = response.1.into();
            flatten_node(&root_val, &mut items);
            items
        });
        done(items);
    });
}

// ── KDE-style global menu (appmenu) ─────────────────────────────────────────
//
// Unlike the SNI tray menu, the appmenu address arrives directly as
// `(service, object_path)` via `org_kde_kwin_appmenu::set_address` — there's
// no `Menu` property indirection. We also need the menu in two levels: the
// top-level bar (root's direct children → File / Edit / View / …) and, on
// click, one menu's items. `fetch_appmenu_children` serves both: pass
// `parent_id = 0` for the bar, `parent_id = N` for submenu N.

/// One node in a global menu — either a bar entry or a submenu row.
#[derive(Debug, Clone)]
pub struct AppMenuNode {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub separator: bool,
    /// `children-display == "submenu"` — the row opens a nested menu.
    pub has_submenu: bool,
}

/// Parse one GetLayout child node — `(i32 id, a{sv} props, av children)`,
/// possibly variant-wrapped — into an `AppMenuNode`. Grandchildren are
/// ignored (we fetch one level at a time).
fn parse_menu_child(child_v: &Value<'_>) -> Option<AppMenuNode> {
    // Children arrive variant-wrapped inside the `av` array.
    let inner = match child_v {
        Value::Value(boxed) => boxed.as_ref(),
        other => other,
    };
    let Value::Structure(cs) = inner else {
        tracing::warn!("appmenu parse: child is not a structure: {:?}", inner);
        return None;
    };
    let cf = cs.fields();
    if cf.len() < 3 {
        return None;
    }
    let id: i32 = match &cf[0] {
        Value::I32(i) => *i,
        _ => return None,
    };
    let mut props: HashMap<String, OwnedValue> = HashMap::new();
    if let Value::Dict(dict) = &cf[1] {
        for (k, v) in dict.iter() {
            if let Value::Str(ks) = k {
                // `a{sv}` dict values are variants — unwrap to the inner
                // value so `String::try_from` / `bool::try_from` work.
                let unwrapped = match v {
                    Value::Value(boxed) => boxed.as_ref(),
                    other => other,
                };
                if let Ok(owned) = unwrapped.try_clone().and_then(OwnedValue::try_from) {
                    props.insert(ks.to_string(), owned);
                }
            }
        }
    }
    let label_raw: String = prop(&props, "label").unwrap_or_default();
    // dbusmenu labels carry '_' mnemonic markers — strip them.
    let label = label_raw.replace('_', "");
    let item_type: String = prop(&props, "type").unwrap_or_default();
    let separator = item_type == "separator";
    let enabled: bool = prop(&props, "enabled").unwrap_or(true);
    let visible: bool = prop(&props, "visible").unwrap_or(true);
    let children_display: String = prop(&props, "children-display").unwrap_or_default();
    if !visible || (!separator && label.is_empty()) {
        tracing::warn!(
            "appmenu parse: dropping node id={id} label={label:?} sep={separator} \
             prop_keys={:?}",
            props.keys().collect::<Vec<_>>()
        );
        return None;
    }
    Some(AppMenuNode {
        id,
        label,
        enabled,
        separator,
        has_submenu: children_display == "submenu",
    })
}

async fn appmenu_proxy<'a>(
    conn: &'a Connection,
    service: &str,
    menu_path: &str,
) -> Option<Proxy<'a>> {
    let bus_name = BusName::try_from(service.to_string()).ok()?;
    // Own the object path (`'static`) so the returned Proxy borrows only
    // from `conn`, not from the caller's `menu_path` slice.
    let obj_path = ObjectPath::try_from(menu_path.to_string()).ok()?;
    Proxy::new(conn, bus_name, obj_path, "com.canonical.dbusmenu")
        .await
        .ok()
}

/// Fetch the direct children of `parent_id` from a `com.canonical.dbusmenu`
/// at `(service, menu_path)`. `parent_id = 0` → the top-level menu bar.
pub fn fetch_appmenu_children<F>(service: String, menu_path: String, parent_id: i32, done: F)
where
    F: FnOnce(Vec<AppMenuNode>) + Send + 'static,
{
    std::thread::spawn(move || {
        let rt = match Builder::new_current_thread().enable_all().build() {
            Ok(r) => r,
            Err(_) => {
                done(Vec::new());
                return;
            }
        };
        let nodes = rt.block_on(async move {
            let conn = match Connection::session().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("appmenu fetch: session bus connect failed: {e}");
                    return Vec::new();
                }
            };
            let Some(proxy) = appmenu_proxy(&conn, &service, &menu_path).await else {
                tracing::warn!("appmenu fetch: could not build proxy for {service} {menu_path}");
                return Vec::new();
            };
            tracing::debug!("appmenu fetch: calling GetLayout({parent_id}) on {service}");
            // Let the app populate the submenu lazily if it wants to.
            let _ = proxy.call::<_, _, bool>("AboutToShow", &(parent_id)).await;
            // GetLayout → `(u revision, (ia{sv}av) layout)`. The layout is a
            // bare structure (NOT a variant), and its third field is the
            // `av` children array. Deserialize it with the exact shape so
            // zbus matches the `(u(ia{sv}av))` signature.
            type Layout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);
            let response: (u32, Layout) = match proxy
                .call(
                    "GetLayout",
                    &(
                        parent_id,
                        1i32,
                        vec!["label", "type", "enabled", "visible", "children-display"],
                    ),
                )
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::debug!("appmenu GetLayout({parent_id}) failed: {e}");
                    return Vec::new();
                }
            };
            let (_revision, (_root_id, _root_props, children)) = response;
            tracing::debug!(
                "appmenu fetch: GetLayout({parent_id}) ok — {} raw children",
                children.len()
            );
            let mut nodes = Vec::new();
            for child in &children {
                if let Some(node) = parse_menu_child(child) {
                    nodes.push(node);
                }
            }
            nodes
        });
        done(nodes);
    });
}

/// Send `Event(item_id, kind, null, now)` to a `com.canonical.dbusmenu`
/// addressed directly by `(service, menu_path)`. `kind` is "clicked" to
/// activate a row, or "opened"/"closed" for submenu lifecycle hints.
pub fn send_appmenu_event(service: String, menu_path: String, item_id: i32, kind: &'static str) {
    std::thread::spawn(move || {
        let rt = match Builder::new_current_thread().enable_all().build() {
            Ok(r) => r,
            Err(_) => return,
        };
        rt.block_on(async move {
            let conn = match Connection::session().await {
                Ok(c) => c,
                Err(_) => return,
            };
            let Some(proxy) = appmenu_proxy(&conn, &service, &menu_path).await else {
                return;
            };
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as u32)
                .unwrap_or(0);
            let _ = proxy
                .call::<_, _, ()>("Event", &(item_id, kind, Value::U32(0), timestamp))
                .await;
        });
    });
}

/// Send `Event(id, "clicked", null, now)` to the DBusMenu service. Used
/// when the user picks an entry in the rendered menu.
pub fn send_clicked(bus: String, sni_path: String, item_id: i32) {
    std::thread::spawn(move || {
        let rt = match Builder::new_current_thread().enable_all().build() {
            Ok(r) => r,
            Err(_) => return,
        };
        rt.block_on(async move {
            let menu_path = match read_menu_property(&bus, &sni_path).await {
                Some(p) => p,
                None => return,
            };
            let conn = match Connection::session().await {
                Ok(c) => c,
                Err(_) => return,
            };
            let bus_name = match BusName::try_from(bus) {
                Ok(b) => b,
                Err(_) => return,
            };
            let obj_path = match ObjectPath::try_from(menu_path.as_str()) {
                Ok(p) => p,
                Err(_) => return,
            };
            let proxy = match Proxy::new(&conn, bus_name, obj_path, "com.canonical.dbusmenu").await
            {
                Ok(p) => p,
                Err(_) => return,
            };
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as u32)
                .unwrap_or(0);
            let _ = proxy
                .call::<_, _, ()>("Event", &(item_id, "clicked", Value::U32(0), timestamp))
                .await;
        });
    });
}
