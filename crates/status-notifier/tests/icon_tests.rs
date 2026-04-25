use status_notifier::{TrayIconPixmap, best_pixmap};

#[test]
fn argb_to_rgba_single_pixel() {
    // Network-order ARGB [A=0xAA, R=0xRR, G=0xGG, B=0xBB]
    let argb = vec![0xAA_u8, 0x11, 0x22, 0x33];
    // Expect RGBA: [R, G, B, A]
    let rgba = status_notifier::argb_network_to_rgba(&argb);
    assert_eq!(rgba, vec![0x11_u8, 0x22, 0x33, 0xAA]);
}

#[test]
fn argb_to_rgba_multiple_pixels() {
    let argb: Vec<u8> = vec![
        0xFF, 0x00, 0xFF, 0x00, // A=255, R=0, G=255, B=0 → RGBA [0,255,0,255]
        0x80, 0xFF, 0x00, 0x00, // A=128, R=255, G=0, B=0 → RGBA [255,0,0,128]
    ];
    let rgba = status_notifier::argb_network_to_rgba(&argb);
    assert_eq!(rgba, vec![0x00, 0xFF, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x80]);
}

#[test]
fn argb_to_rgba_empty_input() {
    let rgba = status_notifier::argb_network_to_rgba(&[]);
    assert!(rgba.is_empty());
}

#[test]
fn argb_to_rgba_ignores_partial_trailing_bytes() {
    // 5 bytes: one full pixel (4) + 1 leftover — leftover must be ignored
    let argb = vec![0xAA_u8, 0x11, 0x22, 0x33, 0xFF];
    let rgba = status_notifier::argb_network_to_rgba(&argb);
    assert_eq!(rgba.len(), 4);
}

#[test]
fn best_pixmap_picks_closest_size() {
    let pixmaps = vec![
        TrayIconPixmap { width: 16, height: 16, argb_data: vec![] },
        TrayIconPixmap { width: 48, height: 48, argb_data: vec![] },
        TrayIconPixmap { width: 128, height: 128, argb_data: vec![] },
    ];
    // Asking for 40 → |16-40|=24, |48-40|=8, |128-40|=88 → unambiguously 48
    let picked = best_pixmap(&pixmaps, 40).expect("should find a pixmap");
    assert_eq!(picked.width, 48);
}

#[test]
fn best_pixmap_exact_match() {
    let pixmaps = vec![
        TrayIconPixmap { width: 16, height: 16, argb_data: vec![] },
        TrayIconPixmap { width: 32, height: 32, argb_data: vec![] },
    ];
    let picked = best_pixmap(&pixmaps, 32).expect("should find a pixmap");
    assert_eq!(picked.width, 32);
}

#[test]
fn best_pixmap_empty_list() {
    assert!(best_pixmap(&[], 48).is_none());
}
