//! Icon utilities: ARGB→RGBA conversion and freedesktop icon lookup.

use std::path::PathBuf;

use crate::types::{TrayIcon, TrayIconPixmap};

/// Convert network-byte-order ARGB pixels (as transmitted by SNI) to RGBA.
///
/// The SNI spec transmits each 32-bit pixel as `[A, R, G, B]` in network
/// (big-endian) order.  We reorder to `[R, G, B, A]` for standard RGBA use.
pub fn argb_network_to_rgba(argb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(argb.len());
    for px in argb.chunks_exact(4) {
        out.push(px[1]); // R
        out.push(px[2]); // G
        out.push(px[3]); // B
        out.push(px[0]); // A
    }
    out
}

/// Look up a freedesktop icon by name and return its path if found.
///
/// Tries the current icon theme first; falls back to hicolor.
pub fn lookup_icon(name: &str, size: u16) -> Option<PathBuf> {
    freedesktop_icons::lookup(name).with_size(size).find()
}

/// Pick the pixmap closest to the requested `size` from a list.
pub fn best_pixmap(pixmaps: &[TrayIconPixmap], size: u16) -> Option<&TrayIconPixmap> {
    let target = size as i32;
    pixmaps.iter().min_by_key(|p| (p.width - target).unsigned_abs())
}

/// Convert raw SNI pixmap data `(width, height, argb_bytes)` into [`TrayIconPixmap`].
pub fn decode_pixmap(width: i32, height: i32, argb: Vec<u8>) -> TrayIconPixmap {
    TrayIconPixmap { width, height, argb_data: argb_network_to_rgba(&argb) }
}

/// Build a [`TrayIcon`] from a name/pixmap pair as returned by item properties.
///
/// Prefers the named icon when the name is non-empty; falls back to pixmaps.
pub fn resolve_icon(
    name: Option<&str>,
    pixmaps: Vec<TrayIconPixmap>,
) -> Option<TrayIcon> {
    match name {
        Some(n) if !n.is_empty() => Some(TrayIcon::Named(n.to_string())),
        _ if !pixmaps.is_empty() => Some(TrayIcon::Pixmap(pixmaps)),
        _ => None,
    }
}
