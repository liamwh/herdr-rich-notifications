//! herdr-notifications: relays herdr agent-status events to native OS
//! desktop notifications on Linux, macOS, Windows, and BSD. Clicking a
//! status-change notification focuses the pane that triggered it back in
//! herdr (see `focus_pane`); manual `notify` invocations have no pane to
//! focus and skip this.
//!
//! Two entry points:
//!   `herdr-notifications event`            — invoked by herdr as a `[[events]]` hook;
//!                                             reads HERDR_PLUGIN_EVENT_JSON from the env.
//!   `herdr-notifications notify ...`       — invoked by herdr as an `[[actions]]` hook,
//!                                             or manually, to fire a notification directly.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::mpsc;
use std::time::Duration;

use notify_rust::{Notification, NotificationResponse, Timeout};
use serde::Deserialize;

/// How long to wait for the notification backend to acknowledge the
/// initial show request before giving up on it.
const SHOW_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait for a click after the notification is shown, when
/// there's a pane to focus on click. Also requested as the notification's
/// own on-screen lifetime (macOS ignores that request, so this bound is
/// enforced ourselves either way — see `send_notification`).
const CLICK_WAIT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sound {
    None,
    Done,
    Request,
}

impl Sound {
    fn parse(s: &str) -> Option<Sound> {
        match s {
            "none" => Some(Sound::None),
            "done" => Some(Sound::Done),
            "request" => Some(Sound::Request),
            _ => None,
        }
    }

    /// Platform-appropriate sound name. Linux/BSD follow the freedesktop
    /// sound-naming spec; macOS names are built-in NSSound names. Windows
    /// notifications use the system default toast sound regardless.
    fn name(self) -> Option<&'static str> {
        match self {
            Sound::None => None,
            #[cfg(target_os = "macos")]
            Sound::Done => Some("Glass"),
            #[cfg(target_os = "macos")]
            Sound::Request => Some("Ping"),
            #[cfg(not(target_os = "macos"))]
            Sound::Done => Some("complete"),
            #[cfg(not(target_os = "macos"))]
            Sound::Request => Some("dialog-warning"),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum EventData {
    #[serde(rename = "pane_agent_status_changed")]
    PaneAgentStatusChanged {
        pane_id: String,
        agent: String,
        agent_status: String,
        display_agent: String,
        #[serde(default)]
        title: String,
    },
    #[serde(other)]
    Other,
}

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let cmd = args.next().unwrap_or_else(|| "event".to_string());

    let result = match cmd.as_str() {
        "event" => run_event(),
        "notify" => run_notify(args.collect()),
        other => {
            eprintln!(
                "herdr-notifications: unknown subcommand '{other}' (expected 'event' or 'notify')"
            );
            Err(())
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(()) => ExitCode::FAILURE,
    }
}

/// Handle a herdr `[[events]]` invocation: herdr sets HERDR_PLUGIN_EVENT to
/// the event's `type` and HERDR_PLUGIN_EVENT_JSON to the full JSON payload.
fn run_event() -> Result<(), ()> {
    let Ok(payload) = env::var("HERDR_PLUGIN_EVENT_JSON") else {
        // Not running as an event hook (e.g. invoked by hand); nothing to do.
        return Ok(());
    };

    let data: EventData = match serde_json::from_str(&payload) {
        Ok(data) => data,
        Err(e) => {
            eprintln!("herdr-notifications: failed to parse event payload: {e}");
            return Err(());
        }
    };

    let EventData::PaneAgentStatusChanged {
        pane_id,
        agent,
        agent_status,
        display_agent,
        title,
        ..
    } = data
    else {
        return Ok(());
    };

    // Record every status transition, not just the actionable ones, so a
    // "blocked -> working -> blocked" cycle is recognized as a fresh
    // "blocked" rather than being suppressed as a repeat of the first one.
    let changed = should_notify(&pane_id, &agent_status);

    let Some((summary, sound)) = decide_notification(&agent_status, &display_agent) else {
        return Ok(()); // idle / working / unknown: not actionable, skip
    };

    if !changed {
        return Ok(());
    }

    let body = if title.is_empty() { agent } else { title };

    send_notification(&summary, &body, sound, Some(&pane_id))
}

/// Pure decision: which `agent_status` values are worth surfacing, and what
/// to say about them.
fn decide_notification(agent_status: &str, display_agent: &str) -> Option<(String, Sound)> {
    match agent_status {
        "blocked" => Some((format!("{display_agent} needs you"), Sound::Request)),
        "done" => Some((format!("{display_agent} is done"), Sound::Done)),
        _ => None,
    }
}

/// Handle a manual/action invocation: `herdr-notifications notify --title T [--body B] [--sound none|done|request]`.
fn run_notify(args: Vec<String>) -> Result<(), ()> {
    match parse_notify_args(args) {
        Ok((title, body, sound)) => send_notification(&title, &body, sound, None),
        Err(msg) => {
            eprintln!("herdr-notifications: {msg}");
            Err(())
        }
    }
}

/// Pure CLI-flag parser for `notify`, factored out so it's testable without
/// touching the notification backend.
fn parse_notify_args(args: Vec<String>) -> Result<(String, String, Sound), String> {
    let mut title: Option<String> = None;
    let mut body = String::new();
    let mut sound = Sound::None;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--title" => title = iter.next(),
            "--body" => body = iter.next().unwrap_or_default(),
            "--sound" => {
                let raw = iter.next().unwrap_or_default();
                sound = Sound::parse(&raw).unwrap_or_else(|| {
                    eprintln!("herdr-notifications: unknown --sound '{raw}', defaulting to none");
                    Sound::None
                });
            }
            other => return Err(format!("unknown argument '{other}'")),
        }
    }

