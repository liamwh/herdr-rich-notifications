//! Plugin configuration, read from `config.toml` under herdr's plugin config
//! directory (`$HERDR_PLUGIN_CONFIG_DIR`; falls back to no config).
//!
//! The surface is deliberately small; every field has a working default so
//! the plugin runs with zero configuration, matching upstream's
//! zero-runtime-config promise.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Detail {
    /// Rich bodies: tab/task line plus a small terminal excerpt.
    #[default]
    Rich,
    /// Privacy mode: labels only, no terminal content.
    Minimal,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Agent statuses worth notifying for.
    #[serde(default = "default_statuses")]
    pub statuses: Vec<String>,
    /// Delay before showing, when set here explicitly (overrides herdr's
    /// `[ui.toast] delay_seconds`).
    delay_ms: Option<u64>,
    /// Skip notifications for panes on the active tab of the focused
    /// workspace (mirrors herdr's own popup suppression).
    #[serde(default = "default_true")]
    pub suppress_active_tab: bool,
    /// Notification body detail level.
    #[serde(default)]
    pub detail: Detail,
    /// Enable click-to-focus (herdr pane focus + compositor foreground).
    #[serde(default = "default_true")]
    pub click_to_focus: bool,
    /// How long the notification stays actionable after being shown.
    #[serde(default = "default_click_wait")]
    pub click_wait_secs: u64,
    /// Requested on-screen lifetime of the notification.
    #[serde(default = "default_expire")]
    pub expire_secs: u64,
    /// Compositor foregrounding (Niri IPC).
    #[serde(default)]
    pub niri: NiriConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NiriConfig {
    /// Master switch; failures are logged and never fatal.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Substring matched against Wayland `app_id` (case-insensitive).
    #[serde(default = "default_app_id")]
    pub app_id: String,
    /// Title marker identifying herdr-hosting terminal windows.
    #[serde(default = "default_title_marker")]
    pub title_marker: String,
    /// How long to wait for the terminal title to settle after
    /// `herdr agent focus` before giving up on workspace-labelled matching.
    #[serde(default = "default_focus_timeout")]
    pub focus_timeout_ms: u64,
    /// Poll interval while waiting for the title to settle.
    #[serde(default = "default_poll_interval")]
    pub poll_interval_ms: u64,
}

impl Default for NiriConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            app_id: default_app_id(),
            title_marker: default_title_marker(),
            focus_timeout_ms: default_focus_timeout(),
            poll_interval_ms: default_poll_interval(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            statuses: default_statuses(),
            delay_ms: None,
            suppress_active_tab: default_true(),
            detail: Detail::default(),
            click_to_focus: default_true(),
            click_wait_secs: default_click_wait(),
            expire_secs: default_expire(),
            niri: NiriConfig::default(),
        }
    }
}

fn default_statuses() -> Vec<String> {
    vec!["blocked".to_string(), "done".to_string()]
}
fn default_true() -> bool {
    true
}
fn default_click_wait() -> u64 {
    600
}
fn default_expire() -> u64 {
    30
}
fn default_app_id() -> String {
    "wezterm".to_string()
}
fn default_title_marker() -> String {
    " · herdr".to_string()
}
fn default_focus_timeout() -> u64 {
    2000
}
fn default_poll_interval() -> u64 {
    100
}

impl Config {
    /// Loads `config.toml` from `dir` when present. Missing file → defaults.
    /// Invalid file → defaults plus a warning on stderr (never fatal: a
    /// broken config must not silence blocked-agent notifications).
    pub fn load(dir: Option<&PathBuf>) -> Self {
        let Some(dir) = dir else {
            return Self::default();
        };
        let path = dir.join("config.toml");
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match toml::from_str::<Self>(&raw) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!(
                    "herdr-notifications: ignoring invalid {} : {e}",
                    path.display()
                );
                Self::default()
            }
        }
    }

    /// Effective delay: explicit `delay_ms` wins, then herdr's own
    /// `[ui.toast] delay_seconds`, then 1s (herdr's default).
    pub fn effective_delay(&self, herdr_delay_secs: Option<u64>) -> Duration {
        let ms = self
            .delay_ms
            .or_else(|| herdr_delay_secs.map(|s| s * 1000))
            .unwrap_or(1000);
        Duration::from_millis(ms)
    }

    pub fn wants_status(&self, status: &str) -> bool {
        self.statuses.iter().any(|s| s == status)
    }
}

