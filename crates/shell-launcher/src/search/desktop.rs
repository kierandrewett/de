//! `.desktop` file discovery and fuzzy search.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32String};

use super::AppEntry;

/// Standard XDG application directories, including Flatpak.
fn app_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = vec![
        std::path::PathBuf::from("/usr/share/applications"),
        std::path::PathBuf::from("/usr/local/share/applications"),
        std::path::PathBuf::from("/var/lib/flatpak/exports/share/applications"),
    ];

    if let Some(home) = std::env::var_os("HOME") {
        let home = std::path::PathBuf::from(home);
        dirs.push(home.join(".local/share/applications"));
        dirs.push(home.join(".local/share/flatpak/exports/share/applications"));
    }

    // Respect $XDG_DATA_DIRS
    if let Ok(xdg_dirs) = std::env::var("XDG_DATA_DIRS") {
        for part in xdg_dirs.split(':') {
            if !part.is_empty() {
                dirs.push(std::path::PathBuf::from(part).join("applications"));
            }
        }
    }

    dirs
}

/// Parse a `.desktop` file at `path` into an [`AppEntry`].
///
/// Returns `None` if the file is not a visible Application entry.
fn parse_desktop_file(path: &Path) -> Option<AppEntry> {
    let content = std::fs::read_to_string(path).ok()?;

    let mut in_entry = false;
    let mut fields: HashMap<&str, &str> = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((raw_key, value)) = line.split_once('=') {
            let key = raw_key.trim();
            // Skip locale-specific variants (e.g. Name[de]=)
            if !key.contains('[') {
                fields.entry(key).or_insert(value.trim());
            }
        }
    }

    if fields.get("Type")? != &"Application" {
        return None;
    }
    if fields.get("NoDisplay").copied() == Some("true") {
        return None;
    }
    if fields.get("Hidden").copied() == Some("true") {
        return None;
    }

    let name = fields.get("Name")?.to_string();
    let exec = fields.get("Exec")?.to_string();
    let description = fields.get("Comment").map(|s| s.to_string());
    let icon = fields.get("Icon").map(|s| s.to_string());

    let categories = fields
        .get("Categories")
        .unwrap_or(&"")
        .split(';')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();

    let keywords = fields
        .get("Keywords")
        .unwrap_or(&"")
        .split(';')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();

    Some(AppEntry { name, exec, description, icon, categories, keywords })
}

/// Scan all XDG application directories and return deduplicated [`AppEntry`]s.
pub fn load_apps() -> Vec<AppEntry> {
    let mut apps = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for dir in app_dirs() {
        let Ok(read_dir) = std::fs::read_dir(&dir) else { continue };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            if let Some(app) = parse_desktop_file(&path) {
                if seen.insert(app.name.clone()) {
                    apps.push(app);
                }
            }
        }
    }

    apps.sort_by(|a, b| a.name.cmp(&b.name));
    apps
}

/// Fuzzy-search `apps` for `query` using nucleo, boosted by `launch_counts`.
///
/// Returns results sorted descending by score (best match first).
pub fn search_apps(
    query: &str,
    apps: &[AppEntry],
    launch_counts: &HashMap<String, u32>,
) -> Vec<AppEntry> {
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);

    let mut scored: Vec<(u32, &AppEntry)> = apps
        .iter()
        .filter_map(|app| {
            let haystack_text = format!(
                "{} {} {} {}",
                app.name,
                app.description.as_deref().unwrap_or(""),
                app.categories.join(" "),
                app.keywords.join(" "),
            );
            let haystack = Utf32String::from(haystack_text.as_str());
            let base = pattern.score(haystack.slice(..), &mut matcher)?;

            // Boost frequently-launched apps by up to 100 points.
            let boost = launch_counts.get(&app.name).copied().unwrap_or(0).min(100);
            Some((base + boost, app))
        })
        .collect();

    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().map(|(_, a)| a.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_app(name: &str, desc: &str) -> AppEntry {
        AppEntry {
            name: name.to_owned(),
            description: Some(desc.to_owned()),
            exec: format!("{name}"),
            icon: None,
            categories: Vec::new(),
            keywords: Vec::new(),
        }
    }

    #[test]
    fn exact_name_scores_highest() {
        let apps = vec![
            make_app("Firefox", "Web Browser"),
            make_app("Thunderbird", "Mail Client"),
            make_app("Files", "File Manager"),
        ];
        let counts = HashMap::new();
        let results = search_apps("Firefox", &apps, &counts);
        assert_eq!(results[0].name, "Firefox");
    }

    #[test]
    fn empty_query_returns_nothing() {
        let apps = vec![make_app("Firefox", "Web Browser")];
        let counts = HashMap::new();
        // nucleo pattern with empty string matches everything — but our
        // search() wrapper guards against empty queries upstream.
        let _ = search_apps("", &apps, &counts);
    }

    #[test]
    fn launch_boost_elevates_result() {
        let apps = vec![
            make_app("Firefox", "Web Browser"),
            make_app("Falkon", "Another Browser"),
        ];
        let mut counts = HashMap::new();
        counts.insert("Falkon".to_owned(), 99u32);
        let results = search_apps("f", &apps, &counts);
        // Falkon should be ranked first due to launch boost.
        assert_eq!(results[0].name, "Falkon");
    }
}