    let title = title.ok_or_else(|| "notify requires --title <TEXT>".to_string())?;
    Ok((title, body, sound))
}

/// Shows the notification on a background thread so a hung notification
/// daemon (stale D-Bus session, unresponsive systemd-user restart) can't
/// block this process forever. When `click_target` is a pane id, also waits
/// (bounded by `CLICK_WAIT_TIMEOUT`) for the user to click the notification
/// body and, if they do, focuses that pane in herdr.
fn send_notification(summary: &str, body: &str, sound: Sound, click_target: Option<&str>) -> Result<(), ()> {
    let summary = summary.to_string();
    let body = body.to_string();
    let click_target = click_target.map(str::to_string);
    let wants_click = click_target.is_some();

    let (shown_tx, shown_rx) = mpsc::channel::<Result<(), String>>();
    let (click_tx, click_rx) = mpsc::channel::<Option<String>>();

    std::thread::spawn(move || {
        let mut notification = Notification::new();
        notification
            .appname("herdr")
            .summary(&summary)
            .body(&body)
            .auto_icon();

        if let Some(name) = sound.name() {
            notification.sound_name(name);
        }
        if wants_click {
            // Ignored on macOS (notify-rust has no manual-timeout support
            // there); CLICK_WAIT_TIMEOUT below bounds our own wait either way.
            notification.timeout(Timeout::Milliseconds(CLICK_WAIT_TIMEOUT.as_millis() as u32));
        }

        let handle = match notification.show() {
            Ok(handle) => handle,
            Err(e) => {
                let _ = shown_tx.send(Err(e.to_string()));
                return;
            }
        };
        let _ = shown_tx.send(Ok(()));

        if let Some(pane_id) = click_target {
            let _ = handle.wait_for_response(move |response: &NotificationResponse| {
                let _ = click_tx.send(response.is_default_action().then_some(pane_id));
            });
        }
    });

    let shown = match shown_rx.recv_timeout(SHOW_TIMEOUT) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            eprintln!("herdr-notifications: failed to show notification: {e}");
            Err(())
        }
        Err(_) => {
            eprintln!(
                "herdr-notifications: notification backend did not respond within {SHOW_TIMEOUT:?}"
            );
            Err(())
        }
    };

    if wants_click && shown.is_ok() {
        // Bounded independently of the platform backend's own timeout
        // support: if nothing arrives in time we just return, and the
        // still-blocked background thread (if any) dies with the process.
        if let Ok(Some(pane_id)) = click_rx.recv_timeout(CLICK_WAIT_TIMEOUT) {
            focus_pane(&pane_id);
        }
    }

    shown
}

/// Best-effort: bring the pane that triggered a notification back into
/// focus in herdr when the user clicks it. Uses the `herdr` binary herdr
/// hands every plugin process via $HERDR_BIN_PATH (falling back to `herdr`
/// on PATH) rather than talking to the socket API directly.
fn focus_pane(pane_id: &str) {
    let bin = env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_string());
    match Command::new(&bin).args(["agent", "focus", pane_id]).output() {
        Ok(output) if !output.status.success() => {
            eprintln!(
                "herdr-notifications: `{bin} agent focus {pane_id}` failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Err(e) => {
            eprintln!("herdr-notifications: failed to run '{bin}' to focus pane {pane_id}: {e}");
        }
        _ => {}
    }
}

