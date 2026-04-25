//! Parsing for `metadata.json` files in KDE SVG cursor theme directories.

use serde::Deserialize;
use std::path::Path;

/// Parsed contents of a cursor's `metadata.json` file.
#[derive(Debug, Deserialize)]
pub struct CursorMetadata {
    /// Hotspot X coordinate at nominal size (pixels).
    pub hotspot_x: u32,
    /// Hotspot Y coordinate at nominal size (pixels).
    pub hotspot_y: u32,
    /// The size at which hotspot coordinates are defined (e.g. 24).
    pub nominal_size: u32,
    /// Frame entries for animated cursors; empty for static cursors.
    #[serde(default)]
    pub frames: Vec<FrameEntry>,
}

/// A single animation frame entry inside `metadata.json`.
#[derive(Debug, Deserialize)]
pub struct FrameEntry {
    /// SVG filename relative to the cursor directory (e.g. `"frame1.svg"`).
    pub filename: String,
    /// Display duration for this frame in milliseconds.
    pub duration: u32,
}

impl CursorMetadata {
    /// Read and deserialise a `metadata.json` file.
    pub fn load(path: &Path) -> crate::Result<Self> {
        let data = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&data)?)
    }

    /// Returns `true` if this cursor has animation frames.
    pub fn is_animated(&self) -> bool {
        !self.frames.is_empty()
    }

    /// Scale a hotspot coordinate from nominal size to physical size.
    pub fn scale_hotspot(hotspot: u32, nominal_size: u32, physical_size: u32) -> i32 {
        ((hotspot as f32 * physical_size as f32) / nominal_size as f32).round() as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_temp(json: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(json.as_bytes()).unwrap();
        f
    }

    #[test]
    fn test_parse_static_cursor() {
        let f = write_temp(r#"{"hotspot_x": 10, "hotspot_y": 3, "nominal_size": 24}"#);
        let meta = CursorMetadata::load(f.path()).unwrap();
        assert_eq!(meta.hotspot_x, 10);
        assert_eq!(meta.hotspot_y, 3);
        assert_eq!(meta.nominal_size, 24);
        assert!(!meta.is_animated());
    }

    #[test]
    fn test_parse_animated_cursor() {
        let json = r#"{
            "hotspot_x": 12,
            "hotspot_y": 12,
            "nominal_size": 24,
            "frames": [
                {"filename": "frame1.svg", "duration": 50},
                {"filename": "frame2.svg", "duration": 50},
                {"filename": "frame3.svg", "duration": 100}
            ]
        }"#;
        let f = write_temp(json);
        let meta = CursorMetadata::load(f.path()).unwrap();
        assert!(meta.is_animated());
        assert_eq!(meta.frames.len(), 3);
        assert_eq!(meta.frames[0].filename, "frame1.svg");
        assert_eq!(meta.frames[2].duration, 100);
    }

    #[test]
    fn test_hotspot_scaling() {
        // nominal_size=24, hotspot=(10,3) → at size 48 should be (20,6)
        assert_eq!(CursorMetadata::scale_hotspot(10, 24, 48), 20);
        assert_eq!(CursorMetadata::scale_hotspot(3, 24, 48), 6);
        // identity
        assert_eq!(CursorMetadata::scale_hotspot(10, 24, 24), 10);
        // non-integer result rounds
        assert_eq!(CursorMetadata::scale_hotspot(10, 24, 96), 40);
    }

    #[test]
    fn test_missing_file_returns_err() {
        let result = CursorMetadata::load(Path::new("/nonexistent/path/metadata.json"));
        assert!(result.is_err());
    }
}
