//! The files on the machine a pane is connected to.
//!
//! The same flat tree the Files page draws, read through
//! [`tuni_core::sftp::Session`] rather than off a disk. Two things follow from
//! the reader being a connection. Every listing is a round trip, so it is asked
//! for on a worker thread and the rows are drawn from what has come back so
//! far, which is what [`Listed`] holds; a directory nobody has asked about yet
//! is empty rather than read, because reading it where the tree is rebuilt
//! would put a network on the main loop. And a remote directory says nothing
//! when it changes, so this page is not in the panel's two-second poll: it
//! reads on navigation, and when somebody asks it to.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::gdk;
use gtk::gio;
use gtk::glib;

use tuni_core::files::{Directory, Failure, Item, Tree, natural_cmp};
use tuni_core::sftp::{Job, Queue, Session, Way, summary};

use crate::files::bind_row;
use crate::files::row::Row;

/// One host's session, shared with the threads that talk on it. The lock is
/// what a single pipe needs anyway: one request, one reply, in that order.
type Link = Arc<Mutex<Session>>;

/// How far a transfer has got: written by the thread moving the bytes, read by
/// the timer that draws them. Two numbers rather than a channel, which this
/// crate has no dependency for and would not be any more accurate.
#[derive(Default)]
pub struct Moved {
    done: AtomicU64,
    total: AtomicU64,
    /// Raised by the bar's cancel button, read by the transfer at every
    /// chunk, which is as promptly as a blocking pipe can honor it.
    stop: AtomicBool,
}

/// The directories that have come back, which is everything there is to draw.
#[derive(Default)]
pub struct Listed(HashMap<PathBuf, Vec<Item>>);

impl Directory for Listed {
    fn read(&mut self, path: &Path) -> Vec<Item> {
        self.0.get(path).cloned().unwrap_or_default()
    }
}

mod imp {
    use super::{
        Arc, Cell, HashSet, Item, Job, Link, Listed, Moved, PathBuf, Queue, RefCell, Session, Tree,
        gio, glib,
    };
    use adw::prelude::*;
    use adw::subclass::prelude::*;

    #[derive(Default)]
    pub struct TuniSftp {
        pub tree: RefCell<Tree>,
        pub listed: RefCell<Listed>,
        pub link: RefCell<Option<Link>>,
        /// A second session that only carries files, so a long one does not
        /// hold the listing behind its lock. Opened by the first transfer
        /// rather than with the page, since most visits never move a file,
        /// and kept while the page is on its host or a job waits for it. A
        /// transfer takes it and hands it back, which is all the sharing one
        /// transfer at a time needs. Named by its host, because the queue
        /// outlives the page's visit to one.
        pub carrier: RefCell<Option<(String, Session)>>,
        /// The host being browsed, and nothing at all when no pane is on one.
        pub alias: RefCell<Option<String>>,
        /// Bumped when the host changes. A listing already in flight names the
        /// generation it was asked for, so what lands late is dropped rather
        /// than drawn over another machine's tree.
        pub generation: Cell<u64>,
        /// The directories a reply is still owed for, so a second click while
        /// the first is in the air costs nothing.
        pub pending: RefCell<HashSet<PathBuf>>,
        pub rows: RefCell<Option<gio::ListStore>>,
        pub list: RefCell<Option<gtk::ListView>>,
        pub stack: RefCell<Option<gtk::Stack>>,
        pub status: RefCell<Option<adw::StatusPage>>,
        /// Under the sentence when the connection is what failed, since the
        /// panel asks again only when the host changes.
        pub reconnect: RefCell<Option<gtk::Button>>,
        pub title: RefCell<Option<gtk::Label>>,
        pub subtitle: RefCell<Option<gtk::Label>>,
        pub menu: RefCell<Option<gtk::PopoverMenu>>,
        pub progress: RefCell<Option<gtk::ProgressBar>>,
        /// The bar, the list of transfers and the cancel button, shown while
        /// any of them has something to say.
        pub progress_row: RefCell<Option<gtk::Box>>,
        /// In place of the bar once nothing is moving and something failed.
        pub failed: RefCell<Option<gtk::Label>>,
        pub cancel: RefCell<Option<gtk::Button>>,
        /// The rows of the transfers popover, rebuilt each time it opens and
        /// whenever a transfer changes while it is open.
        pub transfers: RefCell<Option<gtk::ListBox>>,
        pub transfers_popover: RefCell<Option<gtk::Popover>>,
        /// The transfer running, if one is. The carrier moves one at a time,
        /// so a second waits in [`Self::queue`] where the list can show it.
        pub moving: RefCell<Option<(Job, Arc<Moved>)>>,
        /// What is waiting for the pipe, in the order it was asked for, and
        /// what failed. A drop of a directory's worth of files fills it and
        /// empties it one file at a time.
        pub queue: RefCell<Queue>,
        /// Where browsing has been, and where it was called back from, on the
        /// host being browsed now. Emptied with the session, since both are
        /// paths on a machine the page has left.
        pub back: RefCell<Vec<PathBuf>>,
        pub forward: RefCell<Vec<PathBuf>>,
        /// The rows a context menu or a key was opened on, and so the ones its
        /// actions act on. Taken when it opens rather than read from the
        /// selection when it is chosen, since a listing landing in between
        /// rebuilds the rows and the selection with them.
        pub targets: RefCell<Vec<Item>>,
        pub location: RefCell<Option<gtk::Entry>>,
        pub location_bar: RefCell<Option<gtk::Revealer>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TuniSftp {
        const NAME: &'static str = "TuniSftp";
        type Type = super::TuniSftp;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for TuniSftp {
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

    impl WidgetImpl for TuniSftp {}
    impl BinImpl for TuniSftp {}
}

glib::wrapper! {
    pub struct TuniSftp(ObjectSubclass<imp::TuniSftp>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for TuniSftp {
    fn default() -> Self {
        Self::new()
    }
}

impl TuniSftp {
    #[must_use]
    pub fn new() -> Self {
        glib::Object::new()
    }

    fn build(&self) {
        let imp = self.imp();

        let title = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        title.add_css_class("heading");
        let subtitle = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Start)
            .build();
        subtitle.add_css_class("caption");
        subtitle.add_css_class("dim-label");
        let names = gtk::Box::new(gtk::Orientation::Vertical, 0);
        names.set_hexpand(true);
        names.append(&title);
        names.append(&subtitle);

        let up = gtk::Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text("Parent Directory")
            .action_name("sftp.up")
            .valign(gtk::Align::Center)
            .build();
        up.add_css_class("flat");

        let jump = gtk::Button::builder()
            .icon_name("folder-open-symbolic")
            .tooltip_text("Go to Directory")
            .action_name("sftp.location")
            .valign(gtk::Align::Center)
            .build();
        jump.add_css_class("flat");

        let here = gio::Menu::new();
        here.append(Some("Upload File Here…"), Some("sftp.upload-here"));
        here.append(Some("Upload Folder Here…"), Some("sftp.upload-folder-here"));
        here.append(Some("New File…"), Some("sftp.new-file-here"));
        here.append(Some("New Folder…"), Some("sftp.new-folder-here"));
        let more = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("More")
            .menu_model(&here)
            .valign(gtk::Align::Center)
            .build();
        more.add_css_class("flat");

        let refresh = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Read Again")
            .action_name("sftp.refresh")
            .valign(gtk::Align::Center)
            .build();
        refresh.add_css_class("flat");

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.set_margin_start(12);
        header.set_margin_end(6);
        header.set_margin_top(8);
        header.set_margin_bottom(8);
        header.append(&names);
        header.append(&up);
        header.append(&jump);
        header.append(&more);

        let (location_bar, location) = crate::files::location_bar(
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |text: &str| this.go_to(text)
            ),
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move || this.show_location(false)
            ),
        );
        header.append(&refresh);

        let progress = gtk::ProgressBar::builder()
            .show_text(true)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        let failed = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .visible(false)
            .build();
        failed.add_css_class("error");
        let transfers = gtk::ListBox::new();
        transfers.set_selection_mode(gtk::SelectionMode::None);
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(320)
            .min_content_width(280)
            .child(&transfers)
            .build();
        let transfers_popover = gtk::Popover::new();
        transfers_popover.set_child(Some(&scrolled));
        transfers_popover.connect_show(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.list_transfers()
        ));
        let list_button = gtk::MenuButton::builder()
            .icon_name("view-list-symbolic")
            .tooltip_text("Transfers")
            .popover(&transfers_popover)
            .valign(gtk::Align::Center)
            .build();
        list_button.add_css_class("flat");
        let cancel = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Cancel All Transfers")
            .valign(gtk::Align::Center)
            .build();
        cancel.add_css_class("flat");
        cancel.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.cancel_transfer()
        ));
        let progress_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        progress_row.set_margin_start(12);
        progress_row.set_margin_end(12);
        progress_row.set_margin_bottom(6);
        progress_row.set_visible(false);
        progress_row.append(&progress);
        progress_row.append(&failed);
        progress_row.append(&list_button);
        progress_row.append(&cancel);

