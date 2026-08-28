//! Heuristic extraction of the relevant prompt text from a herdr detection
//! snapshot (the live bottom-buffer plain-text view of an agent pane).
//!
//! Deterministic and local: no model calls, ever. Two entry points:
//!
//! * [`extract_question`] — for `blocked` agents, find the line the agent is
//!   asking the user (approval wording, question dialog, permission prompt);
//! * [`extract_tail_line`] — for `done` agents, the last meaningful output
//!   line as a tiny completion excerpt.
//!
//! Both work on cleaned lines (see [`crate::sanitize::clean_lines`]) and are
//! pure functions over text, so they are unit-testable without a live herdr.

/// How many trailing snapshot lines the heuristics consider at most.
const TAIL_LINES: usize = 30;

/// Minimum characters for a line to be considered "substantial" content.
const MIN_SUBSTANTIAL: usize = 12;

/// Header words that introduce a prompt block in common agent UIs
/// (OMP's "Ask" box, Claude Code permission sheets, generic tools).
const PROMPT_HEADERS: [&str; 10] = [
    "ask",
    "question",
    "approve",
    "approval",
    "permission",
    "confirm",
    "allow",
    "trust",
    "proceed",
    "continue",
];

/// True for lines that are keybinding help chrome, e.g.
/// "Enter select · n note · ↑↓ move · Esc cancel".
fn is_help_line(line: &str) -> bool {
    let has_esc = line.contains("Esc ");
    let has_enter = line.contains("Enter ");
    let has_arrows = line.contains("↑↓") || line.contains("move") || line.contains("navigate");
    (has_esc || has_enter) && (has_arrows || has_esc)
}

/// True for option/bullet rows inside a prompt dialog ("◈ Yes, spoke",
/// "❯ 1. Yes", "2. Yes, and auto-accept edits").
fn is_option_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    let mut chars = trimmed.chars();
    match chars.next() {
        Some(
            '◈' | '◆' | '●' | '◉' | '○' | '❯' | '➜' | '›' | '>' | '*' | '-' | '·' | '•' | '►' | '▸',
        ) => true,
        Some(c) if c.is_ascii_digit() => matches!(chars.next(), Some('.' | ')')),
        _ => false,
    }
}

/// True when a cleaned line is (only) a prompt-box header, e.g. "Ask",
/// "Approve", "Permission request".
fn prompt_header_word(line: &str) -> Option<&'static str> {
    let lowered = line.to_ascii_lowercase();
    let trimmed = lowered.trim_end_matches(':').trim();
    if trimmed.chars().count() > 24 {
        return None;
    }
    PROMPT_HEADERS
        .iter()
        .find(|word| trimmed == **word || trimmed == format!("{}?", word))
        .copied()
}

/// A cleaned line is a question candidate when it is substantial prose and
/// not help/option chrome.
fn is_content_line(line: &str) -> bool {
    line.chars().count() >= MIN_SUBSTANTIAL && !is_help_line(line) && !is_option_line(line)
}

/// Finds the text the blocked agent is waiting on, if one can be located.
///
/// Strategy, in order:
/// 1. the first content line after the last prompt-box header ("Ask",
///    "Approve", …) in the trailing window;
/// 2. the last content line ending in `?`;
/// 3. the longest content line in the trailing window (prompt sentences carry
///    the payload; transcripts and option blurbs are shorter).
pub fn extract_question(snapshot: &str, max_chars: usize) -> Option<String> {
    let lines = tail_lines(snapshot);
    if lines.is_empty() {
        return None;
    }

    // 1. content line following the last prompt header.
    if let Some(header_idx) = lines.iter().rposition(|l| prompt_header_word(l).is_some())
        && let Some(question) = lines
            .iter()
            .skip(header_idx + 1)
            .find(|l| is_content_line(l))
    {
        return Some(crate::sanitize::truncate_chars(
            &crate::sanitize::redact(question),
            max_chars,
        ));
    }

    // 2. last content line that reads as a question.
    if let Some(question) = lines
        .iter()
        .rev()
        .find(|l| is_content_line(l) && l.ends_with('?'))
    {
        return Some(crate::sanitize::truncate_chars(
            &crate::sanitize::redact(question),
            max_chars,
        ));
    }

    // 3. longest content line in the window.
    lines
        .iter()
        .filter(|l| is_content_line(l))
        .max_by_key(|l| l.chars().count())
        .map(|q| crate::sanitize::truncate_chars(&crate::sanitize::redact(q), max_chars))
}

