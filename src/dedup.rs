//! Deduplication state: one small JSON file under herdr's per-plugin state
//! directory, guarded by a short-lived exclusive lock.
//!
//! Two concerns are tracked per pane:
//!
//! * the last status **seen** (so replayed/unchanged events never
//!   re-notify — the upstream `herdr-notifications` behaviour); and
//! * a monotonically increasing **generation** for that record.
//!
//! The generation closes a race the plain status table cannot: for
//! `blocked → working → blocked` inside the notification delay, TWO event
//! processes both observe "blocked" and both pass the post-delay
//! still-blocked recheck. Only the process that recorded the CURRENT record
//! (its generation is still the latest) owns the notification; the earlier
//! process sees a newer generation, stands down, and the second process
//! notifies exactly once.
//!
//! File-writes are atomic (temp + rename, never through a symlink), and state
//! is deliberately minimal: no terminal content, ever.

use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    /// True when this call changed the pane's recorded status.
    pub changed: bool,
    /// Generation token of the record this call wrote or confirmed.
    pub generation: u64,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct PaneEntry {
    status: String,
    generation: u64,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub(crate) struct StateFile {
    v: u8,
    #[serde(default)]
    next_generation: u64,
    #[serde(default)]
    panes: HashMap<String, PaneEntry>,
}

/// Pure update over the state map: records `status` for `pane`, bumping the
/// generation only when the status actually changed.
pub fn record(state: &mut StateFile, pane_id: &str, status: &str) -> Recorded {
    let next_gen = state.next_generation + 1;
    match state.panes.get(pane_id) {
        Some(entry) if entry.status == status => {
            let generation = entry.generation;
            Recorded {
                changed: false,
                generation,
            }
        }
        _ => {
            state.next_generation = next_gen;
            state.panes.insert(
                pane_id.to_string(),
                PaneEntry {
                    status: status.to_string(),
                    generation: next_gen,
                },
            );
            Recorded {
                changed: true,
                generation: next_gen,
            }
        }
    }
}

/// True when `generation` is still the pane's latest record (no newer event
/// has superseded it).
pub fn is_latest(state: &StateFile, pane_id: &str, generation: u64) -> bool {
    state
        .panes
        .get(pane_id)
        .is_some_and(|entry| entry.generation == generation)
}

/// Locked read-modify-write of the state file: records the transition and
/// returns the change/generation outcome.
pub fn record_on_disk(path: &Path, pane_id: &str, status: &str) -> Recorded {
    with_state_lock(path, || {
        let mut state = load_state(path);
        let outcome = record(&mut state, pane_id, status);
        save_state(path, &state);
        outcome
    })
}

/// Locked read: is `generation` still the latest record for `pane_id`?
pub fn latest_on_disk(path: &Path, pane_id: &str, generation: u64) -> bool {
    with_state_lock(path, || {
        let state = load_state(path);
        is_latest(&state, pane_id, generation)
    })
}

fn load_state(path: &Path) -> StateFile {
    match fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<StateFile>(&bytes) {
            Ok(state) => state,
            Err(e) => {
                eprintln!(
                    "herdr-notifications: dedup state at {} is corrupt, resetting: {e}",
                    path.display()
                );
                StateFile::default()
            }
        },
        Err(e) if e.kind() == ErrorKind::NotFound => StateFile::default(),
        Err(e) => {
            eprintln!(
                "herdr-notifications: failed to read dedup state at {}: {e}",
                path.display()
            );
            StateFile::default()
        }
    }
}

