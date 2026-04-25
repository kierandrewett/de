//! Web search fallback: opens `xdg-open` with a Google search URL.

/// Open the default browser with a Google search for `query`.
///
/// Spawns `xdg-open` as a detached child process.
pub fn open_web_search(query: &str) {
    let encoded = url_encode(query);
    let url = format!("https://www.google.com/search?q={encoded}");
    tracing::info!("opening web search: {url}");

    let _ = std::process::Command::new("xdg-open")
        .arg(&url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Percent-encode a query string for use in a URL.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            b => {
                out.push('%');
                out.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('0'));
                out.push(char::from_digit((b & 0xf) as u32, 16).unwrap_or('0'));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_spaces_as_plus() {
        assert_eq!(url_encode("hello world"), "hello+world");
    }

    #[test]
    fn encode_special_chars() {
        let enc = url_encode("rust lang!");
        assert!(enc.contains("%21") || enc.contains("!") == false);
    }

    #[test]
    fn plain_ascii_unchanged() {
        assert_eq!(url_encode("rustlang"), "rustlang");
    }
}
