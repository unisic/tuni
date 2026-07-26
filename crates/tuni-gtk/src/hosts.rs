//! The host launcher: everything there is to connect to, in a pane.
//!
//! It is the command palette that stays on screen rather than a wall of cards.
//! Structurally that is [`crate::palette`]: a list rebuilt whole on every
//! keystroke, ranked by [`tuni_core::fuzzy`], driven from the search entry so
//! the arrows and Return work without the keyboard ever leaving it. A row
//! carries a name, an address and where it was declared, which a card would
//! truncate and a list does not.
//!
//! Rebuilding every row per keystroke is fine at the size an `~/.ssh/config`
//! reaches. Past a few hundred hosts the right answer is the `GtkListView` and
//! `GtkListStore` splice that [`crate::files`] uses, and the row builder below
//! is the part that would move.
//!
//! Nothing here connects to anything. Reading the configuration is files, so it
//! happens off this thread; picking a host is a message to the window, which
//! owns the panes a connection could go in.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::gio;
use gtk::glib;

use tuni_core::fuzzy;
use tuni_core::ssh::{self, Host, Hosts};

/// How wide the column gets before the space around it grows instead. A
/// launcher on a wide monitor is a column, not a field of whitespace with three
/// words in the middle of it.
const WIDTH: i32 = 860;

/// What a row stands for.
#[derive(Clone, Debug)]
enum Choice {
    /// Put a plain shell in this pane. Always first under an empty query, so a
    /// launcher opened by accident costs one Return rather than a pane close.
    Local,
    /// Connect, by the name `ssh` is to be given.
    Host(String),
}

mod imp {
    use super::{Cell, Choice, Hosts, Rc, RefCell, glib};
    use adw::prelude::*;
    use adw::subclass::prelude::*;

    pub type Handler = Rc<dyn Fn(Message)>;

    /// What the launcher cannot do itself, on its way to the window.
    pub enum Message {
        /// Put a connection in this pane: the list has done its job.
        Connect(String),
        /// Beside the pane being worked in.
        ConnectToSide(String),
        /// In a tab of its own.
        ConnectInTab(String),
        /// Put a plain shell in this pane instead.
        LocalShell,
        /// Open a file for editing, which is how the real configuration is
        /// reached from a list that only reads it.
        OpenFile(std::path::PathBuf),
    }

    #[derive(Default)]
    pub struct TuniHosts {
        /// Everything the configuration names, and the files it was read from,
        /// which is what answers whether the list has gone stale.
        pub hosts: RefCell<Hosts>,
        /// What each row on screen stands for, in the order they are in. The
        /// widgets are rebuilt on every keystroke and this with them.
        pub(super) rows: RefCell<Vec<(gtk::ListBoxRow, Choice)>>,
        pub search: RefCell<Option<gtk::SearchEntry>>,
        pub list: RefCell<Option<gtk::ListBox>>,
        pub scroller: RefCell<Option<gtk::ScrolledWindow>>,
        pub stack: RefCell<Option<gtk::Stack>>,
        pub count: RefCell<Option<gtk::Label>>,
        pub menu: RefCell<Option<gtk::PopoverMenu>>,
        /// The row whose context menu is open, and so the one every action in
        /// it acts on. The menu fires long after the click that opened it.
        pub target: RefCell<Option<String>>,
        pub message: RefCell<Option<Handler>>,
        /// Set while a read is out, so a refresh during one is dropped rather
        /// than queued behind it.
        pub loading: Cell<bool>,
        /// Bumped whenever a read is started, so a reply that arrives after
        /// another went out is thrown away.
        pub generation: Cell<u64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TuniHosts {
        const NAME: &'static str = "TuniHosts";
        type Type = super::TuniHosts;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for TuniHosts {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build();
        }

        fn dispose(&self) {
            // Parented rather than packed, so it has to be taken off by hand.
            if let Some(menu) = self.menu.take() {
                menu.unparent();
            }
        }
    }

