//! File list: directory entry display with metadata.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Display metadata for a single filesystem entry.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct FileEntry {
    /// File or directory name.
    pub name: String,
    /// Absolute path.
    pub path: PathBuf,
    /// Whether this entry is a directory.
    pub is_dir: bool,
    /// File size in bytes (0 for directories).
    pub size: u64,
    /// Last-modified time.
    pub modified: Option<SystemTime>,
    /// MIME type hint for icon selection.
    pub mime_hint: MimeHint,
}

/// Broad MIME category for icon selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MimeHint {
    /// Directory.
    Directory,
    /// Image file.
    Image,
    /// Video file.
    Video,
    /// Audio file.
    Audio,
    /// Document (PDF, Word, etc.).
    Document,
    /// Source code.
    Code,
    /// Archive (zip, tar, …).
    Archive,
    /// Unknown / other.
    Generic,
}

/// Read and sort the contents of `dir`, applying `filter` if non-empty.
///
/// Directories always appear first.  Hidden entries (name starts with `.`)
/// are skipped.  Returns an empty list on read error.
pub fn list_dir(dir: &Path, filter: Option<&[crate::args::FileFilter]>) -> Vec<FileEntry> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut entries: Vec<FileEntry> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            let path = e.path();
            let meta = e.metadata().ok()?;
            let is_dir = meta.is_dir();
            let size = if is_dir { 0 } else { meta.len() };
            let modified = meta.modified().ok();
            let mime_hint = if is_dir {
                MimeHint::Directory
            } else {
                mime_from_extension(&name)
            };

            // Apply filter for non-directory entries.
            if !is_dir {
                if let Some(filters) = filter {
                    if !filters.is_empty() && !any_filter_matches(&name, filters) {
                        return None;
                    }
                }
            }

            Some(FileEntry { name, path, is_dir, size, modified, mime_hint })
        })
        .collect();

    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });

    entries
}

fn any_filter_matches(name: &str, filters: &[crate::args::FileFilter]) -> bool {
    let name_lower = name.to_lowercase();
    filters.iter().any(|f| {
        f.patterns.iter().any(|(kind, pat)| match kind {
            0 => {
                // Glob: only simple "*.ext" patterns supported.
                if let Some(ext) = pat.strip_prefix("*.") {
                    name_lower.ends_with(&ext.to_lowercase())
                } else {
                    name_lower == pat.to_lowercase()
                }
            }
            _ => false, // MIME matching not implemented here.
        })
    })
}

fn mime_from_extension(name: &str) -> MimeHint {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" | "tiff" => MimeHint::Image,
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "flv" | "wmv" => MimeHint::Video,
        "mp3" | "flac" | "ogg" | "wav" | "aac" | "m4a" | "opus" => MimeHint::Audio,
        "pdf" | "doc" | "docx" | "odt" | "rtf" | "tex" => MimeHint::Document,
        "rs" | "py" | "js" | "ts" | "c" | "cpp" | "h" | "go" | "java" | "sh" | "toml" | "yaml" | "json" | "xml" | "html" | "css" => MimeHint::Code,
        "zip" | "tar" | "gz" | "bz2" | "xz" | "zst" | "rar" | "7z" => MimeHint::Archive,
        _ => MimeHint::Generic,
    }
}

/// Format a file size for display (e.g. `"1.2 MB"`).
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn format_size_bytes() {
        assert_eq!(format_size(512), "512 B");
    }

    #[test]
    fn format_size_kilobytes() {
        assert_eq!(format_size(1536), "1.5 KB");
    }

    #[test]
    fn format_size_megabytes() {
        assert_eq!(format_size(1_048_576), "1.0 MB");
    }

    #[test]
    fn mime_from_extension_image() {
        assert_eq!(mime_from_extension("photo.png"), MimeHint::Image);
    }

    #[test]
    fn mime_from_extension_code() {
        assert_eq!(mime_from_extension("main.rs"), MimeHint::Code);
    }

    #[test]
    fn mime_from_extension_generic() {
        assert_eq!(mime_from_extension("file.xyz"), MimeHint::Generic);
    }

    #[test]
    fn list_dir_on_tmp_is_nonempty_or_empty() {
        // Just ensure it doesn't panic; /tmp may be empty.
        let _ = list_dir(Path::new("/tmp"), None);
    }

    #[test]
    fn filter_matches_png() {
        let filters = vec![crate::args::FileFilter {
            name: "Images".into(),
            patterns: vec![(0, "*.png".into())],
        }];
        assert!(any_filter_matches("photo.png", &filters));
        assert!(!any_filter_matches("document.pdf", &filters));
    }
}
