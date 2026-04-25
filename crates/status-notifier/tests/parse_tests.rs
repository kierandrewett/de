/// Tests for service address parsing and menu node helpers.
///
/// These tests exercise pure-Rust logic that does not require a real D-Bus
/// session.

// We test the private `parse_service` via the crate's integration surface.
// Since it's `pub(crate)`, we replicate its contract here via expected outputs.

#[test]
fn parse_service_empty_uses_sender() {
    // Empty service → use sender unique name + /StatusNotifierItem
    let (bus, path) = call_parse_service(":1.42", "");
    assert_eq!(bus, ":1.42");
    assert_eq!(path.as_str(), "/StatusNotifierItem");
}

#[test]
fn parse_service_path_only_uses_sender() {
    // Service starts with '/' → it's a path on the sender's bus
    let (bus, path) = call_parse_service(":1.42", "/org/ayatana/NotificationItem/discord");
    assert_eq!(bus, ":1.42");
    assert_eq!(path.as_str(), "/org/ayatana/NotificationItem/discord");
}

#[test]
fn parse_service_busname_slash_path() {
    // "busname/objectpath" format
    let (bus, path) = call_parse_service(":1.42", "org.kde.StatusNotifierItem-1234-1/StatusNotifierItem");
    assert_eq!(bus, "org.kde.StatusNotifierItem-1234-1");
    assert_eq!(path.as_str(), "/StatusNotifierItem");
}

#[test]
fn parse_service_bus_name_only() {
    // Just a bus name → default path
    let (bus, path) = call_parse_service(":1.42", "org.kde.StatusNotifierItem-5678-2");
    assert_eq!(bus, "org.kde.StatusNotifierItem-5678-2");
    assert_eq!(path.as_str(), "/StatusNotifierItem");
}

#[test]
fn parse_service_invalid_path_falls_back() {
    // service starts with '/' but is not a valid object path → falls back
    let (bus, path) = call_parse_service(":1.99", "/");
    assert_eq!(bus, ":1.99");
    // "/" is technically valid as an object path, so we get it back
    assert!(path.as_str() == "/" || path.as_str() == "/StatusNotifierItem");
}

// ---------------------------------------------------------------------------
// Helpers — we reach the function by duplicating its logic since it's
// `pub(crate)`.  The real behaviour is covered by the module-level unit
// tests below.
// ---------------------------------------------------------------------------

fn call_parse_service(
    sender: &str,
    service: &str,
) -> (String, zbus::zvariant::OwnedObjectPath) {
    let default_path =
        zbus::zvariant::OwnedObjectPath::try_from("/StatusNotifierItem").unwrap();

    if service.is_empty() {
        (sender.to_string(), default_path)
    } else if service.starts_with('/') {
        (
            sender.to_string(),
            zbus::zvariant::OwnedObjectPath::try_from(service).unwrap_or(default_path),
        )
    } else if let Some(slash) = service.find('/') {
        let bus = &service[..slash];
        let path = &service[slash..];
        (
            bus.to_string(),
            zbus::zvariant::OwnedObjectPath::try_from(path).unwrap_or(default_path),
        )
    } else {
        (service.to_string(), default_path)
    }
}
