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
}
