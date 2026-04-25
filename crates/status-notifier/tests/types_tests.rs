use status_notifier::{DbusMenu, ItemStatus, MenuItem, TrayEvent, TrayIcon, TrayIconPixmap,
                     UpdatedProperty};

// ItemStatus parsing

#[test]
fn item_status_from_str_active() {
    assert_eq!("Active".parse::<ItemStatus>(), Ok(ItemStatus::Active));
}

#[test]
fn item_status_from_str_needs_attention() {
    assert_eq!(
        "NeedsAttention".parse::<ItemStatus>(),
        Ok(ItemStatus::NeedsAttention)
    );
}

#[test]
fn item_status_from_str_passive() {
    assert_eq!("Passive".parse::<ItemStatus>(), Ok(ItemStatus::Passive));
}

#[test]
fn item_status_from_str_unknown_falls_back_to_passive() {
    // Unknown strings should default to Passive, not error.
    assert_eq!("Something".parse::<ItemStatus>(), Ok(ItemStatus::Passive));
}

#[test]
fn item_status_default_is_passive() {
    assert_eq!(ItemStatus::default(), ItemStatus::Passive);
}

// TrayIcon variants

#[test]
fn tray_icon_named_roundtrip() {
    let icon = TrayIcon::Named("firefox".to_string());
    match &icon {
        TrayIcon::Named(n) => assert_eq!(n, "firefox"),
        _ => panic!("expected Named"),
    }
}

#[test]
fn tray_icon_pixmap_stores_data() {
    let px = TrayIconPixmap { width: 32, height: 32, argb_data: vec![0xFF; 32 * 32 * 4] };
    let icon = TrayIcon::Pixmap(vec![px]);
    match &icon {
        TrayIcon::Pixmap(ps) => {
            assert_eq!(ps.len(), 1);
            assert_eq!(ps[0].width, 32);
        }
        _ => panic!("expected Pixmap"),
    }
}

// DbusMenu / MenuItem structure

#[test]
fn dbumenu_empty_items() {
    let menu = DbusMenu { revision: 1, items: vec![] };
    assert_eq!(menu.revision, 1);
    assert!(menu.items.is_empty());
}

#[test]
fn menu_item_separator_flag() {
    let sep = MenuItem {
        id: 0,
        label: String::new(),
        enabled: false,
        visible: true,
        icon: None,
        children: vec![],
        is_separator: true,
        toggle_type: None,
        toggle_state: None,
    };
    assert!(sep.is_separator);
}

#[test]
fn menu_item_nested_children() {
    let child = MenuItem {
        id: 2,
        label: "Child".to_string(),
        enabled: true,
        visible: true,
        icon: None,
        children: vec![],
        is_separator: false,
        toggle_type: None,
        toggle_state: None,
    };
    let parent = MenuItem {
        id: 1,
        label: "Parent".to_string(),
        enabled: true,
        visible: true,
        icon: None,
        children: vec![child],
        is_separator: false,
        toggle_type: None,
        toggle_state: None,
    };
    assert_eq!(parent.children.len(), 1);
    assert_eq!(parent.children[0].label, "Child");
}

// TrayEvent variants are Debug-printable

#[test]
fn tray_event_registered_debug() {
    let ev = TrayEvent::ItemRegistered(":1.42/StatusNotifierItem".to_string());
    let s = format!("{ev:?}");
    assert!(s.contains("ItemRegistered"));
}

#[test]
fn tray_event_updated_debug() {
    let ev = TrayEvent::ItemUpdated {
        id: "key".to_string(),
        property: UpdatedProperty::Title,
    };
    let s = format!("{ev:?}");
    assert!(s.contains("ItemUpdated"));
    assert!(s.contains("Title"));
}
