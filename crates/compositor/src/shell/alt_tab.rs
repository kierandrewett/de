//! Alt-Tab window switcher overlay.
#![allow(dead_code)]
//!
//! [`AltTabState`] is created when the user presses Alt+Tab and stored in
//! [`Shell::alt_tab`].  The render subagent (08) reads it to draw the overlay.
//! The input handler calls [`next`] / [`prev`] on Tab / Shift+Tab, [`confirm`]
//! on Alt release, and [`cancel`] on Escape.

use super::Shell;

/// Entry in the alt-tab window list.
#[derive(Debug, Clone)]
pub struct AltTabEntry {
    /// Compositor window ID.
    pub id: u64,
    /// Wayland `app_id`.
    pub app_id: String,
    /// Window title.
    pub title: String,
}

/// State for the active alt-tab overlay session.
#[derive(Debug)]
pub struct AltTabState {
    /// Ordered list of windows to cycle through (MRU order, index 0 = most recent).
    pub entries: Vec<AltTabEntry>,
    /// Index of the currently highlighted entry.
    pub selected: usize,
    /// Window that was focused when alt-tab started (restored on Escape).
    pub original_focus: Option<u64>,
}

impl AltTabState {
    /// Build alt-tab state from the current shell focus stack and window list.
    ///
    /// Windows are ordered most-recently-used first.  Minimized windows are
    /// excluded.
    pub fn begin(shell: &Shell) -> Option<Self> {
        // Build MRU list from focus_stack (last = most recent) in reverse order.
        let mut entries: Vec<AltTabEntry> = shell
            .focus_stack
            .iter()
            .rev()
            .filter_map(|&id| shell.window(id))
            .filter(|w| !w.is_minimized)
            .map(|w| AltTabEntry {
                id: w.id,
                app_id: w.app_id.clone(),
                title: w.title.clone(),
            })
            .collect();

        // Append any visible windows not yet in the focus stack (e.g. new windows).
        for win in shell.visible_windows() {
            if !entries.iter().any(|e| e.id == win.id) {
                entries.push(AltTabEntry {
                    id: win.id,
                    app_id: win.app_id.clone(),
                    title: win.title.clone(),
                });
            }
        }

        if entries.is_empty() {
            return None;
        }

        // Start with the second entry selected (like macOS / Windows alt-tab).
        let selected = if entries.len() > 1 { 1 } else { 0 };
        let original_focus = shell.focused_window_id();

        Some(Self { entries, selected, original_focus })
    }

    /// Move selection forward (Tab key).
    pub fn next(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.entries.len();
    }

    /// Move selection backward (Shift+Tab key).
    pub fn prev(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        self.selected = self.selected
            .checked_sub(1)
            .unwrap_or(self.entries.len() - 1);
    }

    /// The window ID of the currently highlighted entry, if any.
    pub fn selected_id(&self) -> Option<u64> {
        self.entries.get(self.selected).map(|e| e.id)
    }
}

/// Confirm the current selection: focus the highlighted window and close the overlay.
///
/// Returns the newly focused window ID if any.
pub fn confirm(shell: &mut Shell) -> Option<u64> {
    let state = shell.alt_tab.take()?;
    let id = state.selected_id()?;
    shell.focus_window(id);
    tracing::debug!(id, "alt-tab confirmed");
    Some(id)
}

/// Cancel alt-tab: restore the original focus and close the overlay.
pub fn cancel(shell: &mut Shell) {
    let state = match shell.alt_tab.take() {
        Some(s) => s,
        None => return,
    };
    if let Some(orig) = state.original_focus {
        shell.focus_window(orig);
    }
    tracing::debug!("alt-tab cancelled");
}

/// Open the alt-tab overlay.  Does nothing if there are no windows to cycle.
pub fn begin(shell: &mut Shell) {
    if shell.alt_tab.is_some() {
        return; // already open
    }
    shell.alt_tab = AltTabState::begin(shell);
    if shell.alt_tab.is_some() {
        tracing::debug!("alt-tab opened");
    }
}

/// Move to the next window while the overlay is open.
pub fn next(shell: &mut Shell) {
    if let Some(state) = &mut shell.alt_tab {
        state.next();
    }
}

