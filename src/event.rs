//! Parsing of herdr's `pane.agent_status_changed` plugin-event payload.
//!
//! Herdr 0.8 hands event hooks an envelope:
//!
//! ```json
//! {"event":"pane_agent_status_changed","data":{"type":"pane_agent_status_changed",
//!  "pane_id":"w1:p1","workspace_id":"w1","agent_status":"blocked","agent":"omp"}}
//! ```
//!
//! (`agent`, `title`, `display_agent`, and `state_labels` are optional and
//! frequently absent). Herdr 0.7 handed plugins the inner object directly with
//! `display_agent`/`title` always present; both shapes are accepted so the
//! plugin keeps working across herdr versions.

use serde::Deserialize;

/// A `pane.agent_status_changed` event, flattened across envelope variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusChanged {
    pub pane_id: String,
    pub workspace_id: String,
    pub agent_status: String,
    pub agent: Option<String>,
    pub title: Option<String>,
    pub display_agent: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Envelope {
    data: Inner,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum Inner {
    #[serde(rename = "pane_agent_status_changed")]
    PaneAgentStatusChanged {
        pane_id: String,
        #[serde(default)]
        workspace_id: String,
        agent_status: String,
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        display_agent: Option<String>,
    },
    #[serde(other)]
    Other,
}

/// Parses `HERDR_PLUGIN_EVENT_JSON` (or an `events.subscribe` push payload)
/// into a [`StatusChanged`]. Returns `Ok(None)` for non-status-changed events
/// and a descriptive error for malformed JSON.
pub fn parse_status_changed(raw: &str) -> Result<Option<StatusChanged>, String> {
    // 0.8 envelope: {"event": "...", "data": {...}}
    if let Ok(envelope) = serde_json::from_str::<Envelope>(raw) {
        return match envelope.data {
            Inner::PaneAgentStatusChanged {
                pane_id,
                workspace_id,
                agent_status,
                agent,
                title,
                display_agent,
            } => Ok(Some(StatusChanged {
                pane_id,
                workspace_id,
                agent_status,
                agent,
                title,
                display_agent,
            })),
            Inner::Other => Ok(None),
        };
    }

    // 0.7-style flat payload: {"type": "pane_agent_status_changed", ...}
    if let Ok(inner) = serde_json::from_str::<Inner>(raw) {
        return match inner {
            Inner::PaneAgentStatusChanged {
                pane_id,
                workspace_id,
                agent_status,
                agent,
                title,
                display_agent,
            } => Ok(Some(StatusChanged {
                pane_id,
                workspace_id,
                agent_status,
                agent,
                title,
                display_agent,
            })),
            Inner::Other => Ok(None),
        };
    }

    Err(format!("unrecognised event payload: {raw}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENVELOPE: &str = "{\"event\":\"pane_agent_status_changed\",\"data\":{\"type\":\"pane_agent_status_changed\",\"pane_id\":\"w1:pS\",\"workspace_id\":\"w1\",\"agent_status\":\"blocked\",\"agent\":\"probe\"}}";

    #[test]
    fn parses_herdr_0_8_envelope() {
        let ev = parse_status_changed(ENVELOPE).unwrap().unwrap();
        assert_eq!(ev.pane_id, "w1:pS");
        assert_eq!(ev.workspace_id, "w1");
        assert_eq!(ev.agent_status, "blocked");
        assert_eq!(ev.agent.as_deref(), Some("probe"));
        assert_eq!(ev.title, None);
        assert_eq!(ev.display_agent, None);
    }

    #[test]
    fn parses_envelope_without_optional_fields() {
        let raw = r#"{"event":"pane.agent_status_changed","data":{"type":"pane_agent_status_changed","pane_id":"w2:p1","workspace_id":"w2","agent_status":"done"}}"#;
        let ev = parse_status_changed(raw).unwrap().unwrap();
        assert_eq!(ev.agent_status, "done");
        assert_eq!(ev.agent, None);
    }

    #[test]
    fn parses_herdr_0_7_flat_payload() {
        let raw = r#"{"type":"pane_agent_status_changed","pane_id":"w1:p1","agent":"omp","agent_status":"done","display_agent":"OMP","title":"π > ship it"}"#;
        let ev = parse_status_changed(raw).unwrap().unwrap();
        assert_eq!(ev.pane_id, "w1:p1");
        assert_eq!(ev.workspace_id, "");
        assert_eq!(ev.display_agent.as_deref(), Some("OMP"));
        assert_eq!(ev.title.as_deref(), Some("π > ship it"));
    }

    #[test]
    fn ignores_other_event_types() {
        let raw = r#"{"event":"pane.output_matched","data":{"type":"pane_output_matched","pane_id":"w1:p1","matched_line":"x"}}"#;
        assert!(parse_status_changed(raw).unwrap().is_none());
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_status_changed("not json").is_err());
        assert!(parse_status_changed("{\"data\":42}").is_err());
    }
}
