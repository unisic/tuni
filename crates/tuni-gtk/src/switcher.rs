//! The tab switcher: hold `Ctrl` and press `Tab` to walk the tabs of the
//! project in front, most recently used first.
//!
//! kero's switcher walks the strip in tab order. This one walks in the order
//! the tabs were last worked in, which is what a held modifier is for: one
//! press and release goes back to where you just were, and holding through
//! several presses reaches further back. It is the order every application
//! switcher on this desktop uses, and the reason the shortcut is worth holding
//! rather than pressing.
//!
//! Nothing is selected while the switcher is up. The highlight moves, the tabs
//! do not, and releasing `Ctrl` commits — so a switch that turns out to be
//! wrong costs one more press rather than a round trip. `Escape` cancels.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use tuni_core::workspace::Id;

/// How many cards fit in a row before the grid wraps, and how big one is.
/// kero's numbers, in the units GTK asks for.
const PER_ROW: u32 = 5;
const CARD_WIDTH: i32 = 194;
const PREVIEW_WIDTH: i32 = 176;
const PREVIEW_HEIGHT: i32 = 119;

/// One tab, as the switcher shows it.
pub struct Card {
    pub tab: Id,
    pub title: String,
    pub icon: &'static str,
    /// The text of whatever has the keyboard in that tab: a terminal's screen,
    /// or the name of the file a pane is holding.
    pub preview: String,
}

mod imp {
    use super::{Cell, Id, RefCell, glib};
    use adw::subclass::prelude::*;

    #[derive(Default)]
    pub struct TuniSwitcher {
        pub(super) grid: RefCell<Option<gtk::FlowBox>>,
        pub(super) scroller: RefCell<Option<gtk::ScrolledWindow>>,
        /// The tabs on offer, in the order the cards are in, and which card the
        /// highlight is on.
        pub(super) tabs: RefCell<Vec<Id>>,
        pub(super) cards: RefCell<Vec<gtk::Widget>>,
        pub(super) at: Cell<usize>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TuniSwitcher {
        const NAME: &'static str = "TuniSwitcher";
        type Type = super::TuniSwitcher;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for TuniSwitcher {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build();
        }
    }

