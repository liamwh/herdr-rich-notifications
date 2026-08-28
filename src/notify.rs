//! Native notification delivery via `notify-rust`, kept behind a trait so
//! decision logic stays unit-testable.
//!
//! Delivery runs on a background thread so a stuck notification daemon can
//! never block (or hang) a herdr event hook: the foreground side waits only
//! bounded amounts — [`SHOW_TIMEOUT`] for the daemon to acknowledge the show,
//! and the configured click-wait for a user click. No sound is requested:
//! herdr's own independent `[ui.sound]` keeps owning audio.

use std::sync::mpsc;
use std::time::Duration;

use notify_rust::{Notification, NotificationResponse, Timeout, Urgency};

use crate::enrich::{NotificationContent, StatusKind};
use crate::focus::ClickTarget;

/// How long to wait for the notification backend to acknowledge the show.
const SHOW_TIMEOUT: Duration = Duration::from_secs(5);

/// What happened to a delivered notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// Nobody clicked before the wait elapsed.
    Expired,
    /// The default action (body/button click) fired.
    Clicked,
    /// The daemon failed to show the notification.
    Failed(String),
}

pub trait Notifier {
    /// Shows `content`; when `target` is present the notification is made
    /// clickable and this call waits (bounded) for a click.
    fn show(
        &self,
        content: &NotificationContent,
        target: Option<&ClickTarget>,
        click_wait: Duration,
        expire: Duration,
    ) -> DeliveryOutcome;
}

/// Production notifier: real D-Bus notifications.
#[derive(Debug, Clone, Default)]
pub struct DesktopNotifier;

impl Notifier for DesktopNotifier {
    fn show(
        &self,
        content: &NotificationContent,
        target: Option<&ClickTarget>,
        click_wait: Duration,
        expire: Duration,
    ) -> DeliveryOutcome {
        send_notification(content, target, click_wait, expire)
    }
}

fn send_notification(
    content: &NotificationContent,
    target: Option<&ClickTarget>,
    click_wait: Duration,
    expire: Duration,
) -> DeliveryOutcome {
    let summary = content.title.clone();
    let body = content.body.clone();
    let urgency = match content.kind {
        StatusKind::Blocked => Urgency::Normal,
        StatusKind::Done => Urgency::Low,
    };
    let target = target.cloned();
    let wants_click = target.is_some();

    let (shown_tx, shown_rx) = mpsc::channel::<Result<(), String>>();
    let (click_tx, click_rx) = mpsc::channel::<bool>();

    std::thread::spawn(move || {
        let mut notification = Notification::new();
        notification
            .appname("herdr")
            .summary(&summary)
            .body(&body)
            .auto_icon()
            .urgency(urgency)
            .timeout(Timeout::Milliseconds(
                expire.as_millis().clamp(1, u32::MAX as u128) as u32,
            ));
        if wants_click {
            // Registered so both body clicks (Noctalia invokes "default" on
            // body click only when the action exists) and an explicit
            // labeled button work across daemons.
            notification.action("default", "Focus agent");
        }
        let handle = match notification.show() {
            Ok(handle) => handle,
            Err(e) => {
                let _ = shown_tx.send(Err(e.to_string()));
                return;
            }
        };
        let _ = shown_tx.send(Ok(()));
        if wants_click {
            let _ = handle.wait_for_response(move |response: &NotificationResponse| {
                let _ = click_tx.send(response.is_default_action());
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

    if shown.is_err() {
        return DeliveryOutcome::Failed("show failed".into());
    }
    if !wants_click {
        return DeliveryOutcome::Expired;
    }

    match click_rx.recv_timeout(click_wait) {
        Ok(true) => DeliveryOutcome::Clicked,
        // Non-default action or channel close without a default click.
        Ok(false) | Err(_) => DeliveryOutcome::Expired,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urgency_maps_from_kind() {
        let blocked = NotificationContent {
            title: "t".into(),
            body: "b".into(),
            kind: StatusKind::Blocked,
        };
        let done = NotificationContent {
            title: "t".into(),
            body: "b".into(),
            kind: StatusKind::Done,
        };
        assert_eq!(
            match blocked.kind {
                StatusKind::Blocked => Urgency::Normal,
                StatusKind::Done => Urgency::Low,
            },
            Urgency::Normal
        );
        assert_eq!(
            match done.kind {
                StatusKind::Blocked => Urgency::Normal,
                StatusKind::Done => Urgency::Low,
            },
            Urgency::Low
        );
    }
}
