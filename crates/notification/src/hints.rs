//! Parsing of the `a{sv}` hints dictionary from D-Bus `Notify` calls.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use tracing::warn;
use zbus::zvariant::{OwnedValue, Value};

use crate::types::{NotificationImage, Urgency};

/// Fields extracted from the raw hints dictionary.
#[derive(Debug)]
pub(crate) struct ParsedHints {
    pub(crate) urgency: Urgency,
    pub(crate) image: Option<NotificationImage>,
    pub(crate) category: Option<String>,
    pub(crate) desktop_entry: Option<String>,
    pub(crate) transient: bool,
    pub(crate) resident: bool,
}

impl Default for ParsedHints {
    fn default() -> Self {
        Self {
            urgency: Urgency::Normal,
            image: None,
            category: None,
            desktop_entry: None,
            transient: false,
            resident: false,
        }
    }
}

/// Parses the `hints` dict and `expire_timeout` into typed fields.
///
/// Returns `(ParsedHints, expire_timeout)` where `expire_timeout` is `None`
/// for "server default / never" and `Some(d)` for an explicit positive value.
pub(crate) fn parse_hints(
    hints: &HashMap<String, OwnedValue>,
    expire_timeout: i32,
) -> (ParsedHints, Option<Duration>) {
    let mut out = ParsedHints::default();

    if let Some(u) = get_u8(hints, "urgency") {
        out.urgency = match u {
            0 => Urgency::Low,
            2 => Urgency::Critical,
            _ => Urgency::Normal,
        };
    }

    // `image-data` (or legacy `icon_data`) takes priority over `image-path`.
    if let Some(img) = hints
        .get("image-data")
        .or_else(|| hints.get("icon_data"))
        .and_then(parse_image_data)
    {
        out.image = Some(img);
    } else if let Some(path) =
        get_str(hints, "image-path").or_else(|| get_str(hints, "image_path"))
    {
        out.image = Some(NotificationImage::Path(PathBuf::from(path)));
    }

    out.category = get_str(hints, "category");
    out.desktop_entry = get_str(hints, "desktop-entry");
    out.transient = get_bool(hints, "transient").unwrap_or(false);
    out.resident = get_bool(hints, "resident").unwrap_or(false);

    let timeout = if expire_timeout > 0 {
        Some(Duration::from_millis(expire_timeout as u64))
    } else {
        None
    };

    (out, timeout)
}

// OwnedValue implements Deref<Target = Value<'static>>, so &**v : &Value<'static>.

fn get_str(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    match &**hints.get(key)? {
        Value::Str(s) => Some(s.as_str().to_owned()),
        _ => None,
    }
}

fn get_u8(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<u8> {
    match &**hints.get(key)? {
        Value::U8(b) => Some(*b),
        _ => None,
    }
}

fn get_bool(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    match &**hints.get(key)? {
        Value::Bool(b) => Some(*b),
        // Some senders encode booleans as u8 0/1.
        Value::U8(b) => Some(*b != 0),
        _ => None,
    }
}

/// Parses the `image-data` hint: `(iiibiiay)` → `NotificationImage::Data`.
///
/// Fields: width (i32), height (i32), rowstride (i32), has_alpha (bool),
/// bits_per_sample (i32), channels (i32), data (ay).
fn parse_image_data(value: &OwnedValue) -> Option<NotificationImage> {
    match &**value {
        Value::Structure(s) => {
            let fields = s.fields();
            if fields.len() < 7 {
                warn!(len = fields.len(), "image-data structure has too few fields");
                return None;
            }
            let width = match &fields[0] {
                Value::I32(v) => *v,
                _ => return None,
            };
            let height = match &fields[1] {
                Value::I32(v) => *v,
                _ => return None,
            };
            // fields[2] = rowstride, [3] = has_alpha, [4] = bpp, [5] = channels
            let pixels: Vec<u8> = match &fields[6] {
                Value::Array(arr) => arr
                    .inner()
                    .iter()
                    .filter_map(|v| match v {
                        Value::U8(b) => Some(*b),
                        _ => None,
                    })
                    .collect(),
                _ => return None,
            };
            Some(NotificationImage::Data { width, height, pixels })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use zbus::zvariant::{OwnedValue, Value};

    fn into_owned(v: Value<'static>) -> OwnedValue {
        OwnedValue::try_from(v).expect("valid value")
    }

    fn str_hint(s: &'static str) -> OwnedValue {
        into_owned(Value::new(s))
    }

    fn u8_hint(v: u8) -> OwnedValue {
        into_owned(Value::new(v))
    }

    fn bool_hint(v: bool) -> OwnedValue {
        into_owned(Value::new(v))
    }

    #[test]
    fn empty_hints_give_defaults() {
        let (hints, timeout) = parse_hints(&HashMap::new(), -1);
        assert_eq!(hints.urgency, Urgency::Normal);
        assert!(hints.image.is_none());
        assert!(hints.category.is_none());
        assert!(!hints.transient);
        assert!(!hints.resident);
        assert!(timeout.is_none());
    }

    #[test]
    fn urgency_low() {
        let mut h = HashMap::new();
        h.insert("urgency".into(), u8_hint(0));
        let (hints, _) = parse_hints(&h, -1);
        assert_eq!(hints.urgency, Urgency::Low);
    }

    #[test]
    fn urgency_critical() {
        let mut h = HashMap::new();
        h.insert("urgency".into(), u8_hint(2));
        let (hints, _) = parse_hints(&h, -1);
        assert_eq!(hints.urgency, Urgency::Critical);
    }

    #[test]
    fn image_path_hint() {
        let mut h = HashMap::new();
        h.insert("image-path".into(), str_hint("/usr/share/icons/test.png"));
        let (hints, _) = parse_hints(&h, -1);
        match hints.image {
            Some(NotificationImage::Path(p)) => {
                assert_eq!(p, PathBuf::from("/usr/share/icons/test.png"));
            }
            other => panic!("expected Path image, got {other:?}"),
        }
    }

    #[test]
    fn category_hint() {
        let mut h = HashMap::new();
        h.insert("category".into(), str_hint("email.arrived"));
        let (hints, _) = parse_hints(&h, -1);
        assert_eq!(hints.category.as_deref(), Some("email.arrived"));
    }

    #[test]
    fn desktop_entry_hint() {
        let mut h = HashMap::new();
        h.insert("desktop-entry".into(), str_hint("org.gnome.Geary"));
        let (hints, _) = parse_hints(&h, -1);
        assert_eq!(hints.desktop_entry.as_deref(), Some("org.gnome.Geary"));
    }

    #[test]
    fn transient_bool_hint() {
        let mut h = HashMap::new();
        h.insert("transient".into(), bool_hint(true));
        let (hints, _) = parse_hints(&h, -1);
        assert!(hints.transient);
    }

    #[test]
    fn resident_bool_hint() {
        let mut h = HashMap::new();
        h.insert("resident".into(), bool_hint(true));
        let (hints, _) = parse_hints(&h, -1);
        assert!(hints.resident);
    }

    #[test]
    fn positive_expire_timeout_converted() {
        let (_, timeout) = parse_hints(&HashMap::new(), 5000);
        assert_eq!(timeout, Some(Duration::from_millis(5000)));
    }

    #[test]
    fn zero_expire_timeout_gives_none() {
        let (_, timeout) = parse_hints(&HashMap::new(), 0);
        assert!(timeout.is_none());
    }

    #[test]
    fn negative_expire_timeout_gives_none() {
        let (_, timeout) = parse_hints(&HashMap::new(), -1);
        assert!(timeout.is_none());
    }
}