    impl WidgetImpl for TuniHosts {}
    impl BinImpl for TuniHosts {}
}

pub use imp::Message;

glib::wrapper! {
    pub struct TuniHosts(ObjectSubclass<imp::TuniHosts>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for TuniHosts {
    fn default() -> Self {
        Self::new()
    }
}

impl TuniHosts {
    #[must_use]
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn connect_message<F: Fn(Message) + 'static>(&self, callback: F) {
        self.imp().message.replace(Some(Rc::new(callback)));
    }

    fn send(&self, message: Message) {
        let callback = self.imp().message.borrow().clone();
        if let Some(callback) = callback {
            callback(message);
        }
    }

    /// Reads the configuration again, if any of the files it came from has
    /// changed. What the window calls when it comes back to the front, so a
    /// host added in an editor beside this one shows up without being asked
    /// for.
    pub fn refresh_if_stale(&self) {
        if self.imp().hosts.borrow().stale() {
            self.reload();
        }
    }

    // --- construction ------------------------------------------------------

    fn build(&self) {
        let imp = self.imp();

        let heading = gtk::Label::builder().label("Connect").xalign(0.0).build();
        heading.add_css_class("heading");
        let count = gtk::Label::builder().xalign(0.0).build();
        count.add_css_class("caption");
        count.add_css_class("dim-label");
        let names = gtk::Box::new(gtk::Orientation::Vertical, 0);
        names.set_hexpand(true);
        names.append(&heading);
        names.append(&count);

        let more = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("More")
            .menu_model(&header_menu())
            .valign(gtk::Align::Center)
            .build();
        more.add_css_class("flat");

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.set_margin_start(12);
        header.set_margin_end(6);
        header.set_margin_top(8);
        header.set_margin_bottom(8);
        header.append(&names);
        header.append(&more);

        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search hosts, or type an address")
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(6)
            .build();

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Browse);
        list.add_css_class("navigation-sidebar");
        // A single click selects and a double click connects. The palette
        // activates on one because it is gone the instant it does; this list
        // stays on screen, where one stray click must not open a login.
        list.set_activate_on_single_click(false);

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();

        let nothing = adw::StatusPage::builder()
            .icon_name("network-server-symbolic")
            .title("No Saved Hosts")
            .description(
                "Aliases in ~/.ssh/config show up here. \
                 Type an address to connect to a machine that is not saved.",
            )
            .build();
        let no_match = adw::StatusPage::builder()
            .icon_name("edit-find-symbolic")
            .title("No Matches")
            .build();
        no_match.add_css_class("compact");

        let stack = gtk::Stack::new();
        stack.add_named(&scroller, Some("list"));
        stack.add_named(&nothing, Some("nothing"));
        stack.add_named(&no_match, Some("no-match"));

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header);
        content.append(&search);
        content.append(&stack);

        let clamp = adw::Clamp::builder()
            .maximum_size(WIDTH)
            .tightening_threshold(WIDTH)
            .child(&content)
            .build();
        self.set_child(Some(&clamp));

        let menu = gtk::PopoverMenu::from_model(None::<&gio::Menu>);
        menu.set_has_arrow(false);
        menu.set_halign(gtk::Align::Start);
        menu.set_parent(self);

        imp.search.replace(Some(search.clone()));
        imp.list.replace(Some(list.clone()));
        imp.scroller.replace(Some(scroller));
        imp.stack.replace(Some(stack));
        imp.count.replace(Some(count));
        imp.menu.replace(Some(menu));

        self.install_actions();
        self.wire(&search, &list);
        self.reload();
    }

