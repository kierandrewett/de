//! D-Bus proxy for `com.canonical.dbusmenu` and menu tree parsing.

use std::collections::HashMap;

use zbus::{Connection, proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::{
    error::Result,
    icon::resolve_icon,
    types::{DbusMenu, MenuItem, ToggleType, TrayIconPixmap},
};

/// Raw menu node layout: `(id, properties, children)`.
///
/// Children are `OwnedValue` wrapping the same tuple recursively.
type RawNode = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

#[proxy(
    interface = "com.canonical.dbusmenu",
    default_path = "/com/canonical/dbusmenu"
)]
trait DbusMenuInterface {
    /// Fetch the full menu layout.
    ///
    /// Pass `parent_id = 0`, `recursion_depth = -1`, `property_names = []`
    /// to get the complete tree.
    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: &[&str],
    ) -> zbus::Result<(u32, RawNode)>;

    /// Send an activation event for a menu item (e.g. a click).
    fn event(
        &self,
        id: i32,
        event_id: &str,
        data: &Value<'_>,
        timestamp: u32,
    ) -> zbus::Result<()>;
}

/// Fetch and parse the full context menu from a `com.canonical.dbusmenu` object.
pub(crate) async fn fetch_menu(
    conn: &Connection,
    bus_name: &str,
    menu_path: &OwnedObjectPath,
) -> Result<DbusMenu> {
    let proxy = DbusMenuInterfaceProxy::builder(conn)
        .destination(bus_name.to_string())?
        .path(menu_path.clone())?
        .build()
        .await?;

    let (revision, root_node) = proxy.get_layout(0, -1, &[]).await?;

    // The root node's children are the top-level items.
    let items = parse_children(&root_node.2);

    Ok(DbusMenu { revision, items })
}

/// Send an `activated` event to a dbusmenu item.
pub(crate) async fn send_menu_event(
    conn: &Connection,
    bus_name: &str,
    menu_path: &OwnedObjectPath,
    menu_item_id: i32,
) -> Result<()> {
    let proxy = DbusMenuInterfaceProxy::builder(conn)
        .destination(bus_name.to_string())?
        .path(menu_path.clone())?
        .build()
        .await?;

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as u32;

    proxy
        .event(menu_item_id, "clicked", &Value::from(0i32), timestamp)
        .await?;

    Ok(())
}

/// Recursively parse an array of child `OwnedValue`s into `MenuItem`s.
fn parse_children(children: &[OwnedValue]) -> Vec<MenuItem> {
    children.iter().filter_map(parse_node).collect()
}

/// Parse a single menu node variant into a `MenuItem`.
fn parse_node(value: &OwnedValue) -> Option<MenuItem> {
    // Each node is a variant wrapping `(i32, a{sv}, av)`.
    let inner: &Value<'_> = value.deref_inner();

    // Unwrap one level of variant if needed.
    let structure = match inner {
        Value::Structure(s) => s,
        Value::Value(boxed) => match boxed.deref_inner() {
            Value::Structure(s) => s,
            _ => return None,
        },
        _ => return None,
    };

    let fields = structure.fields();
    if fields.len() < 3 {
        return None;
    }

    let id = match &fields[0] {
        Value::I32(i) => *i,
        _ => return None,
    };

    let props: HashMap<String, OwnedValue> = match &fields[1] {
        Value::Dict(d) => {
            let mut map = HashMap::new();
            for (k, v) in d.iter() {
                if let (Value::Str(key), val) = (k, v) {
                    map.insert(key.to_string(), OwnedValue::try_from(val.clone()).ok()?);
                }
            }
            map
        }
        _ => HashMap::new(),
    };

    let children_raw: Vec<OwnedValue> = match &fields[2] {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| OwnedValue::try_from(v.clone()).ok())
            .collect(),
        _ => vec![],
    };

    let is_separator = prop_str(&props, "type").as_deref() == Some("separator");
    let label = prop_str(&props, "label").unwrap_or_default();
    let enabled = prop_bool(&props, "enabled").unwrap_or(true);
    let visible = prop_bool(&props, "visible").unwrap_or(true);
    let toggle_type = prop_str(&props, "toggle-type").and_then(|t| match t.as_str() {
        "checkmark" => Some(ToggleType::Checkmark),
        "radio" => Some(ToggleType::Radio),
        _ => None,
    });
    let toggle_state = prop_i32(&props, "toggle-state").map(|s| s == 1);

    // Icon
    let icon_name = prop_str(&props, "icon-name");
    let icon_pixmaps = prop_icon_data(&props);
    let icon = resolve_icon(icon_name.as_deref(), icon_pixmaps);

    let children = parse_children(&children_raw);

    Some(MenuItem {
        id,
        label,
        enabled,
        visible,
        icon,
        children,
        is_separator,
        toggle_type,
        toggle_state,
    })
}

// --- property helpers -------------------------------------------------------

fn prop_str(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    match props.get(key)?.deref_inner() {
        Value::Str(s) => Some(s.to_string()),
        _ => None,
    }
}

fn prop_bool(props: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    match props.get(key)?.deref_inner() {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

fn prop_i32(props: &HashMap<String, OwnedValue>, key: &str) -> Option<i32> {
    match props.get(key)?.deref_inner() {
        Value::I32(i) => Some(*i),
        _ => None,
    }
}

fn prop_icon_data(props: &HashMap<String, OwnedValue>) -> Vec<TrayIconPixmap> {
    let val = match props.get("icon-data") {
        Some(v) => v,
        None => return vec![],
    };

    // icon-data is `ay` (array of bytes) — raw PNG data, not a pixmap array.
    // We store it as a single-element pixmap with width=0 as a signal that it's
    // PNG-encoded and must be decoded by the renderer.
    if let Value::Array(arr) = val.deref_inner() {
        let bytes: Vec<u8> = arr
            .iter()
            .filter_map(|v| match v {
                Value::U8(b) => Some(*b),
                _ => None,
            })
            .collect();
        if !bytes.is_empty() {
            return vec![TrayIconPixmap { width: 0, height: 0, argb_data: bytes }];
        }
    }
    vec![]
}

/// Helper to reach through a single layer of `Value::Value` wrapping.
trait DerefInner {
    fn deref_inner(&self) -> &Value<'_>;
}

impl DerefInner for OwnedValue {
    fn deref_inner(&self) -> &Value<'_> {
        let v: &Value<'_> = self;
        match v {
            Value::Value(inner) => inner.deref_inner(),
            other => other,
        }
    }
}

impl<'a> DerefInner for Value<'a> {
    fn deref_inner(&self) -> &Value<'_> {
        match self {
            Value::Value(inner) => inner.deref_inner(),
            other => other,
        }
    }
}
