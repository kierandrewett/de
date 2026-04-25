//! Icon theme directory discovery.

use std::path::PathBuf;

/// Search standard icon paths for a theme directory that contains cursor data.
///
/// Returns the theme's root directory (the directory that contains `index.theme`,
/// `cursors/`, and/or `cursors_scalable/`), or `None` if no match is found.
///
/// Search order:
/// 1. Each path in `$XCURSOR_PATH` (colon-separated)
/// 2. `~/.local/share/icons`
/// 3. `~/.icons`
/// 4. `/usr/share/icons`
/// 5. `/usr/local/share/icons`
pub fn find_theme(name: &str) -> Option<PathBuf> {
    for base in search_paths() {
        let candidate = base.join(name);
        if candidate.join("cursors_scalable").is_dir() || candidate.join("cursors").is_dir() {
            return Some(candidate);
        }
    }
    None
}

/// Return the ordered list of base directories to search for icon themes.
fn search_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();

    if let Ok(xcursor_path) = std::env::var("XCURSOR_PATH") {
        for segment in xcursor_path.split(':') {
            if !segment.is_empty() {
                paths.push(PathBuf::from(segment));
            }
        }
    }

    if let Some(home) = home_dir() {
        paths.push(home.join(".local/share/icons"));
        paths.push(home.join(".icons"));
    }

    paths.push(PathBuf::from("/usr/share/icons"));
    paths.push(PathBuf::from("/usr/local/share/icons"));

    paths
}

/// Portable home-directory lookup (avoids the deprecated `std::env::home_dir`).
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_paths_includes_system() {
        let paths = search_paths();
        assert!(paths.contains(&PathBuf::from("/usr/share/icons")));
    }

    #[test]
    fn test_find_nonexistent_theme_returns_none() {
        assert!(find_theme("__definitely_does_not_exist_xyz__").is_none());
    }
}