        let rows = gio::ListStore::new::<Row>();
        // Several at once, with the clicks and keys a file manager takes for
        // that, because deleting or fetching twenty files one menu at a time is
        // twenty menus.
        let selection = gtk::MultiSelection::new(Some(rows.clone()));

        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, object| this.setup_row(object)
        ));
        factory.connect_bind(move |_, object| bind_row(object));

        let list = gtk::ListView::builder()
            .model(&selection)
            .factory(&factory)
            .vexpand(true)
            .build();
        list.add_css_class("navigation-sidebar");
        // On the list, not the window: Delete and F2 mean nothing to a shell
        // in the pane beside it, and the list is what has the focus when they
        // are meant for it.
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, modifiers| {
                // Caps Lock and Num Lock are modifiers too, and say nothing
                // about which key was meant.
                let held = modifiers
                    & (gdk::ModifierType::CONTROL_MASK
                        | gdk::ModifierType::ALT_MASK
                        | gdk::ModifierType::SHIFT_MASK);
                let plain = held.is_empty();
                match key {
                    gdk::Key::Delete | gdk::Key::KP_Delete if plain => {
                        this.delete(this.selected());
                    }
                    gdk::Key::F2 if plain => {
                        if let [item] = this.selected().as_slice() {
                            this.rename(item);
                        }
                    }
                    gdk::Key::l if held == gdk::ModifierType::CONTROL_MASK => {
                        this.show_location(true);
                    }
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            }
        ));
        list.add_controller(keys);
        list.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |list, position| {
                let row = list
                    .model()
                    .and_then(|model| model.item(position))
                    .and_downcast::<Row>();
                // Stepped into, which is what the parent button undoes. The
                // chevron is the one that opens a directory in place.
                match row.and_then(|row| row.item()) {
                    Some(item) if item.is_directory => this.browse(&item.path),
                    Some(item) => this.edit(&item),
                    None => {}
                }
            }
        ));

        let menu = gtk::PopoverMenu::from_model(None::<&gio::Menu>);
        menu.set_has_arrow(false);
        menu.set_halign(gtk::Align::Start);
        menu.set_parent(&list);

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();

        // Whatever a drop misses is still the directory the page is showing,
        // so files dropped past the last row go where the header says.
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        drop.connect_drop(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            false,
            move |_, value, _, _| {
                let root = this.imp().tree.borrow().root().to_path_buf();
                this.receive(&root, value)
            }
        ));
        scroller.add_controller(drop);

        let reconnect = gtk::Button::builder()
            .label("Reconnect")
            .halign(gtk::Align::Center)
            .visible(false)
            .build();
        reconnect.add_css_class("pill");
        reconnect.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.reconnect()
        ));
        let status = adw::StatusPage::builder()
            .icon_name("network-server-symbolic")
            .vexpand(true)
            .child(&reconnect)
            .build();

        let stack = gtk::Stack::new();
        stack.add_named(&scroller, Some("list"));
        stack.add_named(&status, Some("status"));
        stack.set_visible_child_name("status");

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header);
        content.append(&location_bar);
        content.append(&progress_row);
        content.append(&stack);
        self.set_child(Some(&content));

        self.add_controller(crate::files::history(self, "sftp"));
        self.install_actions();

        imp.rows.replace(Some(rows));
        imp.list.replace(Some(list));
        imp.stack.replace(Some(stack));
        imp.status.replace(Some(status));
        imp.reconnect.replace(Some(reconnect));
        imp.title.replace(Some(title));
        imp.subtitle.replace(Some(subtitle));
        imp.menu.replace(Some(menu));
        imp.progress.replace(Some(progress));
        imp.progress_row.replace(Some(progress_row));
        imp.failed.replace(Some(failed));
        imp.cancel.replace(Some(cancel));
        imp.transfers.replace(Some(transfers));
        imp.transfers_popover.replace(Some(transfers_popover));
        imp.location.replace(Some(location));
        imp.location_bar.replace(Some(location_bar));

        self.say(
            "No Connection",
            "A pane connected to a host is what this page reads.",
        );
    }

    /// One row's widgets, in the shape [`bind_row`] fills: a chevron, an icon,
    /// a name and a size. The chevron opens a directory on one click, which
    /// the row itself asks two for.
    fn setup_row(&self, object: &glib::Object) {
        let Some(list_item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };

        let expander = gtk::Image::from_icon_name("pan-end-symbolic");
        expander.set_pixel_size(12);
        let icon = gtk::Image::new();
        let label = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();

        let size = gtk::Label::builder().xalign(1.0).build();
        size.add_css_class("caption");
        size.add_css_class("dim-label");
        size.add_css_class("numeric");

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        content.append(&expander);
        content.append(&icon);
        content.append(&label);
        content.append(&size);

        let toggle = gtk::GestureClick::new();
        toggle.connect_released(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            list_item,
            move |gesture, _, _, _| {
                let Some(item) = list_item
                    .item()
                    .and_downcast::<Row>()
                    .and_then(|row| row.item())
                else {
                    return;
                };
                if item.is_directory {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    this.toggle(&item.path);
                }
            }
        ));
        expander.add_controller(toggle);

        let press = gtk::GestureClick::new();
        press.set_button(gdk::BUTTON_SECONDARY);
        press.connect_pressed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            list_item,
            #[weak]
            content,
            move |_, _, x, y| {
                let Some(item) = list_item
                    .item()
                    .and_downcast::<Row>()
                    .and_then(|row| row.item())
                else {
                    return;
                };
                this.popup_menu(&content, &item, list_item.position(), x, y);
            }
        ));
        content.add_controller(press);

        // Dropped on a directory the files go inside it, and dropped on a file
        // they go beside it, which is where a file manager puts them.
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        drop.connect_enter(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            list_item,
            #[upgrade_or]
            gdk::DragAction::empty(),
            move |_, _, _| {
                // Selected while the pointer is over it, since a row that is
                // about to be dropped on should say so before the drop.
                if let Some(list) = this.imp().list.borrow().as_ref()
                    && let Some(selection) = list.model()
                {
                    selection.select_item(list_item.position(), true);
                }
                gdk::DragAction::COPY
            }
        ));
        drop.connect_drop(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            list_item,
            #[upgrade_or]
            false,
            move |_, value, _, _| {
                let Some(item) = list_item
                    .item()
                    .and_downcast::<Row>()
                    .and_then(|row| row.item())
                else {
                    return false;
                };
                let directory = if item.is_directory {
                    Some(item.path.clone())
                } else {
                    item.path.parent().map(Path::to_path_buf)
                };
                directory.is_some_and(|directory| this.receive(&directory, value))
            }
        ));
        content.add_controller(drop);

        list_item.set_child(Some(&content));
    }

    fn install_actions(&self) {
        let actions = gio::SimpleActionGroup::new();
        actions.add_action_entries([
            entry("up", self, |sftp| {
                let parent = sftp
                    .imp()
                    .tree
                    .borrow()
                    .root()
                    .parent()
                    .map(Path::to_path_buf);
                if let Some(parent) = parent {
                    sftp.browse(&parent);
                }
            }),
            entry("back", self, |sftp| sftp.walk(false)),
            entry("forward", self, |sftp| sftp.walk(true)),
            entry("refresh", self, TuniSftp::reread),
            entry("location", self, |sftp| {
                let bar = sftp.imp().location_bar.borrow().clone();
                let open = bar.is_some_and(|bar| bar.reveals_child());
                sftp.show_location(!open);
            }),
            entry("edit", self, |sftp| {
                if let Some(item) = sftp.target().filter(|item| !item.is_directory) {
                    sftp.edit(&item);
                }
            }),
            entry("download", self, |sftp| {
                let targets = sftp.imp().targets.borrow().clone();
                sftp.download(targets);
            }),
            entry("upload", self, |sftp| {
                if let Some(item) = sftp.folder_target() {
                    sftp.upload(&item.path, false);
                }
            }),
            entry("upload-folder", self, |sftp| {
                if let Some(item) = sftp.folder_target() {
                    sftp.upload(&item.path, true);
                }
            }),
            entry("upload-here", self, |sftp| {
                let root = sftp.imp().tree.borrow().root().to_path_buf();
                sftp.upload(&root, false);
            }),
            entry("upload-folder-here", self, |sftp| {
                let root = sftp.imp().tree.borrow().root().to_path_buf();
                sftp.upload(&root, true);
            }),
            entry("new-folder", self, |sftp| {
                if let Some(item) = sftp.folder_target() {
                    sftp.new_folder(&item.path);
                }
            }),
            entry("new-folder-here", self, |sftp| {
                let root = sftp.imp().tree.borrow().root().to_path_buf();
                sftp.new_folder(&root);
            }),
            entry("new-file", self, |sftp| {
                if let Some(item) = sftp.folder_target() {
                    sftp.new_file(&item.path);
                }
            }),
            entry("new-file-here", self, |sftp| {
                let root = sftp.imp().tree.borrow().root().to_path_buf();
                sftp.new_file(&root);
            }),
            entry("rename", self, |sftp| {
                if let Some(item) = sftp.target() {
                    sftp.rename(&item);
                }
            }),
            entry("delete", self, |sftp| {
                let targets = sftp.imp().targets.borrow().clone();
                sftp.delete(targets);
            }),
            entry("properties", self, |sftp| {
                if let Some(item) = sftp.target() {
                    sftp.properties(&item);
                }
            }),
            entry("copy-path", self, |sftp| {
                if let Some(item) = sftp.target() {
                    sftp.clipboard().set_text(&item.path.to_string_lossy());
                }
            }),
            // Host and path in the one string `scp` and `rsync` take, under the
            // name the pane connected with.
            entry("copy-address", self, |sftp| {
                let alias = sftp.imp().alias.borrow().clone();
                if let (Some(alias), Some(item)) = (alias, sftp.target()) {
                    let address = format!("{alias}:{}", item.path.to_string_lossy());
                    sftp.clipboard().set_text(&address);
                }
            }),
        ]);
        self.insert_action_group("sftp", Some(&actions));
    }

    // --- which host ---------------------------------------------------------

    /// Points the page at the host the focused pane is on, and at nothing when
    /// that pane is a shell on this machine.
    ///
    /// Called with the same host over and over, since the panel syncs whenever
    /// the focus moves, so a host that has not changed is left entirely alone:
    /// re-reading it here would be a round trip per click.
    pub fn sync(&self, host: Option<&str>) {
        let imp = self.imp();
        if imp.alias.borrow().as_deref() == host {
            return;
        }
        imp.generation.set(imp.generation.get().wrapping_add(1));
        imp.alias.replace(host.map(ToOwned::to_owned));
        imp.pending.borrow_mut().clear();
        // The history names paths on the host that is going away. The queue
        // does too, but each job knows its host, and a file halfway up to one
        // machine has no reason to stop because the page went to look at
        // another.
        imp.back.borrow_mut().clear();
        imp.forward.borrow_mut().clear();
        imp.listed.replace(Listed::default());
        // Dropped, which kills the child and lets the master close the
        // channel. A session is one host's, and this is another host now.
        imp.link.replace(None);
        self.release();
        *imp.tree.borrow_mut() = Tree::new();
        self.reload();
        self.refresh_header();

        let Some(alias) = host else {
            self.say(
                "No Connection",
                "A pane connected to a host is what this page reads.",
            );
            return;
        };
        self.say(&format!("Opening {alias}"), "");
        self.open(alias.to_owned());
    }

    /// The button under a failure: the same host, from the start.
    ///
    /// Not done on its own when the host comes round again, because the panel
    /// syncs on a timer and a host that cannot be reached would be dialled
    /// every two seconds for as long as the page is open.
    fn reconnect(&self) {
        let alias = self.imp().alias.take();
        self.sync(alias.as_deref());
    }

    /// Lets go of a session that will not answer again, and says why in place
    /// of a listing that can no longer be trusted.
    ///
    /// The carrier goes too when it is this host's, since it came over the
    /// same connection. What is queued stays: the first of this host's jobs
    /// to meet the same failure takes the rest with it, and the other hosts'
    /// jobs were never on this connection.
    fn lose(&self, failure: &Failure) {
        let imp = self.imp();
        imp.link.replace(None);
        let alias = imp.alias.borrow().clone();
        let mine = matches!(
            (imp.carrier.borrow().as_ref(), alias),
            (Some((carried, _)), Some(alias)) if *carried == alias
        );
        if mine {
            imp.carrier.replace(None);
        }
        imp.pending.borrow_mut().clear();
        self.fail(&failure.message, &failure.detail);
    }

    /// Drops the carrier if nothing needs it: not the host the page is on, and
    /// nothing waiting to go to or from the host it is for. A transfer that is
    /// running has it, so there is nothing here to drop while it does.
    fn release(&self) {
        let imp = self.imp();
        let idle = match imp.carrier.borrow().as_ref() {
            Some((carried, _)) => {
                imp.alias.borrow().as_deref() != Some(carried.as_str())
                    && !imp.queue.borrow().wants(carried)
            }
            None => false,
        };
        if idle {
            imp.carrier.replace(None);
        }
    }

    /// Starts a session and lists whatever the far end calls home.
    fn open(&self, alias: String) {
        let generation = self.imp().generation.get();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let name = alias.clone();
                let Ok(opened) = gio::spawn_blocking(move || {
                    let control = crate::hosts::control();
                    let host = tuni_core::ssh::host(&alias);
                    let argv = tuni_core::ssh::sftp_command(&host, &control);
                    let mut session = Session::open(&argv).map_err(|failure| failure.detail)?;
                    let Some(home) = session.realpath(".") else {
                        return Err(reason(&session));
                    };
                    let rows = rows(&mut session, &home).ok_or_else(|| reason(&session))?;
                    Ok((session, PathBuf::from(home), rows))
                })
                .await
                else {
                    return;
                };
                if generation != this.imp().generation.get() {
                    return;
                }
                match opened {
                    Ok((session, home, rows)) => {
                        this.imp().link.replace(Some(Arc::new(Mutex::new(session))));
                        this.imp().listed.borrow_mut().0.insert(home.clone(), rows);
                        this.imp()
                            .tree
                            .borrow_mut()
                            .sync(&home, &mut *this.imp().listed.borrow_mut());
                        this.show_list();
                        this.reload();
                        this.refresh_header();
                    }
                    Err(detail) => this.fail(&format!("Couldn't Read {name}"), &detail),
                }
            }
        ));
    }

    // --- moving about --------------------------------------------------------

    /// Opens a directory if it was closed, closes it if it was open. A
    /// directory nothing has been read from yet is asked for first, and opens
    /// when the answer arrives.
    fn toggle(&self, path: &Path) {
        let imp = self.imp();
        let known = imp.listed.borrow().0.contains_key(path);
        if !known && !imp.tree.borrow().is_expanded(path) {
            self.list(path.to_path_buf(), true);
            return;
        }
        let changed = imp
            .tree
            .borrow_mut()
            .toggle(path, &mut *imp.listed.borrow_mut());
        if changed {
            self.reload();
        }
    }

    /// Puts the tree somewhere else on the same host, and remembers where it
    /// was standing.
    fn browse(&self, directory: &Path) {
        let imp = self.imp();
        let root = imp.tree.borrow().root().to_path_buf();
        if root == directory {
            return;
        }
        imp.back.borrow_mut().push(root);
        imp.forward.borrow_mut().clear();
        self.go(directory);
    }

    /// Back to the directory before this one, forward to the one it was left
    /// for.
    fn walk(&self, forward: bool) {
        let imp = self.imp();
        let (from, to) = if forward {
            (&imp.forward, &imp.back)
        } else {
            (&imp.back, &imp.forward)
        };
        let Some(directory) = from.borrow_mut().pop() else {
            return;
        };
        to.borrow_mut().push(imp.tree.borrow().root().to_path_buf());
        self.go(&directory);
    }

    /// The move itself, which the history is written around.
    fn go(&self, directory: &Path) {
        let imp = self.imp();
        if imp.link.borrow().is_none() {
            return;
        }
        let changed = imp
            .tree
            .borrow_mut()
            .sync(directory, &mut *imp.listed.borrow_mut());
        if changed {
            self.reload();
        }
        self.refresh_header();
        if !imp.listed.borrow().0.contains_key(directory) {
            self.list(directory.to_path_buf(), false);
        }
    }

    /// Throws away what was read and asks for it again: the root, and every
    /// directory that is open under it.
    fn reread(&self) {
        let imp = self.imp();
        if imp.link.borrow().is_none() {
            return;
        }
        let open: Vec<PathBuf> = imp.listed.borrow().0.keys().cloned().collect();
        imp.listed.borrow_mut().0.clear();
        for path in open {
            self.list(path, false);
        }
    }

    /// Asks the far end for one directory. `expand` opens it when it lands,
    /// which is what a click on a closed folder means.
    fn list(&self, path: PathBuf, expand: bool) {
        let imp = self.imp();
        let Some(link) = imp.link.borrow().clone() else {
            return;
        };
        if !imp.pending.borrow_mut().insert(path.clone()) {
            return;
        }
        let generation = imp.generation.get();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let remote = path.to_string_lossy().into_owned();
                let Ok((listed, gone)) = gio::spawn_blocking(move || {
                    let Ok(mut session) = link.lock() else {
                        return (None, Some(ended()));
                    };
                    let listed = rows(&mut session, &remote);
                    (listed, gone(&session))
                })
                .await
                else {
                    return;
                };
                if generation != this.imp().generation.get() {
                    return;
                }
                this.imp().pending.borrow_mut().remove(&path);
                if let Some(failure) = gone {
                    this.lose(&failure);
                    return;
                }
                // A directory that would not open is left closed and left
                // where it is: the row above it is still a row, and the tree
                // has nowhere to say more than that.
                let Some(listed) = listed else {
                    return;
                };
                let imp = this.imp();
                imp.listed.borrow_mut().0.insert(path.clone(), listed);
                let changed = if expand {
                    imp.tree
                        .borrow_mut()
                        .expand(&path, &mut *imp.listed.borrow_mut())
                } else {
                    imp.tree.borrow_mut().rebuild(&mut *imp.listed.borrow_mut())
                };
                if changed {
                    this.reload();
                }
            }
        ));
    }

    // --- moving files ---------------------------------------------------------

    /// Opens a remote file in an editor pane, from a copy fetched over the
    /// browsing session; saving the pane sends the copy back.
    ///
    /// A copy some pane already has open is shown rather than fetched again,
    /// since fetching would write the far end's text under an unsaved buffer.
    fn edit(&self, item: &Item) {
        let imp = self.imp();
        let Some(window) = self.root().and_downcast::<crate::window::TuniWindow>() else {
            return;
        };
        let Some(alias) = imp.alias.borrow().clone() else {
            return;
        };
        let remote = item.path.to_string_lossy().into_owned();
        let Some(local) = tuni_core::sftp::copy_path(&alias, &remote) else {
            self.report(&Failure {
                message: format!("Couldn't edit {remote}."),
                detail: String::from("The host or the path cannot name a file on this machine."),
            });
            return;
        };
        if window.is_editing(&local) {
            window.open_file(&local);
            return;
        }
        let Some(link) = imp.link.borrow().clone() else {
            return;
        };
        let generation = imp.generation.get();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            window,
            async move {
                let into = local.clone();
                let Ok(fetched) = gio::spawn_blocking(move || {
                    let Ok(mut session) = link.lock() else {
                        return Err((ended(), true));
                    };
                    session
                        .fetch_to_edit(&remote, &into)
                        .ok_or_else(|| (failed(&session), session.is_lost()))
                })
                .await
                else {
                    return;
                };
                if generation != this.imp().generation.get() {
                    return;
                }
                match fetched {
                    Ok(()) => window.edit_fetched(&local),
                    Err((failure, true)) => this.lose(&failure),
                    Err((failure, false)) => this.report(&failure),
                }
            }
        ));
    }

    /// Copies one remote file to wherever the user says, or folders and
    /// several files, each under its own name, into the folder the user picks.
    fn download(&self, items: Vec<Item>) {
        let one_file = match items.as_slice() {
            [] => return,
            [item] if !item.is_directory => Some(item.name.clone()),
            _ => None,
        };
        let dialog = gtk::FileDialog::builder()
            .title(match items.as_slice() {
                [item] if item.is_directory => "Download Folder",
                [_] => "Download File",
                _ => "Download",
            })
            .accept_label("Download")
            .build();
        if let Some(folder) = glib::user_special_dir(glib::UserDirectory::Downloads) {
            dialog.set_initial_folder(Some(&gio::File::for_path(folder)));
        }
        let into_folder = one_file.is_none();
        let chosen = glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |chosen: Result<gio::File, glib::Error>| {
                let Some(local) = chosen.ok().and_then(|file| file.path()) else {
                    return;
                };
                for item in items {
                    let local = if into_folder {
                        local.join(&item.name)
                    } else {
                        local.clone()
                    };
                    this.start(Way::Down, item.path.to_string_lossy().into_owned(), local);
                }
            }
        );
        if let Some(name) = one_file {
            dialog.set_initial_name(Some(&name));
            dialog.save(self.window().as_ref(), gio::Cancellable::NONE, chosen);
        } else {
            dialog.select_folder(self.window().as_ref(), gio::Cancellable::NONE, chosen);
        }
    }

    /// Sends one local file, or one folder and everything in it, into a
    /// remote directory under the name it has here.
    fn upload(&self, directory: &Path, folder: bool) {
        if self.imp().link.borrow().is_none() {
            return;
        }
        let directory = directory.to_path_buf();
        let dialog = gtk::FileDialog::builder()
            .title(if folder {
                "Upload Folder"
            } else {
                "Upload File"
            })
            .accept_label("Upload")
            .build();
        let chosen = glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |chosen: Result<gio::File, glib::Error>| {
                let Some(local) = chosen.ok().and_then(|file| file.path()) else {
                    return;
                };
                let Some(name) = local.file_name() else {
                    return;
                };
                let remote = directory.join(name).to_string_lossy().into_owned();
                this.start(Way::Up, remote, local);
            }
        );
        if folder {
            dialog.select_folder(self.window().as_ref(), gio::Cancellable::NONE, chosen);
        } else {
            dialog.open(self.window().as_ref(), gio::Cancellable::NONE, chosen);
        }
    }

    /// Takes what a drag left: every file and folder goes into `directory`
    /// under the name it has here.
    fn receive(&self, directory: &Path, dropped: &glib::Value) -> bool {
        if self.imp().link.borrow().is_none() {
            return false;
        }
        let Ok(list) = dropped.get::<gdk::FileList>() else {
            return false;
        };
        // A drag can carry something with no path at all, a file inside an
        // archive or a photo an application only holds in memory, and there is
        // nothing here to send for one of those.
        let mut any = false;
        for local in list.files().iter().filter_map(gio::File::path) {
            let Some(name) = local.file_name() else {
                continue;
            };
            let remote = directory.join(name).to_string_lossy().into_owned();
            self.start(Way::Up, remote, local.clone());
            any = true;
        }
        any
    }

    /// Puts one transfer at the back of the queue, against the host the page
    /// is on, and starts it if the pipe is free.
    fn start(&self, way: Way, remote: String, local: PathBuf) {
        let Some(alias) = self.imp().alias.borrow().clone() else {
            return;
        };
        self.imp()
            .queue
            .borrow_mut()
            .push(&alias, way, remote, local);
        self.pump();
        self.refresh_transfers();
    }

    /// The button on the bar. Stops the transfer at its next chunk and
    /// forgets the ones queued behind it: the press means "stop
    /// transferring", and eleven files nobody wants any more would otherwise
    /// start the moment this one dies. One of them alone is the list's.
    fn cancel_transfer(&self) {
        self.imp().queue.borrow_mut().clear();
        self.stop_moving();
        self.refresh_transfers();
    }

    fn stop_moving(&self) {
        if let Some((_, moved)) = self.imp().moving.borrow().as_ref() {
            moved.stop.store(true, Ordering::Relaxed);
        }
    }

    /// Starts the next transfer, if there is one and nothing else is going.
    fn pump(&self) {
        if self.imp().moving.borrow().is_some() {
            return;
        }
        let next = self.imp().queue.borrow_mut().pop();
        if let Some(job) = next {
            self.transfer(job);
        }
    }

    /// Moves one file, and draws how far it has got while it goes.
    ///
    /// One at a time, on [`imp::TuniSftp::carrier`] rather than the session
    /// the page lists with, so browsing goes on while a file moves. What is
    /// waiting waits in [`imp::TuniSftp::queue`], where the bar can count it.
    /// The job names its host, so the page going to another one changes
    /// nothing here: the carrier is the job's host's, not the page's.
    fn transfer(&self, job: Job) {
        let imp = self.imp();
        let carrier = imp
            .carrier
            .take()
            .filter(|(carried, _)| *carried == job.alias)
            .map(|(_, session)| session);
        let moved = Arc::new(Moved::default());
        imp.moving.replace(Some((job.clone(), Arc::clone(&moved))));

        let name = job.name();
        let label = match job.way {
            Way::Up => format!("Sending {name}"),
            Way::Down => format!("Fetching {name}"),
        };
        self.watch(label, Arc::clone(&moved));

        let flag = Arc::clone(&moved);
        let (alias, way, remote, local) = (
            job.alias.clone(),
            job.way,
            job.remote.clone(),
            job.local.clone(),
        );
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let host = alias.clone();
                let planned = gio::spawn_blocking(move || {
                    // Opened the way the page's own session was. Over a shared
                    // connection that is one more channel on the master;
                    // without one it is a second login, as unattended as the
                    // first.
                    let mut session = match carrier {
                        Some(session) => session,
                        None => {
                            let host = tuni_core::ssh::host(&host);
                            let control = crate::hosts::control();
                            match Session::open(&tuni_core::ssh::sftp_command(&host, &control)) {
                                Ok(session) => session,
                                Err(failure) => return (Err((failure, false)), None),
                            }
                        }
                    };
                    // A folder is walked whole before anything moves, so the
                    // bar can say how far through the bytes it is and the
                    // question about landing on a folder of the same name
                    // comes before any of it is written.
                    let plan = match way {
                        Way::Up => session.plan_upload(&local, &remote),
                        Way::Down => session.plan_download(&remote, &local),
                    };
                    match plan {
                        Some(plan) => (Ok(plan), Some(session)),
                        None => {
                            let lost = session.is_lost();
                            (Err((failed(&session), lost)), (!lost).then_some(session))
                        }
                    }
                })
                .await;

                let halted = || flag.stop.load(Ordering::Relaxed);
                let (moved, session) = 'run: {
                    let (plan, session) = match planned {
                        Ok((Ok(plan), Some(session))) => (plan, session),
                        Ok((Err(failure), session)) => break 'run (Some(Err(failure)), session),
                        Ok((Ok(_), None)) | Err(_) => break 'run (None, None),
                    };
                    if halted() || (plan.merges && !this.merge(&name).await) || halted() {
                        break 'run (None, Some(session));
                    }
                    let verb = match way {
                        Way::Up => "send",
                        Way::Down => "fetch",
                    };
                    let copied = gio::spawn_blocking(move || {
                        let mut session = session;
                        let mut note = |done: u64, total: u64| {
                            moved.done.store(done, Ordering::Relaxed);
                            moved.total.store(total, Ordering::Relaxed);
                            !moved.stop.load(Ordering::Relaxed)
                        };
                        // Every file lands under a temporary name and is moved
                        // on when whole, so a cancel or a dead link leaves
                        // what was there before, at either end.
                        let report = session.copy(way, &plan, &mut note);
                        let lost = session.is_lost();
                        let said = summary(&plan, &report, verb);
                        let done = if report.stopped {
                            Err((failed(&session), lost))
                        // Nothing went at all, which is what the files behind
                        // it would run into as well.
                        } else if report.done == 0 && report.failed > 0 {
                            Err((said.unwrap_or_else(|| failed(&session)), lost))
                        } else {
                            Ok(said)
                        };
                        // A session that will not answer again ends here, and
                        // the next file opens another.
                        (done, (!lost).then_some(session))
                    })
                    .await;
                    match copied {
                        Ok((done, session)) => (Some(done), session),
                        Err(_) => (None, None),
                    }
                };

                // Back for the next file, and let go of at once if no file
                // for that host is coming.
                let imp = this.imp();
                imp.carrier.replace(session.map(|session| (alias, session)));
                let job = imp.moving.take().map(|(job, _)| job);
                this.release();
                if let (Some(job), Some(moved)) = (job, moved) {
                    this.finish(job, moved, flag.stop.load(Ordering::Relaxed));
                }
                this.pump();
                this.refresh_transfers();
            }
        ));
    }

    /// Whether a folder may land on one of the same name, which replaces what
    /// is in both. Asked once the walk has found it there and before anything
    /// is written.
    async fn merge(&self, name: &str) -> bool {
        let dialog = adw::AlertDialog::new(
            Some(&format!("Merge with “{name}”?")),
            Some(&format!(
                "There is already a folder called “{name}” where this is going. A \
                 file in both is replaced by the copy coming in; the rest stays."
            )),
        );
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("merge", "Merge");
        dialog.set_response_appearance("merge", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.choose_future(Some(self)).await == "merge"
    }

    /// What one transfer's end means for the page. Nothing here is a dialog:
    /// a failure goes on the transfers list, where it waits to be read
    /// without standing in front of whatever is being done meanwhile.
    fn finish(
        &self,
        job: Job,
        moved: Result<Option<Failure>, (Failure, bool)>,
        stopped_by_hand: bool,
    ) {
        let here = self.imp().alias.borrow().as_deref() == Some(job.alias.as_str());
        // The directory has something in it that it did not have, even after a
        // folder stopped partway, and nothing remote will say so on its own.
        // Only if the page is still showing that host, and only if the
        // connection is still there to ask.
        if job.way == Way::Up
            && here
            && !matches!(moved, Err((_, true)))
            && let Some(directory) = Path::new(&job.remote).parent()
        {
            self.list(directory.to_path_buf(), false);
        }
        match moved {
            // The cancel button doing its job is not news, and a failure line
            // would only repeat what the hand that pressed it knows.
            Err(_) if stopped_by_hand => {}
            Err((failure, lost)) => {
                // A connection that died under the carrier died under the
                // listing too, since both go over the same one.
                if lost && here {
                    self.lose(&failure);
                }
                // What was behind it for the same host goes no further: a
                // connection that died on the first file is going to die on
                // the other eleven, and eleven failures is not eleven pieces
                // of news.
                self.imp().queue.borrow_mut().fail(job, failure, true);
            }
            // Some of a folder went and some did not, which is said without
            // holding up what is queued behind it.
            Ok(Some(partly)) => self.imp().queue.borrow_mut().fail(job, partly, false),
            Ok(None) => {}
        }
    }

    // --- changing them --------------------------------------------------------

    /// Makes a directory inside another one.
    fn new_folder(&self, directory: &Path) {
        let directory = directory.to_path_buf();
        self.ask(
            "New Folder",
            &format!("Inside “{}”", directory.to_string_lossy()),
            "",
            "Create",
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |name: String| {
                    let Some(path) = this.named(&directory, &name) else {
                        return;
                    };
                    // Opened when it lands, so the thing that was just made is
                    // visible rather than inside a folder nobody opened.
                    this.change(directory.clone(), true, move |session| session.mkdir(&path));
                }
            ),
        );
    }

    /// Makes an empty file, refused rather than emptied when the name is
    /// taken.
    fn new_file(&self, directory: &Path) {
        let directory = directory.to_path_buf();
        self.ask(
            "New File",
            &format!("Inside “{}”", directory.to_string_lossy()),
            "",
            "Create",
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |name: String| {
                    let Some(path) = this.named(&directory, &name) else {
                        return;
                    };
                    this.change(directory.clone(), true, move |session| {
                        session.create(&path)
                    });
                }
            ),
        );
    }

    /// What `ls -l` would say about one thing, with its permissions as nine
    /// boxes to tick. The kind of file and the setuid, setgid and sticky bits
    /// are kept as they were: nothing here offers them, so nothing here
    /// changes them.
    fn properties(&self, item: &Item) {
        let Some(directory) = item.path.parent().map(Path::to_path_buf) else {
            return;
        };
        let details = item.details.unwrap_or_default();
        let grid = gtk::Grid::builder()
            .row_spacing(6)
            .column_spacing(12)
            .halign(gtk::Align::Center)
            .build();
        for (column, heading) in ["Read", "Write", "Run"].into_iter().enumerate() {
            let label = gtk::Label::new(Some(heading));
            label.add_css_class("dim-label");
            grid.attach(&label, column as i32 + 1, 0, 1, 1);
        }
        let octal = gtk::Label::new(None);
        octal.add_css_class("numeric");
        let mut boxes = Vec::with_capacity(9);
        for (row, who) in ["Owner", "Group", "Others"].into_iter().enumerate() {
            let label = gtk::Label::builder().label(who).xalign(0.0).build();
            grid.attach(&label, 0, row as i32 + 1, 1, 1);
            for column in 0..3 {
                let bit = 0o400 >> (row * 3 + column);
                let check = gtk::CheckButton::builder()
                    .active(details.mode & bit != 0)
                    .halign(gtk::Align::Center)
                    .build();
                grid.attach(&check, column as i32 + 1, row as i32 + 1, 1, 1);
                boxes.push((check, bit));
            }
        }
        let boxes = Rc::new(boxes);
        let kept = details.mode & 0o7000;
        let mode = {
            let boxes = Rc::clone(&boxes);
            move || {
                boxes
                    .iter()
                    .filter(|(check, _)| check.is_active())
                    .fold(kept, |mode, (_, bit)| mode | bit)
            }
        };
        let show = {
            let mode = mode.clone();
            let octal = octal.clone();
            move || octal.set_text(&format!("{:04o}", mode()))
        };
        show();
        for (check, _) in boxes.iter() {
            let show = show.clone();
            check.connect_toggled(move |_| show());
        }
        grid.attach(&octal, 0, 4, 4, 1);

        let mut body = item.path.to_string_lossy().into_owned();
        if item.details.is_some() {
            body.push('\n');
            if !item.is_directory {
                body.push_str(&tuni_core::editor::size_text(details.size));
                body.push_str(", ");
            }
            body.push_str("changed ");
            body.push_str(&crate::files::when(details.mtime));
        }
        let dialog = adw::AlertDialog::new(Some(&item.name), Some(&body));
        dialog.set_extra_child(Some(&grid));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("apply", "Apply");
        dialog.set_response_appearance("apply", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("apply"));
        dialog.set_close_response("cancel");
        let path = item.path.to_string_lossy().into_owned();
        let before = details.mode & 0o7777;
        dialog.connect_response(
            None,
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_, response| {
                    let after = mode();
                    if response != "apply" || after == before {
                        return;
                    }
                    let path = path.clone();
                    this.change(directory.clone(), false, move |session| {
                        session.chmod(&path, after)
                    });
                }
            ),
        );
        dialog.present(Some(self));
    }

    /// Gives one file or directory another name in the same directory.
    fn rename(&self, item: &Item) {
        let Some(directory) = item.path.parent().map(Path::to_path_buf) else {
            return;
        };
        let from = item.path.to_string_lossy().into_owned();
        let current = item.name.clone();
        self.ask(
            "Rename",
            &format!("“{}”", item.name),
            &item.name,
            "Rename",
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |name: String| {
                    if name.trim() == current {
                        return;
                    }
                    let Some(to) = this.named(&directory, &name) else {
                        return;
                    };
                    let from = from.clone();
                    this.change(directory.clone(), false, move |session| {
                        session.rename(&from, &to)
                    });
                }
            ),
        );
    }

    /// Deletes files, and folders with everything in them.
    ///
    /// A folder is walked first, so the question can say what it holds, and
    /// then emptied from the bottom up.
    ///
    /// ponytail: walk and delete both hold the session the page browses with,
    /// one round trip a folder for the walk, so a big tree stops browsing
    /// until it is gone. On the carrier, behind the queue, if that matters.
    fn delete(&self, items: Vec<Item>) {
        if items.is_empty() {
            return;
        }
        let Some(link) = self.imp().link.borrow().clone() else {
            return;
        };
        let paths: Vec<String> = items
            .iter()
            .map(|item| item.path.to_string_lossy().into_owned())
            .collect();
        // Each directory something went from, read again once it is gone.
        let mut directories: Vec<PathBuf> = items
            .iter()
            .filter_map(|item| item.path.parent().map(Path::to_path_buf))
            .collect();
        directories.sort();
        directories.dedup();
        let title = match items.as_slice() {
            [item] => format!("Delete “{}”?", item.name),
            _ => format!("Delete {} items?", items.len()),
        };
        let generation = self.imp().generation.get();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let walk = Arc::clone(&link);
                let Ok(planned) = gio::spawn_blocking(move || {
                    let Ok(mut session) = walk.lock() else {
                        return Err((ended(), true));
                    };
                    session
                        .plan_delete(&paths)
                        .ok_or_else(|| (failed(&session), session.is_lost()))
                })
                .await
                else {
                    return;
                };
                if generation != this.imp().generation.get() {
                    return;
                }
                let plan = match planned {
                    Ok(plan) => plan,
                    Err((failure, true)) => {
                        this.lose(&failure);
                        return;
                    }
                    Err((failure, false)) => {
                        this.report(&failure);
                        return;
                    }
                };

                // `inside` counts under the one folder chosen, which says
                // nothing true about several.
                let inside = if items.len() == 1 {
                    plan.inside().map(|inside| format!("It holds {inside}."))
                } else {
                    Some(format!("That is {}.", plan.count()))
                };
                let body = match inside {
                    Some(inside) => format!(
                        "{inside} There is no trash on the other machine, \
                         so all of it is gone for good."
                    ),
                    None => String::from(
                        "There is no trash on the other machine, so this is gone for good.",
                    ),
                };
                let dialog = adw::AlertDialog::new(Some(&title), Some(&body));
                dialog.add_response("cancel", "Cancel");
                dialog.add_response("delete", "Delete");
                dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");
                if dialog.choose_future(Some(&this)).await != "delete"
                    || generation != this.imp().generation.get()
                {
                    return;
                }

                let Ok(done) = gio::spawn_blocking(move || {
                    let Ok(mut session) = link.lock() else {
                        return Err((ended(), true));
                    };
                    let report = session.delete(&plan);
                    if report.stopped {
                        return Err((failed(&session), session.is_lost()));
                    }
                    Ok(summary(&plan, &report, "delete"))
                })
                .await
                else {
                    return;
                };
                if generation != this.imp().generation.get() {
                    return;
                }
                if let Err((failure, true)) = &done {
                    this.lose(failure);
                    return;
                }
                for directory in directories {
                    this.list(directory, false);
                }
                match done {
                    Err((failure, _)) | Ok(Some(failure)) => this.report(&failure),
                    Ok(None) => {}
                }
            }
        ));
    }

    /// Runs one change at the far end and reads the directory it happened in,
    /// since nothing over there will say what changed.
    fn change<F>(&self, directory: PathBuf, expand: bool, apply: F)
    where
        F: FnOnce(&mut Session) -> Option<()> + Send + 'static,
    {
        let Some(link) = self.imp().link.borrow().clone() else {
            return;
        };
        let generation = self.imp().generation.get();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let Ok(done) = gio::spawn_blocking(move || {
                    let Ok(mut session) = link.lock() else {
                        return Err((ended(), true));
                    };
                    apply(&mut session).ok_or_else(|| (failed(&session), session.is_lost()))
                })
                .await
                else {
                    return;
                };
                if generation != this.imp().generation.get() {
                    return;
                }
                match done {
                    Ok(()) => {
                        // The cached listing is a directory ago, and `list`
                        // writes over it with what is there now.
                        this.list(directory, expand);
                    }
                    Err((failure, true)) => this.lose(&failure),
                    Err((failure, false)) => this.report(&failure),
                }
            }
        ));
    }

    /// Draws a transfer while it runs: a local timer over two numbers, not a
    /// poll of anything on the other machine.
    ///
    /// The timer ends when `moved` is no longer the transfer running, which
    /// the next one starting in the same turn of the loop would otherwise hide
    /// from it: two timers on one bar, each writing its own file's name.
    fn watch(&self, label: String, moved: Arc<Moved>) {
        let Some(bar) = self.imp().progress.borrow().clone() else {
            return;
        };
        bar.set_text(Some(&label));
        bar.set_fraction(0.0);
        let started = std::time::Instant::now();
        glib::timeout_add_local(
            std::time::Duration::from_millis(100),
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    let imp = this.imp();
                    let running = imp
                        .moving
                        .borrow()
                        .as_ref()
                        .is_some_and(|(_, now)| Arc::ptr_eq(now, &moved));
                    if !running {
                        return glib::ControlFlow::Break;
                    }
                    let done = moved.done.load(Ordering::Relaxed);
                    let total = moved.total.load(Ordering::Relaxed);
                    // Counted each tick rather than when the file started,
                    // since the list can take one off while this one moves.
                    let waiting = imp.queue.borrow().waiting().len();
                    let rest = if waiting == 0 {
                        String::new()
                    } else {
                        format!(" · {waiting} to go")
                    };
                    // Bytes over the whole run rather than over the last
                    // tick: an average settles where a 100ms sample flickers
                    // between a stall and a burst. Held back for the first
                    // moment because dividing by almost no time opens every
                    // transfer on a nonsense number.
                    let elapsed = started.elapsed().as_secs_f64();
                    let speed = if elapsed < 0.5 {
                        String::new()
                    } else {
                        let rate = done as f64 / elapsed;
                        format!(" · {}/s", glib::format_size(rate as u64))
                    };
                    // A server that did not say how big a file is leaves
                    // nothing to draw a fraction from, so the bar moves
                    // without claiming to know how far along it is.
                    if total == 0 {
                        bar.pulse();
                        bar.set_text(Some(&format!("{label}{speed}{rest}")));
                    } else {
                        let fraction = done as f64 / total as f64;
                        bar.set_fraction(fraction);
                        let percent = (fraction * 100.0) as u32;
                        bar.set_text(Some(&format!("{label} · {percent}%{speed}{rest}")));
                    }
                    glib::ControlFlow::Continue
                }
            ),
        );
    }

    /// Shows the transfers row while it has anything to say, and says it: the
    /// bar while something moves, a count once nothing does and something
    /// failed. The list under its button is redrawn if it is open.
    fn refresh_transfers(&self) {
        let imp = self.imp();
        let moving = imp.moving.borrow().is_some();
        let (waiting, failed) = {
            let queue = imp.queue.borrow();
            (queue.waiting().len(), queue.failed().len())
        };
        let busy = moving || waiting > 0;
        if let Some(row) = imp.progress_row.borrow().as_ref() {
            row.set_visible(busy || failed > 0);
        }
        if let Some(bar) = imp.progress.borrow().as_ref() {
            bar.set_visible(busy);
        }
        if let Some(cancel) = imp.cancel.borrow().as_ref() {
            cancel.set_visible(busy);
        }
        if let Some(label) = imp.failed.borrow().as_ref() {
            label.set_visible(!busy && failed > 0);
            label.set_text(&if failed == 1 {
                String::from("A transfer failed")
            } else {
                format!("{failed} transfers failed")
            });
        }
        if imp
            .transfers_popover
            .borrow()
            .as_ref()
            .is_some_and(gtk::Popover::is_visible)
        {
            self.list_transfers();
        }
    }

    /// Fills the transfers popover: the one running, the ones waiting, the ones
    /// that failed, each with the button that takes it away.
    ///
    /// ponytail: rebuilt whole on every change while open, and cut at
    /// [`SHOWN_TRANSFERS`] rows with a count of the rest, since a drop of a
    /// thousand files would otherwise be a thousand rows rebuilt per file.
    fn list_transfers(&self) {
        let imp = self.imp();
        let Some(list) = imp.transfers.borrow().clone() else {
            return;
        };
        list.remove_all();
        let mut shown = 0;
        let mut more = 0;
        let mut add = |row: gtk::Box| {
            if shown < SHOWN_TRANSFERS {
                list.append(&row);
                shown += 1;
            } else {
                more += 1;
            }
        };
        if let Some((job, _)) = imp.moving.borrow().as_ref() {
            add(self.transfer_row(job, "Moving", "Stop", None));
        }
        let queue = imp.queue.borrow();
        for job in queue.waiting() {
            add(self.transfer_row(job, "Waiting", "Remove", Some(job.id)));
        }
        for (job, failure) in queue.failed() {
            let row = self.transfer_row(job, &failure.message, "Dismiss", Some(job.id));
            row.set_tooltip_text(Some(&failure.detail));
            add(row);
        }
        let failed = !queue.failed().is_empty();
        drop(queue);
        if more > 0 {
            let rest = gtk::Label::new(Some(&format!("and {more} more")));
            rest.add_css_class("dim-label");
            rest.set_margin_top(6);
            rest.set_margin_bottom(6);
            list.append(&rest);
        }
        if shown == 0 {
            let none = gtk::Label::new(Some("No transfers"));
            none.add_css_class("dim-label");
            none.set_margin_top(6);
            none.set_margin_bottom(6);
            list.append(&none);
        }
        if failed {
            let clear = gtk::Button::with_label("Clear Failed");
            clear.add_css_class("flat");
            clear.connect_clicked(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.imp().queue.borrow_mut().dismiss();
                    this.refresh_transfers();
                }
            ));
            list.append(&clear);
        }
    }

    /// One line of the transfers list: the file, what is happening to it and
    /// on which host, and a button. `id` is the job to take off the queue, and
    /// nothing at all for the one running, which is stopped instead.
    fn transfer_row(&self, job: &Job, state: &str, action: &str, id: Option<u64>) -> gtk::Box {
        let name = gtk::Label::builder()
            .label(job.name())
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        let way = match job.way {
            Way::Up => "to",
            Way::Down => "from",
        };
        let said = gtk::Label::builder()
            .label(format!("{state}, {way} {}", job.alias))
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        said.add_css_class("caption");
        said.add_css_class("dim-label");
        let words = gtk::Box::new(gtk::Orientation::Vertical, 0);
        words.set_hexpand(true);
        words.append(&name);
        words.append(&said);

        let button = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text(action)
            .valign(gtk::Align::Center)
            .build();
        button.add_css_class("flat");
        button.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                match id {
                    Some(id) => {
                        this.imp().queue.borrow_mut().remove(id);
                    }
                    None => this.stop_moving(),
                }
                this.refresh_transfers();
            }
        ));

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.set_margin_start(6);
        row.set_margin_end(2);
        row.set_margin_top(4);
        row.set_margin_bottom(4);
        row.append(&words);
        row.append(&button);
        row
    }

    // --- drawing --------------------------------------------------------------

    fn reload(&self) {
        let imp = self.imp();
        let Some(rows) = imp.rows.borrow().clone() else {
            return;
        };
        let tree = imp.tree.borrow();
        let items: Vec<Row> = tree
            .items()
            .iter()
            .map(|item| Row::new(item.clone(), tree.is_expanded(&item.path)))
            .collect();
        rows.splice(0, rows.n_items(), &items);
    }

    fn refresh_header(&self) {
        let imp = self.imp();
        let alias = imp.alias.borrow().clone().unwrap_or_default();
        let root = imp.tree.borrow().root().to_string_lossy().into_owned();
        if let Some(title) = imp.title.borrow().as_ref() {
            title.set_text(&alias);
        }
        if let Some(subtitle) = imp.subtitle.borrow().as_ref() {
            subtitle.set_text(&root);
            subtitle.set_tooltip_text(Some(&root));
        }
    }

    /// Shows a sentence instead of a listing.
    fn say(&self, heading: &str, body: &str) {
        self.tell(heading, body, false);
    }

    /// Shows why the connection failed, with the button that tries again.
    fn fail(&self, heading: &str, body: &str) {
        self.tell(heading, body, true);
    }

    fn tell(&self, heading: &str, body: &str, retry: bool) {
        let imp = self.imp();
        if let Some(button) = imp.reconnect.borrow().as_ref() {
            button.set_visible(retry);
        }
        if let Some(status) = imp.status.borrow().as_ref() {
            status.set_title(heading);
            status.set_description(Some(body).filter(|body| !body.is_empty()));
        }
        if let Some(stack) = imp.stack.borrow().as_ref() {
            stack.set_visible_child_name("status");
        }
    }

    fn show_list(&self) {
        if let Some(stack) = self.imp().stack.borrow().as_ref() {
            stack.set_visible_child_name("list");
        }
    }

    /// The context menu for a row. A click on a row inside the selection acts
    /// on all of it, and anywhere else selects that row alone first, which is
    /// what a file manager does.
    fn popup_menu(&self, widget: &gtk::Box, item: &Item, position: u32, x: f64, y: f64) {
        let imp = self.imp();
        let (Some(menu), Some(list)) = (imp.menu.borrow().clone(), imp.list.borrow().clone())
        else {
            return;
        };
        if let Some(selection) = list.model()
            && !selection.is_selected(position)
        {
            selection.select_item(position, true);
        }
        let mut targets = self.selected();
        if targets.is_empty() {
            targets.push(item.clone());
        }
        let many = targets.len() > 1;
        imp.targets.replace(targets);

        let model = gio::Menu::new();
        let move_files = gio::Menu::new();
        let edit = gio::Menu::new();
        if many {
            move_files.append(Some("Download…"), Some("sftp.download"));
            edit.append(Some("Delete…"), Some("sftp.delete"));
        } else {
            if !item.is_directory {
                move_files.append(Some("Edit"), Some("sftp.edit"));
            }
            move_files.append(Some("Download…"), Some("sftp.download"));
            if item.is_directory {
                move_files.append(Some("Upload File Here…"), Some("sftp.upload"));
                move_files.append(Some("Upload Folder Here…"), Some("sftp.upload-folder"));
                move_files.append(Some("New File…"), Some("sftp.new-file"));
                move_files.append(Some("New Folder…"), Some("sftp.new-folder"));
            }
            move_files.append(Some("Copy Path"), Some("sftp.copy-path"));
            move_files.append(Some("Copy Remote Address"), Some("sftp.copy-address"));
            edit.append(Some("Rename…"), Some("sftp.rename"));
            edit.append(Some("Properties…"), Some("sftp.properties"));
            edit.append(Some("Delete…"), Some("sftp.delete"));
        }
        model.append_section(None, &move_files);
        model.append_section(None, &edit);
        menu.set_menu_model(Some(&model));

        let point = widget
            .compute_point(&list, &gtk::graphene::Point::new(x as f32, y as f32))
            .unwrap_or_else(|| gtk::graphene::Point::new(x as f32, y as f32));
        crate::menu::popup_at(&menu, point);
    }

    /// The rows selected now, top to bottom.
    fn selected(&self) -> Vec<Item> {
        let Some(model) = self
            .imp()
            .list
            .borrow()
            .as_ref()
            .and_then(|list| list.model())
        else {
            return Vec::new();
        };
        let set = model.selection();
        let Some((rest, first)) = gtk::BitsetIter::init_first(&set) else {
            return Vec::new();
        };
        std::iter::once(first)
            .chain(rest)
            .filter_map(|position| model.item(position).and_downcast::<Row>())
            .filter_map(|row| row.item())
            .collect()
    }

    /// The one row a menu was opened on, for the actions that act on one.
    fn target(&self) -> Option<Item> {
        match self.imp().targets.borrow().as_slice() {
            [item] => Some(item.clone()),
            _ => None,
        }
    }

    fn folder_target(&self) -> Option<Item> {
        self.target().filter(|item| item.is_directory)
    }

    /// Goes to a path typed into the bar. `~` is wherever the session started,
    /// which is the far end's home, and a path that is not absolute starts
    /// from the directory being shown, the way `cd` would take it.
    fn go_to(&self, text: &str) {
        let imp = self.imp();
        let Some(link) = imp.link.borrow().clone() else {
            return;
        };
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let asked = match text.strip_prefix('~') {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => {
                format!(".{rest}")
            }
            _ if text.starts_with('/') => text.to_owned(),
            _ => imp
                .tree
                .borrow()
                .root()
                .join(text)
                .to_string_lossy()
                .into_owned(),
        };
        let generation = imp.generation.get();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let Ok((found, lost)) = gio::spawn_blocking(move || {
                    let Ok(mut session) = link.lock() else {
                        return (None, Some(ended()));
                    };
                    let found = session.realpath(&asked).and_then(|path| {
                        let is_directory = session.stat(&path)?.is_directory;
                        Some((path, is_directory))
                    });
                    (found, gone(&session))
                })
                .await
                else {
                    return;
                };
                if generation != this.imp().generation.get() {
                    return;
                }
                if let Some(failure) = lost {
                    this.lose(&failure);
                    return;
                }
                match found {
                    Some((path, true)) => {
                        this.show_location(false);
                        this.browse(Path::new(&path));
                    }
                    _ => {
                        if let Some(location) = this.imp().location.borrow().as_ref() {
                            location.add_css_class("error");
                        }
                    }
                }
            }
        ));
    }

    fn show_location(&self, show: bool) {
        let imp = self.imp();
        let (Some(bar), Some(location)) = (
            imp.location_bar.borrow().clone(),
            imp.location.borrow().clone(),
        ) else {
            return;
        };
        if show && imp.link.borrow().is_none() {
            return;
        }
        location.remove_css_class("error");
        bar.set_reveal_child(show);
        if show {
            location.set_text(&imp.tree.borrow().root().to_string_lossy());
            location.set_position(-1);
            location.grab_focus();
        } else if let Some(list) = imp.list.borrow().as_ref() {
            list.grab_focus();
        }
    }

    /// A name typed into a dialog, joined onto the directory it belongs in.
    ///
    /// Nothing at all for an empty name, which is what an untouched field
    /// means, and a complaint for a name that would put the thing somewhere
    /// else: neither dialog offered that, and the far end would do it.
    fn named(&self, directory: &Path, name: &str) -> Option<String> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        if name.contains('/') || name == "." || name == ".." {
            self.report(&Failure {
                message: format!("Couldn't use “{name}”."),
                detail: String::from("A name can't contain “/” or be “.” or “..”."),
            });
            return None;
        }
        Some(directory.join(name).to_string_lossy().into_owned())
    }

    /// One name, asked for the way the local tree asks for one.
    fn ask(
        &self,
        heading: &str,
        body: &str,
        current: &str,
        confirm: &str,
        apply: impl Fn(String) + 'static,
    ) {
        let entry = gtk::Entry::builder()
            .text(current)
            .activates_default(true)
            .build();
        let dialog = adw::AlertDialog::new(Some(heading), Some(body));
        dialog.set_extra_child(Some(&entry));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("confirm", confirm);
        dialog.set_response_appearance("confirm", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("confirm"));
        dialog.set_close_response("cancel");
        dialog.connect_response(None, move |_, response| {
            if response == "confirm" {
                apply(entry.text().to_string());
            }
        });
        dialog.present(Some(self));
    }

    /// What went wrong, in the two lines whatever failed wrote it in.
    fn report(&self, failure: &Failure) {
        let dialog = adw::AlertDialog::new(Some(&failure.message), Some(&failure.detail));
        dialog.add_response("close", "Close");
        dialog.set_default_response(Some("close"));
        dialog.set_close_response("close");
        dialog.present(Some(self));
    }

    fn window(&self) -> Option<gtk::Window> {
        self.root().and_downcast::<gtk::Window>()
    }
}

