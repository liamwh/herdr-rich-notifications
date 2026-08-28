//! Display sanitisation for notification text.
//!
//! Notifications can surface outside herdr — lock screens, notification
//! centers, logs — so everything that reaches [`crate::notify`] passes through
//! here first: ANSI/control characters are stripped, box-drawing and spinner
//! glyphs are blanked, whitespace is normalised, obvious secrets are redacted,
//! and length is bounded on `char` boundaries.

use std::sync::LazyLock;

use regex::Regex;

/// Matches CSI sequences, two-byte escapes, and OSC strings.
static ANSI_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\x1B\][^\x07\x1B]*(?:\x07|\x1B\\)", // OSC ... (BEL or ST)
        r"|\x1B\[[0-?]*[ -/]*[@-~]",          // CSI ... final byte
        r"|\x1B[PX^_][^\x1B]*(?:\x1B\\)?",    // DCS/SOS/PM/APC
        r"|\x1B[@-Z\\-_]",                    // two-byte escape
    ))
    .expect("static ANSI regex must compile")
});

/// C0/C1 control characters except tab and newline (handled separately).
static CONTROL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\x00-\x08\x0B-\x1F\x7F-\x9F]").expect("static control regex"));

/// Box-drawing, block, geometric, and braille-spinner glyphs that terminals
/// use for UI chrome; blanked rather than passed through to notification
/// bodies.
static GLYPH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\u{2500}-\u{25FF}\u{2800}-\u{28FF}]").expect("static glyph regex")
});

/// `key=value` / `"key": "value"` / `key: value` assignments whose key looks
static KEY_ASSIGN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b((?:api|access|auth|secret|client|private|public)[_-]?(?:key|token|secret)s?|password|passwd|token|bearer|authorization|credentials?)\b("?)(\s*[=:]\s*)("[^"]{3,}"|'[^']{3,}'|[^\s"',;)}\]]{4,})"#)
        .expect("static key-assign regex")
});

/// Environment-variable-shaped secrets: `MY_API_KEY=abcd...`.
static ENV_SECRET_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\b([A-Z][A-Z0-9_]{2,}(?:KEY|TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIALS?))=("[^"]{3,}"|[^\s"']{4,})"#)
        .expect("static env-secret regex")
});

/// Well-known token shapes: OpenAI-style `sk-…`, GitHub `ghp_/gho_/…`,
/// Slack `xox…`, AWS access-key ids, Google `AIza…`, JWT prefixes.
static TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(?:sk-[A-Za-z0-9_-]{8,}|ghp_[A-Za-z0-9]{15,}|gho_[A-Za-z0-9]{15,}|ghu_[A-Za-z0-9]{15,}|ghs_[A-Za-z0-9]{15,}|ghr_[A-Za-z0-9]{15,}|github_pat_[A-Za-z0-9_]{20,}|xox[baprs]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16}|ASIA[0-9A-Z]{16}|AIza[0-9A-Za-z_-]{30,}|eyJ[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,})\b",
    )
    .expect("static token regex")
});

/// PEM private-key blocks (single line already stripped of newlines is
/// handled by the prefix-only arm below; multi-line blocks by this one).
static PEM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----")
        .expect("static PEM regex")
});

/// Credentials embedded in URLs: `https://user:pass@host/`.
static URL_CREDS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(https?://)[^\s/:@]+:[^\s/@]+@").expect("static url-creds regex")
});

/// Strips ANSI escape sequences (CSI, OSC, DCS, and two-byte escapes).
pub fn strip_ansi(input: &str) -> String {
    ANSI_RE.replace_all(input, "").into_owned()
}

/// Removes control characters (keeping `\t`/`\n`), turns tabs into spaces.
pub fn strip_controls(input: &str) -> String {
    let no_ctrl = CONTROL_RE.replace_all(input, "");
    no_ctrl.replace('\t', " ")
}

/// Replaces box-drawing/block/geometric/braille glyphs with spaces.
pub fn strip_frame_glyphs(input: &str) -> String {
    GLYPH_RE.replace_all(input, " ").into_owned()
}

/// Collapses runs of whitespace to single spaces and trims the ends.
pub fn normalize_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Applies [`strip_ansi`], [`strip_controls`], and [`strip_frame_glyphs`],
/// then collapses whitespace. Newlines are removed — use [`clean_lines`] when
/// line structure matters.
pub fn clean_flat(input: &str) -> String {
    normalize_whitespace(&strip_frame_glyphs(&strip_controls(&strip_ansi(input))))
}

/// Line-oriented variant of [`clean_flat`] that preserves line boundaries and
/// drops lines that become empty.
pub fn clean_lines(input: &str) -> Vec<String> {
    strip_ansi(input)
        .lines()
        .map(|line| normalize_whitespace(&strip_frame_glyphs(&strip_controls(line))))
        .filter(|line| !line.is_empty())
        .collect()
}

