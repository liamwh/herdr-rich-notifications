//! Niri window targeting: map a herdr pane to the Wayland window hosting its
//! herdr client, and foreground it.
//!
//! herdr runs inside a terminal (WezTerm on the target workstation) whose
//! Wayland title follows herdr's `[ui] window_title` template. A distinctive
//! marker in that template (default ` · herdr`) identifies herdr-hosting
//! windows among ordinary terminals sharing the same `app_id`, and the
//! workspace token in the title disambiguates after `herdr agent focus`
//! moves the target pane's workspace to the foreground. All subprocess work
//! sits behind [`NiriApi`]; [`select_window`] is pure and unit-tested.

use std::process::Command;

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NiriWindow {
    pub id: u64,
    pub app_id: Option<String>,
    pub title: Option<String>,
}

/// Outcome of matching windows against the herdr markers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// Exactly one window matched (workspace-labelled preferred).
    Unique(u64),
    /// Several matched; after `herdr agent focus` every herdr client shows
    /// the same (server-global) focused workspace, so any of them displays
    /// the right pane — the deterministic lowest-id pick is returned.
    Ambiguous(u64),
    /// Nothing matched.
    None,
}

/// Picks the herdr-hosting window for `ws_label` from a `niri msg --json
/// windows` snapshot.
///
/// Preference order:
/// 1. windows with matching `app_id` AND title marker AND workspace label;
/// 2. windows with matching `app_id` AND title marker (unique only).
pub fn select_window(
    windows: &[NiriWindow],
    app_id_substr: &str,
    title_marker: &str,
    ws_label: Option<&str>,
) -> Selection {
    let herdr_windows: Vec<&NiriWindow> = windows
        .iter()
        .filter(|w| {
            let app_ok = w.app_id.as_deref().is_some_and(|id| {
                id.to_ascii_lowercase()
                    .contains(&app_id_substr.to_ascii_lowercase())
            });
            let marker_ok = w.title.as_deref().is_some_and(|t| t.contains(title_marker));
            app_ok && marker_ok
        })
        .collect();

    if herdr_windows.is_empty() {
        return Selection::None;
    }

    if let Some(label) = ws_label {
        let labelled: Vec<&&NiriWindow> = herdr_windows
            .iter()
            .filter(|w| w.title.as_deref().is_some_and(|t| t.contains(label)))
            .collect();
        if !labelled.is_empty() {
            let id = labelled.iter().map(|w| w.id).min().expect("non-empty");
            return if labelled.len() == 1 {
                Selection::Unique(id)
            } else {
                Selection::Ambiguous(id)
            };
        }
        // No window shows the workspace label yet (title may still be
        // updating after `agent focus`); fall through to the marker-only
        // match only when it is unambiguous, otherwise keep waiting.
        if herdr_windows.len() == 1 {
            return Selection::Unique(herdr_windows[0].id);
        }
        return Selection::None;
    }

    let id = herdr_windows.iter().map(|w| w.id).min().expect("non-empty");
    if herdr_windows.len() == 1 {
        Selection::Unique(id)
    } else {
        Selection::Ambiguous(id)
    }
}

pub trait NiriApi {
    fn windows(&self) -> Result<Vec<NiriWindow>, String>;
    fn focus_window(&self, id: u64) -> Result<(), String>;
}

/// Production implementation over `niri msg`.
#[derive(Debug, Clone, Default)]
pub struct CliNiri;

