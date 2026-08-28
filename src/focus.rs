//! Click-to-focus orchestration: notification click → herdr focuses the
//! workspace/tab/pane → the compositor foregrounds the terminal window
//! hosting that herdr client.
//!
//! Ordering and failure isolation:
//!
//! 1. `herdr agent focus <pane>` runs FIRST via `$HERDR_BIN_PATH` and
//!    `$HERDR_SOCKET_PATH` captured from the event, so the herdr side always
//!    targets the session that emitted the event. Failure here is logged but
//!    does not stop compositor foregrounding.
//! 2. Compositor foregrounding polls `niri msg --json windows` (bounded,
//!    short-interval, state-based — no fixed sleeps) for the herdr-hosting
//!    window: `app_id` match + the window-title marker, preferring the window
//!    whose title now carries the target workspace's label (herdr's
//!    `[ui] window_title` template refreshes it after the focus command).
//!    Failure here never undoes the herdr focus.
//!
//! Every step logs; nothing panics or blocks indefinitely.

use std::time::{Duration, Instant};

use crate::config::NiriConfig;
use crate::herdr_api::HerdrApi;
use crate::niri::{NiriApi, Selection};

/// Everything needed to re-target the exact pane/session a notification came
/// from, retained from the event's runtime context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickTarget {
    pub pane_id: String,
    /// Workspace display label, used for window-title matching.
    pub workspace_label: Option<String>,
    /// Session socket the event came from (`HERDR_SOCKET_PATH`).
    pub socket_path: Option<String>,
    /// herdr binary herdr handed the plugin (`HERDR_BIN_PATH`).
    pub bin_path: Option<String>,
}

