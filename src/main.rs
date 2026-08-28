//! herdr-notifications: rich native desktop notifications for herdr agent
//! status changes. See README.md for the full architecture.
//!
//! Entry points (herdr invokes each via the manifest):
//!
//! * `event` — `pane.agent_status_changed` hook: dedup → delay → re-verify →
//!   suppress → enrich → notify → click-to-focus.
//! * `notify` — manual/test notification (`--smoke` builds a real one for the
//!   focused agent pane, including click-to-focus).
//! * `inspect` — print the notification context for a pane without notifying
//!   (debugging/privacy check).
//!
//! Enrichment is exclusively herdr's own deterministic, local data. The
//! plugin never calls a model, an API, or the network.

mod config;
mod dedup;
mod enrich;
mod event;
mod extract;
mod focus;
mod herdr_api;
mod niri;
mod notify;
mod pipeline;
mod sanitize;

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use config::{Config, Detail};
use enrich::{NotificationContent, StatusKind};
use focus::{ClickResult, ClickTarget};
use herdr_api::{AgentInfo, ApiError, CliHerdr, ExplainInfo, HerdrApi};
use niri::CliNiri;
use notify::{DeliveryOutcome, DesktopNotifier, Notifier};
use pipeline::{PostDelay, decide};

fn log(msg: impl std::fmt::Display) {
    eprintln!("herdr-notifications: {msg}");
}

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let cmd = args.next().unwrap_or_else(|| "event".to_string());
    let rest = args.collect::<Vec<_>>();

    let result = match cmd.as_str() {
        "event" => run_event(),
        "notify" => run_notify(&rest),
        "inspect" => run_inspect(&rest),
        "--version" | "version" => {
            println!("herdr-notifications {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        other => {
            log(format!(
                "unknown subcommand '{other}' (expected 'event', 'notify', or 'inspect')"
            ));
            Err(())
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(()) => ExitCode::FAILURE,
    }
}

/// Handles a herdr `[[events]]` invocation.
fn run_event() -> Result<(), ()> {
    let Ok(payload) = env::var("HERDR_PLUGIN_EVENT_JSON") else {
        // Not running as an event hook (e.g. invoked by hand); nothing to do.
        return Ok(());
    };

    let Some(ev) = event::parse_status_changed(&payload).map_err(|e| {
        log(format!("failed to parse event payload: {e}"));
    })?
    else {
        return Ok(()); // some other pane event; not ours
    };

    log(format!(
        "event: pane {} status {} agent {:?}",
        ev.pane_id, ev.agent_status, ev.agent
    ));

    let cfg = load_config();

    // 1. Record the transition; unchanged statuses are replay noise.
    let state_path = dedup::state_file_path();
    let recorded = dedup::record_on_disk(&state_path, &ev.pane_id, &ev.agent_status);
    if !recorded.changed {
        log(format!(
            "dedup: {} unchanged ({}); skipping",
            ev.pane_id, ev.agent_status
        ));
        return Ok(());
    }

    let Some(kind) = StatusKind::from_status(&ev.agent_status) else {
        log(format!(
            "status {} is not actionable; recorded only",
            ev.agent_status
        ));
        return Ok(());
    };
    if !cfg.wants_status(&ev.agent_status) {
        log(format!(
            "status {} disabled by config; skipping",
            ev.agent_status
        ));
        return Ok(());
    }

    // 2. Delay, mirroring herdr's own toast delay.
    let delay = cfg.effective_delay(config::herdr_toast_delay_secs());
    if !delay.is_zero() {
        log(format!(
            "waiting {delay:?} before re-checking pane {}",
            ev.pane_id
        ));
        std::thread::sleep(delay);
    }

    // 3. Re-verify against live herdr state.
    let herdr = CliHerdr::from_env();
    let info_result = herdr.agent_get(&ev.pane_id);
    let workspace = info_result
        .as_ref()
        .ok()
        .and_then(|info| herdr.workspace_of(&info.workspace_id).ok().flatten());
    match &info_result {
        Err(ApiError::Herdr { code, .. }) if code == "agent_not_found" => {
            log(format!(
                "pane {} disappeared before display; skipping",
                ev.pane_id
            ));
            return Ok(());
        }
        Err(e) => {
            log(format!(
                "agent get failed for {}: {e}; skipping",
                ev.pane_id
            ));
            return Ok(());
        }
        Ok(_) => {}
    }
    let info = info_result.expect("checked above");

    let still_latest = dedup::latest_on_disk(&state_path, &ev.pane_id, recorded.generation);
    let decision = decide(
        &cfg,
        kind,
        PostDelay {
            info: Some(&info),
            workspace: workspace.as_ref(),
            still_latest,
        },
    );
    let pipeline::Decision::Notify = decision else {
        if let pipeline::Decision::Skip(reason) = decision {
            log(format!("suppressed: {reason}"));
        }
        return Ok(());
    };

    // 4. Enrich + notify.
    let content = build_content(&herdr, &info, workspace.as_ref(), kind, &cfg);
    log(format!("notifying: title {:?}", content.title));
    log(format!("notifying: body  {:?}", content.body));

    let target = click_target(&info, workspace.as_ref(), &cfg);
    let click_wait = Duration::from_secs(cfg.click_wait_secs);
    let expire = Duration::from_secs(cfg.expire_secs);
    let outcome = DesktopNotifier.show(&content, target.as_ref(), click_wait, expire);
    if let DeliveryOutcome::Failed(e) = &outcome {
        log(format!("notification delivery failed: {e}"));
    }

    // 5. Click handling (only when a click target was attached).
    if outcome == DeliveryOutcome::Clicked
        && let Some(target) = &target
    {
        let click_herdr =
            CliHerdr::from_target(target.bin_path.as_deref(), target.socket_path.as_deref());
        let result: ClickResult = focus::handle_click(
            target,
            &click_herdr,
            &CliNiri,
            &cfg.niri,
            std::thread::sleep,
        );
        log(format!(
            "click: herdr focus {:?}, niri focus {:?} (window {:?})",
            result.herdr_focus, result.niri_focus, result.window_id
        ));
    }

    Ok(())
}

/// Handles the manual/action invocation.
fn run_notify(args: &[String]) -> Result<(), ()> {
    let mut smoke = false;
    let mut title: Option<String> = None;
    let mut body = String::new();
    let mut iter = args.iter().map(String::as_str);
    while let Some(arg) = iter.next() {
        match arg {
            "--smoke" => smoke = true,
            "--title" => title = iter.next().map(String::from),
            "--body" => body = iter.next().unwrap_or_default().to_string(),
            other => {
                log(format!("notify: unknown argument '{other}'"));
                return Err(());
            }
        }
    }

    if smoke {
        let cfg = load_config();
        let herdr = CliHerdr::from_env();
        let pane = focused_pane_from_context()
            .or_else(|| herdr.agent_get("current").ok().map(|i| i.pane_id));
        let Some(pane) = pane else {
            log("smoke: no focused pane available; falling back to plain notification");
            let content = NotificationContent {
                title: "herdr".into(),
                body: "Notifications are working.".into(),
                kind: StatusKind::Done,
            };
            DesktopNotifier.show(
                &content,
                None,
                Duration::from_secs(5),
                Duration::from_secs(15),
            );
            return Ok(());
        };
        let info = match herdr.agent_get(&pane) {
            Ok(info) => info,
            Err(e) => {
                log(format!("smoke: agent get for {pane} failed: {e}"));
                return Err(());
            }
        };
        let workspace = herdr.workspace_of(&info.workspace_id).ok().flatten();
        let kind = StatusKind::from_status(&info.agent_status).unwrap_or(StatusKind::Blocked);
        let content = build_content(&herdr, &info, workspace.as_ref(), kind, &cfg);
        log(format!("smoke: title {:?}", content.title));
        log(format!("smoke: body  {:?}", content.body));
        let target = click_target(&info, workspace.as_ref(), &cfg);
        let outcome = DesktopNotifier.show(
            &content,
            target.as_ref(),
            Duration::from_secs(300),
            Duration::from_secs(30),
        );
        if outcome == DeliveryOutcome::Clicked
            && let Some(target) = &target
        {
            let click_herdr =
                CliHerdr::from_target(target.bin_path.as_deref(), target.socket_path.as_deref());
            let result = focus::handle_click(
                target,
                &click_herdr,
                &CliNiri,
                &cfg.niri,
                std::thread::sleep,
            );
            log(format!(
                "smoke click: herdr {:?}, niri {:?} (window {:?})",
                result.herdr_focus, result.niri_focus, result.window_id
            ));
        }
        return Ok(());
    }

    let Some(title) = title else {
        log("notify requires --title <TEXT> (or --smoke)");
        return Err(());
    };
    let content = NotificationContent {
        title,
        body,
        kind: StatusKind::Done,
    };
    DesktopNotifier.show(
        &content,
        None,
        Duration::from_secs(1),
        Duration::from_secs(15),
    );
    Ok(())
}

/// Prints the notification context for a pane without notifying.
fn run_inspect(args: &[String]) -> Result<(), ()> {
    let target = args
        .first()
        .cloned()
        .or_else(focused_pane_from_context)
        .ok_or_else(|| {
            log("inspect requires <pane-id> (or a focused pane in the plugin context)");
        })?;
    let cfg = load_config();
    let herdr = CliHerdr::from_env();
    let info = herdr
        .agent_get(&target)
        .map_err(|e| log(format!("agent get failed: {e}")))?;
    let workspace = herdr.workspace_of(&info.workspace_id).ok().flatten();
    let kind = StatusKind::from_status(&info.agent_status).unwrap_or(StatusKind::Blocked);

    let mut explain_json = serde_json::Value::Null;
    let explain = herdr.explain(&target).ok().flatten();
    if let Some(explain) = &explain {
        explain_json = serde_json::json!({
            "state": explain.state,
            "matched_rule_id": explain.matched_rule_id,
            "screen_detection_skipped": explain.screen_detection_skipped,
        });
    }
    let mut excerpt = serde_json::Value::Null;
    if cfg.detail == Detail::Rich
        && let Ok(snapshot) = herdr.read_detection(&target, 40)
    {
        let extracted = match kind {
            StatusKind::Blocked => extract::extract_question(&snapshot, 180),
            StatusKind::Done => extract::extract_tail_line(&snapshot, 180),
        };
        excerpt = serde_json::json!({ "extracted": extracted });
    }

    let content = build_content(&herdr, &info, workspace.as_ref(), kind, &cfg);
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "pane_id": info.pane_id,
            "status": info.agent_status,
            "title": content.title,
            "body": content.body,
            "explain": explain_json,
            "excerpt": excerpt,
        }))
        .map_err(|e| log(format!("serialize failed: {e}")))?
    );
    Ok(())
}

