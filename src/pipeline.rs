//! Post-delay notification decision: pure logic separating the event from
//! its side effects so suppression semantics are unit-testable.
//!
//! Mirrors herdr's own popup behaviour: a notification only fires when the
//! pane is still in the same state after the configured delay, is not on the
//! active tab of the focused workspace, and no newer transition has
//! superseded this one.

use crate::config::Config;
use crate::enrich::StatusKind;
use crate::herdr_api::{AgentInfo, WorkspaceInfo};

/// What herdr's APIs reported when the delay expired.
pub struct PostDelay<'a> {
    /// `agent get` result; `None` = pane gone or herdr unreachable.
    pub info: Option<&'a AgentInfo>,
    /// Workspace snapshot for the pane's workspace, when available.
    pub workspace: Option<&'a WorkspaceInfo>,
    /// Whether this event's dedup generation is still the pane's latest.
    pub still_latest: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Notify,
    Skip(&'static str),
}

pub fn decide(cfg: &Config, kind: StatusKind, post: PostDelay<'_>) -> Decision {
    let Some(info) = post.info else {
        return Decision::Skip("pane is gone or herdr is unreachable");
    };

    let expected = match kind {
        StatusKind::Blocked => "blocked",
        StatusKind::Done => "done",
    };
    if info.agent_status != expected {
        return Decision::Skip("stale: pane moved on during the delay");
    }

    if !post.still_latest {
        return Decision::Skip("superseded by a newer transition");
    }

    if cfg.suppress_active_tab
        && let Some(ws) = post.workspace
        && ws.focused
        && ws.active_tab_id.as_deref() == Some(info.tab_id.as_str())
    {
        return Decision::Skip("pane is on the active tab of the focused workspace");
    }

    Decision::Notify
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(status: &str) -> AgentInfo {
        AgentInfo {
            pane_id: "w1:pK".into(),
            workspace_id: "w1".into(),
            tab_id: "w1:t7".into(),
            agent: Some("omp".into()),
            agent_status: status.into(),
            cwd: Some("/w/infra".into()),
            terminal_title_stripped: Some("π ! task".into()),
            focused: false,
        }
    }

    fn ws(focused: bool, active_tab: Option<&str>) -> WorkspaceInfo {
        WorkspaceInfo {
            workspace_id: "w1".into(),
            label: Some("infra".into()),
            focused,
            active_tab_id: active_tab.map(String::from),
        }
    }

    fn post<'a>(
        info: Option<&'a AgentInfo>,
        workspace: Option<&'a WorkspaceInfo>,
    ) -> PostDelay<'a> {
        PostDelay {
            info,
            workspace,
            still_latest: true,
        }
    }

    #[test]
    fn matching_state_notifies() {
        let cfg = Config::default();
        let i = info("blocked");
        assert_eq!(
            decide(&cfg, StatusKind::Blocked, post(Some(&i), None)),
            Decision::Notify
        );
    }

    #[test]
    fn pane_gone_skips() {
        let cfg = Config::default();
        assert_eq!(
            decide(&cfg, StatusKind::Blocked, post(None, None)),
            Decision::Skip("pane is gone or herdr is unreachable")
        );
    }

    #[test]
    fn stale_state_skips() {
        // blocked event, but the agent went back to working during the delay.
        let cfg = Config::default();
        let i = info("working");
        assert_eq!(
            decide(&cfg, StatusKind::Blocked, post(Some(&i), None)),
            Decision::Skip("stale: pane moved on during the delay")
        );
        let i = info("idle");
        assert_eq!(
            decide(&cfg, StatusKind::Done, post(Some(&i), None)),
            Decision::Skip("stale: pane moved on during the delay")
        );
    }

    #[test]
    fn superseded_generation_skips() {
        let cfg = Config::default();
        let i = info("blocked");
        let p = PostDelay {
            info: Some(&i),
            workspace: None,
            still_latest: false,
        };
        assert_eq!(
            decide(&cfg, StatusKind::Blocked, p),
            Decision::Skip("superseded by a newer transition")
        );
    }

    #[test]
    fn active_tab_of_focused_workspace_suppresses() {
        let cfg = Config::default();
        let i = info("blocked");
        let w = ws(true, Some("w1:t7"));
        assert_eq!(
            decide(&cfg, StatusKind::Blocked, post(Some(&i), Some(&w))),
            Decision::Skip("pane is on the active tab of the focused workspace")
        );
    }

    #[test]
    fn background_tab_of_focused_workspace_still_notifies() {
        let cfg = Config::default();
        let i = info("blocked");
        let w = ws(true, Some("w1:t9"));
        assert_eq!(
            decide(&cfg, StatusKind::Blocked, post(Some(&i), Some(&w))),
            Decision::Notify
        );
    }

    #[test]
    fn same_tab_of_unfocused_workspace_notifies() {
        let cfg = Config::default();
        let i = info("done");
        let w = ws(false, Some("w1:t7"));
        assert_eq!(
            decide(&cfg, StatusKind::Done, post(Some(&i), Some(&w))),
            Decision::Notify
        );
    }

    #[test]
    fn suppression_can_be_disabled() {
        let mut cfg = Config::default();
        cfg.suppress_active_tab = false;
        let i = info("blocked");
        let w = ws(true, Some("w1:t7"));
        assert_eq!(
            decide(&cfg, StatusKind::Blocked, post(Some(&i), Some(&w))),
            Decision::Notify
        );
    }

    #[test]
    fn missing_workspace_snapshot_defaults_to_notifying() {
        let cfg = Config::default();
        let i = info("blocked");
        assert_eq!(
            decide(&cfg, StatusKind::Blocked, post(Some(&i), None)),
            Decision::Notify
        );
    }
}
