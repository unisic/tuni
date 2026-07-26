//! Lifetime diagnostics for the objects that own the expensive state.
//!
//! A closed pane is supposed to take its terminal, its VT, its scrollback and
//! its images with it. Nothing in a running window can show that: a widget
//! that has been unparented and dropped from its registry looks exactly like
//! one that is still alive behind a stale reference. `TUNI_DEBUG_LIFETIME`
//! prints a line per construction and destruction with the live count per
//! type, which turns "it should be gone" into something a test can read.
//!
//! Off unless the variable is set, and the variable is read once per process
//! rather than per event.

use std::cell::RefCell;

use gtk::glib;
use gtk::prelude::*;

fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("TUNI_DEBUG_LIFETIME").is_some())
}

thread_local! {
    /// Live count per type. A short association list rather than a map: there
    /// are a handful of types, and this only runs when the variable is set.
    static LIVE: RefCell<Vec<(&'static str, i64)>> = const { RefCell::new(Vec::new()) };
}

/// One of `kind` now exists.
pub fn born(kind: &'static str) {
    track(kind, 1, '+');
}

/// One of `kind` has been destroyed — the Rust destructor ran, so the GObject
/// reached finalize and nothing holds it any more.
pub fn died(kind: &'static str) {
    track(kind, -1, '-');
}

/// Asks, three seconds after something was supposed to be released, whether it
/// still exists — and if it does, how many references are holding it. Three
/// seconds is long enough for a tab-close animation, a hangup and the frame
/// after them to have finished.
pub fn watch<T: IsA<glib::Object>>(object: &T, kind: &'static str, id: u64) {
    if !enabled() {
        return;
    }
    let weak = object.as_ref().downgrade();
    glib::timeout_add_seconds_local_once(3, move || match weak.upgrade() {
        Some(alive) => {
            let refs = alive.ref_count();
            eprintln!("tuni lifetime !{kind} id={id} still alive refs={refs}");
        }
        None => eprintln!("tuni lifetime ok {kind} id={id} released"),
    });
}

/// A one-off line about something that happened, for the same runs.
pub fn note(what: &str) {
    if enabled() {
        eprintln!("tuni lifetime . {what}");
    }
}

fn track(kind: &'static str, delta: i64, sign: char) {
    if !enabled() {
        return;
    }
    let live = LIVE.with_borrow_mut(|counts| {
        if let Some(entry) = counts.iter_mut().find(|(name, _)| *name == kind) {
            entry.1 += delta;
            entry.1
        } else {
            counts.push((kind, delta));
            delta
        }
    });
    eprintln!("tuni lifetime {sign}{kind} live={live}");
}
