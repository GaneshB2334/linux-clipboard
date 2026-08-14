//! SPDX-License-Identifier: GPL-3.0-or-later
//! Copyright (C) 2026 Ganesh Bastapure
//!
//! Content classification and secret detection.
//!
//! Deliberately regex-free: these run on every copy, and hand-rolled scanners
//! keep the daemon's dependency surface (and RSS) small.

use clipd_ipc::Kind;

/// Classify a copy from its best MIME type and payload.
pub fn kind_of(mime: &str, data: &[u8]) -> Kind {
    if mime.starts_with("image/") {
        return Kind::Image;
    }
    if mime == "text/html" {
        return Kind::Html;
    }
    if mime == "text/uri-list" {
        return Kind::Files;
    }
    let Ok(text) = std::str::from_utf8(data) else {
        return Kind::Text;
    };
    let t = text.trim();
    if t.is_empty() {
        return Kind::Text;
    }
    if is_url(t) {
        return Kind::Url;
    }
    if is_color(t) {
        return Kind::Color;
    }
    if looks_like_code(t) {
        return Kind::Code;
    }
    Kind::Text
}

/// Payloads at or above this are skipped rather than read into memory —
/// matches the X11 backend's own image size cap, for the same reason.
const MAX_FILE_IMAGE: u64 = 50 * 1024 * 1024;

/// A `text/uri-list` copy (Ctrl+C on a file in a file manager) carries only a
/// path, never the file's bytes — unlike copying an image *element* in a
/// browser, which puts real `image/*` data on the clipboard directly. This
/// reads the file when the list names exactly one local image, so it renders
/// and thumbnails the same way a browser-copied image does, rather than
/// showing up as a bare file-path reference.
///
/// Deliberately narrow: more than one entry stays a plain file-list capture
/// — a multi-file selection doesn't have one obvious "the" image to promote,
/// and guessing wrong would be worse than leaving it as paths. The format is
/// confirmed from the file's own bytes (`image::guess_format`), not trusted
/// from its extension, since nothing stops a file being named `photo.png`
/// and containing text.
pub fn image_from_local_file(uri_list: &[u8]) -> Option<(String, Vec<u8>)> {
    let text = std::str::from_utf8(uri_list).ok()?;
    let mut uris = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'));
    let uri = uris.next()?;
    if uris.next().is_some() {
        return None;
    }

    let path = file_uri_to_path(uri)?;
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_FILE_IMAGE {
        return None;
    }

    let bytes = std::fs::read(&path).ok()?;
    let mime = match image::guess_format(&bytes).ok()? {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::Gif => "image/gif",
        image::ImageFormat::WebP => "image/webp",
        image::ImageFormat::Bmp => "image/bmp",
        _ => return None, // a format clipd doesn't ship a decoder for
    };
    Some((mime.to_string(), bytes))
}

/// `file:///home/you/pic.png` -> `/home/you/pic.png`. No crate for this: the
/// grammar clipd needs is one prefix strip and percent-decoding, not the
/// general URI spec — GVFS/Nautilus always writes the empty-authority
/// (triple-slash) form; `file://localhost/...` is accepted too since it's a
/// common equivalent. Any other authority names a different machine, which
/// isn't something clipd can read a file from.
fn file_uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = if let Some(p) = rest.strip_prefix('/') {
        format!("/{p}")
    } else if let Some(p) = rest.strip_prefix("localhost/") {
        format!("/{p}")
    } else {
        return None;
    };
    Some(std::path::PathBuf::from(percent_decode(&path)))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_url(t: &str) -> bool {
    if t.split_whitespace().count() != 1 {
        return false;
    }
    (t.starts_with("http://") || t.starts_with("https://") || t.starts_with("ftp://"))
        && t.len() > 10
}

/// `#rgb`, `#rrggbb`, `#rrggbbaa`, plus `rgb(...)` / `hsl(...)`.
fn is_color(t: &str) -> bool {
    if let Some(hex) = t.strip_prefix('#') {
        return matches!(hex.len(), 3 | 4 | 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit());
    }
    let lower = t.to_ascii_lowercase();
    (lower.starts_with("rgb(")
        || lower.starts_with("rgba(")
        || lower.starts_with("hsl(")
        || lower.starts_with("hsla("))
        && lower.ends_with(')')
}