    fn wire(&self, search: &gtk::SearchEntry, list: &gtk::ListBox) {
        search.connect_search_changed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.fill()
        ));

        // The entry keeps the keyboard the whole time, so the list is driven
        // from here: the arrows move the selection, and Return with a modifier
        // says where the connection goes.
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| {
                let control = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
                let shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);
                match key {
                    gtk::gdk::Key::Down => this.step(1),
                    gtk::gdk::Key::Up => this.step(-1),
                    gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter if control => {
                        this.choose(Where::Side);
                    }
                    gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter if shift => {
                        this.choose(Where::Tab);
                    }
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            }
        ));
        search.add_controller(keys);

        // Plain Return, which the controller above let through.
        search.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.choose(Where::Here)
        ));

        list.connect_row_activated(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| this.choose(Where::Here)
        ));

        let middle = gtk::GestureClick::new();
        middle.set_button(gtk::gdk::BUTTON_MIDDLE);
        middle.connect_pressed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            list,
            move |_, _, _, y| {
                if let Some(row) = list.row_at_y(y as i32) {
                    list.select_row(Some(&row));
                    this.choose(Where::Side);
                }
            }
        ));
        list.add_controller(middle);

        let secondary = gtk::GestureClick::new();
        secondary.set_button(gtk::gdk::BUTTON_SECONDARY);
        secondary.connect_pressed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            list,
            move |_, _, x, y| {
                let Some(row) = list.row_at_y(y as i32) else {
                    return;
                };
                list.select_row(Some(&row));
                this.popup_for(&row, &list, x, y);
            }
        ));
        list.add_controller(secondary);
    }

    fn install_actions(&self) {
        let actions = gio::SimpleActionGroup::new();
        actions.add_action_entries([
            entry("connect", self, |hosts| hosts.send_target(Where::Here)),
            entry("connect-to-side", self, |hosts| {
                hosts.send_target(Where::Side);
            }),
            entry("connect-in-tab", self, |hosts| {
                hosts.send_target(Where::Tab);
            }),
            entry("copy-command", self, |hosts| {
                let Some(alias) = hosts.imp().target.borrow().clone() else {
                    return;
                };
                // The bare command a person would have typed, not the one tuni
                // runs: the options it adds are its own business, and pasting
                // them into a shell would be noise.
                hosts.clipboard().set_text(&format!("ssh {alias}"));
            }),
            entry("local-shell", self, |hosts| hosts.send(Message::LocalShell)),
            entry("refresh", self, TuniHosts::reload),
            entry("open-config", self, |hosts| {
                hosts.send(Message::OpenFile(ssh::config_path()));
            }),
        ]);
        self.insert_action_group("hosts", Some(&actions));
    }

    fn popup_for(&self, row: &gtk::ListBoxRow, list: &gtk::ListBox, x: f64, y: f64) {
        let choice = {
            let rows = self.imp().rows.borrow();
            rows.iter()
                .find(|(candidate, _)| candidate == row)
                .map(|(_, choice)| choice.clone())
        };
        let Some(Choice::Host(alias)) = choice else {
            return;
        };
        let origin = self.imp().hosts.borrow().get(&alias).and_then(|host| {
            host.origin
                .as_ref()
                .map(|origin| origin.path.to_string_lossy().into_owned())
        });
        self.imp().target.replace(Some(alias));

        let Some(menu) = self.imp().menu.borrow().clone() else {
            return;
        };
        menu.set_menu_model(Some(&row_menu(origin.is_some())));
        // The gesture measures from the list; the popover hangs off this
        // widget, and the two are a header and a search entry apart.
        let point = gtk::graphene::Point::new(x as f32, y as f32);
        let point = list
            .compute_point(self, &point)
            .unwrap_or_else(|| gtk::graphene::Point::new(x as f32, y as f32));
        crate::menu::popup_at(&menu, point);
    }

    /// Sends the row the context menu was opened on somewhere.
    fn send_target(&self, wher: Where) {
        let Some(alias) = self.imp().target.borrow().clone() else {
            return;
        };
        self.send(wher.message(alias));
    }

    // --- reading -----------------------------------------------------------

    fn reload(&self) {
        let imp = self.imp();
        if imp.loading.get() {
            return;
        }
        imp.loading.set(true);
        let generation = imp.generation.get() + 1;
        imp.generation.set(generation);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                // Files and no subprocess, but files on a network share are
                // files that block, and this pane is the whole window's
                // keyboard while it is up.
                let read = gio::spawn_blocking(Hosts::load).await;
                let imp = this.imp();
                imp.loading.set(false);
                let Ok(hosts) = read else {
                    return;
                };
                if imp.generation.get() != generation {
                    return;
                }
                imp.hosts.replace(hosts);
                this.fill();
            }
        ));
    }

    // --- drawing -----------------------------------------------------------

    /// Rebuilds the list for whatever is in the search entry.
    fn fill(&self) {
        let imp = self.imp();
        let (Some(list), Some(stack)) = (imp.list.borrow().clone(), imp.stack.borrow().clone())
        else {
            return;
        };
        let query = imp
            .search
            .borrow()
            .as_ref()
            .map(|search| search.text().trim().to_owned())
            .unwrap_or_default();

        while let Some(child) = list.first_child() {
            list.remove(&child);
        }

        let hosts = imp.hosts.borrow();
        if let Some(label) = imp.count.borrow().as_ref() {
            label.set_text(&count(hosts.all().len()));
        }

        let mut rows = Vec::new();
        if query.is_empty() {
            list.append(&header("Local"));
            let row = local_row();
            list.append(&row);
            rows.push((row, Choice::Local));

            let mut sorted: Vec<&Host> = hosts.all().iter().collect();
            sorted.sort_by(|left, right| left.alias.cmp(&right.alias));
            if !sorted.is_empty() {
                list.append(&header("Hosts"));
            }
            for host in sorted {
                let row = host_row(host);
                list.append(&row);
                rows.push((row, Choice::Host(host.alias.clone())));
            }
        } else {
            let mut ranked: Vec<(&Host, i32)> = hosts
                .all()
                .iter()
                .filter_map(|host| {
                    fuzzy::score(&format!("{} {}", host.alias, host.address()), &query)
                        .map(|score| (host, score))
                })
                .collect();
            ranked.sort_by(|left, right| {
                right
                    .1
                    .cmp(&left.1)
                    .then_with(|| left.0.alias.cmp(&right.0.alias))
            });
            for (host, _) in ranked {
                let row = host_row(host);
                list.append(&row);
                rows.push((row, Choice::Host(host.alias.clone())));
            }
            if fuzzy::score("local shell terminal", &query).is_some() {
                let row = local_row();
                list.append(&row);
                rows.push((row, Choice::Local));
            }
            // Nothing saved answers to it, so take it for what it looks like:
            // an address somebody wants to reach once.
            if rows.is_empty()
                && let Some(host) = Host::adhoc(&query)
            {
                let row = adhoc_row(&host);
                list.append(&row);
                rows.push((row, Choice::Host(host.target())));
            }
        }

        stack.set_visible_child_name(if !rows.is_empty() {
            "list"
        } else if query.is_empty() {
            "nothing"
        } else {
            "no-match"
        });
        if let Some((row, _)) = rows.first() {
            list.select_row(Some(row));
        }
        drop(hosts);
        imp.rows.replace(rows);
    }

    /// Moves the selection, wrapping at both ends, and scrolls it into view
    /// without taking the keyboard off the search entry.
    fn step(&self, delta: i32) {
        let imp = self.imp();
        let (Some(list), Some(scroller)) =
            (imp.list.borrow().clone(), imp.scroller.borrow().clone())
        else {
            return;
        };
        let rows = imp.rows.borrow();
        if rows.is_empty() {
            return;
        }
        let count = rows.len() as i32;
        let current = list
            .selected_row()
            .and_then(|row| rows.iter().position(|(candidate, _)| *candidate == row))
            .map_or(0, |at| at as i32);
        let (row, _) = &rows[(current + delta).rem_euclid(count) as usize];
        list.select_row(Some(row));
        reveal(&scroller, row);
    }

    /// Acts on the selected row.
    fn choose(&self, wher: Where) {
        let imp = self.imp();
        let Some(list) = imp.list.borrow().clone() else {
            return;
        };
        let choice = {
            let rows = imp.rows.borrow();
            list.selected_row().and_then(|row| {
                rows.iter()
                    .find(|(candidate, _)| *candidate == row)
                    .map(|(_, choice)| choice.clone())
            })
        };
        match choice {
            // A local shell is this pane whichever key asked for it: nobody
            // means "open a second empty terminal somewhere else" by it.
            Some(Choice::Local) => self.send(Message::LocalShell),
            Some(Choice::Host(alias)) => self.send(wher.message(alias)),
            None => (),
        }
    }
}

