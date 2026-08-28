//! Herdr API access behind a trait so notification logic is unit-testable
//! without a live herdr server.
//!
//! The real implementation shells out to the herdr binary herdr hands every
//! plugin process via `$HERDR_BIN_PATH` (falling back to `herdr` on PATH),
//! keeping `$HERDR_SOCKET_PATH` from the environment so every call targets
//! the same session that emitted the event — never an unrelated default
//! server.

use std::process::Command;

use serde::Deserialize;

/// Subset of `herdr agent get`'s `.result.agent` object the plugin uses.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentInfo {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub agent: Option<String>,
    pub agent_status: String,
    pub cwd: Option<String>,
    pub terminal_title_stripped: Option<String>,
    pub focused: bool,
}

/// Subset of `herdr workspace list`'s workspace objects.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    pub label: Option<String>,
    pub focused: bool,
    pub active_tab_id: Option<String>,
}

/// Subset of `herdr agent explain --json` output.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExplainInfo {
    pub state: String,
    pub matched_rule_id: Option<String>,
    pub screen_detection_skipped: bool,
}

#[derive(Debug)]
pub enum ApiError {
    /// herdr answered with a JSON `{"error":{...}}` object.
    Herdr { code: String, message: String },
    /// The binary could not be spawned or answered.
    Spawn(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Herdr { code, message } => write!(f, "herdr error {code}: {message}"),
            ApiError::Spawn(e) => write!(f, "herdr spawn failed: {e}"),
        }
    }
}

pub trait HerdrApi {
    fn agent_get(&self, pane_id: &str) -> Result<AgentInfo, ApiError>;
    fn workspace_of(&self, workspace_id: &str) -> Result<Option<WorkspaceInfo>, ApiError>;
    fn tab_label(&self, tab_id: &str) -> Result<Option<String>, ApiError>;
    fn explain(&self, pane_id: &str) -> Result<Option<ExplainInfo>, ApiError>;
    fn read_detection(&self, pane_id: &str, lines: usize) -> Result<String, ApiError>;
    fn agent_focus(&self, pane_id: &str) -> Result<(), ApiError>;
}

/// Production implementation: `herdr …` subprocesses against the event's
/// session socket.
#[derive(Debug, Clone)]
pub struct CliHerdr {
    bin: String,
    socket_path: Option<String>,
}

