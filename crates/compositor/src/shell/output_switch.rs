//! Super+P monitor configuration overlay.
#![allow(dead_code)]
//!
//! Pressing Super+P cycles through common multi-monitor arrangements.
//! [`OutputSwitchState`] is stored in `Shell::output_switch` while the
//! overlay is visible; the render subagent draws it.

/// A multi-monitor output arrangement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputArrangement {
    /// All outputs show the same content (mirror / clone).
    Mirror,
    /// Outputs form a single extended desktop.
    Extend,
    /// Only the built-in / primary display is active.
    PrimaryOnly,
    /// Only the external display is active.
    ExternalOnly,
}

impl OutputArrangement {
    /// Short human-readable label shown in the overlay.
    pub fn label(self) -> &'static str {
        match self {
            Self::Mirror => "Mirror",
            Self::Extend => "Extend",
            Self::PrimaryOnly => "PC screen only",
            Self::ExternalOnly => "Second screen only",
        }
    }

    /// Icon name hint for the UI renderer.
    pub fn icon(self) -> &'static str {
        match self {
            Self::Mirror => "display-mirror",
            Self::Extend => "display-extend",
            Self::PrimaryOnly => "display-internal",
            Self::ExternalOnly => "display-external",
        }
    }
}

/// All arrangements in the fixed cycle order (matches Windows Win+P behaviour).
const ARRANGEMENTS: [OutputArrangement; 4] = [
    OutputArrangement::Mirror,
    OutputArrangement::Extend,
    OutputArrangement::PrimaryOnly,
    OutputArrangement::ExternalOnly,
];

/// State for the active Super+P output-switch overlay.
#[derive(Debug)]
pub struct OutputSwitchState {
    /// The arrangement that is currently highlighted in the overlay.
    pub selected: usize,
    /// Arrangement that was active when the overlay was opened.
    pub original: usize,
}

impl OutputSwitchState {
    /// Open the overlay, starting at `current_arrangement`.
    ///
    /// Returns `None` if the arrangement is not in the known list.
    pub fn begin(current: OutputArrangement) -> Self {
        let original = ARRANGEMENTS
            .iter()
            .position(|&a| a == current)
            .unwrap_or(0);
        Self { selected: original, original }
    }

    /// The currently highlighted arrangement.
    pub fn current(&self) -> OutputArrangement {
        ARRANGEMENTS[self.selected]
    }

    /// All arrangements in cycle order.
    pub fn all() -> &'static [OutputArrangement] {
        &ARRANGEMENTS
    }

    /// Advance to the next arrangement.
    pub fn next(&mut self) {
        self.selected = (self.selected + 1) % ARRANGEMENTS.len();
    }

    /// Step back to the previous arrangement.
    pub fn prev(&mut self) {
        self.selected = self.selected
            .checked_sub(1)
            .unwrap_or(ARRANGEMENTS.len() - 1);
    }

    /// The arrangement that was active when the overlay was opened.
    pub fn original_arrangement(&self) -> OutputArrangement {
        ARRANGEMENTS[self.original]
    }
}

/// Open the Super+P overlay on `shell`, defaulting to `current` arrangement.
pub fn begin(shell: &mut super::Shell, current: OutputArrangement) {
    if shell.output_switch.is_some() {
        return; // already open
    }
    shell.output_switch = Some(OutputSwitchState::begin(current));
    tracing::debug!("output-switch overlay opened");
}

/// Advance to the next arrangement.
pub fn next(shell: &mut super::Shell) {
    if let Some(s) = &mut shell.output_switch {
        s.next();
    }
}

/// Step back to the previous arrangement.
pub fn prev(shell: &mut super::Shell) {
    if let Some(s) = &mut shell.output_switch {
        s.prev();
    }
}

/// Confirm the selected arrangement and close the overlay.
///
/// Returns the chosen [`OutputArrangement`], or `None` if the overlay was not open.
pub fn confirm(shell: &mut super::Shell) -> Option<OutputArrangement> {
    let state = shell.output_switch.take()?;
    let chosen = state.current();
    tracing::debug!(?chosen, "output-switch confirmed");
    Some(chosen)
}

/// Cancel without changing the arrangement.
pub fn cancel(shell: &mut super::Shell) {
    shell.output_switch = None;
    tracing::debug!("output-switch cancelled");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::Shell;

    #[test]
    fn begin_sets_original_arrangement() {
        let state = OutputSwitchState::begin(OutputArrangement::Extend);
        assert_eq!(state.original_arrangement(), OutputArrangement::Extend);
        assert_eq!(state.current(), OutputArrangement::Extend);
    }

    #[test]
    fn next_cycles_through_all_arrangements() {
        let mut state = OutputSwitchState::begin(OutputArrangement::Mirror);
        let mut seen = vec![state.current()];
        for _ in 0..ARRANGEMENTS.len() - 1 {
            state.next();
            seen.push(state.current());
        }
        // Should have visited all arrangements.
        for arr in ARRANGEMENTS {
            assert!(seen.contains(&arr), "{arr:?} not visited");
        }
    }

    #[test]
    fn next_wraps_around() {
        let mut state = OutputSwitchState::begin(OutputArrangement::Mirror);
        for _ in 0..ARRANGEMENTS.len() {
            state.next();
        }
        // After a full cycle we should be back at Mirror.
        assert_eq!(state.current(), OutputArrangement::Mirror);
    }

    #[test]
    fn prev_wraps_backward() {
        let mut state = OutputSwitchState::begin(OutputArrangement::Mirror);
        state.prev(); // should wrap to last
        assert_eq!(state.current(), ARRANGEMENTS[ARRANGEMENTS.len() - 1]);
    }

    #[test]
    fn shell_confirm_returns_selected_and_closes_overlay() {
        let mut shell = Shell::new();
        begin(&mut shell, OutputArrangement::Extend);
        next(&mut shell); // move to next option

        let chosen = confirm(&mut shell);
        assert!(chosen.is_some());
        assert!(shell.output_switch.is_none());
    }

    #[test]
    fn shell_cancel_closes_overlay_without_selection() {
        let mut shell = Shell::new();
        begin(&mut shell, OutputArrangement::Extend);
        cancel(&mut shell);
        assert!(shell.output_switch.is_none());
    }

    #[test]
    fn all_arrangements_have_labels() {
        for arr in ARRANGEMENTS {
            assert!(!arr.label().is_empty());
            assert!(!arr.icon().is_empty());
        }
    }
}
