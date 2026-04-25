//! Breadcrumb path bar: breaks a path into clickable segments.

use std::path::{Path, PathBuf};

/// A single segment of the breadcrumb path bar.
#[derive(Debug, Clone)]
pub struct Segment {
    /// Display label for this segment.
    pub label: String,
    /// Absolute path this segment navigates to when clicked.
    pub path: PathBuf,
}

/// Decompose `path` into an ordered list of breadcrumb segments.
///
/// The first segment is always `/` (root) for absolute paths.
pub fn segments(path: &Path) -> Vec<Segment> {
    if !path.is_absolute() {
        return Vec::new();
    }

    let mut result = Vec::new();
    let mut cumulative = PathBuf::from("/");
    result.push(Segment { label: "/".into(), path: cumulative.clone() });

    // Canonicalize to eliminate `..` etc., fall back to raw path on error.
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    for component in canonical.components().skip(1) {
        // skip(1) because we already added root above
        use std::path::Component;
        if let Component::Normal(s) = component {
            cumulative.push(s);
            let label = s.to_string_lossy().into_owned();
            result.push(Segment { label, path: cumulative.clone() });
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_path_has_one_segment() {
        let segs = segments(Path::new("/"));
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].label, "/");
    }

    #[test]
    fn home_path_segments() {
        let segs = segments(Path::new("/home/user"));
        // "/" + "home" + "user" = 3 segments (if /home/user is not a symlink)
        // At least 2 (root + home) because /home is real.
        assert!(segs.len() >= 2);
        assert_eq!(segs[0].label, "/");
    }

    #[test]
    fn relative_path_returns_empty() {
        let segs = segments(Path::new("relative/path"));
        assert!(segs.is_empty());
    }

    #[test]
    fn segment_paths_are_prefixes() {
        let segs = segments(Path::new("/tmp"));
        // Each segment's path should be a prefix of the next.
        for w in segs.windows(2) {
            assert!(w[1].path.starts_with(&w[0].path));
        }
    }
}