/// Where a connection the launcher was asked for should go.
#[derive(Clone, Copy)]
enum Where {
    Here,
    Side,
    Tab,
}

impl Where {
    fn message(self, alias: String) -> Message {
        match self {
            Self::Here => Message::Connect(alias),
            Self::Side => Message::ConnectToSide(alias),
            Self::Tab => Message::ConnectInTab(alias),
        }
    }
}

/// One action, holding the launcher weakly: the group is inserted into the
/// launcher, so anything stronger than this would be a cycle it never leaves.
fn entry<F>(name: &str, hosts: &TuniHosts, activate: F) -> gio::ActionEntry<gio::SimpleActionGroup>
where
    F: Fn(&TuniHosts) + 'static,
{
    let weak = hosts.downgrade();
    gio::ActionEntry::builder(name)
        .activate(move |_: &gio::SimpleActionGroup, _, _| {
            if let Some(hosts) = weak.upgrade() {
                activate(&hosts);
            }
        })
        .build()
}

fn header_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    menu.append(Some("Local Shell"), Some("hosts.local-shell"));
    menu.append(Some("Refresh"), Some("hosts.refresh"));
    menu.append(Some("Open ~/.ssh/config"), Some("hosts.open-config"));
    menu
}

fn row_menu(saved: bool) -> gio::Menu {
    let menu = gio::Menu::new();

    let open = gio::Menu::new();
    open.append(Some("Connect"), Some("hosts.connect"));
    open.append(Some("Connect to the Side"), Some("hosts.connect-to-side"));
    open.append(Some("Connect in a New Tab"), Some("hosts.connect-in-tab"));
    menu.append_section(None, &open);

    let rest = gio::Menu::new();
    rest.append(Some("Copy ssh Command"), Some("hosts.copy-command"));
    if saved {
        rest.append(Some("Open ~/.ssh/config"), Some("hosts.open-config"));
    }
    menu.append_section(None, &rest);

    menu
}