/// How many rows the transfers list draws before it counts the rest instead.
const SHOWN_TRANSFERS: usize = 100;

/// The session behind a lock that a panicked thread left poisoned, which is
/// as gone as a broken pipe.
fn ended() -> Failure {
    Failure {
        message: String::from("The connection ended."),
        detail: String::from("Nothing is holding it open any more."),
    }
}

/// Why the session will not answer again, if it will not.
fn gone(session: &Session) -> Option<Failure> {
    session.is_lost().then(|| failed(session))
}

/// Why a transfer stopped, in the two parts a dialog wants.
fn failed(session: &Session) -> Failure {
    session.failure().cloned().unwrap_or_else(|| Failure {
        message: String::from("The transfer stopped."),
        detail: String::from("The connection ended."),
    })
}

/// One directory's rows, in the order a file manager shows them.
///
/// A symlink pays for a second round trip to learn whether it points at a
/// directory, since what a listing describes is the link. The local tree makes
/// the same trade for the same reason, and only a link pays it.
fn rows(session: &mut Session, directory: &str) -> Option<Vec<Item>> {
    let entries = session.list(directory)?;
    let base = PathBuf::from(directory);
    let mut items: Vec<Item> = entries
        .into_iter()
        // A repository's own bookkeeping, left out here for the reason the
        // local tree leaves it out: nobody opens it from a file tree.
        .filter(|entry| entry.name != ".git")
        .map(|entry| {
            let path = base.join(&entry.name);
            let is_directory = if entry.is_link {
                session
                    .stat(&path.to_string_lossy())
                    .is_some_and(|target| target.is_directory)
            } else {
                entry.is_directory
            };
            Item {
                name: entry.name,
                path,
                is_directory,
                depth: 0,
                details: Some(tuni_core::files::Details {
                    size: entry.size,
                    mtime: entry.mtime,
                    mode: entry.mode,
                }),
            }
        })
        .collect();
    // A pipe that broke on one of the links' second round trip leaves every
    // link after it drawn as a file.
    if session.is_lost() {
        return None;
    }
    items.sort_by(|a, b| {
        b.is_directory
            .cmp(&a.is_directory)
            .then_with(|| natural_cmp(&a.name, &b.name))
    });
    Some(items)
}

/// Why the last call did not answer, in one sentence for the page to show.
fn reason(session: &Session) -> String {
    session.failure().map_or_else(
        || String::from("The connection ended."),
        |failure| failure.detail.clone(),
    )
}

/// One action, holding the page weakly: the group is inserted into the page,
/// so anything stronger than this would be a cycle the page never leaves.
fn entry<F>(name: &str, sftp: &TuniSftp, activate: F) -> gio::ActionEntry<gio::SimpleActionGroup>
where
    F: Fn(&TuniSftp) + 'static,
{
    let weak = sftp.downgrade();
    gio::ActionEntry::builder(name)
        .activate(move |_: &gio::SimpleActionGroup, _, _| {
            if let Some(sftp) = weak.upgrade() {
                activate(&sftp);
            }
        })
        .build()
}
