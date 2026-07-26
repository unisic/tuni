//! The command palette: everything the window can do, by name.
//!
//! Two kinds of entry in one list. A command is an action the window already
//! has — the palette runs the same action the menu and the keyboard do, which
//! is why nothing here knows what any of them mean. A terminal is a jump to a
//! pane, wherever in the workspace it is, which is the part a strip of tabs
//! cannot do once there is more than one project.
//!
//! Ranking is [`tuni_core::fuzzy`], the same scoring kero's palette uses.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use tuni_core::fuzzy;

/// How tall the list is allowed to get before it scrolls. Roughly ten rows,
/// which is as much as can be read without the eye leaving the entry.
const LIST_HEIGHT: i32 = 340;
const WIDTH: i32 = 560;

/// One row: what it says, and the action it stands for.
pub struct Entry {
    pub title: String,
    /// Shown after the title in dim text — a terminal's working directory.
    pub subtitle: Option<String>,
    pub icon: &'static str,
    /// The keys that do the same thing, so the palette teaches them.
    pub shortcut: Option<&'static str>,
    /// A `win.` action and what to pass it.
    pub action: &'static str,
    pub target: Option<glib::Variant>,
    /// Whether this is a terminal rather than a command, which is what puts it
    /// in the second section.
    pub terminal: bool,
    /// What the query is matched against: the title, widened for a terminal to
    /// take in its project and directory as well, so typing a repository name
    /// finds the shells working in it.
    pub search: String,
}

impl Entry {
    /// A command, named by the action it runs.
    pub fn command(title: &str, icon: &'static str, shortcut: Option<&'static str>, action: &'static str) -> Self {
        Self {
            title: title.to_owned(),
            subtitle: None,
            icon,
            shortcut,
            action,
            target: None,
            terminal: false,
            search: title.to_owned(),
        }
    }

    #[must_use]
    pub fn with_target(mut self, target: glib::Variant) -> Self {
        self.target = Some(target);
        self
    }
}

/// One row's place in the list: which entry it stands for, and nothing else —
/// the widgets are rebuilt on every keystroke and the indices with them.
struct Rows {
    rows: Vec<gtk::ListBoxRow>,
    entries: Vec<usize>,
}

thread_local! {
    /// The palette that is on screen, for the smoke captures to drive. Nothing
    /// else reaches into it: a palette is opened, used and gone within one
    /// keystroke's worth of attention, so there is nothing here to own.
    static OPEN: RefCell<Option<(gtk::SearchEntry, gtk::ListBox)>> = const { RefCell::new(None) };
}

/// Types a query on the harness's behalf, into the palette that is open.
pub(crate) fn type_query(query: &str) {
    OPEN.with_borrow(|open| {
        if let Some((search, _)) = open.as_ref() {
            search.set_text(query);
        }
    });
}

/// What the palette would run if Return were pressed now.
pub(crate) fn selection() -> Option<String> {
    OPEN.with_borrow(|open| {
        let (_, list) = open.as_ref()?;
        row_title(&list.selected_row()?)
    })
}

/// Moves the selection down, the way the arrow key does.
pub(crate) fn move_selection(steps: u32) {
    OPEN.with_borrow(|open| {
        let Some((_, list)) = open.as_ref() else {
            return;
        };
        for _ in 0..steps {
            let next = list
                .selected_row()
                .and_then(|row| next_selectable(row.next_sibling()));
            if let Some(row) = next.or_else(|| next_selectable(list.first_child())) {
                list.select_row(Some(&row));
            }
        }
    });
}

/// The first row from here on that a selection can land on, skipping the
/// section headers between them.
fn next_selectable(from: Option<gtk::Widget>) -> Option<gtk::ListBoxRow> {
    let mut child = from;
    while let Some(widget) = child {
        if let Ok(row) = widget.clone().downcast::<gtk::ListBoxRow>()
            && row.is_selectable()
        {
            return Some(row);
        }
        child = widget.next_sibling();
    }
    None
}

/// Runs the selected row, the way Return does.
pub(crate) fn run_selection() {
    // Out of the borrow first: activating a row closes the dialog, and the
    // handler that runs then clears what is being borrowed here.
    let list = OPEN.with_borrow(|open| open.as_ref().map(|(_, list)| list.clone()));
    if let Some(list) = list
        && let Some(row) = list.selected_row()
    {
        list.emit_by_name::<()>("row-activated", &[&row]);
    }
}

