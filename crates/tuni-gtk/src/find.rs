//! The find bar: what is typed here is looked for in the terminal below it.
//!
//! One bar for the window rather than one per pane. A pane's widgets are thrown
//! away and rebuilt whenever the layout changes shape, and a bar that lived
//! there would go with them mid-search; this one floats over the content area
//! and remembers which terminal it was opened for. Moving the keyboard to
//! another pane closes it, so what is highlighted always belongs to the
//! terminal the bar is pointing at.

use std::cell::RefCell;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use crate::terminal::{FindStatus, TuniTerminal};

mod imp {
    use super::{RefCell, TuniTerminal, glib};
    use adw::subclass::prelude::*;

    #[derive(Default)]
    pub struct TuniFind {
        pub entry: RefCell<Option<gtk::SearchEntry>>,
        pub tally: RefCell<Option<gtk::Label>>,
        pub previous: RefCell<Option<gtk::Button>>,
        pub next: RefCell<Option<gtk::Button>>,
        /// The terminal being searched, and the handler listening for its output
        /// to move the matches, so both can be let go when the bar closes.
        pub target: RefCell<Option<TuniTerminal>>,
        pub watch: RefCell<Option<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TuniFind {
        const NAME: &'static str = "TuniFind";
        type Type = super::TuniFind;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for TuniFind {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build();
        }
    }

