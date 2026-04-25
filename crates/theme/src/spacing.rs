//! Spacing scale tokens.

use serde::{Deserialize, Serialize};

/// Spacing scale — discrete step values for margins, padding, and gaps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Spacing {
    /// Extra small — 4 px.
    pub xs: f32,
    /// Small — 8 px.
    pub sm: f32,
    /// Medium — 12 px.
    pub md: f32,
    /// Large — 16 px.
    pub lg: f32,
    /// Extra large — 24 px.
    pub xl: f32,
    /// Double extra large — 32 px.
    pub xxl: f32,
}

impl Default for Spacing {
    fn default() -> Self {
        Self {
            xs: 4.0,
            sm: 8.0,
            md: 12.0,
            lg: 16.0,
            xl: 24.0,
            xxl: 32.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_are_ascending() {
        let s = Spacing::default();
        assert!(s.xs < s.sm);
        assert!(s.sm < s.md);
        assert!(s.md < s.lg);
        assert!(s.lg < s.xl);
        assert!(s.xl < s.xxl);
    }

    #[test]
    fn xs_is_four() {
        assert_eq!(Spacing::default().xs, 4.0);
    }

    #[test]
    fn xxl_is_thirty_two() {
        assert_eq!(Spacing::default().xxl, 32.0);
    }
}
