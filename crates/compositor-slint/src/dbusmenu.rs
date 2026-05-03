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
                if let Ok(owned) = v.try_clone().and_then(|v| OwnedValue::try_from(v)) {
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
