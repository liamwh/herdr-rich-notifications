//! Notification content construction — deterministic, local, zero-LLM.
//!
//! Inputs are herdr's own structured data (agent kind, workspace/tab labels,
//! stripped terminal title, detection explain, extracted prompt text) and the
//! output is a two-line body plus a title:
//!
//! ```text
//! OMP needs input · infra
//! headset › Fix jabra headset audio output
//! Recording the Jabra mic right now for 25 seconds — please say …
//! ```
//!
//! Everything is sanitised ([`crate::sanitize`]) and length-bounded before it
//! reaches the notification daemon.

use crate::config::Detail;
use crate::herdr_api::{AgentInfo, ExplainInfo};
use crate::sanitize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Blocked,
    Done,
}

impl StatusKind {
    pub fn from_status(status: &str) -> Option<Self> {
        match status {
            "blocked" => Some(Self::Blocked),
            "done" => Some(Self::Done),
            _ => None,
        }
    }

    pub fn headline(self, agent: &str) -> String {
        match self {
            Self::Blocked => format!("{agent} needs input"),
            Self::Done => format!("{agent} finished"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationContent {
    pub title: String,
    pub body: String,
    pub kind: StatusKind,
}

const TITLE_MAX: usize = 96;
const LINE_MAX: usize = 180;

/// Pretty agent-kind names for well-known agents; anything else keeps its
/// label with the first character upper-cased.
pub fn agent_display_name(raw: &str) -> String {
    let lowered = raw.trim().to_ascii_lowercase();
    match lowered.as_str() {
        "omp" | "omp-agent" => "OMP".to_string(),
        "pi" => "Pi".to_string(),
        "claude" | "claude-code" => "Claude".to_string(),
        "codex" => "Codex".to_string(),
        "gemini" | "gemini-cli" => "Gemini".to_string(),
        "copilot" | "github-copilot" => "Copilot".to_string(),
        "cursor" | "cursor-agent" => "Cursor".to_string(),
        "droid" => "Droid".to_string(),
        "devin" => "Devin".to_string(),
        "amp" => "Amp".to_string(),
        "grok" => "Grok".to_string(),
        "hermes" => "Hermes".to_string(),
        "opencode" => "OpenCode".to_string(),
        "cline" => "Cline".to_string(),
        "kimi" => "Kimi".to_string(),
        "qwen" => "Qwen".to_string(),
        "" => "Agent".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => "Agent".to_string(),
            }
        }
    }
}

/// Strips the leading status glyph cluster herdr agents put in terminal
/// titles ("π > task", "π ! task", "⠇ task") so the task reads cleanly.
pub fn clean_task_title(raw: &str) -> String {
    let flat = sanitize::clean_flat(raw);
    // 1. Drop a leading run of non-alphanumeric glyphs ("⠇ ", "✻ ", "! ").
    let stripped = flat
        .trim_start_matches(|c: char| !c.is_alphanumeric() && c != '(')
        .trim_start();
    // 2. Drop an "π > "-style cluster: one non-ASCII status letter followed
    //    by a separator run, but only when real text follows.
    let mut chars = stripped.chars();
    if let Some(first) = chars.next()
        && !first.is_ascii()
    {
        let rest = chars.as_str();
        let after_sep = rest.trim_start_matches(|c: char| !c.is_alphanumeric() && c != '(');
        if after_sep.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '(') {
            return after_sep.trim().to_string();
        }
    }
    stripped.trim().to_string()
}

/// Workspace label, falling back to the cwd basename when the workspace has
/// no label.
pub fn workspace_context(info: &AgentInfo, ws_label: Option<&str>) -> Option<String> {
    if let Some(label) = ws_label.filter(|l| !l.trim().is_empty()) {
        return Some(label.trim().to_string());
    }
    info.cwd
        .as_deref()
        .and_then(|cwd| std::path::Path::new(cwd).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
}

/// Friendly, deterministic text for an `agent explain` matched rule id
/// ("blocked:ask-dialog" → "Asking a question").
fn friendly_rule(rule_id: &str) -> Option<String> {
    let slug = rule_id.rsplit(':').next()?.trim();
    if slug.is_empty() {
        return None;
    }
    let lowered = slug.to_ascii_lowercase();
    let phrase = if lowered.contains("ask") || lowered.contains("question") {
        "Asking a question"
    } else if lowered.contains("approv")
        || lowered.contains("permission")
        || lowered.contains("trust")
        || lowered.contains("allow")
    {
        "Waiting for approval"
    } else if lowered.contains("confirm") {
        "Waiting for confirmation"
    } else if lowered.contains("finish") || lowered.contains("done") || lowered.contains("idle") {
        "Finished"
    } else {
        // Deterministic fallback: humanised rule slug.
        let mut out = String::new();
        for (i, word) in lowered
            .split(['-', '_'])
            .filter(|w| !w.is_empty())
            .enumerate()
        {
            if i > 0 {
                out.push(' ');
            }
            let mut chars = word.chars();
            if let Some(first) = chars.next() {
                out.extend(first.to_uppercase());
                out.push_str(chars.as_str());
            }
        }
        return (!out.is_empty()).then_some(out);
    };
    Some(phrase.to_string())
}

/// Inputs to [`build`], gathered to keep the signature small.
pub struct EnrichInput<'a> {
    pub kind: StatusKind,
    pub info: &'a AgentInfo,
    /// Workspace display label (falls back to the cwd basename internally).
    pub ws_label: Option<&'a str>,
    pub tab_label: Option<&'a str>,
    /// `herdr agent explain` result, when it was fetched.
    pub explain: Option<&'a ExplainInfo>,
    /// Extracted prompt text for blocked agents (rich detail).
    pub question: Option<&'a str>,
    /// Small final-output excerpt for done agents (rich detail).
    pub tail: Option<&'a str>,
    pub detail: Detail,
}

/// Builds the notification title and body from herdr's own metadata.
pub fn build(input: EnrichInput<'_>) -> NotificationContent {
    let EnrichInput {
        kind,
        info,
        ws_label,
        tab_label,
        explain,
        question,
        tail,
        detail,
    } = input;
    let agent = agent_display_name(
        info.agent
            .as_deref()
            .filter(|a| !a.trim().is_empty())
            .unwrap_or("agent"),
    );

    // Title: "<Agent> needs input · <context>"
    let context = workspace_context(info, ws_label);
    let title = match &context {
        Some(label) => format!(
            "{} · {}",
            kind.headline(&agent),
            sanitize::clean_flat(label)
        ),
        None => kind.headline(&agent),
    };
    let title = sanitize::truncate_chars(&sanitize::redact(&title), TITLE_MAX);

    // Body line 1: "<tab> › <task>"
    let task = info
        .terminal_title_stripped
        .as_deref()
        .map(clean_task_title)
        .filter(|t| !t.is_empty());
    let location = tab_label
        .map(sanitize::clean_flat)
        .filter(|l| !l.is_empty());
    let line1 = match (&location, &task) {
        (Some(tab), Some(task)) => format!("{tab} › {task}"),
        (None, Some(task)) => task.clone(),
        (Some(tab), None) => tab.clone(),
        (None, None) => context.clone().unwrap_or_default(),
    };

    // Body line 2: state-specific explanation.
    let line2 = match kind {
        StatusKind::Blocked => blocked_reason(explain, question, detail),
        StatusKind::Done => done_reason(explain, tail, detail),
    };

    let body = [line1, line2]
        .into_iter()
        .filter(|line| !line.is_empty())
        .map(|line| sanitize::truncate_chars(&sanitize::redact(&line), LINE_MAX))
        .collect::<Vec<_>>()
        .join("\n");

    NotificationContent { title, body, kind }
}

fn blocked_reason(explain: Option<&ExplainInfo>, question: Option<&str>, detail: Detail) -> String {
    if detail == Detail::Rich
        && let Some(question) = question.filter(|q| !q.trim().is_empty())
    {
        return question.trim().to_string();
    }
    if let Some(rule) = explain
        .and_then(|e| e.matched_rule_id.as_deref())
        .and_then(friendly_rule)
    {
        return rule;
    }
    "Agent needs input".to_string()
}

fn done_reason(explain: Option<&ExplainInfo>, tail: Option<&str>, detail: Detail) -> String {
    if detail == Detail::Rich
        && let Some(tail) = tail.filter(|t| !t.trim().is_empty())
    {
        return format!("Finished — {}", tail.trim());
    }
    if let Some(rule) = explain
        .and_then(|e| e.matched_rule_id.as_deref())
        .and_then(friendly_rule)
    {
        return rule;
    }
    "Finished".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(agent: Option<&str>, title: Option<&str>, cwd: Option<&str>) -> AgentInfo {
        AgentInfo {
            pane_id: "w1:pK".into(),
            workspace_id: "w1".into(),
            tab_id: "w1:t7".into(),
            agent: agent.map(String::from),
            agent_status: "blocked".into(),
            cwd: cwd.map(String::from),
            terminal_title_stripped: title.map(String::from),
            focused: false,
        }
    }

    #[test]
    fn agent_names_are_prettified() {
        assert_eq!(agent_display_name("omp"), "OMP");
        assert_eq!(agent_display_name("claude"), "Claude");
        assert_eq!(agent_display_name("codex"), "Codex");
        assert_eq!(agent_display_name("my-reviewer"), "My-reviewer");
        assert_eq!(agent_display_name(""), "Agent");
    }

    #[test]
    fn task_titles_lose_status_glyphs() {
        assert_eq!(
            clean_task_title("π > Deploy hermes-agent on zeus"),
            "Deploy hermes-agent on zeus"
        );
        assert_eq!(
            clean_task_title("π ! Fix jabra headset audio output"),
            "Fix jabra headset audio output"
        );
        assert_eq!(
            clean_task_title("⠇ Set up Qwen3.8 GGUF stack"),
            "Set up Qwen3.8 GGUF stack"
        );
        assert_eq!(clean_task_title("✻ Compiling v0.2.0"), "Compiling v0.2.0");
        // Titles that start with meaningful characters are untouched.
        assert_eq!(clean_task_title("3 tests passed"), "3 tests passed");
    }

    #[test]
    fn builds_blocked_content_with_question() {
        let i = info(
            Some("omp"),
            Some("π ! Fix jabra headset audio output"),
            Some("/home/liam/git/infra"),
        );
        let content = build(EnrichInput {
            kind: StatusKind::Blocked,
            info: &i,
            ws_label: Some("infra"),
            tab_label: Some("headset"),
            explain: None,
            question: Some("Recording the Jabra mic right now — please speak"),
            tail: None,
            detail: Detail::Rich,
        });
        assert_eq!(content.title, "OMP needs input · infra");
        assert_eq!(
            content.body,
            "headset › Fix jabra headset audio output\nRecording the Jabra mic right now — please speak"
        );
    }

    #[test]
    fn builds_done_content_with_tail() {
        let mut i = info(
            Some("omp"),
            Some("π > ship the release"),
            Some("/srv/evidia"),
        );
        i.agent_status = "done".into();
        let content = build(EnrichInput {
            kind: StatusKind::Done,
            info: &i,
            ws_label: Some("evidia"),
            tab_label: Some("email-inbox"),
            explain: None,
            question: None,
            tail: Some("cargo test passed"),
            detail: Detail::Rich,
        });
        assert_eq!(content.title, "OMP finished · evidia");
        assert_eq!(
            content.body,
            "email-inbox › ship the release\nFinished — cargo test passed"
        );
    }

    #[test]
    fn minimal_detail_suppresses_excerpts() {
        let i = info(Some("omp"), Some("π ! task"), Some("/w/evidia"));
        let content = build(EnrichInput {
            kind: StatusKind::Blocked,
            info: &i,
            ws_label: None,
            tab_label: Some("tab"),
            explain: None,
            question: Some("secret-ish question text"),
            tail: None,
            detail: Detail::Minimal,
        });
        assert_eq!(content.title, "OMP needs input · evidia");
        assert_eq!(content.body, "tab › task\nAgent needs input");
    }

    #[test]
    fn explain_rule_maps_to_friendly_reason() {
        let explain = ExplainInfo {
            state: "blocked".into(),
            matched_rule_id: Some("blocked:approval-dialog".into()),
            screen_detection_skipped: false,
        };
        let i = info(Some("codex"), None, Some("/w/evidia"));
        let content = build(EnrichInput {
            kind: StatusKind::Blocked,
            info: &i,
            ws_label: Some("evidia"),
            tab_label: None,
            explain: Some(&explain),
            question: None,
            tail: None,
            detail: Detail::Rich,
        });
        assert_eq!(content.title, "Codex needs input · evidia");
        assert!(
            content.body.contains("Waiting for approval"),
            "{}",
            content.body
        );
    }

    #[test]
    fn missing_metadata_still_builds_clean_content() {
        let i = info(None, None, None);
        let content = build(EnrichInput {
            kind: StatusKind::Blocked,
            info: &i,
            ws_label: None,
            tab_label: None,
            explain: None,
            question: None,
            tail: None,
            detail: Detail::Rich,
        });
        assert_eq!(content.title, "Agent needs input");
        assert_eq!(content.body, "Agent needs input");
    }

    #[test]
    fn secrets_never_reach_title_or_body() {
        let i = info(
            Some("omp"),
            Some("π > deploy API_KEY=sk-abcdefgh12345 now"),
            None,
        );
        let content = build(EnrichInput {
            kind: StatusKind::Done,
            info: &i,
            ws_label: Some("infra"),
            tab_label: Some("t"),
            explain: None,
            question: None,
            tail: Some("used token ghp_abcdefghijklmnopqrst to push"),
            detail: Detail::Rich,
        });
        assert!(
            !content.title.contains("sk-abcdefgh12345"),
            "{}",
            content.title
        );
        assert!(!content.body.contains("ghp_"), "{}", content.body);
        assert!(content.body.contains("[redacted]"), "{}", content.body);
    }

    #[test]
    fn long_content_is_truncated() {
        let long_title = "π > ".to_string() + &"very long task ".repeat(40);
        let i = info(Some("omp"), Some(&long_title), Some("/w/evidia"));
        let question = "question ".repeat(60);
        let content = build(EnrichInput {
            kind: StatusKind::Blocked,
            info: &i,
            ws_label: Some("evidia"),
            tab_label: Some("tab"),
            explain: None,
            question: Some(&question),
            tail: None,
            detail: Detail::Rich,
        });
        assert!(content.title.chars().count() <= 96, "{}", content.title);
        for line in content.body.lines() {
            assert!(line.chars().count() <= 180, "{line}");
        }
    }

    #[test]
    fn workspace_falls_back_to_cwd_basename() {
        let i = info(Some("omp"), None, Some("/home/liam/git/evidia"));
        assert_eq!(workspace_context(&i, None).as_deref(), Some("evidia"));
        assert_eq!(
            workspace_context(&i, Some("custom-ws")).as_deref(),
            Some("custom-ws")
        );
        assert_eq!(
            workspace_context(&info(Some("omp"), None, None), None),
            None
        );
    }

    #[test]
    fn friendly_rule_fallbacks() {
        assert_eq!(
            friendly_rule("blocked:ask-dialog").as_deref(),
            Some("Asking a question")
        );
        assert_eq!(
            friendly_rule("blocked:permission_sheet").as_deref(),
            Some("Waiting for approval")
        );
        assert_eq!(
            friendly_rule("done:build_complete").as_deref(),
            Some("Build Complete")
        );
        assert_eq!(friendly_rule("blocked:"), None);
    }
}