/// Heuristic, intentionally conservative — a false positive only changes an
/// icon and enables syntax highlighting, so we bias toward missing rather than
/// labelling ordinary prose as code.
fn looks_like_code(t: &str) -> bool {
    const MARKERS: &[&str] = &[
        "function ", "const ", "let ", "var ", "=>", "def ", "class ", "import ", "#include",
        "public ", "fn ", "SELECT ", "npm ", "cargo ", "sudo ", "git ", "docker ", "return ",
        "if (", "for (", "while (", "};", "()", "&&", "||", "::",
    ];
    let hits = MARKERS.iter().filter(|m| t.contains(**m)).count();
    let multiline = t.lines().count() > 1;
    let indented = t.lines().filter(|l| l.starts_with("  ") || l.starts_with('\t')).count();
    hits >= 2 || (hits >= 1 && (multiline || indented > 0))
}

/// Does this look like a credential? Used to blur the row, keep it out of the
/// search index, and optionally refuse to persist it at all.
///
/// This is the *second* line of defence. The reliable signal is the
/// `x-kde-passwordManagerHint` MIME target set by password managers; this
/// catches secrets pasted from terminals and editors, which carry no hint.
pub fn is_sensitive(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() || t.len() > 8192 {
        return false;
    }

    if t.contains("-----BEGIN") && t.contains("PRIVATE KEY") {
        return true;
    }

    // Single-token secrets: prefixed API keys and JWTs.
    if t.split_whitespace().count() == 1 {
        const PREFIXES: &[&str] = &[
            "sk-", "sk_live_", "sk_test_", "pk_live_", "rk_live_", "ghp_", "gho_", "ghu_",
            "ghs_", "ghr_", "github_pat_", "glpat-", "xoxb-", "xoxp-", "xoxa-", "AKIA",
            "ASIA", "AIza", "ya29.", "hf_", "sbp_", "dop_v1_", "shpat_", "npm_",
        ];
        if PREFIXES.iter().any(|p| t.starts_with(p)) && t.len() >= 16 {
            return true;
        }
        // JWT: three base64url segments, header decodes to something starting `{"`.
        if t.starts_with("eyJ") && t.matches('.').count() == 2 && t.len() > 40 {
            return true;
        }
        // A bare payment card number.
        if is_payment_card(t) {
            return true;
        }
    }

    // `PASSWORD=hunter2`, `api_key: ...` — assignment of a secret-ish name.
    if let Some((lhs, rhs)) = t.split_once(['=', ':']) {
        let key = lhs.trim().to_ascii_lowercase();
        let key = key.rsplit([' ', '\t']).next().unwrap_or(&key);
        const NAMES: &[&str] = &[
            "password", "passwd", "secret", "token", "api_key", "apikey", "access_key",
            "private_key", "client_secret", "auth", "credential",
        ];
        if NAMES.iter().any(|n| key.contains(n)) && rhs.trim().len() >= 6 && !rhs.contains(' ') {
            return true;
        }
    }

    false
}

/// Luhn check over a 13–19 digit string, allowing spaces and dashes. Without
/// the checksum, any long number (order IDs, phone numbers) would be flagged.
fn is_payment_card(t: &str) -> bool {
    let digits: Vec<u8> = t
        .bytes()
        .filter(|b| !matches!(b, b' ' | b'-'))
        .map(|b| b.wrapping_sub(b'0'))
        .collect();
    if digits.len() < 13 || digits.len() > 19 || digits.iter().any(|d| *d > 9) {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, d)| {
            let mut v = *d as u32;
            if i % 2 == 1 {
                v *= 2;
                if v > 9 {
                    v -= 9;
                }
            }
            v
        })
        .sum();
    sum % 10 == 0
}