/// Redacts obvious secrets: credential-shaped assignments, well-known token
/// prefixes, PEM blocks, URL userinfo, and `*_KEY=…`-style env assignments.
pub fn redact(input: &str) -> String {
    let s = URL_CREDS_RE.replace_all(input, "${1}[redacted]@");
    let s = PEM_RE.replace_all(&s, "[redacted private key]");
    let s = KEY_ASSIGN_RE.replace_all(&s, "${1}${2}${3}[redacted]");
    let s = ENV_SECRET_RE.replace_all(&s, "${1}=[redacted]");
    TOKEN_RE.replace_all(&s, "[redacted]").into_owned()
}
/// Truncates to at most `max_chars` characters on a `char` boundary, adding
/// an ellipsis when truncation happens. Prefers breaking at a word boundary
/// near the limit when one exists.
pub fn truncate_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }
    let keep = max_chars.saturating_sub(1); // room for the ellipsis
    let prefix: String = input.chars().take(keep).collect();
    // Prefer ending on a word boundary within the last 40% of the budget.
    let floor = keep * 3 / 5;
    if let Some(idx) = prefix.rfind(|c: char| c.is_whitespace()) {
        let char_idx = prefix[..idx].chars().count();
        if char_idx >= floor && char_idx > 0 {
            let cut: String = prefix.chars().take(char_idx).collect();
            return format!("{cut}…");
        }
    }
    format!("{prefix}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_and_osc_sequences() {
        assert_eq!(strip_ansi("\x1B[31mred\x1B[0m"), "red");
        assert_eq!(strip_ansi("\x1B]0;title\x07body"), "body");
        assert_eq!(strip_ansi("\x1B]2;title\x1B\\body"), "body");
        assert_eq!(strip_ansi("a\x1B[Mb"), "ab");
    }

    #[test]
    fn strips_control_characters() {
        assert_eq!(strip_controls("a\u{7}b\u{1}c"), "abc");
        assert_eq!(strip_controls("tab\there"), "tab here");
        assert_eq!(strip_controls("keep\nnewline"), "keep\nnewline");
    }

    #[test]
    fn strips_frame_and_spinner_glyphs() {
        assert_eq!(strip_frame_glyphs("│no│"), " no ");
        assert_eq!(strip_frame_glyphs("⠇spin"), " spin");
        // Replacement is a space; the flat cleaner normalises it away.
        assert_eq!(strip_frame_glyphs("▸bullet"), " bullet");
        assert_eq!(clean_flat("▸bullet"), "bullet");
    }

    #[test]
    fn normalizes_whitespace() {
        assert_eq!(normalize_whitespace("  a   b  "), "a b");
        assert_eq!(normalize_whitespace("a\n\tb"), "a b");
    }

    #[test]
    fn clean_flat_end_to_end() {
        let raw = "\x1B[1m│ │\x1B[0m Done:  3   passed";
        assert_eq!(clean_flat(raw), "Done: 3 passed");
    }

    #[test]
    fn clean_lines_preserves_structure() {
        let raw = "one\n│ two │\n\nthree";
        assert_eq!(clean_lines(raw), vec!["one", "two", "three"]);
    }

    #[test]
    fn redacts_key_assignments() {
        assert_eq!(
            redact("export API_KEY=supersecret123 now"),
            "export API_KEY=[redacted] now"
        );
        assert_eq!(redact("password: hunter2rest"), "password: [redacted]");
        assert_eq!(
            redact(r#""token": "abcdef12345""#),
            r#""token": [redacted]"#
        );
    }

    #[test]
    fn redacts_known_token_shapes() {
        assert_eq!(redact("sk-abcdefgh12345678"), "[redacted]");
        assert_eq!(redact("ghp_abcdefghijklmnopqrst"), "[redacted]");
        assert_eq!(redact("AKIAIOSFODNN7EXAMPLE"), "[redacted]");
        let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        assert_eq!(redact(jwt), "[redacted]");
    }

    #[test]
    fn redacts_pem_and_url_credentials() {
        assert!(
            redact("-----BEGIN RSA PRIVATE KEY-----MIIEow-----END RSA PRIVATE KEY-----")
                .contains("[redacted private key]")
        );
        assert_eq!(
            redact("https://user:pass123@example.com/x"),
            "https://[redacted]@example.com/x"
        );
    }

    #[test]
    fn keeps_ordinary_text_intact() {
        let plain = "Approve running: cargo sqlx prepare -- --force";
        assert_eq!(redact(plain), plain);
    }

    #[test]
    fn truncates_on_char_boundaries() {
        let emoji = "🦀🦀🦀🦀";
        assert_eq!(truncate_chars(emoji, 3), "🦀🦀…");
        assert_eq!(truncate_chars("short", 10), "short");
    }

    #[test]
    fn truncation_prefers_word_boundaries() {
        assert_eq!(
            truncate_chars("approve cargo sqlx prepare workspace offline", 20),
            "approve cargo sqlx…"
        );
    }
}