    impl WidgetImpl for TuniFind {}
    impl BinImpl for TuniFind {}
}

glib::wrapper! {
    pub struct TuniFind(ObjectSubclass<imp::TuniFind>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for TuniFind {
    fn default() -> Self {
        Self::new()
    }
}

impl TuniFind {
    #[must_use]
    pub fn new() -> Self {
        glib::Object::new()
    }

    fn build(&self) {
        let imp = self.imp();

        let entry = gtk::SearchEntry::builder()
            .placeholder_text("Find")
            .width_chars(24)
            .build();
        entry.connect_search_changed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |entry| this.look_for(&entry.text())
        ));
        // Enter walks forward through the matches, Shift+Enter back, which is
        // what every find bar on this desktop does. `SearchEntry` reports the
        // plain one; the shifted one arrives as a key press.
        entry.connect_next_match(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.step(true)
        ));
        entry.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.step(true)
        ));
        entry.connect_stop_search(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.close()
        ));

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| {
                let shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);
                match key {
                    gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter if shift => {
                        this.step(false);
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            }
        ));
        entry.add_controller(keys);

        let tally = gtk::Label::new(None);
        tally.add_css_class("dim-label");
        tally.add_css_class("numeric");
        tally.set_width_chars(7);

        let previous = icon_button("go-up-symbolic", "Previous Match (Shift+Enter)");
        previous.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.step(false)
        ));
        let next = icon_button("go-down-symbolic", "Next Match (Enter)");
        next.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.step(true)
        ));
        let close = icon_button("window-close-symbolic", "Close (Escape)");
        close.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.close()
        ));

        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.append(&entry);
        bar.append(&tally);
        bar.append(&previous);
        bar.append(&next);
        bar.append(&close);
        // The overlay styling libadwaita gives floating controls: a bar over a
        // terminal has no background of its own to sit on.
        bar.add_css_class("toolbar");
        bar.add_css_class("osd");
        bar.add_css_class("tuni-find");

        self.set_child(Some(&bar));
        self.set_halign(gtk::Align::End);
        self.set_valign(gtk::Align::Start);
        self.set_margin_top(12);
        self.set_margin_end(12);
        self.set_visible(false);

        imp.entry.replace(Some(entry));
        imp.tally.replace(Some(tally));
        imp.previous.replace(Some(previous));
        imp.next.replace(Some(next));
    }

    /// Points the bar at a terminal and shows it.
    ///
    /// Asking for the bar while it is already up re-runs the search rather than
    /// clearing it, so the shortcut is a way back to the entry with what was
    /// typed still in it and selected, ready to be replaced.
    pub fn open(&self, terminal: &TuniTerminal) {
        let imp = self.imp();
        let same = imp
            .target
            .borrow()
            .as_ref()
            .is_some_and(|current| current == terminal);
        if !same {
            self.release();
            let watch = terminal.connect_local(
                "find-changed",
                false,
                glib::clone!(
                    #[weak(rename_to = this)]
                    self,
                    #[upgrade_or]
                    None,
                    move |_| {
                        this.show_status(
                            this.imp()
                                .target
                                .borrow()
                                .as_ref()
                                .map(TuniTerminal::find_status)
                                .unwrap_or_default(),
                        );
                        None
                    }
                ),
            );
            imp.target.replace(Some(terminal.clone()));
            imp.watch.replace(Some(watch));
        }

        self.set_visible(true);
        let Some(entry) = imp.entry.borrow().clone() else {
            return;
        };
        self.look_for(&entry.text());
        entry.grab_focus();
        entry.select_region(0, -1);
    }

    /// Takes the bar down, the highlight with it, and hands the keyboard back to
    /// the terminal — which is where it came from, and where the next thing
    /// typed almost certainly belongs.
    pub fn close(&self) {
        self.shut(true);
    }

    /// Closes the bar if it is pointing at a terminal other than this one, which
    /// is how moving the keyboard to another pane puts it away. The keyboard is
    /// left where it went rather than dragged back.
    pub fn close_unless(&self, terminal: Option<&TuniTerminal>) {
        let pointed = self.imp().target.borrow().clone();
        match (pointed, terminal) {
            (Some(pointed), Some(terminal)) if &pointed == terminal => {}
            (None, _) => {}
            _ => self.shut(false),
        }
    }

    fn shut(&self, refocus: bool) {
        if !self.is_visible() {
            return;
        }
        self.set_visible(false);
        let terminal = self.imp().target.borrow().clone();
        self.release();
        if let Some(terminal) = terminal {
            terminal.find_clear();
            if refocus {
                terminal.grab_focus();
            }
        }
    }

    /// Drops the terminal being searched and stops listening to it.
    fn release(&self) {
        let imp = self.imp();
        let target = imp.target.take();
        if let (Some(target), Some(watch)) = (target, imp.watch.take()) {
            target.disconnect(watch);
        }
    }

    /// Types into the entry on the harness's behalf, so a capture drives the
    /// same path a person's keystrokes do.
    pub(crate) fn search_text(&self, needle: &str) {
        if let Some(entry) = self.imp().entry.borrow().as_ref() {
            entry.set_text(needle);
        }
    }

    /// Steps forward or back, for the harness.
    pub(crate) fn step_match(&self, forward: bool) {
        self.step(forward);
    }

    /// What the bar says beside the entry, for the harness to check.
    #[must_use]
    pub(crate) fn tally(&self) -> String {
        self.imp()
            .tally
            .borrow()
            .as_ref()
            .map(|label| label.label().to_string())
            .unwrap_or_default()
    }

    fn look_for(&self, needle: &str) {
        let Some(terminal) = self.imp().target.borrow().clone() else {
            return;
        };
        let status = terminal.find(needle);
        // Typing walks to a match as it goes, the way a browser's find does, so
        // what the tally counts is on screen rather than somewhere in the
        // scrollback.
        if status.total > 0 {
            self.show_status(terminal.find_step(true));
        } else {
            self.show_status(status);
        }
    }

    fn step(&self, forward: bool) {
        let Some(terminal) = self.imp().target.borrow().clone() else {
            return;
        };
        self.show_status(terminal.find_step(forward));
    }

    fn show_status(&self, status: FindStatus) {
        let imp = self.imp();
        let Some(tally) = imp.tally.borrow().clone() else {
            return;
        };
        let empty = imp
            .entry
            .borrow()
            .as_ref()
            .is_none_or(|entry| entry.text().is_empty());

        tally.set_label(&match (empty, status.total, status.current) {
            (true, _, _) => String::new(),
            (false, 0, _) => "No results".to_owned(),
            (false, total, Some(current)) => format!("{current} of {total}"),
            (false, total, None) => format!("{total} found"),
        });

        let any = status.total > 0;
        for button in [&imp.previous, &imp.next] {
            if let Some(button) = button.borrow().as_ref() {
                button.set_sensitive(any);
            }
        }
        if let Some(entry) = imp.entry.borrow().as_ref() {
            // Red text for a needle that is not there, the same signal
            // GtkSearchEntry gives when a search comes back with nothing.
            if any || empty {
                entry.remove_css_class("error");
            } else {
                entry.add_css_class("error");
            }
        }
    }
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .build();
    button.add_css_class("flat");
    button
}