/// Executes one click: herdr focus, then compositor foreground. Returns
/// human-readable step results for logging.
pub fn handle_click(
    target: &ClickTarget,
    herdr: &dyn HerdrApi,
    niri: &dyn NiriApi,
    niri_cfg: &NiriConfig,
    sleep: impl Fn(Duration),
) -> ClickResult {
    let herdr_focus = match herdr.agent_focus(&target.pane_id) {
        Ok(()) => StepOutcome::Ok,
        Err(e) => StepOutcome::Failed(e.to_string()),
    };
    let mut result = ClickResult {
        herdr_focus,
        ..ClickResult::default()
    };

    // 2. Compositor foregrounding.
    if !niri_cfg.enabled {
        result.niri_focus = StepOutcome::Skipped("disabled in config".into());
        return result;
    }

    let deadline = Instant::now() + Duration::from_millis(niri_cfg.focus_timeout_ms);
    let poll = Duration::from_millis(niri_cfg.poll_interval_ms.max(10));

    loop {
        match niri.windows() {
            Ok(windows) => {
                let selection = crate::niri::select_window(
                    &windows,
                    &niri_cfg.app_id,
                    &niri_cfg.title_marker,
                    target.workspace_label.as_deref(),
                );
                match selection {
                    Selection::Unique(id) | Selection::Ambiguous(id) => {
                        if matches!(selection, Selection::Ambiguous(_)) {
                            eprintln!(
                                "herdr-notifications: multiple herdr windows matched; focusing lowest-id window {id}"
                            );
                        }
                        result.niri_focus = match niri.focus_window(id) {
                            Ok(()) => StepOutcome::Ok,
                            Err(e) => StepOutcome::Failed(e),
                        };
                        result.window_id = Some(id);
                        return result;
                    }
                    Selection::None => {
                        eprintln!(
                            "herdr-notifications: no herdr window matched yet (app_id '{}', marker '{}', label {:?}); polling until deadline",
                            niri_cfg.app_id, niri_cfg.title_marker, target.workspace_label
                        );
                    }
                }
            }
            Err(e) => {
                result.niri_focus = StepOutcome::Failed(e);
                return result;
            }
        }

        if Instant::now() + poll >= deadline {
            break;
        }
        sleep(poll);
    }

    // Deadline hit: fall back to any herdr-marked window (after the herdr
    // focus above it shows the right workspace anyway).
    match niri.windows() {
        Ok(windows) => match crate::niri::select_window(
            &windows,
            &niri_cfg.app_id,
            &niri_cfg.title_marker,
            None,
        ) {
            Selection::Unique(id) | Selection::Ambiguous(id) => {
                result.niri_focus = match niri.focus_window(id) {
                    Ok(()) => StepOutcome::Ok,
                    Err(e) => StepOutcome::Failed(e),
                };
                result.window_id = Some(id);
            }
            Selection::None => {
                result.niri_focus =
                    StepOutcome::Failed("no herdr-marked terminal window found".into());
            }
        },
        Err(e) => result.niri_focus = StepOutcome::Failed(e),
    }
    result
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClickResult {
    pub herdr_focus: StepOutcome,
    pub niri_focus: StepOutcome,
    pub window_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    Ok,
    Skipped(String),
    Failed(String),
}

impl Default for StepOutcome {
    fn default() -> Self {
        Self::Skipped("not attempted".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr_api::{AgentInfo, ApiError};
    use crate::niri::NiriWindow;
    use std::cell::RefCell;

    struct FakeHerdr {
        focus_calls: RefCell<Vec<String>>,
        fail: bool,
    }
    impl HerdrApi for FakeHerdr {
        fn agent_get(&self, _pane: &str) -> Result<AgentInfo, ApiError> {
            unreachable!()
        }
        fn workspace_of(
            &self,
            _ws: &str,
        ) -> Result<Option<crate::herdr_api::WorkspaceInfo>, ApiError> {
            unreachable!()
        }
        fn tab_label(&self, _tab: &str) -> Result<Option<String>, ApiError> {
            unreachable!()
        }
        fn explain(&self, _pane: &str) -> Result<Option<crate::herdr_api::ExplainInfo>, ApiError> {
            unreachable!()
        }
        fn read_detection(&self, _pane: &str, _lines: usize) -> Result<String, ApiError> {
            unreachable!()
        }
        fn agent_focus(&self, pane: &str) -> Result<(), ApiError> {
            self.focus_calls.borrow_mut().push(pane.to_string());
            if self.fail {
                Err(ApiError::Herdr {
                    code: "agent_not_found".into(),
                    message: format!("{pane} not found"),
                })
            } else {
                Ok(())
            }
        }
    }

    struct FakeNiri {
        windows: RefCell<Vec<Vec<NiriWindow>>>,
        focus_calls: RefCell<Vec<u64>>,
    }
    impl NiriApi for FakeNiri {
        fn windows(&self) -> Result<Vec<NiriWindow>, String> {
            let mut frames = self.windows.borrow_mut();
            if frames.len() > 1 {
                Ok(frames.remove(0))
            } else {
                Ok(frames.first().cloned().unwrap_or_default())
            }
        }
        fn focus_window(&self, id: u64) -> Result<(), String> {
            self.focus_calls.borrow_mut().push(id);
            Ok(())
        }
    }

    fn target() -> ClickTarget {
        ClickTarget {
            pane_id: "w1:pK".into(),
            workspace_label: Some("infra".into()),
            socket_path: Some("/run/herdr.sock".into()),
            bin_path: Some("/usr/bin/herdr".into()),
        }
    }

    fn cfg() -> NiriConfig {
        NiriConfig {
            enabled: true,
            app_id: "wezterm".into(),
            title_marker: " · herdr".into(),
            focus_timeout_ms: 500,
            poll_interval_ms: 10,
        }
    }

    fn win(id: u64, title: &str) -> NiriWindow {
        NiriWindow {
            id,
            app_id: Some("org.wezfurlong.wezterm".into()),
            title: Some(title.to_string()),
        }
    }

    #[test]
    fn focuses_herdr_then_matching_window() {
        let herdr = FakeHerdr {
            focus_calls: RefCell::new(vec![]),
            fail: false,
        };
        let niri = FakeNiri {
            windows: RefCell::new(vec![vec![
                win(4, "zeus: infra · herdr"),
                win(6, "liam@zeus: ~"),
            ]]),
            focus_calls: RefCell::new(vec![]),
        };
        let result = handle_click(&target(), &herdr, &niri, &cfg(), |_| {});
        assert_eq!(*herdr.focus_calls.borrow(), vec!["w1:pK".to_string()]);
        assert_eq!(result.herdr_focus, StepOutcome::Ok);
        assert_eq!(result.niri_focus, StepOutcome::Ok);
        assert_eq!(result.window_id, Some(4));
        assert_eq!(*niri.focus_calls.borrow(), vec![4]);
    }

    #[test]
    fn polls_until_title_settles() {
        let herdr = FakeHerdr {
            focus_calls: RefCell::new(vec![]),
            fail: false,
        };
        let niri = FakeNiri {
            // First frame: title not yet updated; second frame: settled.
            windows: RefCell::new(vec![
                vec![win(4, "zeus: other · herdr")],
                vec![win(4, "zeus: infra · herdr")],
            ]),
            focus_calls: RefCell::new(vec![]),
        };
        let result = handle_click(&target(), &herdr, &niri, &cfg(), |_| {});
        assert_eq!(result.window_id, Some(4));
        assert_eq!(result.niri_focus, StepOutcome::Ok);
    }

    #[test]
    fn herdr_failure_does_not_block_niri() {
        let herdr = FakeHerdr {
            focus_calls: RefCell::new(vec![]),
            fail: true,
        };
        let niri = FakeNiri {
            windows: RefCell::new(vec![vec![win(9, "zeus: infra · herdr")]]),
            focus_calls: RefCell::new(vec![]),
        };
        let result = handle_click(&target(), &herdr, &niri, &cfg(), |_| {});
        assert!(matches!(result.herdr_focus, StepOutcome::Failed(_)));
        assert_eq!(result.niri_focus, StepOutcome::Ok);
        assert_eq!(result.window_id, Some(9));
    }

    #[test]
    fn niri_failure_does_not_undo_herdr_focus() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static BROKEN: AtomicBool = AtomicBool::new(false);
        struct BrokenNiri;
        impl NiriApi for BrokenNiri {
            fn windows(&self) -> Result<Vec<NiriWindow>, String> {
                Err("niri IPC unavailable".into())
            }
            fn focus_window(&self, _id: u64) -> Result<(), String> {
                unreachable!()
            }
        }
        let _ = BROKEN.fetch_or(true, Ordering::Relaxed);
        let herdr = FakeHerdr {
            focus_calls: RefCell::new(vec![]),
            fail: false,
        };
        let result = handle_click(&target(), &herdr, &BrokenNiri, &cfg(), |_| {});
        assert_eq!(result.herdr_focus, StepOutcome::Ok);
        assert!(matches!(result.niri_focus, StepOutcome::Failed(_)));
    }

    #[test]
    fn disabled_niri_skips_compositor_step() {
        let herdr = FakeHerdr {
            focus_calls: RefCell::new(vec![]),
            fail: false,
        };
        let niri = FakeNiri {
            windows: RefCell::new(vec![vec![]]),
            focus_calls: RefCell::new(vec![]),
        };
        let mut c = cfg();
        c.enabled = false;
        let result = handle_click(&target(), &herdr, &niri, &c, |_| {});
        assert_eq!(result.herdr_focus, StepOutcome::Ok);
        assert!(matches!(result.niri_focus, StepOutcome::Skipped(_)));
        assert!(niri.focus_calls.borrow().is_empty());
    }

    #[test]
    fn no_matching_window_reports_failure_after_deadline() {
        let herdr = FakeHerdr {
            focus_calls: RefCell::new(vec![]),
            fail: false,
        };
        let niri = FakeNiri {
            windows: RefCell::new(vec![vec![win(6, "liam@zeus: ~")]]),
            focus_calls: RefCell::new(vec![]),
        };
        let mut c = cfg();
        c.focus_timeout_ms = 20;
        let result = handle_click(&target(), &herdr, &niri, &c, |_| {});
        assert_eq!(result.herdr_focus, StepOutcome::Ok);
        assert!(matches!(result.niri_focus, StepOutcome::Failed(_)));
        assert_eq!(result.window_id, None);
    }
}