/// Writes via temp file + rename so an interrupted process can never leave a
/// truncated file behind, and refuses to follow a symlink planted at the
/// state path.
fn save_state(path: &Path, state: &StateFile) {
    let Some(parent) = path.parent() else {
        return;
    };
    if let Err(e) = fs::create_dir_all(parent) {
        eprintln!(
            "herdr-notifications: failed to create state dir {}: {e}",
            parent.display()
        );
        return;
    }
    if let Ok(meta) = fs::symlink_metadata(path)
        && meta.file_type().is_symlink()
    {
        eprintln!(
            "herdr-notifications: refusing to write dedup state through a symlink at {}",
            path.display()
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
        eprintln!(
            "herdr-notifications: failed to write dedup state tmp {}: {e}",
            tmp_path.display()
        );
        return;
    }
    if let Err(e) = fs::rename(&tmp_path, path) {
        eprintln!(
            "herdr-notifications: failed to persist dedup state to {}: {e}",
            path.display()
        );
    }
}

/// Best-effort exclusive lock (upstream's approach): herdr spawns one event
/// process per pane and two panes can race on the shared file. A missed lock
/// degrades to racy-but-atomic writes; it never deadlocks.
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

/// State file path: `$HERDR_PLUGIN_STATE_DIR/herdr-notifications-state.json`
/// with a per-user fallback.
pub fn state_file_path() -> PathBuf {
    let dir = std::env::var_os("HERDR_PLUGIN_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(default_state_dir);
    dir.join("herdr-notifications-state.json")
}

/// Per-user, non-shared fallback directory when herdr doesn't supply
/// `$HERDR_PLUGIN_STATE_DIR`. Deliberately avoids the world-writable system
/// temp dir so another local user cannot pre-plant a symlink.
fn default_state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(dir).join("herdr-notifications");
    }
    if cfg!(target_os = "macos")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join("Library/Application Support/herdr-notifications");
    }
    if cfg!(target_os = "windows")
        && let Some(dir) = std::env::var_os("LOCALAPPDATA")
    {
        return PathBuf::from(dir).join("herdr-notifications");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local/state/herdr-notifications");
    }
    std::env::temp_dir().join("herdr-notifications")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_status_is_a_change() {
        let mut state = StateFile::default();
        let rec = record(&mut state, "w1:p1", "blocked");
        assert!(rec.changed);
        assert_eq!(rec.generation, 1);
    }

    #[test]
    fn repeated_status_is_deduped() {
        let mut state = StateFile::default();
        record(&mut state, "w1:p1", "blocked");
        let rec = record(&mut state, "w1:p1", "blocked");
        assert!(!rec.changed);
    }

    #[test]
    fn blocked_working_blocked_renotifies() {
        let mut state = StateFile::default();
        assert!(record(&mut state, "w1:p1", "blocked").changed);
        assert!(record(&mut state, "w1:p1", "working").changed);
        assert!(record(&mut state, "w1:p1", "blocked").changed);
    }

    #[test]
    fn panes_are_independent() {
        let mut state = StateFile::default();
        assert!(record(&mut state, "w1:p1", "blocked").changed);
        assert!(record(&mut state, "w1:p2", "blocked").changed);
        assert!(!record(&mut state, "w1:p1", "blocked").changed);
        assert!(!record(&mut state, "w1:p2", "blocked").changed);
    }

    #[test]
    fn newer_generation_supersedes_older_process() {
        // Process A records blocked (gen 1), then working (gen 2) and a new
        // blocked (gen 3) arrive from other events. A's gen-1 record is no
        // longer latest, so A must not notify.
        let mut state = StateFile::default();
        let a = record(&mut state, "w1:p1", "blocked");
        record(&mut state, "w1:p1", "working");
        let c = record(&mut state, "w1:p1", "blocked");
        assert!(is_latest(&state, "w1:p1", c.generation));
        assert!(!is_latest(&state, "w1:p1", a.generation));
        assert_ne!(a.generation, c.generation);
    }

    #[test]
    fn same_generation_still_latest_without_interleaving() {
        let mut state = StateFile::default();
        let a = record(&mut state, "w1:p1", "blocked");
        assert!(is_latest(&state, "w1:p1", a.generation));
    }

    #[test]
    fn disk_roundtrip_and_stale_file() {
        let dir =
            std::env::temp_dir().join(format!("herdr-notifications-dedup-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");

        let first = record_on_disk(&path, "w1:pK", "blocked");
        assert!(first.changed);
        // Unchanged replay is deduped on disk.
        assert!(!record_on_disk(&path, "w1:pK", "blocked").changed);
        assert!(latest_on_disk(&path, "w1:pK", first.generation));
        // A newer record supersedes.
        let third = record_on_disk(&path, "w1:pK", "working");
        assert!(third.changed);
        assert!(!latest_on_disk(&path, "w1:pK", first.generation));
        assert!(latest_on_disk(&path, "w1:pK", third.generation));

        // State persists across processes (fresh load of the same path).
        assert!(!record_on_disk(&path, "w1:pK", "working").changed);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_state_resets() {
        let dir = std::env::temp_dir().join(format!(
            "herdr-notifications-dedup-corrupt-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        fs::write(&path, "{not json").unwrap();
        // Recovers by resetting instead of failing.
        assert!(record_on_disk(&path, "w1:p1", "done").changed);
        fs::remove_dir_all(&dir).ok();
    }
}