/// Collapse to one line and clamp, for list rendering.
pub fn make_preview(text: &str, max: usize) -> String {
    let mut out = String::with_capacity(max.min(text.len()) + 1);
    let mut ws = false;
    for ch in text.trim().chars() {
        if out.chars().count() >= max {
            out.push('…');
            break;
        }
        if ch.is_whitespace() {
            if !ws && !out.is_empty() {
                out.push(' ');
                ws = true;
            }
        } else {
            out.push(ch);
            ws = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_copies() {
        assert_eq!(kind_of("UTF8_STRING", b"https://github.com/x/y"), Kind::Url);
        assert_eq!(kind_of("UTF8_STRING", b"#ff8800"), Kind::Color);
        assert_eq!(kind_of("UTF8_STRING", b"rgba(1,2,3,0.5)"), Kind::Color);
        assert_eq!(kind_of("UTF8_STRING", b"const app = express()"), Kind::Code);
        assert_eq!(kind_of("UTF8_STRING", b"just some prose here"), Kind::Text);
        assert_eq!(kind_of("image/png", b"\x89PNG"), Kind::Image);
        assert_eq!(kind_of("text/html", b"<b>hi</b>"), Kind::Html);
    }

    #[test]
    fn a_url_with_trailing_words_is_not_a_url() {
        assert_eq!(kind_of("UTF8_STRING", b"see https://example.com for more"), Kind::Text);
    }

    #[test]
    fn detects_secrets() {
        assert!(is_sensitive("ghp_1234567890abcdefghijklmnop"));
        assert!(is_sensitive("AKIAIOSFODNN7EXAMPLE"));
        assert!(is_sensitive(
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N"
        ));
        assert!(is_sensitive("-----BEGIN RSA PRIVATE KEY-----\nMIIE..."));
        assert!(is_sensitive("PASSWORD=hunter2xyz"));
        assert!(is_sensitive("4111111111111111")); // valid Luhn
    }

    #[test]
    fn does_not_flag_ordinary_text() {
        assert!(!is_sensitive("the quick brown fox"));
        assert!(!is_sensitive("https://example.com/page"));
        assert!(!is_sensitive("4111111111111112")); // fails Luhn
        assert!(!is_sensitive("1234567890123456")); // fails Luhn
        assert!(!is_sensitive("select * from users"));
        // A sentence mentioning a password is not itself a password.
        assert!(!is_sensitive("password: please pick something memorable"));
    }

    #[test]
    fn preview_is_single_line_and_clamped() {
        assert_eq!(make_preview("  a\n\n  b  ", 40), "a b");
        assert_eq!(make_preview(&"x".repeat(100), 5).chars().count(), 6); // 5 + ellipsis
    }

    fn tiny_png() -> Vec<u8> {
        // 1x1 transparent PNG — small enough to inline here, real enough for
        // `image::guess_format` to actually recognise.
        base64_decode(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR4nGNgAAIAAAUAAen63NgAAAAASUVORK5CYII=",
        )
    }

    // Minimal decoder so this test file doesn't need a base64 dependency
    // just for one fixture.
    fn base64_decode(s: &str) -> Vec<u8> {
        fn val(c: u8) -> Option<u8> {
            match c {
                b'A'..=b'Z' => Some(c - b'A'),
                b'a'..=b'z' => Some(c - b'a' + 26),
                b'0'..=b'9' => Some(c - b'0' + 52),
                b'+' => Some(62),
                b'/' => Some(63),
                _ => None,
            }
        }
        let mut out = Vec::new();
        let mut buf = 0u32;
        let mut bits = 0;
        for &b in s.as_bytes() {
            let Some(v) = val(b) else { continue };
            buf = (buf << 6) | v as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buf >> bits) as u8);
            }
        }
        out
    }

    #[test]
    fn file_uri_round_trips_a_plain_path() {
        assert_eq!(
            file_uri_to_path("file:///home/you/pic.png"),
            Some(std::path::PathBuf::from("/home/you/pic.png"))
        );
    }

    #[test]
    fn file_uri_decodes_percent_escapes() {
        assert_eq!(
            file_uri_to_path("file:///home/you/My%20Photo.png"),
            Some(std::path::PathBuf::from("/home/you/My Photo.png"))
        );
    }

    #[test]
    fn file_uri_accepts_explicit_localhost() {
        assert_eq!(
            file_uri_to_path("file://localhost/home/you/pic.png"),
            Some(std::path::PathBuf::from("/home/you/pic.png"))
        );
    }

    #[test]
    fn file_uri_rejects_a_remote_host() {
        assert_eq!(file_uri_to_path("file://otherbox/home/you/pic.png"), None);
    }

    #[test]
    fn single_local_image_is_read_and_identified() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.png");
        std::fs::write(&path, tiny_png()).unwrap();

        let uri_list = format!("file://{}\n", path.display());
        let (mime, data) = image_from_local_file(uri_list.as_bytes()).unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(data, tiny_png());
    }

    #[test]
    fn multiple_files_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        std::fs::write(&a, tiny_png()).unwrap();
        std::fs::write(&b, tiny_png()).unwrap();

        let uri_list = format!("file://{}\nfile://{}\n", a.display(), b.display());
        assert_eq!(image_from_local_file(uri_list.as_bytes()), None);
    }

    #[test]
    fn a_non_image_file_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"just some text, not a picture").unwrap();

        let uri_list = format!("file://{}\n", path.display());
        assert_eq!(image_from_local_file(uri_list.as_bytes()), None);
    }

    #[test]
    fn a_missing_file_is_left_alone() {
        assert_eq!(image_from_local_file(b"file:///no/such/file.png\n"), None);
    }
}
