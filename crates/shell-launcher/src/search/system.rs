//! System power/session command matching.

use super::SystemAction;

const COMMANDS: &[(&[&str], SystemAction)] = &[
    (&["shutdown", "power off", "poweroff", "turn off"], SystemAction::Shutdown),
    (&["restart", "reboot"], SystemAction::Restart),
    (&["lock", "lock screen"], SystemAction::Lock),
    (&["sleep", "suspend"], SystemAction::Sleep),
    (&["logout", "log out", "sign out"], SystemAction::Logout),
];

/// Returns system actions whose keywords match `query` (case-insensitive prefix
/// or substring match).
pub fn match_commands(query: &str) -> Vec<SystemAction> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();
    for (keywords, action) in COMMANDS {
        let matched = keywords
            .iter()
            .any(|kw| kw.starts_with(q.as_str()) || kw.contains(q.as_str()));
        if matched {
            results.push(*action);
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_prefix() {
        let r = match_commands("shut");
        assert!(r.contains(&SystemAction::Shutdown));
    }

    #[test]
    fn restart_exact() {
        let r = match_commands("restart");
        assert!(r.contains(&SystemAction::Restart));
    }

    #[test]
    fn empty_query_returns_nothing() {
        assert!(match_commands("").is_empty());
    }

    #[test]
    fn unrelated_query_returns_nothing() {
        assert!(match_commands("firefox").is_empty());
    }

    #[test]
    fn lock_prefix() {
        let r = match_commands("loc");
        assert!(r.contains(&SystemAction::Lock));
    }
}
