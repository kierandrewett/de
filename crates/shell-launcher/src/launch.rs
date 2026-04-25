//! Application and system-action launchers.

use std::process::{Command, Stdio};

use crate::search::{SearchResult, SystemAction};

/// Launch the given search result.
///
/// For `Application` results the `.desktop` `Exec` field is expanded and the
/// process is spawned in a new session so it outlives the launcher.
/// For `SystemAction` the appropriate system command is executed.
/// For `WebSearch` `xdg-open` is called with a Google search URL.
pub fn launch(result: &SearchResult) {
    match result {
        SearchResult::Application(app) => launch_exec(&app.exec),
        SearchResult::SystemAction(action) => launch_system_action(*action),
        SearchResult::WebSearch(query) => crate::search::web::open_web_search(query),
        SearchResult::RecentFile(f) => open_path(&f.path),
        SearchResult::Calculator { .. } => {
            // Nothing to launch — the result was already shown to the user.
        }
    }
}

/// Expand and execute a `.desktop` `Exec` field.
fn launch_exec(exec: &str) {
    let cleaned = strip_exec_placeholders(exec);
    tracing::info!("launching: {cleaned}");

    let result = Command::new("sh")
        .arg("-c")
        // setsid detaches the process from the launcher's session.
        .arg(format!("setsid {cleaned}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    if let Err(e) = result {
        tracing::warn!("failed to launch '{cleaned}': {e}");
    }
}

/// Strip field codes from a `.desktop` Exec value.
///
/// Handles the full set of substitutions defined in the Desktop Entry spec:
/// `%f`, `%F`, `%u`, `%U` (file/URL arguments) — replaced with empty string
/// since we launch without a file target.
/// `%i`, `%c`, `%k` — icon, name, desktop file path — also removed.
fn strip_exec_placeholders(exec: &str) -> String {
    let mut out = String::with_capacity(exec.len());
    let mut chars = exec.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '%' {
            if let Some(&next) = chars.peek() {
                chars.next();
                match next {
                    // Skip these placeholders entirely.
                    'f' | 'F' | 'u' | 'U' | 'i' | 'c' | 'k' => {}
                    // Literal percent sign.
                    '%' => out.push('%'),
                    // Unknown code: keep as-is.
                    other => {
                        out.push('%');
                        out.push(other);
                    }
                }
            } else {
                out.push('%');
            }
        } else {
            out.push(ch);
        }
    }

    out.trim().to_owned()
}

/// Open a file with `xdg-open`.
fn open_path(path: &std::path::Path) {
    tracing::info!("opening file: {}", path.display());
    let _ = Command::new("xdg-open")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Execute a power/session action.
fn launch_system_action(action: SystemAction) {
    let cmd: &[&str] = match action {
        SystemAction::Shutdown => &["systemctl", "poweroff"],
        SystemAction::Restart => &["systemctl", "reboot"],
        SystemAction::Sleep => &["systemctl", "suspend"],
        SystemAction::Lock => &["loginctl", "lock-session"],
        SystemAction::Logout => &["loginctl", "terminate-session", "self"],
    };

    tracing::info!("system action: {cmd:?}");
    let result = Command::new(cmd[0])
        .args(&cmd[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    if let Err(e) = result {
        tracing::warn!("system action failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_file_placeholder() {
        assert_eq!(strip_exec_placeholders("firefox %u"), "firefox");
    }

    #[test]
    fn strips_multiple_placeholders() {
        assert_eq!(strip_exec_placeholders("code %F %i"), "code");
    }

    #[test]
    fn preserves_literal_percent() {
        assert_eq!(strip_exec_placeholders("echo %%"), "echo %");
    }

    #[test]
    fn no_placeholders_unchanged() {
        assert_eq!(strip_exec_placeholders("gedit"), "gedit");
    }

    #[test]
    fn quoted_exec_preserved() {
        let result = strip_exec_placeholders("/usr/bin/firefox --new-window");
        assert_eq!(result, "/usr/bin/firefox --new-window");
    }
}