impl CliHerdr {
    /// Client using the ambient plugin environment.
    pub fn from_env() -> Self {
        Self {
            bin: std::env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_string()),
            socket_path: std::env::var("HERDR_SOCKET_PATH").ok(),
        }
    }

    /// Client pinned to a notification click target's captured session
    /// identity (`HERDR_BIN_PATH`/`HERDR_SOCKET_PATH` from event time), so
    /// the focus command talks to the same herdr session that emitted the
    /// event even if the ambient environment differs.
    pub fn from_target(bin: Option<&str>, socket: Option<&str>) -> Self {
        Self {
            bin: bin
                .filter(|b| !b.is_empty())
                .map(String::from)
                .or_else(|| std::env::var("HERDR_BIN_PATH").ok())
                .unwrap_or_else(|| "herdr".to_string()),
            socket_path: socket
                .filter(|s| !s.is_empty())
                .map(String::from)
                .or_else(|| std::env::var("HERDR_SOCKET_PATH").ok()),
        }
    }

    fn run_json(&self, args: &[&str]) -> Result<serde_json::Value, ApiError> {
        let stdout = self.run_capture(args)?;
        // herdr CLI prints result and error envelopes on stdout as JSON.
        let value: serde_json::Value = serde_json::from_str(stdout.trim()).map_err(|e| {
            ApiError::Spawn(format!(
                "unparseable {} output: {e} (stderr captured)",
                args.first().copied().unwrap_or("herdr"),
            ))
        })?;
        Self::error_from(&value)?;
        Ok(value)
    }

    fn run_text(&self, args: &[&str]) -> Result<String, ApiError> {
        let stdout = self.run_capture(args)?;
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout.trim()) {
            Self::error_from(&value)?;
        }
        Ok(stdout)
    }

    fn run_capture(&self, args: &[&str]) -> Result<String, ApiError> {
        let mut command = Command::new(&self.bin);
        command.args(args);
        if let Some(socket) = &self.socket_path {
            command.env("HERDR_SOCKET_PATH", socket);
        }
        let output = command
            .output()
            .map_err(|e| ApiError::Spawn(e.to_string()))?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn error_from(value: &serde_json::Value) -> Result<(), ApiError> {
        if let Some(error) = value.get("error").and_then(|e| e.as_object()) {
            return Err(ApiError::Herdr {
                code: error
                    .get("code")
                    .and_then(|c| c.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                message: error
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or_default()
                    .to_string(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct RawAgent {
    #[serde(default)]
    pane_id: String,
    #[serde(default)]
    workspace_id: String,
    #[serde(default)]
    tab_id: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    agent_status: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    terminal_title_stripped: Option<String>,
    #[serde(default)]
    focused: bool,
}

#[derive(Debug, Deserialize)]
struct RawWorkspace {
    #[serde(default)]
    workspace_id: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    focused: bool,
    #[serde(default)]
    active_tab_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawTab {
    #[serde(default)]
    tab_id: String,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawExplain {
    #[serde(default)]
    state: String,
    #[serde(default)]
    matched_rule: Option<RawMatchedRule>,
    #[serde(default)]
    screen_detection_skipped: bool,
}

#[derive(Debug, Deserialize)]
struct RawMatchedRule {
    #[serde(default)]
    id: String,
}

impl HerdrApi for CliHerdr {
    fn agent_get(&self, pane_id: &str) -> Result<AgentInfo, ApiError> {
        let value = self.run_json(&["agent", "get", pane_id])?;
        let raw: RawAgent =
            serde_json::from_value(value.pointer("/result/agent").cloned().unwrap_or_default())
                .map_err(|e| ApiError::Spawn(format!("agent get shape: {e}")))?;
        Ok(AgentInfo {
            pane_id: raw.pane_id,
            workspace_id: raw.workspace_id,
            tab_id: raw.tab_id,
            agent: raw.agent,
            agent_status: raw.agent_status,
            cwd: raw.cwd,
            terminal_title_stripped: raw.terminal_title_stripped,
            focused: raw.focused,
        })
    }

    fn workspace_of(&self, workspace_id: &str) -> Result<Option<WorkspaceInfo>, ApiError> {
        let value = self.run_json(&["workspace", "list"])?;
        let raws: Vec<RawWorkspace> = serde_json::from_value(
            value
                .pointer("/result/workspaces")
                .cloned()
                .unwrap_or_default(),
        )
        .map_err(|e| ApiError::Spawn(format!("workspace list shape: {e}")))?;
        Ok(raws
            .into_iter()
            .find(|w| w.workspace_id == workspace_id)
            .map(|w| WorkspaceInfo {
                workspace_id: w.workspace_id,
                label: w.label,
                focused: w.focused,
                active_tab_id: w.active_tab_id,
            }))
    }

    fn tab_label(&self, tab_id: &str) -> Result<Option<String>, ApiError> {
        let value = self.run_json(&["tab", "list"])?;
        let raws: Vec<RawTab> =
            serde_json::from_value(value.pointer("/result/tabs").cloned().unwrap_or_default())
                .map_err(|e| ApiError::Spawn(format!("tab list shape: {e}")))?;
        Ok(raws
            .into_iter()
            .find(|t| t.tab_id == tab_id)
            .and_then(|t| t.label))
    }

    fn explain(&self, pane_id: &str) -> Result<Option<ExplainInfo>, ApiError> {
        let value = self.run_json(&["agent", "explain", pane_id, "--json"])?;
        // explain prints the object directly (no {"id","result"} envelope).
        let raw: RawExplain = serde_json::from_value(value)
            .map_err(|e| ApiError::Spawn(format!("explain shape: {e}")))?;
        Ok(Some(ExplainInfo {
            state: raw.state,
            matched_rule_id: raw.matched_rule.filter(|r| !r.id.is_empty()).map(|r| r.id),
            screen_detection_skipped: raw.screen_detection_skipped,
        }))
    }

    fn read_detection(&self, pane_id: &str, lines: usize) -> Result<String, ApiError> {
        self.run_text(&[
            "agent",
            "read",
            pane_id,
            "--source",
            "detection",
            "--lines",
            &lines.to_string(),
        ])
    }

    fn agent_focus(&self, pane_id: &str) -> Result<(), ApiError> {
        self.run_json(&["agent", "focus", pane_id]).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_agent_get_envelope() {
        let raw = r#"{"id":"cli:agent:get","result":{"agent":{"agent":"omp","agent_status":"blocked","cwd":"/home/liam/git/infra","focused":false,"pane_id":"w1:pK","tab_id":"w1:t7","terminal_title_stripped":"π ! Fix jabra headset audio output","workspace_id":"w1"},"type":"agent_info"}}"#;
        let value: serde_json::Value = serde_json::from_str(raw).unwrap();
        let info: RawAgent =
            serde_json::from_value(value.pointer("/result/agent").cloned().unwrap()).unwrap();
        assert_eq!(info.agent.as_deref(), Some("omp"));
        assert_eq!(info.agent_status, "blocked");
        assert_eq!(info.tab_id, "w1:t7");
        assert!(!info.focused);
    }

    #[test]
    fn parses_explain_shapes() {
        // Screen-manifest agent: matched rule present.
        let with_rule = r#"{"agent":"codex","state":"blocked","matched_rule":{"id":"blocked:approval","priority":5,"region":"bottom","state":"blocked"},"screen_detection_skipped":false}"#;
        let raw: RawExplain = serde_json::from_str(with_rule).unwrap();
        assert_eq!(raw.matched_rule.as_ref().unwrap().id, "blocked:approval");
        // Lifecycle-hook agent (e.g. OMP): no rule, detection skipped.
        let skipped = r#"{"agent":"omp","state":"blocked","matched_rule":null,"screen_detection_skipped":true,"screen_detection_skip_reason":"full_lifecycle_hook_authority"}"#;
        let raw: RawExplain = serde_json::from_str(skipped).unwrap();
        assert!(raw.matched_rule.is_none());
        assert!(raw.screen_detection_skipped);
    }

    #[test]
    fn parses_workspace_and_tab_lists() {
        let ws = r#"{"id":"cli:workspace:list","result":{"type":"workspace_list","workspaces":[{"active_tab_id":"w1:t9","agent_status":"blocked","focused":true,"label":"infra","workspace_id":"w1"}]}}"#;
        let value: serde_json::Value = serde_json::from_str(ws).unwrap();
        let raws: Vec<RawWorkspace> =
            serde_json::from_value(value.pointer("/result/workspaces").cloned().unwrap()).unwrap();
        assert_eq!(raws[0].label.as_deref(), Some("infra"));
        assert!(raws[0].focused);
        assert_eq!(raws[0].active_tab_id.as_deref(), Some("w1:t9"));

        let tabs = r#"{"id":"cli:tab:list","result":{"tabs":[{"label":"headset","tab_id":"w1:t7","workspace_id":"w1"}]}}"#;
        let value: serde_json::Value = serde_json::from_str(tabs).unwrap();
        let raws: Vec<RawTab> =
            serde_json::from_value(value.pointer("/result/tabs").cloned().unwrap()).unwrap();
        assert_eq!(raws[0].label.as_deref(), Some("headset"));
    }
}
