//! Search source aggregation and shared types.

pub mod calculator;
pub mod desktop;
pub mod recent;
pub mod system;
pub mod web;

use std::collections::HashMap;
use std::path::PathBuf;

/// A parsed `.desktop` application entry.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AppEntry {
    /// Display name (e.g. "Firefox Web Browser").
    pub name: String,
    /// Short description / comment.
    pub description: Option<String>,
    /// Raw `Exec` field from the .desktop file.
    pub exec: String,
    /// Icon theme name or absolute path.
    pub icon: Option<String>,
    /// Semicolon-delimited categories.
    pub categories: Vec<String>,
    /// Search keywords.
    pub keywords: Vec<String>,
}

/// A file from `~/.local/share/recently-used.xbel`.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct RecentFile {
    /// File basename.
    pub name: String,
    /// Absolute path on disk.
    pub path: PathBuf,
    /// MIME type if available.
    pub mime_type: Option<String>,
}

/// System power/session commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemAction {
    Shutdown,
    Restart,
    Lock,
    Sleep,
    Logout,
}

impl SystemAction {
    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Shutdown => "Shut Down",
            Self::Restart => "Restart",
            Self::Lock => "Lock Screen",
            Self::Sleep => "Sleep",
            Self::Logout => "Log Out",
        }
    }

    /// Icon theme name for this action.
    #[allow(dead_code)]
    pub fn icon_name(self) -> &'static str {
        match self {
            Self::Shutdown => "system-shutdown-symbolic",
            Self::Restart => "system-reboot-symbolic",
            Self::Lock => "system-lock-screen-symbolic",
            Self::Sleep => "system-suspend-symbolic",
            Self::Logout => "system-log-out-symbolic",
        }
    }
}

/// A single ranked search result.
#[derive(Debug, Clone)]
pub enum SearchResult {
    /// Installed desktop application.
    Application(AppEntry),
    /// Inline calculator result.
    Calculator {
        /// The original expression string.
        expression: String,
        /// Evaluated numeric result.
        result: f64,
    },
    /// Recently-opened file.
    RecentFile(RecentFile),
    /// System power/session action.
    SystemAction(SystemAction),
    /// Web search fallback.
    WebSearch(String),
}

impl SearchResult {
    /// Category header shown in the results list.
    pub fn category(&self) -> &'static str {
        match self {
            Self::Application(_) => "Applications",
            Self::Calculator { .. } => "Calculator",
            Self::RecentFile(_) => "Recent Files",
            Self::SystemAction(_) => "System",
            Self::WebSearch(_) => "Web",
        }
    }

    /// Primary display line for the result.
    pub fn display_name(&self) -> String {
        match self {
            Self::Application(app) => app.name.clone(),
            Self::Calculator { result, .. } => {
                // Format without trailing zeros for integers.
                if result.fract() == 0.0 && result.abs() < 1e15 {
                    format!("= {}", *result as i64)
                } else {
                    format!("= {result}")
                }
            }
            Self::RecentFile(f) => f.name.clone(),
            Self::SystemAction(cmd) => cmd.label().to_owned(),
            Self::WebSearch(q) => format!("Search the web for \"{q}\""),
        }
    }

    /// Optional secondary line (description / path / expression).
    pub fn subtitle(&self) -> Option<String> {
        match self {
            Self::Application(app) => app.description.clone(),
            Self::Calculator { expression, .. } => Some(expression.clone()),
            Self::RecentFile(f) => Some(f.path.display().to_string()),
            Self::SystemAction(_) | Self::WebSearch(_) => None,
        }
    }
}

/// Aggregate results from all search sources for the given `query`.
///
/// Results are ordered: Calculator → System → Applications → Recent → Web.
/// Maximum 8 results total.
pub fn search(
    query: &str,
    apps: &[AppEntry],
    recent: &[RecentFile],
    launch_counts: &HashMap<String, u32>,
) -> Vec<SearchResult> {
    if query.trim().is_empty() {
        return Vec::new();
    }

    let mut results: Vec<SearchResult> = Vec::new();

    // 1. Calculator (single top result if expression detected).
    if let Some(val) = calculator::evaluate(query) {
        results.push(SearchResult::Calculator {
            expression: query.to_owned(),
            result: val,
        });
    }

    // 2. System commands.
    for action in system::match_commands(query) {
        results.push(SearchResult::SystemAction(action));
    }

    // 3. Applications (fuzzy + launch-count boost, up to 6).
    let mut app_hits = desktop::search_apps(query, apps, launch_counts);
    app_hits.truncate(6);
    results.extend(app_hits.into_iter().map(SearchResult::Application));

    // 4. Recent files (up to 3).
    let file_hits = recent::search_recent_owned(query, recent);
    results.extend(file_hits.into_iter().take(3).map(SearchResult::RecentFile));

    // 5. Web fallback — always appended last.
    results.push(SearchResult::WebSearch(query.to_owned()));

    results.truncate(8);
    results
}