fn header(title: &str) -> gtk::ListBoxRow {
    let label = gtk::Label::builder()
        .label(title)
        .halign(gtk::Align::Start)
        .margin_top(6)
        .margin_start(12)
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

fn local_row() -> gtk::ListBoxRow {
    row("utilities-terminal-symbolic", "Local shell", "", "")
}

fn host_row(host: &Host) -> gtk::ListBoxRow {
    let row = row(
        "network-server-symbolic",
        &host.alias,
        &host.address(),
        // First value obtained wins, so of two blocks naming one alias only
        // the first is doing anything. A list that shows both without saying
        // so is a list that lies about which one an edit would change.
        if host.shadowed { "declared twice" } else { "" },
    );
    if let Some(origin) = &host.origin {
        row.set_tooltip_text(Some(&format!(
            "{}:{}",
            origin.path.to_string_lossy(),
            origin.line
        )));
    }
    row
}

fn adhoc_row(host: &Host) -> gtk::ListBoxRow {
    row(
        "network-server-symbolic",
        &format!("Connect to {}", host.target()),
        &host.address(),
        "not saved",
    )
}

fn row(icon: &str, name: &str, address: &str, note: &str) -> gtk::ListBoxRow {
    let image = gtk::Image::from_icon_name(icon);
    image.add_css_class("dim-label");

    let title = gtk::Label::builder()
        .label(name)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .xalign(0.0)
        .build();

    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.set_margin_start(12);
    line.set_margin_end(6);
    line.set_margin_top(4);
    line.set_margin_bottom(4);
    line.append(&image);
    line.append(&title);

    let label = gtk::Label::builder()
        .label(address)
        // The end of an address says more than its beginning, and the middle is
        // what a reader can do without.
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .xalign(0.0)
        .hexpand(true)
        .build();
    label.add_css_class("monospace");
    label.add_css_class("caption");
    label.add_css_class("dim-label");
    line.append(&label);

    if !note.is_empty() {
        let label = gtk::Label::new(Some(note));
        label.add_css_class("caption");
        label.add_css_class("dim-label");
        line.append(&label);
    }

    gtk::ListBoxRow::builder().child(&line).build()
}

fn count(hosts: usize) -> String {
    match hosts {
        0 => "No hosts".to_owned(),
        1 => "1 host".to_owned(),
        many => format!("{many} hosts"),
    }
}

/// Scrolls a selected row into view without giving it the keyboard, which
/// belongs to the search entry for as long as the launcher is up.
fn reveal(scroller: &gtk::ScrolledWindow, row: &gtk::ListBoxRow) {
    let adjustment = scroller.vadjustment();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_count_reads_as_a_sentence_at_every_size() {
        assert_eq!(count(0), "No hosts");
        assert_eq!(count(1), "1 host");
        assert_eq!(count(14), "14 hosts");
    }
}