/// Gathers the labels + deterministic evidence and builds the content.
fn build_content(
    herdr: &dyn HerdrApi,
    info: &AgentInfo,
    workspace: Option<&herdr_api::WorkspaceInfo>,
    kind: StatusKind,
    cfg: &Config,
) -> NotificationContent {
    let tab_label = herdr.tab_label(&info.tab_id).ok().flatten();
    let ws_label = workspace.and_then(|ws| ws.label.clone());

    let explain: Option<ExplainInfo> = match kind {
        // explain is a local rule evaluation; for blocked panes it may name
        // the matched detection rule (screen-manifest agents).
        StatusKind::Blocked => herdr.explain(&info.pane_id).ok().flatten(),
        StatusKind::Done => None,
    };

    let mut question = None;
    let mut tail = None;
    if cfg.detail == Detail::Rich
        && let Ok(snapshot) = herdr.read_detection(&info.pane_id, 40)
    {
        match kind {
            StatusKind::Blocked => question = extract::extract_question(&snapshot, 180),
            StatusKind::Done => tail = extract::extract_tail_line(&snapshot, 180),
        }
    }

    enrich::build(enrich::EnrichInput {
        kind,
        info,
        ws_label: ws_label.as_deref(),
        tab_label: tab_label.as_deref(),
        explain: explain.as_ref(),
        question: question.as_deref(),
        tail: tail.as_deref(),
        detail: cfg.detail,
    })
}

/// Click target carrying the event's session identity through to the action.
fn click_target(
    info: &AgentInfo,
    workspace: Option<&herdr_api::WorkspaceInfo>,
    cfg: &Config,
) -> Option<ClickTarget> {
    if !cfg.click_to_focus {
        return None;
    }
    Some(ClickTarget {
        pane_id: info.pane_id.clone(),
        workspace_label: enrich::workspace_context(
            info,
            workspace.and_then(|ws| ws.label.as_deref()),
        ),
        socket_path: env::var("HERDR_SOCKET_PATH").ok(),
        bin_path: env::var("HERDR_BIN_PATH").ok(),
    })
}

fn load_config() -> Config {
    let dir = env::var_os("HERDR_PLUGIN_CONFIG_DIR").map(PathBuf::from);
    Config::load(dir.as_ref())
}

/// `focused_pane_id` from `$HERDR_PLUGIN_CONTEXT_JSON` (action invocations).
fn focused_pane_from_context() -> Option<String> {
    let raw = env::var("HERDR_PLUGIN_CONTEXT_JSON").ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value
        .get("focused_pane_id")
        .and_then(|v| v.as_str())
        .map(String::from)
        .filter(|p| !p.is_empty())
}
