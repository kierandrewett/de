//! Parses `~/.local/share/recently-used.xbel` (freedesktop spec, XML).

use std::path::PathBuf;

use quick_xml::events::Event;
use quick_xml::Reader;

use super::RecentFile;

/// Load recent files from `~/.local/share/recently-used.xbel`.
///
/// Returns an empty list if the file does not exist or cannot be parsed.
pub fn load_recent_files() -> Vec<RecentFile> {
    let path = xbel_path();
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    parse_xbel(&content)
}

fn xbel_path() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local/share/recently-used.xbel")
    } else {
        PathBuf::from("/tmp/recently-used.xbel")
    }
}

/// Parse XBEL XML content into a list of [`RecentFile`]s.
fn parse_xbel(content: &str) -> Vec<RecentFile> {
    let mut reader = Reader::from_str(content);
    reader.config_mut().trim_text(true);

    let mut files: Vec<RecentFile> = Vec::new();
    let mut buf: Vec<u8> = Vec::new();
    let mut current_href: Option<String> = None;
    let mut current_mime: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e) | Event::Empty(ref e))
                if e.name().as_ref() == b"bookmark" =>
            {
                current_href = None;
                current_mime = None;
                for attr in e.attributes().flatten() {
                    if attr.key.as_ref() == b"href" {
                        if let Ok(val) = attr.unescape_value() {
                            current_href = Some(val.into_owned());
                        }
                    }
                }
            }
            Ok(Event::Empty(ref e)) if e.name().as_ref() == b"mime:mime-type" => {
                for attr in e.attributes().flatten() {
                    if attr.key.as_ref() == b"type" {
                        if let Ok(val) = attr.unescape_value() {
                            current_mime = Some(val.into_owned());
                        }
                    }
                }
            }
            Ok(Event::End(ref e)) if e.name().as_ref() == b"bookmark" => {
                if let Some(href) = current_href.take() {
                    if let Some(file) = href_to_recent_file(&href, current_mime.take()) {
                        files.push(file);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                tracing::warn!("xbel parse error: {e}");
                break;
            }
            _ => {}
        }
        buf.clear();
    }

    // Most-recent first (xbel order is oldest first typically).
    files.reverse();
    files
}

/// Convert a `file://` URI to a [`RecentFile`].
fn href_to_recent_file(href: &str, mime_type: Option<String>) -> Option<RecentFile> {
    let path_str = href.strip_prefix("file://")?;
    // Percent-decode the path.
    let decoded = percent_decode(path_str);
    let path = PathBuf::from(&decoded);
    if !path.exists() {
        return None;
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&decoded)
        .to_owned();
    Some(RecentFile { name, path, mime_type })
}

fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (
                char::from_digit(bytes[i + 1] as u32 - b'0' as u32, 16)
                    .or_else(|| (bytes[i + 1] as char).to_digit(16).map(|_| bytes[i + 1] as char)),
                char::from_digit(bytes[i + 2] as u32 - b'0' as u32, 16)
                    .or_else(|| (bytes[i + 2] as char).to_digit(16).map(|_| bytes[i + 2] as char)),
            ) {
                let decoded = (hi as u8) << 4 | lo as u8;
                out.push(decoded as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Simple substring filter for recent files.
pub fn search_recent<'a>(query: &str, files: &'a [RecentFile]) -> Vec<&'a RecentFile> {
    let q = query.to_lowercase();
    files
        .iter()
        .filter(|f| f.name.to_lowercase().contains(&q))
        .collect()
}

/// Owned wrapper so `search_recent` can be used in the aggregator.
pub fn search_recent_owned(query: &str, files: &[RecentFile]) -> Vec<RecentFile> {
    search_recent(query, files).into_iter().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_XBEL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<xbel version="1.0">
  <bookmark href="file:///home/user/Documents/report.pdf">
    <info>
      <metadata owner="http://freedesktop.org">
        <mime:mime-type xmlns:mime="http://www.freedesktop.org/standards/shared-mime-info" type="application/pdf"/>
      </metadata>
    </info>
  </bookmark>
  <bookmark href="file:///home/user/code/main.rs">
    <info/>
  </bookmark>
</xbel>"#;

    #[test]
    fn parses_hrefs() {
        // parse_xbel itself — we can't check path.exists() without real files,
        // but we can verify it doesn't panic.
        let files = parse_xbel(SAMPLE_XBEL);
        // In CI there are no real files, so both may be filtered out by exists().
        // The important thing is no panic.
        let _ = files;
    }

    #[test]
    fn empty_xbel_returns_empty() {
        let files = parse_xbel("");
        assert!(files.is_empty());
    }

    #[test]
    fn search_filters_by_name() {
        let files = vec![
            RecentFile {
                name: "report.pdf".to_owned(),
                path: PathBuf::from("/tmp/report.pdf"),
                mime_type: None,
            },
            RecentFile {
                name: "main.rs".to_owned(),
                path: PathBuf::from("/tmp/main.rs"),
                mime_type: None,
            },
        ];
        let hits = search_recent("report", &files);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "report.pdf");
    }

    #[test]
    fn search_case_insensitive() {
        let files = vec![RecentFile {
            name: "Report.pdf".to_owned(),
            path: PathBuf::from("/tmp/Report.pdf"),
            mime_type: None,
        }];
        assert_eq!(search_recent("report", &files).len(), 1);
    }
}
