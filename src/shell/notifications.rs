// SPDX-License-Identifier: GPL-3.0-or-later

//! The notification service apps send their notifications to.

use std::{collections::HashMap, time::Duration};

use chrono::{DateTime, Local};
use tracing::warn;
use zbus::{blocking::Connection, fdo::RequestNameFlags, zvariant::OwnedValue};

pub const BUS_NAME: &str = "org.freedesktop.Notifications";
pub const OBJECT_PATH: &str = "/org/freedesktop/Notifications";

/// How long a pop-up stays when the app doesn't say.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// The urgency of notifications that stay until they're dealt with.
const URGENCY_CRITICAL: u8 = 2;

/// The action of clicking a notification.
const DEFAULT_ACTION: &str = "default";

/// Why a notification went away, as the specification numbers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    Dismissed = 2,
    Closed = 3,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub id: u32,
    pub app: String,
    pub summary: String,
    pub body: String,
    /// Whether clicking it does something in the app.
    pub has_default_action: bool,
    pub critical: bool,
    /// How long its pop-up stays. Without this, it stays until it's dismissed.
    pub timeout: Option<Duration>,
    pub received: DateTime<Local>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A new notification, or a new version of one with the same id.
    Received(Notification),
    /// The app took it back.
    Closed(u32),
}

/// Removes the markup some apps put in the body of notifications.
fn strip_markup(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut in_tag = false;

    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => plain.push(c),
            _ => (),
        }
    }

    plain
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// How long a pop-up stays, from what the app asked for.
fn timeout(requested: i32, critical: bool) -> Option<Duration> {
    match requested {
        // The app leaves it up to us.
        ..0 if critical => None,
        ..0 => Some(DEFAULT_TIMEOUT),
        0 => None,
        milliseconds => Some(Duration::from_millis(milliseconds as u64)),
    }
}

struct Server {
    next_id: u32,
    send: Box<dyn Fn(Event) + Send + Sync>,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    fn get_capabilities(&self) -> Vec<&'static str> {
        vec!["actions", "body", "body-markup", "persistence"]
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(
        &mut self,
        app_name: String,
        replaces_id: u32,
        _app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let id = if replaces_id != 0 {
            replaces_id
        } else {
            self.next_id = self.next_id.wrapping_add(1).max(1);
            self.next_id
        };

        let critical = hints
            .get("urgency")
            .and_then(|urgency| u8::try_from(urgency).ok())
            .is_some_and(|urgency| urgency >= URGENCY_CRITICAL);

        (self.send)(Event::Received(Notification {
            id,
            app: app_name,
            summary: strip_markup(&summary),
            body: strip_markup(&body),
            // Actions come in pairs of a key and a label.
            has_default_action: actions.chunks(2).any(|pair| pair[0] == DEFAULT_ACTION),
            critical,
            timeout: timeout(expire_timeout, critical),
            received: Local::now(),
        }));

        id
    }

    fn close_notification(&self, id: u32) {
        (self.send)(Event::Closed(id));
    }

    fn get_server_information(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        ("Pinnacle", "pinnacle", env!("CARGO_PKG_VERSION"), "1.2")
    }
}

/// Takes over as the notification service on `bus`, sending what comes in to `send`.
///
/// Only one program can do this. If something else already does, there's no way around
/// that, and notifications just go there instead.
pub fn serve(bus: &Connection, send: impl Fn(Event) + Send + Sync + 'static) {
    let result = bus
        .object_server()
        .at(
            OBJECT_PATH,
            Server {
                next_id: 0,
                send: Box::new(send),
            },
        )
        .and_then(|_| bus.request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into()));

    if let Err(err) = result {
        warn!("Not handling notifications, as something else may already be: {err}");
    }
}

/// Tells the app a notification went away.
pub fn emit_closed(bus: &Connection, id: u32, reason: CloseReason) {
    let emitted = bus.emit_signal(
        None::<&str>,
        OBJECT_PATH,
        BUS_NAME,
        "NotificationClosed",
        &(id, reason as u32),
    );
    if let Err(err) = emitted {
        warn!("Failed to tell an app about its notification: {err}");
    }
}

/// Tells the app its notification was clicked.
pub fn emit_clicked(bus: &Connection, id: u32) {
    let emitted = bus.emit_signal(
        None::<&str>,
        OBJECT_PATH,
        BUS_NAME,
        "ActionInvoked",
        &(id, DEFAULT_ACTION),
    );
    if let Err(err) = emitted {
        warn!("Failed to tell an app about its notification: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup() {
        assert_eq!(strip_markup("<b>Hi</b> there"), "Hi there");
        assert_eq!(strip_markup("a &amp; b &lt;c&gt;"), "a & b <c>");
        assert_eq!(strip_markup("1 > 0"), "1 > 0");
        assert_eq!(strip_markup("<a href=\"x\">link</a>"), "link");
    }

    #[test]
    fn timeouts() {
        assert_eq!(timeout(-1, false), Some(DEFAULT_TIMEOUT));
        assert_eq!(timeout(-1, true), None);
        assert_eq!(timeout(0, false), None);
        assert_eq!(timeout(1500, false), Some(Duration::from_millis(1500)));
        // An app can still choose for itself.
        assert_eq!(timeout(1500, true), Some(Duration::from_millis(1500)));
    }
}