#[derive(Debug, Deserialize)]
struct RawWindow {
    id: u64,
    #[serde(default)]
    app_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

impl NiriApi for CliNiri {
    fn windows(&self) -> Result<Vec<NiriWindow>, String> {
        let output = Command::new("niri")
            .args(["msg", "--json", "windows"])
            .output()
            .map_err(|e| format!("spawn niri: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "niri msg --json windows failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let raw: Vec<RawWindow> = serde_json::from_slice(&output.stdout)
            .map_err(|e| format!("parse niri windows: {e}"))?;
        Ok(raw
            .into_iter()
            .map(|w| NiriWindow {
                id: w.id,
                app_id: w.app_id,
                title: w.title,
            })
            .collect())
    }

    fn focus_window(&self, id: u64) -> Result<(), String> {
        let output = Command::new("niri")
            .args(["msg", "action", "focus-window", "--id", &id.to_string()])
            .output()
            .map_err(|e| format!("spawn niri: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "niri focus-window {id} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(id: u64, app_id: &str, title: &str) -> NiriWindow {
        NiriWindow {
            id,
            app_id: Some(app_id.to_string()),
            title: Some(title.to_string()),
        }
    }

    const MARKER: &str = " · herdr";

    #[test]
    fn single_herdr_window_is_selected_with_label() {
        let windows = vec![
            win(4, "org.wezfurlong.wezterm", "zeus: infra · herdr"),
            win(6, "helium", "GPU Model Comparison"),
        ];
        assert_eq!(
            select_window(&windows, "wezterm", MARKER, Some("infra")),
            Selection::Unique(4)
        );
    }

    #[test]
    fn herdr_plus_ordinary_wezterm_targets_herdr_only() {
        let windows = vec![
            win(11, "org.wezfurlong.wezterm", "liam@zeus: ~"),
            win(4, "org.wezfurlong.wezterm", "zeus: evidia · herdr"),
        ];
        assert_eq!(
            select_window(&windows, "wezterm", MARKER, Some("evidia")),
            Selection::Unique(4)
        );
    }

    #[test]
    fn label_disambiguates_multiple_herdr_windows() {
        let windows = vec![
            win(9, "org.wezfurlong.wezterm", "zeus: side · herdr"),
            win(4, "org.wezfurlong.wezterm", "zeus: infra · herdr"),
        ];
        assert_eq!(
            select_window(&windows, "wezterm", MARKER, Some("infra")),
            Selection::Unique(4)
        );
    }

    #[test]
    fn multiple_candidates_without_label_resolution() {
        let windows = vec![
            win(9, "org.wezfurlong.wezterm", "zeus: side · herdr"),
            win(4, "org.wezfurlong.wezterm", "zeus: infra · herdr"),
        ];
        // Label matches nothing yet; two candidates without labels → wait.
        assert_eq!(
            select_window(&windows, "wezterm", MARKER, Some("nomatch")),
            Selection::None
        );
        // No label given at all → deterministic lowest-id ambiguity pick.
        assert_eq!(
            select_window(&windows, "wezterm", MARKER, None),
            Selection::Ambiguous(4)
        );
    }

    #[test]
    fn same_label_twice_is_ambiguous_but_usable() {
        let windows = vec![
            win(12, "org.wezfurlong.wezterm", "zeus: infra · herdr"),
            win(4, "org.wezfurlong.wezterm", "zeus: infra · herdr"),
        ];
        assert_eq!(
            select_window(&windows, "wezterm", MARKER, Some("infra")),
            Selection::Ambiguous(4)
        );
    }

    #[test]
    fn no_matching_window_returns_none() {
        let windows = vec![win(11, "org.wezfurlong.wezterm", "liam@zeus: ~")];
        assert_eq!(
            select_window(&windows, "wezterm", MARKER, Some("infra")),
            Selection::None
        );
        assert_eq!(select_window(&[], "wezterm", MARKER, None), Selection::None);
    }

    #[test]
    fn marker_window_without_label_prefers_unique_fallback() {
        // One herdr window, but its title has not picked up the workspace
        // label yet — still safe to focus it when it is the only candidate.
        let windows = vec![
            win(11, "org.wezfurlong.wezterm", "liam@zeus: ~"),
            win(4, "org.wezfurlong.wezterm", "zeus: other · herdr"),
        ];
        assert_eq!(
            select_window(&windows, "wezterm", MARKER, Some("infra")),
            Selection::Unique(4)
        );
    }

    #[test]
    fn app_id_match_is_case_insensitive_substring() {
        let windows = vec![win(3, "org.wezfurlong.WezTerm", "zeus: infra · herdr")];
        assert_eq!(
            select_window(&windows, "wezterm", MARKER, Some("infra")),
            Selection::Unique(3)
        );
    }
}
