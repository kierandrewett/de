//! Typography scale tokens.

use serde::{Deserialize, Serialize};

/// Typography scale — font family and size/weight steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Typography {
    /// Primary UI font family name.
    pub font_family: String,
    /// Small label size (11 px).
    pub font_size_small: f32,
    /// Normal body/UI text size (13 px).
    pub font_size_normal: f32,
    /// Large text size (16 px).
    pub font_size_large: f32,
    /// Title / heading size (20 px).
    pub font_size_title: f32,
    /// Normal font weight (400).
    pub font_weight_normal: u16,
    /// Bold / semi-bold font weight (600).
    pub font_weight_bold: u16,
}

impl Default for Typography {
    fn default() -> Self {
        Self {
            font_family: "Inter".to_string(),
            font_size_small: 11.0,
            font_size_normal: 13.0,
            font_size_large: 16.0,
            font_size_title: 20.0,
            font_weight_normal: 400,
            font_weight_bold: 600,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_font_family_is_inter() {
        assert_eq!(Typography::default().font_family, "Inter");
    }

    #[test]
    fn sizes_are_ascending() {
        let t = Typography::default();
        assert!(t.font_size_small < t.font_size_normal);
        assert!(t.font_size_normal < t.font_size_large);
        assert!(t.font_size_large < t.font_size_title);
    }

    #[test]
    fn weights_are_valid() {
        let t = Typography::default();
        assert_eq!(t.font_weight_normal, 400);
        assert_eq!(t.font_weight_bold, 600);
    }
}