/// Reads herdr's own configured toast delay from the user's herdr config
/// (best effort; any parse trouble yields `None`).
pub fn herdr_toast_delay_secs() -> Option<u64> {
    toast_delay_from_file(&herdr_config_path()?)
}

/// herdr's config file: `$HERDR_CONFIG_FILE` when set, otherwise
/// `<user config dir>/herdr/config.toml`.
fn herdr_config_path() -> Option<std::path::PathBuf> {
    if let Ok(path) = std::env::var("HERDR_CONFIG_FILE") {
        return Some(std::path::PathBuf::from(path));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
        })?;
    Some(base.join("herdr").join("config.toml"))
}

fn toast_delay_from_file(path: &std::path::Path) -> Option<u64> {
    #[derive(Deserialize)]
    struct Toast {
        delay_seconds: Option<u64>,
    }
    #[derive(Deserialize)]
    struct Ui {
        toast: Option<Toast>,
    }
    #[derive(Deserialize)]
    struct HerdrConfig {
        ui: Option<Ui>,
    }
    let raw = std::fs::read_to_string(path).ok()?;
    let cfg: HerdrConfig = toml::from_str(&raw).ok()?;
    cfg.ui?.toast?.delay_seconds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_notify_blocked_and_done() {
        let cfg = Config::default();
        assert!(cfg.wants_status("blocked"));
        assert!(cfg.wants_status("done"));
        assert!(!cfg.wants_status("working"));
        assert!(!cfg.wants_status("idle"));
        assert_eq!(cfg.detail, Detail::Rich);
        assert!(cfg.click_to_focus);
        assert_eq!(cfg.niri.title_marker, " · herdr");
    }

    #[test]
    fn effective_delay_prefers_plugin_config_then_herdr() {
        let mut cfg = Config::default();
        assert_eq!(cfg.effective_delay(None), Duration::from_millis(1000));
        assert_eq!(cfg.effective_delay(Some(3)), Duration::from_millis(3000));
        cfg.delay_ms = Some(250);
        assert_eq!(cfg.effective_delay(Some(3)), Duration::from_millis(250));
    }

    #[test]
    fn loads_toml_overrides() {
        let dir = std::env::temp_dir().join("herdr-notifications-test-config");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            "statuses = [\"blocked\"]\ndelay_ms = 250\ndetail = \"minimal\"\n[niri]\ntitle_marker = \" ~ herdr\"\n",
        )
        .unwrap();
        let cfg = Config::load(Some(&dir));
        assert!(!cfg.wants_status("done"));
        assert!(cfg.wants_status("blocked"));
        assert_eq!(cfg.effective_delay(None), Duration::from_millis(250));
        assert_eq!(cfg.detail, Detail::Minimal);
        assert_eq!(cfg.niri.title_marker, " ~ herdr");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalid_toml_falls_back_to_defaults() {
        let dir = std::env::temp_dir().join("herdr-notifications-test-bad");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), "statuses = oops").unwrap();
        let cfg = Config::load(Some(&dir));
        assert!(cfg.wants_status("blocked") && cfg.wants_status("done"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_dir_is_defaults() {
        assert_eq!(Config::load(None), Config::default());
        let dir = std::env::temp_dir().join("herdr-notifications-test-absent");
        assert_eq!(Config::load(Some(&dir)), Config::default());
    }

    #[test]
    fn herdr_delay_parses_real_config_shape() {
        let dir = std::env::temp_dir().join("herdr-notifications-test-herdr-cfg");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            "[ui.toast]\ndelivery = \"system\"\ndelay_seconds = 2\n",
        )
        .unwrap();
        assert_eq!(toast_delay_from_file(&dir.join("config.toml")), Some(2));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn toast_delay_tolerates_missing_or_other_shapes() {
        let dir = std::env::temp_dir().join("herdr-notifications-test-herdr-cfg2");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), "[ui.toast]\ndelivery = \"off\"\n").unwrap();
        assert_eq!(toast_delay_from_file(&dir.join("config.toml")), None);
        assert_eq!(toast_delay_from_file(&dir.join("absent.toml")), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