/// Finds a small final-output excerpt for a `done` agent: the last
/// substantial, non-chrome line of the trailing window.
pub fn extract_tail_line(snapshot: &str, max_chars: usize) -> Option<String> {
    tail_lines(snapshot)
        .iter()
        .rev()
        .find(|l| is_content_line(l))
        .map(|l| crate::sanitize::truncate_chars(&crate::sanitize::redact(l), max_chars))
}

fn tail_lines(snapshot: &str) -> Vec<String> {
    let mut lines = crate::sanitize::clean_lines(snapshot);
    if lines.len() > TAIL_LINES {
        lines.drain(..lines.len() - TAIL_LINES);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real captured OMP `Ask` dialog from a live blocked pane (whitespace
    /// collapsed the way `clean_lines` produces it).
    const OMP_ASK: &str = "
│ ⏺ Capturing speech sample from Jabra mic ⟱esc⟰
╭─ Ask ───────────────────────────────────────╮
│ Recording the Jabra mic right now for 25 seconds — please say a few sentences out loud, then answer. │
╰──────────────────────────────────────────────╯
 ◈ Yes, spoke
       I spoke out loud while this prompt was up.
   ◈ Didn't speak
       I stayed quiet — treat recording as ambient-only.
   ◈ Other (type your own)
 Enter select · n note · ↑↓ move · Esc cancel
";

    #[test]
    fn extracts_omp_ask_question() {
        let q = extract_question(OMP_ASK, 160).unwrap();
        assert!(q.starts_with("Recording the Jabra mic"), "{q}");
        assert!(q.contains("please say a few sentences"), "{q}");
    }

    #[test]
    fn extracts_omp_question_with_short_limit() {
        let q = extract_question(OMP_ASK, 40).unwrap();
        assert!(q.chars().count() <= 40, "{q}");
        assert!(q.ends_with('…'), "{q}");
    }

    #[test]
    fn question_line_is_redacted() {
        let snapshot = "Approve\nPlease confirm API_TOKEN=supersecretvalue123 for this run\n Enter select · Esc cancel";
        let q = extract_question(snapshot, 200).unwrap();
        assert!(q.contains("API_TOKEN=[redacted]"), "{q}");
    }

    #[test]
    fn prefers_last_question_mark_line_without_header() {
        let snapshot = "building docs\nwhich migration strategy should we use?\n ❯ 1. Option one\n ❯ 2. Option two";
        let q = extract_question(snapshot, 200).unwrap();
        assert_eq!(q, "which migration strategy should we use?");
    }

    #[test]
    fn falls_back_to_longest_content_line() {
        let snapshot =
            "first line\nthis line is considerably longer than the others around it\nshort";
        let q = extract_question(snapshot, 200).unwrap();
        assert!(q.starts_with("this line is considerably"), "{q}");
    }

    #[test]
    fn extracts_tail_line_for_done() {
        let tail = extract_tail_line(
            "cargo test\nrunning 84 tests\ntest result: ok. 84 passed",
            120,
        )
        .unwrap();
        assert_eq!(tail, "test result: ok. 84 passed");
    }

    #[test]
    fn tail_skips_help_chrome() {
        let snapshot = "test result: ok. 84 passed; 0 failed\n Enter select · Esc cancel";
        let tail = extract_tail_line(snapshot, 120).unwrap();
        assert_eq!(tail, "test result: ok. 84 passed; 0 failed");
    }

    #[test]
    fn returns_none_on_empty_snapshot() {
        assert!(extract_question("", 100).is_none());
        assert!(extract_question("   \n \n", 100).is_none());
        assert!(extract_tail_line("", 100).is_none());
    }

    #[test]
    fn help_line_detection() {
        assert!(is_help_line("Enter select · n note · ↑↓ move · Esc cancel"));
        assert!(!is_help_line(
            "Enter your password to continue the operation"
        ));
    }

    #[test]
    fn option_line_detection() {
        assert!(is_option_line("◈ Yes, spoke"));
        assert!(is_option_line("1. Yes, and auto-accept edits"));
        assert!(is_option_line("❯ Deploy to production"));
        assert!(!is_option_line("Deploy to production now please"));
    }

    #[test]
    fn ansi_and_box_noise_survives_cleaning() {
        let snapshot = "\x1B[1m\x1B[32m╭─ Ask ─────────╮\x1B[0m\n│ \x1B[36mApprove running: cargo sqlx prepare\x1B[0m │\n╰───────────────╯";
        let q = extract_question(snapshot, 200).unwrap();
        // "Ask" header is skipped as a header, the approval line is the content.
        assert!(q.contains("Approve running"), "{q}");
    }
}
