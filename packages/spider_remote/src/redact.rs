//! Redaction for anything that leaves the gaming PC.
//!
//! Minecraft logs can contain account tokens, session identifiers, email
//! addresses, and launcher arguments. We apply a conservative allowlist of
//! patterns before truncating and before the text is placed in a Discord
//! response or stored by the backend.

use std::sync::OnceLock;

use regex::Regex;

use crate::protocol::{MAX_LOG_BYTES, truncate_text};

struct Patterns {
    key_value: Regex,
    bearer: Regex,
    launch_arg: Regex,
    email: Regex,
    jwt: Regex,
    microsoft_code: Regex,
}

fn patterns() -> &'static Patterns {
    static PATTERNS: OnceLock<Patterns> = OnceLock::new();
    PATTERNS.get_or_init(|| Patterns {
        key_value: Regex::new(
            r#"(?i)\b(access[_-]?token|refresh[_-]?token|session[_-]?token|client[_-]?secret|authorization|password|passwd|secret)\b\s*[:=]\s*("[^"]*"|[^\s,;]+)"#,
        )
        .expect("valid redaction regex"),
        bearer: Regex::new(r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{8,}")
            .expect("valid redaction regex"),
        launch_arg: Regex::new(r"(?i)(--accessToken|--clientId|--xuid)\s+\S+")
            .expect("valid redaction regex"),
        email: Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")
            .expect("valid redaction regex"),
        jwt: Regex::new(r"eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}")
            .expect("valid redaction regex"),
        microsoft_code: Regex::new(r"\bM\.C[0-9A-Za-z._-]{16,}")
            .expect("valid redaction regex"),
    })
}

/// Remove credentials and personal identifiers from a log excerpt.
pub fn redact(input: &str) -> String {
    let patterns = patterns();
    // Remove the bearer value before the key/value rule collapses the scheme.
    let value = patterns.bearer.replace_all(input, "Bearer [REDACTED]");
    let value = patterns.key_value.replace_all(&value, "$1=[REDACTED]");
    let value = patterns.launch_arg.replace_all(&value, "$1 [REDACTED]");
    let value = patterns.email.replace_all(&value, "[redacted-email]");
    let value = patterns.jwt.replace_all(&value, "[REDACTED-TOKEN]");
    let value = patterns
        .microsoft_code
        .replace_all(&value, "[REDACTED-CODE]");
    truncate_text(value.as_ref(), MAX_LOG_BYTES)
}

/// Bounded tail of a log file, newest lines only.
pub fn tail_text(input: &str, max_lines: usize) -> String {
    let mut lines: Vec<&str> = input.lines().rev().take(max_lines).collect();
    lines.reverse();
    let joined = lines.join("\n");
    truncate_text(&joined, MAX_LOG_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_common_credential_shapes() {
        let input = r#"
[12:00:00] accessToken=eyJhbGciOiJSUzI1NiJ9.eyJ4Ijox.sig
[12:00:01] Authorization: Bearer abcdefghijklmnop
[12:00:02] password = hunter2
[12:00:03] email player@example.com joined
[12:00:04] --accessToken supersecretvalue --version 1.21.11
"#;
        let out = redact(input);
        assert!(!out.contains("eyJhbGci"));
        assert!(!out.contains("abcdefghijklmnop"));
        assert!(!out.contains("hunter2"));
        assert!(!out.contains("player@example.com"));
        assert!(!out.contains("supersecretvalue"));
        assert!(out.contains("[REDACTED]"));
        assert!(out.contains("--version 1.21.11"));
    }

    #[test]
    fn preserves_ordinary_chat_and_coordinates() {
        let input = "<Steve> hello\nPosition: 120, 64, -33\n";
        assert_eq!(redact(input), input);
    }

    #[test]
    fn tail_keeps_newest_lines_in_order() {
        let input = (1..=10)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(tail_text(&input, 3), "line 8\nline 9\nline 10");
    }

    #[test]
    fn output_is_bounded() {
        let input = "x".repeat(MAX_LOG_BYTES * 2);
        assert!(redact(&input).len() <= MAX_LOG_BYTES + 4);
    }
}