/// Move to the previous window while the overlay is open.
pub fn prev(shell: &mut Shell) {
    if let Some(state) = &mut shell.alt_tab {
        state.prev();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use smithay::utils::{Point, Rectangle, Size};

    use super::*;
    use crate::shell::{DecorationMode, MappedWindow, Shell, WindowSurface};

    fn add_window(shell: &mut Shell, title: &str) -> u64 {
        let id = shell.alloc_window_id();
        let surface = WindowSurface { token: id };
        let rect = Rectangle::new(
            Point::from((0, 0)),
            Size::from((800, 600)),
        );
        shell.windows.push(MappedWindow::new(
            id,
            surface,
            rect,
            DecorationMode::ServerSide,
            "app".into(),
            title.into(),
        ));
        shell.focus_window(id);
        id
    }

    #[test]
    fn begin_creates_mru_list() {
        let mut shell = Shell::new();
        let id1 = add_window(&mut shell, "Win 1");
        let id2 = add_window(&mut shell, "Win 2");
        let id3 = add_window(&mut shell, "Win 3");

        // focus order: id1 → id2 → id3, so MRU = [id3, id2, id1]
        begin(&mut shell);
        let state = shell.alt_tab.as_ref().unwrap();
        assert_eq!(state.entries[0].id, id3);
        assert_eq!(state.entries[1].id, id2);
        assert_eq!(state.entries[2].id, id1);
        // starts at index 1 (second entry)
        assert_eq!(state.selected, 1);
    }

    #[test]
    fn next_cycles_forward_and_wraps() {
        let mut shell = Shell::new();
        for i in 0..3 {
            add_window(&mut shell, &format!("Win {i}"));
        }
        begin(&mut shell);
        let len = shell.alt_tab.as_ref().unwrap().entries.len();

        // Cycle all the way around.
        for _ in 0..len {
            next(&mut shell);
        }
        // Should be back to where we started (index 1).
        assert_eq!(shell.alt_tab.as_ref().unwrap().selected, 1);
    }

    #[test]
    fn prev_cycles_backward_and_wraps() {
        let mut shell = Shell::new();
        for i in 0..3 {
            add_window(&mut shell, &format!("Win {i}"));
        }
        begin(&mut shell);

        prev(&mut shell); // 1 → 0
        assert_eq!(shell.alt_tab.as_ref().unwrap().selected, 0);

        prev(&mut shell); // 0 → 2 (wraps)
        assert_eq!(shell.alt_tab.as_ref().unwrap().selected, 2);
    }

    #[test]
    fn confirm_focuses_selected_window() {
        let mut shell = Shell::new();
        let id1 = add_window(&mut shell, "Win 1");
        let _id2 = add_window(&mut shell, "Win 2");

        begin(&mut shell);
        // Selected starts at 1 (id1 in MRU order [id2, id1]).
        let confirmed = confirm(&mut shell);
        assert_eq!(confirmed, Some(id1));
        assert_eq!(shell.focused_window_id(), Some(id1));
        assert!(shell.alt_tab.is_none());
    }

    #[test]
    fn cancel_restores_original_focus() {
        let mut shell = Shell::new();
        let id1 = add_window(&mut shell, "Win 1");
        let id2 = add_window(&mut shell, "Win 2");
        let _ = id2;
        // id2 is focused now
        assert_eq!(shell.focused_window_id(), Some(id2));

        begin(&mut shell);
        next(&mut shell); // move away from default
        cancel(&mut shell);

        // Focus should be back on id2.
        assert_eq!(shell.focused_window_id(), Some(id2));
        assert!(shell.alt_tab.is_none());
        let _ = id1;
    }

    #[test]
    fn begin_with_no_windows_leaves_alt_tab_none() {
        let mut shell = Shell::new();
        begin(&mut shell);
        assert!(shell.alt_tab.is_none());
    }

    #[test]
    fn minimized_windows_excluded_from_alt_tab() {
        let mut shell = Shell::new();
        let id1 = add_window(&mut shell, "Win 1");
        let id2 = add_window(&mut shell, "Win 2");

        shell.window_mut(id1).unwrap().is_minimized = true;

        begin(&mut shell);
        let state = shell.alt_tab.as_ref().unwrap();
        assert!(state.entries.iter().all(|e| e.id != id1));
        assert!(state.entries.iter().any(|e| e.id == id2));
    }
}
