//! Window shortcuts, defaults and overrides applied as one.
//!
//! The defaults stay spelled in `main.rs`, which remains the one place a
//! shortcut is written down; the config file carries only what someone
//! changed, action by action. Applying is idempotent and cheap, so it runs at
//! startup and again whenever the settings move, and the accelerators in
//! force are always the table with the overrides laid over it.
//!
//! Only the actions in the table are configurable. The editor's own keys are
//! a shortcut controller scoped to the widget, deliberately out of reach of
//! window accelerators, and the numbered tab and project keys are a family
//! that only makes sense whole.

use gtk::prelude::*;

use tuni_core::settings::Settings;

pub fn apply(application: &impl IsA<gtk::Application>, settings: &Settings) {
    for (action, defaults) in crate::ACCELS {
        let accels: Vec<&str> = match settings.key_override(action) {
            // Turned off: the action stays reachable from menus and the
            // palette, it just stops owning a key.
            Some("") => Vec::new(),
            Some(accel) => vec![accel],
            None => defaults.to_vec(),
        };
        application.set_accels_for_action(action, &accels);
    }
}

/// What a row in the settings can show for an action: the accelerator in
/// force, or `None` for one turned off.
#[must_use]
pub fn effective(settings: &Settings, action: &str, defaults: &[&str]) -> Option<String> {
    match settings.key_override(action) {
        Some("") => None,
        Some(accel) => Some(accel.to_owned()),
        None => defaults.first().map(|accel| (*accel).to_owned()),
    }
}

/// "win.split-right" said the way a row title says it.
#[must_use]
pub fn label(action: &str) -> String {
    let name = action.strip_prefix("win.").unwrap_or(action);
    let mut out = String::with_capacity(name.len());
    for word in name.split('-') {
        if !out.is_empty() {
            out.push(' ');
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}