/// The first label in a row, which is its title — the icon before it is an
/// image and the dim text after it comes later.
fn row_title(row: &gtk::ListBoxRow) -> Option<String> {
    let mut child = row.child()?.first_child();
    while let Some(widget) = child {
        if let Ok(label) = widget.clone().downcast::<gtk::Label>() {
            return Some(label.label().to_string());
        }
        child = widget.next_sibling();
    }
    None
}

/// Opens the palette over `parent`, listing `entries`.
pub fn present(parent: &impl IsA<gtk::Widget>, entries: Vec<Entry>) {
    let entries = Rc::new(entries);
    let rows = Rc::new(RefCell::new(Rows {
        rows: Vec::new(),
        entries: Vec::new(),
    }));

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Browse);
    list.add_css_class("navigation-sidebar");

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(LIST_HEIGHT)
        .child(&list)
        .build();

    let nothing = adw::StatusPage::builder()
        .icon_name("edit-find-symbolic")
        .title("No Matches")
        .visible(false)
        .build();
    nothing.add_css_class("compact");

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search commands and terminals")
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();

    // The list and the "nothing found" page take turns rather than sharing a
    // GtkStack: a stack is as tall as its tallest page whichever one is up, and
    // the palette has to be as short as the two rows a narrow query leaves.
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&search);
    content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    content.append(&scroller);
    content.append(&nothing);

    // The width is asked for by the content, because a dialog that follows its
    // content ignores `content-width` — and follow it must, or a palette with
    // two rows in it is as tall as one with ten.
    content.set_size_request(WIDTH, -1);
    let dialog = adw::Dialog::builder()
        .follows_content_size(true)
        .presentation_mode(adw::DialogPresentationMode::Floating)
        .child(&content)
        .build();

    let fill = {
        let entries = Rc::clone(&entries);
        let rows = Rc::clone(&rows);
        let list = list.clone();
        let scroller = scroller.clone();
        let nothing = nothing.clone();
        move |query: &str| {
            let filled = fill_list(&list, &entries, query);
            scroller.set_visible(!filled.rows.is_empty());
            nothing.set_visible(filled.rows.is_empty());
            if let Some(first) = filled.rows.first() {
                list.select_row(Some(first));
            }
            rows.replace(filled);
        }
    };
    fill("");

    search.connect_search_changed({
        let fill = fill.clone();
        move |entry| fill(&entry.text())
    });

    let run = {
        let entries = Rc::clone(&entries);
        let rows = Rc::clone(&rows);
        let dialog = dialog.clone();
        let parent = parent.as_ref().clone();
        move |row: &gtk::ListBoxRow| {
            let index = {
                let rows = rows.borrow();
                rows.rows
                    .iter()
                    .position(|candidate| candidate == row)
                    .and_then(|at| rows.entries.get(at).copied())
            };
            let Some(entry) = index.and_then(|index| entries.get(index)) else {
                return;
            };
            let action = entry.action;
            let target = entry.target.clone();
            dialog.close();
            // After the palette is gone, not before: several of these move the
            // keyboard, and a dialog still on screen would take it back.
            glib::idle_add_local_once({
                let parent = parent.clone();
                move || {
                    let _ = parent.activate_action(action, target.as_ref());
                }
            });
        }
    };

    list.connect_row_activated({
        let run = run.clone();
        move |_, row| run(row)
    });

    // The entry keeps the keyboard the whole time, so the list is driven from
    // here: the arrows move the selection and Return runs it, which is what a
    // palette is for.
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed({
        let list = list.clone();
        let scroller = scroller.clone();
        let rows = Rc::clone(&rows);
        move |_, key, _, _| match key {
            gtk::gdk::Key::Down => {
                step(&list, &scroller, &rows.borrow(), 1);
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Up => {
                step(&list, &scroller, &rows.borrow(), -1);
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    search.add_controller(keys);

    search.connect_activate({
        let list = list.clone();
        move |_| {
            if let Some(row) = list.selected_row() {
                run(&row);
            }
        }
    });

    OPEN.with_borrow_mut(|open| *open = Some((search.clone(), list.clone())));
    dialog.connect_closed(|_| OPEN.with_borrow_mut(|open| *open = None));

    dialog.present(Some(parent.as_ref()));
    search.grab_focus();
}

/// Rebuilds the list for a query, in section order and by score within a
/// section, and reports which entry each row stands for.
fn fill_list(list: &gtk::ListBox, entries: &[Entry], query: &str) -> Rows {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }

    let query = query.trim();
    let mut ranked: Vec<(usize, i32)> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            if query.is_empty() {
                Some((index, 0))
            } else {
                fuzzy::score(&entry.search, query).map(|score| (index, score))
            }
        })
        .collect();
    // Commands stay above terminals however the scores fall, so the list does
    // not reshuffle wholesale between one keystroke and the next.
    ranked.sort_by(|left, right| {
        entries[left.0]
            .terminal
            .cmp(&entries[right.0].terminal)
            .then(right.1.cmp(&left.1))
    });

    let sections = ranked.iter().any(|(index, _)| entries[*index].terminal)
        && ranked.iter().any(|(index, _)| !entries[*index].terminal);

    let mut filled = Rows {
        rows: Vec::new(),
        entries: Vec::new(),
    };
    let mut last: Option<bool> = None;
    for (index, _) in ranked {
        let entry = &entries[index];
        if sections && last != Some(entry.terminal) {
            list.append(&header(if entry.terminal {
                "Terminals"
            } else {
                "Commands"
            }));
            last = Some(entry.terminal);
        }
        let row = build_row(entry);
        list.append(&row);
        filled.rows.push(row);
        filled.entries.push(index);
    }
    filled
}

fn header(title: &str) -> gtk::ListBoxRow {
    let label = gtk::Label::builder()
        .label(title)
        .halign(gtk::Align::Start)
        .margin_top(6)
        .margin_start(6)
        .build();
    label.add_css_class("heading");
    label.add_css_class("dim-label");

    let row = gtk::ListBoxRow::builder()
        .child(&label)
        .selectable(false)
        .activatable(false)
        .build();
    row.set_can_focus(false);
    row
}

fn build_row(entry: &Entry) -> gtk::ListBoxRow {
    let icon = gtk::Image::from_icon_name(entry.icon);
    icon.add_css_class("dim-label");

    let title = gtk::Label::builder()
        .label(&entry.title)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .xalign(0.0)
        .build();

    let line = gtk::Box::new(gtk::Orientation::Horizontal, 9);
    line.append(&icon);
    line.append(&title);

    if let Some(subtitle) = entry.subtitle.as_deref().filter(|text| !text.is_empty()) {
        let label = gtk::Label::builder()
            .label(subtitle)
            // The end of a path says more than its beginning, and the middle is
            // what a reader can do without.
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .xalign(0.0)
            .hexpand(true)
            .build();
        label.add_css_class("dim-label");
        label.add_css_class("caption");
        line.append(&label);
    } else {
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        line.append(&spacer);
    }

    if let Some(shortcut) = entry.shortcut {
        let label = gtk::Label::new(Some(shortcut));
        label.add_css_class("dim-label");
        label.add_css_class("caption");
        line.append(&label);
    }

    gtk::ListBoxRow::builder().child(&line).build()
}

/// Moves the selection, wrapping at both ends the way a palette does.
fn step(list: &gtk::ListBox, scroller: &gtk::ScrolledWindow, rows: &Rows, delta: i32) {
    if rows.rows.is_empty() {
        return;
    }
    let count = rows.rows.len() as i32;
    let current = list
        .selected_row()
        .and_then(|row| rows.rows.iter().position(|candidate| *candidate == row))
        .map_or(0, |at| at as i32);
    let next = (current + delta).rem_euclid(count) as usize;
    let row = &rows.rows[next];
    list.select_row(Some(row));
    reveal(scroller, row);
}

/// Scrolls a selected row into view without giving it the keyboard, which
/// belongs to the entry for as long as the palette is open.
fn reveal(scroller: &gtk::ScrolledWindow, row: &gtk::ListBoxRow) {
    let adjustment = scroller.vadjustment();
    // In the list's own coordinates, which is what the adjustment counts in.
    let Some(list) = row.parent() else {
        return;
    };
    let Some(bounds) = row.compute_bounds(&list) else {
        return;
    };
    let top = f64::from(bounds.y());
    let bottom = top + f64::from(bounds.height());
    if top < adjustment.value() {
        adjustment.set_value(top);
    } else if bottom > adjustment.value() + adjustment.page_size() {
        adjustment.set_value(bottom - adjustment.page_size());
    }
}