/// Dedupe notifications: only fire when this pane's status actually changed
/// since the last time we saw it. State lives under $HERDR_PLUGIN_STATE_DIR
/// (falling back to a per-user local-data directory) so it survives across
/// the short-lived processes herdr spawns per event.
fn should_notify(pane_id: &str, agent_status: &str) -> bool {
    let path = state_file_path();
    with_state_lock(&path, || {
        let mut state = load_state(&path);
        let changed = record_status_if_changed(&mut state, pane_id, agent_status);
        if changed {
            save_state(&path, &state);
        }
        changed
    })
}

/// Pure dedup-table update: records `agent_status` for `pane_id`, returning
/// whether it differs from what was previously recorded. No I/O, so this is
/// unit-testable without a filesystem.
fn record_status_if_changed(
    state: &mut HashMap<String, String>,
    pane_id: &str,
    agent_status: &str,
) -> bool {
    if state.get(pane_id).map(String::as_str) == Some(agent_status) {
        return false;
    }
    state.insert(pane_id.to_string(), agent_status.to_string());
    true
}

fn load_state(path: &Path) -> HashMap<String, String> {
    match fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(state) => state,
            Err(e) => {
                eprintln!(
                    "herdr-notifications: dedup state at {path:?} is corrupt, resetting: {e}"
                );
                HashMap::new()
            }
        },
        Err(e) if e.kind() == ErrorKind::NotFound => HashMap::new(),
        Err(e) => {
            eprintln!("herdr-notifications: failed to read dedup state at {path:?}: {e}");
            HashMap::new()
        }
    }
}

/// Writes via a temp file + rename so a process interrupted mid-write can
/// never leave `path` holding truncated/corrupt JSON, and refuses to follow
/// an existing symlink at `path` (defense against another local user planting
/// one in a shared directory).
fn save_state(path: &Path, state: &HashMap<String, String>) {
    let Some(parent) = path.parent() else {
        return;
    };
    if let Err(e) = fs::create_dir_all(parent) {
        eprintln!("herdr-notifications: failed to create state dir {parent:?}: {e}");
        return;
    }

    if let Ok(meta) = fs::symlink_metadata(path)
        && meta.file_type().is_symlink()
    {
        eprintln!(
            "herdr-notifications: refusing to write dedup state through a symlink at {path:?}"
        );
        return;
    }

    let bytes = match serde_json::to_vec(state) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("herdr-notifications: failed to serialize dedup state: {e}");
            return;
        }
    };

    let tmp_path = path.with_extension("json.tmp");
    if let Err(e) = fs::write(&tmp_path, &bytes) {
        eprintln!("herdr-notifications: failed to write dedup state tmp file {tmp_path:?}: {e}");
        return;
    }
    if let Err(e) = fs::rename(&tmp_path, path) {
        eprintln!("herdr-notifications: failed to persist dedup state to {path:?}: {e}");
    }
}

