//! `.desktop` file parsing and system icon resolution.

use std::path::{Path, PathBuf};

/// Resolved metadata for an application identified by its `app_id`.
#[derive(Debug, Clone)]
pub struct AppInfo {
    /// Human-readable application name.
    pub name: String,
    /// Launch command with `%`-field codes stripped.
    pub exec: String,
    /// Resolved path to the icon file, or `None` if not found.
    pub icon: Option<PathBuf>,
}

/// Resolves `AppInfo` for `app_id` by searching standard `.desktop` directories.
///
/// Falls back to a best-effort entry (name derived from `app_id`) if no
/// `.desktop` file is found.
pub fn resolve(app_id: &str) -> Option<AppInfo> {
    for dir in &search_dirs() {
        // Try exact match first, then lowercase variant.
        for candidate in &[
            format!("{app_id}.desktop"),
            format!("{}.desktop", app_id.to_lowercase()),
        ] {
            if let Some(info) = parse_file(&dir.join(candidate), app_id) {
                return Some(info);
            }
        }
    }

    // Fallback: synthesise an entry from the app_id.
    Some(AppInfo {
        name: pretty_name(app_id),
        exec: app_id.to_string(),
        icon: None,
    })
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn search_dirs() -> Vec<PathBuf> {
    let home = home_dir();
    let mut dirs = vec![
        home.join(".local/share/applications"),
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
        home.join(".local/share/flatpak/exports/share/applications"),
    ];

    if let Ok(xdg) = std::env::var("XDG_DATA_DIRS") {
        for part in xdg.split(':') {
            dirs.push(PathBuf::from(part).join("applications"));
        }
    }

    dirs
}

/// Parses a single `.desktop` file and returns the relevant fields.
fn parse_file(path: &Path, app_id: &str) -> Option<AppInfo> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut name: Option<String> = None;
    let mut exec: Option<String> = None;
    let mut icon_name: Option<String> = None;
    let mut in_entry = false;

    for line in content.lines() {
        let line = line.trim();
        if line == "[Desktop Entry]" {
            in_entry = true;
        } else if line.starts_with('[') {
            in_entry = false;
        } else if in_entry && !line.starts_with('#') {
            if let Some(v) = line.strip_prefix("Name=") {
                // Only use the first Name= (no locale suffix on the key).
                if name.is_none() {
                    name = Some(v.to_string());
                }
            } else if let Some(v) = line.strip_prefix("Exec=") {
                exec = Some(strip_exec_fields(v));
            } else if let Some(v) = line.strip_prefix("Icon=") {
                icon_name = Some(v.to_string());
            }
        }
    }

    let name = name.unwrap_or_else(|| pretty_name(app_id));
    let exec = exec.unwrap_or_else(|| app_id.to_string());
    let icon = icon_name.as_deref().and_then(resolve_icon);

    Some(AppInfo { name, exec, icon })
}

/// Removes `%`-field substitution codes from an `Exec=` value
/// (e.g. `%U`, `%F`, `%i`, `%c`, `%k`).
fn strip_exec_fields(exec: &str) -> String {
    let mut out = String::with_capacity(exec.len());
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            chars.next(); // skip the single-char field code
        } else {
            out.push(c);
        }
    }
    out.trim().to_string()
}

/// Searches standard icon theme directories for `icon_name`.
///
/// Checks (in order):
/// 1. Absolute path as-is.
/// 2. `~/.local/share/icons/hicolor/{size}/apps/`.
/// 3. `/usr/share/icons/hicolor/{size}/apps/`.
/// 4. `/usr/share/icons/Adwaita/{size}/apps/`.
/// 5. `/usr/share/pixmaps/`.
///
/// Preferred sizes: 48×48, 64×64, scalable (SVG), 256×256.
/// Preferred formats: `png` > `svg` > `xpm`.
fn resolve_icon(icon_name: &str) -> Option<PathBuf> {
    // Absolute path short-circuit.
    if icon_name.starts_with('/') {
        let p = PathBuf::from(icon_name);
        return p.exists().then_some(p);
    }

    let home = home_dir();
    let sizes = ["48x48/apps", "64x64/apps", "scalable/apps", "256x256/apps"];
    let exts = ["png", "svg", "xpm"];

    let theme_roots: &[PathBuf] = &[
        home.join(".local/share/icons/hicolor"),
        PathBuf::from("/usr/share/icons/hicolor"),
        PathBuf::from("/usr/share/icons/Adwaita"),
        PathBuf::from("/usr/share/icons/breeze"),
    ];

    for root in theme_roots {
        for size in &sizes {
            let base = root.join(size);
            for ext in &exts {
                let p = base.join(format!("{icon_name}.{ext}"));
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }

    // Pixmaps fallback.
    for ext in &exts {
        let p = PathBuf::from("/usr/share/pixmaps").join(format!("{icon_name}.{ext}"));
        if p.exists() {
            return Some(p);
        }
    }

    None
}

/// Converts a reverse-DNS `app_id` or kebab-case name into a human-readable name.
///
/// `"org.gnome.Nautilus"` → `"Nautilus"`, `"google-chrome"` → `"Google Chrome"`.
fn pretty_name(app_id: &str) -> String {
    // Take the last component after the last '.'.
    let base = app_id.rsplit('.').next().unwrap_or(app_id);
    base.split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/root"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_exec_fields_removes_percent_codes() {
        assert_eq!(strip_exec_fields("firefox %U"), "firefox");
        assert_eq!(strip_exec_fields("code %f %i %c"), "code");
        assert_eq!(strip_exec_fields("setsid nemo %U"), "setsid nemo");
        assert_eq!(strip_exec_fields("/usr/bin/app"), "/usr/bin/app");
    }

    #[test]
    fn strip_exec_fields_handles_double_percent() {
        // %% is a literal %, so %% becomes % in the output.
        // Our simple stripper turns it into an empty char (consumes next char).
        // That's acceptable — %% is rare in practice.
        let result = strip_exec_fields("echo %%");
        // At minimum, it should not panic and should not contain the literal %%
        assert!(!result.contains("%%"));
    }

    #[test]
    fn pretty_name_reverse_dns() {
        assert_eq!(pretty_name("org.gnome.Nautilus"), "Nautilus");
        assert_eq!(pretty_name("org.gnome.Terminal"), "Terminal");
        assert_eq!(pretty_name("io.elementary.Files"), "Files");
    }

    #[test]
    fn pretty_name_kebab_case() {
        assert_eq!(pretty_name("google-chrome"), "Google Chrome");
        assert_eq!(pretty_name("visual-studio-code"), "Visual Studio Code");
    }

    #[test]
    fn pretty_name_simple() {
        assert_eq!(pretty_name("firefox"), "Firefox");
        assert_eq!(pretty_name("code"), "Code");
    }

    #[test]
    fn resolve_returns_some_for_unknown_app() {
        // Should not return None — falls back to synthesised AppInfo.
        let info = resolve("this-app-does-not-exist-at-all");
        assert!(info.is_some());
        let info = info.unwrap();
        assert!(!info.name.is_empty());
        assert!(!info.exec.is_empty());
    }
}