    impl WidgetImpl for TuniSwitcher {}
    impl BinImpl for TuniSwitcher {}
}

glib::wrapper! {
    pub struct TuniSwitcher(ObjectSubclass<imp::TuniSwitcher>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for TuniSwitcher {
    fn default() -> Self {
        Self::new()
    }
}

impl TuniSwitcher {
    #[must_use]
    pub fn new() -> Self {
        glib::Object::new()
    }

    fn build(&self) {
        let grid = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .max_children_per_line(PER_ROW)
            .min_children_per_line(1)
            .homogeneous(true)
            .row_spacing(0)
            .column_spacing(0)
            .build();

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .propagate_natural_width(true)
            .max_content_height(600)
            .child(&grid)
            .build();

        let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
        frame.append(&scroller);
        frame.add_css_class("osd");
        frame.add_css_class("tuni-switcher");

        self.set_child(Some(&frame));
        self.set_halign(gtk::Align::Center);
        self.set_valign(gtk::Align::Center);
        // A switcher is a thing to look through, not to click: the pointer goes
        // to the terminal underneath, which is where it was.
        self.set_can_target(false);
        self.set_visible(false);
        self.imp().grid.replace(Some(grid));
        self.imp().scroller.replace(Some(scroller));
    }

    /// Whether the switcher is up, which is what makes `Tab` mean "next card"
    /// rather than "open the switcher".
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.is_visible()
    }

    /// Shows the cards with the highlight on `at`, replacing whatever was
    /// there.
    pub fn open(&self, cards: &[Card], at: usize) {
        let imp = self.imp();
        let Some(grid) = imp.grid.borrow().clone() else {
            return;
        };
        while let Some(child) = grid.first_child() {
            grid.remove(&child);
        }

        let mut widgets = Vec::with_capacity(cards.len());
        for card in cards {
            let widget = build_card(card);
            grid.append(&widget);
            widgets.push(widget);
        }
        // A row is as wide as it has cards to hold, up to the cap: a flow box
        // asked for five per line reserves five per line whether or not there
        // are five, and the switcher would sit off-center over empty space.
        grid.set_max_children_per_line((cards.len() as u32).clamp(1, PER_ROW));
        imp.tabs
            .replace(cards.iter().map(|card| card.tab).collect());
        imp.cards.replace(widgets);
        imp.at.set(0);
        self.set_visible(true);
        self.highlight(at.min(cards.len().saturating_sub(1)));
    }

    /// Moves the highlight, wrapping at both ends: the switcher is a ring, and
    /// holding `Tab` down long enough comes back to where it started.
    pub fn step(&self, forward: bool) {
        let imp = self.imp();
        let count = imp.cards.borrow().len();
        if count == 0 {
            return;
        }
        let delta = if forward { 1 } else { -1 };
        let next = (imp.at.get() as i32 + delta).rem_euclid(count as i32) as usize;
        self.highlight(next);
    }

    fn highlight(&self, at: usize) {
        let imp = self.imp();
        let cards = imp.cards.borrow();
        for (index, card) in cards.iter().enumerate() {
            if index == at {
                card.add_css_class("selected");
            } else {
                card.remove_css_class("selected");
            }
        }
        if let Some(card) = cards.get(at) {
            self.reveal(card);
        }
        imp.at.set(at);
    }

    /// Scrolls a card into view, for a project with more tabs than fit on
    /// screen at once.
    fn reveal(&self, card: &gtk::Widget) {
        let Some(scroller) = self.imp().scroller.borrow().clone() else {
            return;
        };
        let Some(grid) = self.imp().grid.borrow().clone() else {
            return;
        };
        let Some(bounds) = card.compute_bounds(&grid) else {
            return;
        };
        let adjustment = scroller.vadjustment();
        let top = f64::from(bounds.y());
        let bottom = top + f64::from(bounds.height());
        if top < adjustment.value() {
            adjustment.set_value(top);
        } else if bottom > adjustment.value() + adjustment.page_size() {
            adjustment.set_value(bottom - adjustment.page_size());
        }
    }

    /// The tab the highlight is on, which is what releasing `Ctrl` selects.
    #[must_use]
    pub fn highlighted(&self) -> Option<Id> {
        let imp = self.imp();
        imp.tabs.borrow().get(imp.at.get()).copied()
    }

    /// Takes the switcher down. Whether anything is selected is the caller's
    /// business: `Escape` throws the highlight away, `Ctrl` released keeps it.
    pub fn close(&self) {
        let imp = self.imp();
        self.set_visible(false);
        if let Some(grid) = imp.grid.borrow().clone() {
            while let Some(child) = grid.first_child() {
                grid.remove(&child);
            }
        }
        imp.cards.borrow_mut().clear();
        imp.tabs.borrow_mut().clear();
        imp.at.set(0);
    }
}

fn build_card(card: &Card) -> gtk::Widget {
    let preview = gtk::Label::builder()
        .label(&card.preview)
        .xalign(0.0)
        .yalign(0.0)
        .wrap(false)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        // Otherwise the card asks to be as wide as the longest line the shell
        // ever printed. An ellipsized label with a width in characters asks for
        // that much and no more, and the card gives it what it has.
        .max_width_chars(1)
        .build();
    preview.add_css_class("monospace");

    // The label is bigger than the card on any terminal worth previewing, so
    // the card clips it rather than growing to fit.
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.append(&preview);
    frame.set_size_request(PREVIEW_WIDTH, PREVIEW_HEIGHT);
    frame.set_overflow(gtk::Overflow::Hidden);
    frame.add_css_class("tuni-switch-preview");

    let icon = gtk::Image::from_icon_name(card.icon);
    let title = gtk::Label::builder()
        .label(&card.title)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .xalign(0.0)
        .hexpand(true)
        .build();

    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.append(&icon);
    line.append(&title);

    let column = gtk::Box::new(gtk::Orientation::Vertical, 9);
    column.append(&frame);
    column.append(&line);
    column.set_size_request(CARD_WIDTH, -1);
    column.add_css_class("tuni-switch-card");
    column.upcast()
}