/// A short-lived, best-effort exclusive lock: herdr can spawn one
/// `event`-handling process per pane, and two panes can flip status close
/// enough together to race on the shared state file. Waits up to ~1s for a
/// sibling process to release the lock before proceeding anyway (a missed
/// lock degrades to the old racy behavior, it doesn't deadlock).
fn with_state_lock<T>(path: &Path, f: impl FnOnce() -> T) -> T {
    let lock_path = path.with_extension("lock");
    if let Some(parent) = lock_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let mut acquired = false;
    for _ in 0..50 {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(_) => {
                acquired = true;
                break;
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }

    let result = f();

    if acquired {
        let _ = fs::remove_file(&lock_path);
    }

    result
}

fn state_file_path() -> PathBuf {
    let dir = env::var_os("HERDR_PLUGIN_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(default_state_dir);
    dir.join("herdr-notifications-state.json")
}

/// Per-user, non-shared fallback directory when herdr doesn't supply
/// $HERDR_PLUGIN_STATE_DIR. Deliberately avoids the shared system temp dir
/// (world-writable on Unix), which would let another local user pre-plant a
/// symlink at a predictable path.
fn default_state_dir() -> PathBuf {
    if let Some(dir) = env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(dir).join("herdr-notifications");
    }
    if cfg!(target_os = "macos")
        && let Some(home) = env::var_os("HOME")
    {
        return PathBuf::from(home).join("Library/Application Support/herdr-notifications");
    }
    if cfg!(target_os = "windows")
        && let Some(dir) = env::var_os("LOCALAPPDATA")
    {
        return PathBuf::from(dir).join("herdr-notifications");
    }
    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home).join(".local/state/herdr-notifications");
    }
    // No known per-user directory (HOME/LOCALAPPDATA unset) — last resort.
    env::temp_dir().join("herdr-notifications")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sound_none_has_no_name() {
        assert_eq!(Sound::None.name(), None);
    }

    #[test]
    fn sound_done_and_request_have_platform_names() {
        assert!(Sound::Done.name().is_some());
        assert!(Sound::Request.name().is_some());
    }

    #[test]
    fn decide_notification_blocked_uses_request_sound() {
        let (summary, sound) = decide_notification("blocked", "Claude").unwrap();
        assert_eq!(summary, "Claude needs you");
        assert_eq!(sound, Sound::Request);
    }

    #[test]
    fn decide_notification_done_uses_done_sound() {
        let (summary, sound) = decide_notification("done", "Claude").unwrap();
        assert_eq!(summary, "Claude is done");
        assert_eq!(sound, Sound::Done);
    }

    #[test]
    fn decide_notification_non_actionable_statuses_are_none() {
        for status in ["idle", "working", "unknown", "Blocked"] {
            assert!(
                decide_notification(status, "Claude").is_none(),
                "status {status} should not notify"
            );
        }
    }

    #[test]
    fn record_status_first_seen_changes() {
        let mut state = HashMap::new();
        assert!(record_status_if_changed(&mut state, "p1", "blocked"));
    }

    #[test]
    fn record_status_same_status_does_not_change() {
        let mut state = HashMap::new();
        assert!(record_status_if_changed(&mut state, "p1", "blocked"));
        assert!(!record_status_if_changed(&mut state, "p1", "blocked"));
    }

    #[test]
    fn record_status_transition_and_back_changes_again() {
        let mut state = HashMap::new();
        assert!(record_status_if_changed(&mut state, "p1", "blocked"));
        assert!(record_status_if_changed(&mut state, "p1", "working"));
        assert!(record_status_if_changed(&mut state, "p1", "blocked"));
    }

    #[test]
    fn record_status_tracks_panes_independently() {
        let mut state = HashMap::new();
        assert!(record_status_if_changed(&mut state, "p1", "blocked"));
        assert!(record_status_if_changed(&mut state, "p2", "blocked"));
    }

    #[test]
    fn parse_notify_args_missing_title_errors() {
        assert!(parse_notify_args(vec![]).is_err());
    }

    #[test]
    fn parse_notify_args_title_only_defaults_empty_body_and_none_sound() {
        let (title, body, sound) =
            parse_notify_args(vec!["--title".into(), "hi".into()]).unwrap();
        assert_eq!(title, "hi");
        assert_eq!(body, "");
        assert_eq!(sound, Sound::None);
    }

    #[test]
    fn parse_notify_args_unknown_sound_falls_back_to_none() {
        let (_, _, sound) = parse_notify_args(vec![
            "--title".into(),
            "hi".into(),
            "--sound".into(),
            "bogus".into(),
        ])
        .unwrap();
        assert_eq!(sound, Sound::None);
    }

    #[test]
    fn parse_notify_args_unknown_flag_errors() {
        assert!(parse_notify_args(vec!["--nope".into()]).is_err());
    }

    #[test]
    fn parse_notify_args_all_flags_parse() {
        let (title, body, sound) = parse_notify_args(vec![
            "--title".into(),
            "T".into(),
            "--body".into(),
            "B".into(),
            "--sound".into(),
            "done".into(),
        ])
        .unwrap();
        assert_eq!(title, "T");
        assert_eq!(body, "B");
        assert_eq!(sound, Sound::Done);
    }

    fn unique_test_path(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "herdr-notifications-test-{name}-{:?}",
            std::thread::current().id()
        ))
    }

    #[test]
    fn load_state_missing_file_returns_empty() {
        let path = unique_test_path("missing").join("state.json");
        assert!(load_state(&path).is_empty());
    }

    #[test]
    fn state_round_trips_through_disk() {
        let dir = unique_test_path("roundtrip");
        let path = dir.join("state.json");
        let _ = fs::remove_dir_all(&dir);

        let mut state = HashMap::new();
        state.insert("p1".to_string(), "blocked".to_string());
        save_state(&path, &state);

        let loaded = load_state(&path);
        assert_eq!(loaded.get("p1").map(String::as_str), Some("blocked"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_state_refuses_to_follow_a_symlink() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let dir = unique_test_path("symlink");
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();

            let target = dir.join("real-target.json");
            fs::write(&target, b"do not touch").unwrap();
            let link = dir.join("state.json");
            symlink(&target, &link).unwrap();

            let mut state = HashMap::new();
            state.insert("p1".to_string(), "blocked".to_string());
            save_state(&link, &state);

            assert_eq!(
                fs::read(&target).unwrap(),
                b"do not touch",
                "save_state must not write through a symlink"
            );

            let _ = fs::remove_dir_all(&dir);
        }
    }
}
