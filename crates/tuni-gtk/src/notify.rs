//! Desktop notifications.
//!
//! A terminal is a place work is left running in, so the things that ask to be
//! shown are `OSC 9`/`OSC 777`/`OSC 99` from a long command, and the bell from
//! one that finished while the pane was out of sight.
//!
//! Delivery is `GNotification`, which reaches the desktop's own service on a
//! session bus and the XDG portal inside a Flatpak without either being named
//! here. The application ID has to match an installed `.desktop` file for the
//! desktop to show anything, which is what `data/dev.unisic.Tuni.desktop` is
//! for.

use gtk::gio;
use gtk::prelude::*;

use tuni_core::workspace::Id;

/// Shows a notification for a pane, replacing whatever that same pane last
/// asked for.
///
/// The replacement is deliberate: a loop that reports every step would
/// otherwise leave a stack of stale banners, and only the last one is worth
/// reading.
///
/// There is no click action on purpose. A terminal launched from a shell has
/// to inherit *that* shell's directory, so the application is non-unique, and
/// a non-unique application cannot be sure a notification's action reaches the
/// window that raised it rather than starting a second copy of Tuni.
pub(crate) fn post(app: &gio::Application, pane: Id, title: &str, body: &str) {
    let title = title.trim();
    let body = body.trim();
    if title.is_empty() && body.is_empty() {
        return;
    }

    let notification = gio::Notification::new(if title.is_empty() { body } else { title });
    if !title.is_empty() && !body.is_empty() {
        notification.set_body(Some(body));
    }
    notification.set_priority(gio::NotificationPriority::Normal);
    app.send_notification(Some(&id(pane)), &notification);
}

/// Takes back whatever a pane last showed — used when the pane is looked at,
/// since a banner about work the user is now watching is noise.
pub(crate) fn withdraw(app: &gio::Application, pane: Id) {
    app.withdraw_notification(&id(pane));
}

fn id(pane: Id) -> String {
    format!("pane-{}", pane.raw())
}
