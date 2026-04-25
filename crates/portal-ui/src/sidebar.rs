//! Sidebar: bookmarks and standard places (Home, Desktop, Downloads, etc.).

use std::path::PathBuf;

/// A single sidebar entry (bookmark or standard place).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SidebarEntry {
    /// Display name.
    pub name: String,
    /// Target path.
    pub path: PathBuf,
    /// SF Symbol-style icon name hint (used by the theme).
    pub icon: &'static str,
}

/// All sidebar entries (places + user bookmarks).
pub fn entries() -> Vec<SidebarEntry> {
    let mut entries = standard_places();
    entries.extend(load_gtk_bookmarks());
    entries
}

fn standard_places() -> Vec<SidebarEntry> {
    let home = home_dir();
    let mut places = vec![SidebarEntry {
        name: "Home".into(),
        path: home.clone(),
        icon: "house",
    }];
    for (name, rel, icon) in [
        ("Desktop", "Desktop", "display"),
        ("Documents", "Documents", "doc"),
        ("Downloads", "Downloads", "arrow.down.circle"),
        ("Music", "Music", "music.note"),
        ("Pictures", "Pictures", "photo"),
        ("Videos", "Videos", "film"),
    ] {
        let p = home.join(rel);
        if p.exists() {
            places.push(SidebarEntry { name: name.into(), path: p, icon });
        }
    }
    places
}

fn load_gtk_bookmarks() -> Vec<SidebarEntry> {
    let bookmark_path = home_dir()
        .join(".config")
        .join("gtk-3.0")
        .join("bookmarks");
    let Ok(content) = std::fs::read_to_string(&bookmark_path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            // Format: "file:///path Optional Name"
            let mut parts = line.splitn(2, ' ');
            let uri = parts.next()?;
            let path = uri.strip_prefix("file://").map(PathBuf::from)?;
            let name = parts
                .next()
                .map(str::to_owned)
                .or_else(|| {
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .map(str::to_owned)
                })?;
            Some(SidebarEntry { name, path, icon: "bookmark" })
        })
        .collect()
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/root"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_places_always_has_home() {
        let places = standard_places();
        assert!(!places.is_empty());
        assert_eq!(places[0].name, "Home");
    }

    #[test]
    fn entries_returns_at_least_home() {
        let e = entries();
        assert!(!e.is_empty());
    }
}
