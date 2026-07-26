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
use tuni_core::ssh::{self, Host, Hosts, Meta, Notes, Source};

/// How many hosts the Recent section holds before the rest of them are just the
/// list underneath it.
const RECENT: usize = 5;

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
    use super::{Cell, Choice, Hosts, Notes, Rc, RefCell, glib};
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
        /// Open a file for editing at a line, which is how the real
        /// configuration is reached from a list that only reads it.
        OpenFile(std::path::PathBuf, usize),
    }

    #[derive(Default)]
    pub struct TuniHosts {
        /// Everything the configuration names, and the files it was read from,
        /// which is what answers whether the list has gone stale.
        pub hosts: RefCell<Hosts>,
        /// Labels, tags and when each host was last connected to, which is what
        /// the Recent section is sorted on.
        pub notes: RefCell<Notes>,
        /// What each row on screen stands for, in the order they are in. The
        /// widgets are rebuilt on every keystroke and this with them.
        pub(super) rows: RefCell<Vec<(gtk::ListBoxRow, Choice)>>,
        pub search: RefCell<Option<gtk::SearchEntry>>,
        pub banner: RefCell<Option<adw::Banner>>,
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

        let add = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("Add a host")
            .action_name("hosts.add")
            .valign(gtk::Align::Center)
            .build();
        add.add_css_class("flat");

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
        header.append(&add);
        header.append(&more);

        // Only ever holds the reason a write failed. A host list that cannot be
        // read is an empty list, which says so by being empty.
        let banner = adw::Banner::new("");

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
        let first = gtk::Button::builder()
            .label("Add a Host")
            .action_name("hosts.add")
            .halign(gtk::Align::Center)
            .build();
        first.add_css_class("pill");
        first.add_css_class("suggested-action");
        nothing.set_child(Some(&first));
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
        content.append(&banner);
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

        imp.banner.replace(Some(banner));
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
            entry("add", self, |hosts| hosts.edit_host(None, false)),
            entry("edit", self, |hosts| {
                let target = hosts.imp().target.borrow().clone();
                hosts.edit_host(target.as_deref(), false);
            }),
            entry("duplicate", self, |hosts| {
                let target = hosts.imp().target.borrow().clone();
                hosts.edit_host(target.as_deref(), true);
            }),
            entry("delete", self, TuniHosts::confirm_delete),
            entry("open-config", self, |hosts| {
                hosts.send(Message::OpenFile(ssh::config_path(), 1));
            }),
            entry("open-origin", self, |hosts| {
                let Some(alias) = hosts.imp().target.borrow().clone() else {
                    return;
                };
                let origin = hosts
                    .imp()
                    .hosts
                    .borrow()
                    .get(&alias)
                    .and_then(|host| host.origin.clone());
                if let Some(origin) = origin {
                    hosts.send(Message::OpenFile(origin.path, origin.line));
                }
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
        let known = self
            .imp()
            .hosts
            .borrow()
            .get(&alias)
            .map(|host| (host.source, host.origin.is_some()));
        self.imp().target.replace(Some(alias));

        let Some(menu) = self.imp().menu.borrow().clone() else {
            return;
        };
        let (source, declared) = known.unwrap_or((Source::Adhoc, false));
        menu.set_menu_model(Some(&row_menu(source, declared)));
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
        self.remember(&alias);
        self.send(wher.message(alias));
    }

    /// Puts a host at the top of the list next time. An address typed once is
    /// not a host, so it gets no history: the file would fill up with lines
    /// nobody can act on.
    fn remember(&self, alias: &str) {
        let imp = self.imp();
        if imp.hosts.borrow().get(alias).is_none() {
            return;
        }
        imp.notes.borrow_mut().used(alias);
        let notes = imp.notes.borrow().clone();
        glib::spawn_future_local(async move {
            let _ = gio::spawn_blocking(move || notes.save()).await;
        });
    }

    // --- writing -----------------------------------------------------------

    /// Opens the editor on a host, or on nothing, which is how one is added.
    /// A duplicate opens the same dialog under a name that is free, so saving
    /// it cannot quietly replace what it was copied from.
    fn edit_host(&self, alias: Option<&str>, duplicate: bool) {
        let imp = self.imp();
        let mut host = alias.and_then(|alias| imp.hosts.borrow().get(alias).cloned());
        let mut meta = alias
            .map(|alias| imp.notes.borrow().get(alias))
            .unwrap_or_default();
        let jumps: Vec<String> = imp
            .hosts
            .borrow()
            .all()
            .iter()
            .map(|host| host.alias.clone())
            .collect();

        if duplicate && let Some(host) = host.as_mut() {
            host.alias = free_alias(&host.alias, &jumps);
            host.origin = None;
            host.shadowed = false;
            meta = Meta {
                label: meta.label,
                tags: meta.tags,
                ..Meta::default()
            };
        }

        crate::host_editor::present(
            self,
            host,
            meta,
            jumps,
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |edited| this.store(edited)
            ),
        );
    }

    /// Writes the store, off this thread: it renders the file, hands it to
    /// `ssh` to be checked, and renames it into place.
    fn store(&self, edited: crate::host_editor::Edited) {
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let written = gio::spawn_blocking(move || write(edited)).await;
                this.report(written.unwrap_or_else(|_| Err("The write did not finish".to_owned())));
            }
        ));
    }

    fn confirm_delete(&self) {
        let Some(alias) = self.imp().target.borrow().clone() else {
            return;
        };
        let dialog = adw::AlertDialog::new(
            Some(&format!("Delete {alias}?")),
            Some(
                "The host is removed from the file tuni keeps. Nothing on the machine it names is touched.",
            ),
        );
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("delete", "Delete");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.connect_response(
            None,
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_, response| {
                    if response != "delete" {
                        return;
                    }
                    let alias = alias.clone();
                    glib::spawn_future_local(glib::clone!(
                        #[weak]
                        this,
                        async move {
                            let removed = gio::spawn_blocking(move || remove(&alias)).await;
                            this.report(
                                removed
                                    .unwrap_or_else(|_| Err("The write did not finish".to_owned())),
                            );
                        }
                    ));
                }
            ),
        );
        dialog.present(Some(self));
    }

    /// Shows what went wrong, or reads the list again because something
    /// changed.
    fn report(&self, outcome: Result<(), String>) {
        if let Some(banner) = self.imp().banner.borrow().as_ref() {
            banner.set_revealed(outcome.is_err());
            if let Err(message) = &outcome {
                banner.set_title(message);
            }
        }
        if outcome.is_ok() {
            self.reload();
        }
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
                let read = gio::spawn_blocking(|| (Hosts::load(), Notes::load())).await;
                let imp = this.imp();
                imp.loading.set(false);
                let Ok((hosts, notes)) = read else {
                    return;
                };
                if imp.generation.get() != generation {
                    return;
                }
                imp.hosts.replace(hosts);
                imp.notes.replace(notes);
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
        let notes = imp.notes.borrow();
        if let Some(label) = imp.count.borrow().as_ref() {
            label.set_text(&count(hosts.all().len()));
        }

        let mut rows = Vec::new();
        if query.is_empty() {
            list.append(&header("Local"));
            let row = local_row();
            list.append(&row);
            rows.push((row, Choice::Local));

            // Last connected first, and then everything else by name. A host
            // in the first group is not repeated in the second: two rows for
            // one machine is two answers to the question of where to click.
            let mut recent: Vec<(&Host, u64)> = hosts
                .all()
                .iter()
                .filter_map(|host| Some((host, notes.get(&host.alias).last_used?)))
                .collect();
            recent.sort_by_key(|(_, when)| std::cmp::Reverse(*when));
            recent.truncate(RECENT);
            if !recent.is_empty() {
                list.append(&header("Recent"));
            }
            for (host, _) in &recent {
                let row = host_row(host, &notes.get(&host.alias));
                list.append(&row);
                rows.push((row, Choice::Host(host.alias.clone())));
            }

            let mut sorted: Vec<&Host> = hosts
                .all()
                .iter()
                .filter(|host| !recent.iter().any(|(shown, _)| shown.alias == host.alias))
                .collect();
            sorted.sort_by(|left, right| left.alias.cmp(&right.alias));
            if !sorted.is_empty() {
                list.append(&header("Hosts"));
            }
            for host in sorted {
                let row = host_row(host, &notes.get(&host.alias));
                list.append(&row);
                rows.push((row, Choice::Host(host.alias.clone())));
            }
        } else {
            let mut ranked: Vec<(&Host, Meta, i32)> = hosts
                .all()
                .iter()
                .filter_map(|host| {
                    let meta = notes.get(&host.alias);
                    let searchable = format!(
                        "{} {} {} {}",
                        host.alias,
                        host.address(),
                        meta.label,
                        meta.tags.join(" ")
                    );
                    fuzzy::score(&searchable, &query).map(|score| (host, meta, score))
                })
                .collect();
            ranked.sort_by(|left, right| {
                right
                    .2
                    .cmp(&left.2)
                    .then_with(|| left.0.alias.cmp(&right.0.alias))
            });
            for (host, meta, _) in &ranked {
                let row = host_row(host, meta);
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
        drop(notes);
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
            Some(Choice::Host(alias)) => {
                self.remember(&alias);
                self.send(wher.message(alias));
            }
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

/// The menu for one row. What it offers depends on which file the host came
/// out of: tuni rewrites its own, and points at the user's.
fn row_menu(source: Source, declared: bool) -> gio::Menu {
    let menu = gio::Menu::new();

    let open = gio::Menu::new();
    open.append(Some("Connect"), Some("hosts.connect"));
    open.append(Some("Connect to the Side"), Some("hosts.connect-to-side"));
    open.append(Some("Connect in a New Tab"), Some("hosts.connect-in-tab"));
    menu.append_section(None, &open);

    if source != Source::Adhoc {
        let change = gio::Menu::new();
        change.append(Some("Edit"), Some("hosts.edit"));
        change.append(Some("Duplicate"), Some("hosts.duplicate"));
        if source == Source::Tuni {
            change.append(Some("Delete"), Some("hosts.delete"));
        }
        menu.append_section(None, &change);
    }

    let rest = gio::Menu::new();
    rest.append(Some("Copy ssh Command"), Some("hosts.copy-command"));
    if declared && source == Source::SshConfig {
        rest.append(Some("Edit in ~/.ssh/config"), Some("hosts.open-origin"));
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

fn host_row(host: &Host, meta: &Meta) -> gtk::ListBoxRow {
    let name = if meta.label.is_empty() {
        &host.alias
    } else {
        &meta.label
    };
    // First value obtained wins, so of two blocks naming one alias only the
    // first is doing anything. A list that shows both without saying so is a
    // list that lies about which one an edit would change, and that is worth
    // the space the tags would have had.
    let note = if host.shadowed {
        "declared twice".to_owned()
    } else {
        meta.tags.join(", ")
    };
    let row = row("network-server-symbolic", name, &host.address(), &note);
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

/// Puts a host in the file tuni owns, and makes sure `ssh` reads that file.
///
/// The whole file is rewritten from the hosts already in it, so an edit made
/// here and an edit made in a text editor cannot half-merge. Without the
/// `Include` the block would be written and then never read, and connecting to
/// the alias would try to reach a machine of that name.
fn write(edited: crate::host_editor::Edited) -> Result<(), String> {
    let crate::host_editor::Edited {
        original,
        host,
        meta,
    } = edited;
    let mut hosts = ssh::saved();
    hosts.retain(|kept| kept.alias != host.alias && Some(&kept.alias) != original.as_ref());
    hosts.push(host.clone());
    hosts.sort_by(|left, right| left.alias.cmp(&right.alias));
    ssh::save(&hosts)?;
    ssh::ensure_include()?;

    let mut notes = Notes::load();
    // A rename leaves the tags filed under a name nothing answers to.
    if let Some(original) = &original
        && *original != host.alias
    {
        notes.set(original, Meta::default());
    }
    notes.set(&host.alias, meta);
    notes.save().map_err(|error| error.to_string())
}

fn remove(alias: &str) -> Result<(), String> {
    let mut hosts = ssh::saved();
    hosts.retain(|kept| kept.alias != alias);
    ssh::save(&hosts)?;

    let mut notes = Notes::load();
    notes.set(alias, Meta::default());
    notes.save().map_err(|error| error.to_string())
}

/// A name like `alias` that nothing else answers to yet.
fn free_alias(alias: &str, taken: &[String]) -> String {
    let candidate = format!("{alias} copy");
    if !taken.contains(&candidate) {
        return candidate;
    }
    (2..)
        .map(|number| format!("{candidate} {number}"))
        .find(|candidate| !taken.contains(candidate))
        .unwrap_or(candidate)
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
    fn a_copy_is_never_named_after_something_that_exists() {
        let taken = ["web".to_owned(), "web copy".to_owned()];
        assert_eq!(free_alias("db", &taken), "db copy");
        assert_eq!(free_alias("web", &taken), "web copy 2");
    }

    #[test]
    fn the_count_reads_as_a_sentence_at_every_size() {
        assert_eq!(count(0), "No hosts");
        assert_eq!(count(1), "1 host");
        assert_eq!(count(14), "14 hosts");
    }
}
