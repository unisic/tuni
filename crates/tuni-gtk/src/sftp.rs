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
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::gdk;
use gtk::gio;
use gtk::glib;

use tuni_core::files::{Directory, Failure, Item, Tree, natural_cmp};
use tuni_core::sftp::Session;

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
}

/// One file waiting its turn. A drop of five files is five of these, four of
/// them behind the one the pipe is carrying.
pub struct Job {
    way: Way,
    remote: String,
    local: PathBuf,
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
        Arc, Cell, HashSet, Item, Job, Link, Listed, Moved, PathBuf, RefCell, Tree, VecDeque, gio,
        glib,
    };
    use adw::prelude::*;
    use adw::subclass::prelude::*;

    #[derive(Default)]
    pub struct TuniSftp {
        pub tree: RefCell<Tree>,
        pub listed: RefCell<Listed>,
        pub link: RefCell<Option<Link>>,
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
        pub title: RefCell<Option<gtk::Label>>,
        pub subtitle: RefCell<Option<gtk::Label>>,
        pub menu: RefCell<Option<gtk::PopoverMenu>>,
        pub progress: RefCell<Option<gtk::ProgressBar>>,
        /// The transfer running, if one is. One pipe carries one at a time, so
        /// a second would sit behind the lock with nothing on screen saying
        /// why; this is what refuses it instead.
        pub moving: RefCell<Option<Arc<Moved>>>,
        /// What is waiting for the pipe, in the order it was asked for. A drop
        /// of a directory's worth of files fills this and empties it one file
        /// at a time.
        pub queue: RefCell<VecDeque<Job>>,
        /// Where browsing has been, and where it was called back from, on the
        /// host being browsed now. Emptied with the session, since both are
        /// paths on a machine the page has left.
        pub back: RefCell<Vec<PathBuf>>,
        pub forward: RefCell<Vec<PathBuf>>,
        /// The row whose context menu is open, and so the one its actions act
        /// on.
        pub target: RefCell<Option<Item>>,
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

        let here = gio::Menu::new();
        here.append(Some("Upload File Here…"), Some("sftp.upload-here"));
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
        header.append(&more);
        header.append(&refresh);

        let progress = gtk::ProgressBar::builder()
            .show_text(true)
            .visible(false)
            .build();
        progress.set_margin_start(12);
        progress.set_margin_end(12);
        progress.set_margin_bottom(6);

        let rows = gio::ListStore::new::<Row>();
        let selection = gtk::SingleSelection::builder()
            .model(&rows)
            .autoselect(false)
            .can_unselect(true)
            .build();

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
                if let Some(item) = row.and_then(|row| row.item())
                    && item.is_directory
                {
                    this.browse(&item.path);
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

        let status = adw::StatusPage::builder()
            .icon_name("network-server-symbolic")
            .vexpand(true)
            .build();

        let stack = gtk::Stack::new();
        stack.add_named(&scroller, Some("list"));
        stack.add_named(&status, Some("status"));
        stack.set_visible_child_name("status");

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header);
        content.append(&progress);
        content.append(&stack);
        self.set_child(Some(&content));

        self.add_controller(crate::files::history(self, "sftp"));
        self.install_actions();

        imp.rows.replace(Some(rows));
        imp.list.replace(Some(list));
        imp.stack.replace(Some(stack));
        imp.status.replace(Some(status));
        imp.title.replace(Some(title));
        imp.subtitle.replace(Some(subtitle));
        imp.menu.replace(Some(menu));
        imp.progress.replace(Some(progress));

        self.say(
            "No Connection",
            "A pane connected to a host is what this page reads.",
        );
    }

    /// One row's widgets, in the shape [`bind_row`] fills: a chevron, an icon
    /// and a name. The chevron opens a directory on one click, which the row
    /// itself asks two for.
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

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        content.append(&expander);
        content.append(&icon);
        content.append(&label);

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
                    && let Some(selection) = list.model().and_downcast::<gtk::SingleSelection>()
                {
                    selection.set_selected(list_item.position());
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
            entry("download", self, |sftp| {
                let target = sftp.imp().target.borrow().clone();
                if let Some(item) = target.filter(|item| !item.is_directory) {
                    sftp.download(&item);
                }
            }),
            entry("upload", self, |sftp| {
                let target = sftp.imp().target.borrow().clone();
                if let Some(item) = target.filter(|item| item.is_directory) {
                    sftp.upload(&item.path);
                }
            }),
            entry("upload-here", self, |sftp| {
                let root = sftp.imp().tree.borrow().root().to_path_buf();
                sftp.upload(&root);
            }),
            entry("new-folder", self, |sftp| {
                let target = sftp.imp().target.borrow().clone();
                if let Some(item) = target.filter(|item| item.is_directory) {
                    sftp.new_folder(&item.path);
                }
            }),
            entry("new-folder-here", self, |sftp| {
                let root = sftp.imp().tree.borrow().root().to_path_buf();
                sftp.new_folder(&root);
            }),
            entry("rename", self, |sftp| {
                let target = sftp.imp().target.borrow().clone();
                if let Some(item) = target {
                    sftp.rename(&item);
                }
            }),
            entry("delete", self, |sftp| {
                let target = sftp.imp().target.borrow().clone();
                if let Some(item) = target {
                    sftp.delete(&item);
                }
            }),
            entry("copy-path", self, |sftp| {
                if let Some(item) = sftp.imp().target.borrow().as_ref() {
                    sftp.clipboard().set_text(&item.path.to_string_lossy());
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
        // Queued against the host that is going away, and every one of them
        // names a path on it. The history goes with them, for the same reason.
        imp.queue.borrow_mut().clear();
        imp.back.borrow_mut().clear();
        imp.forward.borrow_mut().clear();
        imp.listed.replace(Listed::default());
        // Dropped, which kills the child and lets the master close the
        // channel. A session is one host's, and this is another host now.
        imp.link.replace(None);
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
                    let Some(mut session) = Session::open(&argv) else {
                        return Err(String::from(
                            "Nothing here can answer a password or a passphrase, \
                             so the connection has to be open already. Connect \
                             the host in a pane first.",
                        ));
                    };
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
                    Err(detail) => this.say(&format!("Couldn't Read {name}"), &detail),
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
                let Ok(listed) = gio::spawn_blocking(move || {
                    let mut session = link.lock().ok()?;
                    rows(&mut session, &remote)
                })
                .await
                else {
                    return;
                };
                if generation != this.imp().generation.get() {
                    return;
                }
                this.imp().pending.borrow_mut().remove(&path);
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

    /// Copies one remote file to wherever the user says.
    fn download(&self, item: &Item) {
        let remote = item.path.to_string_lossy().into_owned();
        let dialog = gtk::FileDialog::builder()
            .title("Download File")
            .accept_label("Download")
            .initial_name(&item.name)
            .build();
        if let Some(folder) = glib::user_special_dir(glib::UserDirectory::Downloads) {
            dialog.set_initial_folder(Some(&gio::File::for_path(folder)));
        }
        dialog.save(
            self.window().as_ref(),
            gio::Cancellable::NONE,
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |chosen| {
                    if let Some(local) = chosen.ok().and_then(|file| file.path()) {
                        this.start(Way::Down, remote, local);
                    }
                }
            ),
        );
    }

    /// Sends one local file into a remote directory, under the name it has
    /// here.
    fn upload(&self, directory: &Path) {
        if self.imp().link.borrow().is_none() {
            return;
        }
        let directory = directory.to_path_buf();
        let dialog = gtk::FileDialog::builder()
            .title("Upload File")
            .accept_label("Upload")
            .build();
        dialog.open(
            self.window().as_ref(),
            gio::Cancellable::NONE,
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |chosen| {
                    let Some(local) = chosen.ok().and_then(|file| file.path()) else {
                        return;
                    };
                    let Some(name) = local.file_name() else {
                        return;
                    };
                    let remote = directory.join(name).to_string_lossy().into_owned();
                    this.start(Way::Up, remote, local);
                }
            ),
        );
    }

    /// Takes what a drag left: every file goes into `directory` under the name
    /// it has here, and a folder goes nowhere at all.
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
        let (files, folders): (Vec<PathBuf>, Vec<PathBuf>) = list
            .files()
            .iter()
            .filter_map(gio::File::path)
            .partition(|path| path.is_file());

        for local in &files {
            let Some(name) = local.file_name() else {
                continue;
            };
            let remote = directory.join(name).to_string_lossy().into_owned();
            self.start(Way::Up, remote, local.clone());
        }

        if let Some(first) = folders.first() {
            let name = first.file_name().unwrap_or(first.as_os_str());
            self.report(&Failure {
                message: if folders.len() == 1 {
                    format!("Couldn't send “{}”.", name.to_string_lossy())
                } else {
                    format!("Couldn't send {} folders.", folders.len())
                },
                detail: String::from(
                    "This carries one file at a time and has no recursive copy \
                     in it. Drop the files inside instead, or use rsync, which \
                     is better at a whole directory than this would be.",
                ),
            });
        }
        !files.is_empty()
    }

    /// Puts one transfer at the back of the queue and starts it if the pipe is
    /// free.
    fn start(&self, way: Way, remote: String, local: PathBuf) {
        self.imp()
            .queue
            .borrow_mut()
            .push_back(Job { way, remote, local });
        self.pump();
    }

    /// Starts the next transfer, if there is one and nothing else is going.
    fn pump(&self) {
        if self.imp().moving.borrow().is_some() {
            return;
        }
        let next = self.imp().queue.borrow_mut().pop_front();
        if let Some(job) = next {
            self.transfer(job);
        }
    }

    /// Moves one file, and draws how far it has got while it goes.
    ///
    /// One at a time, since the session is a single pipe behind a lock: a
    /// second transfer would wait there with nothing on screen to say why. What
    /// is waiting waits in [`imp::TuniSftp::queue`] instead, where the bar can
    /// count it.
    fn transfer(&self, job: Job) {
        let Job { way, remote, local } = job;
        let imp = self.imp();
        let Some(link) = imp.link.borrow().clone() else {
            return;
        };
        let moved = Arc::new(Moved::default());
        imp.moving.replace(Some(Arc::clone(&moved)));

        let directory = Path::new(&remote).parent().map(Path::to_path_buf);
        let name = Path::new(&remote).file_name().map_or_else(
            || remote.clone(),
            |name| name.to_string_lossy().into_owned(),
        );
        let label = match way {
            Way::Up => format!("Sending {name}"),
            Way::Down => format!("Fetching {name}"),
        };
        let waiting = imp.queue.borrow().len();
        self.watch(&if waiting == 0 {
            label
        } else {
            format!("{label}, {waiting} to go")
        });

        let generation = imp.generation.get();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let moved = gio::spawn_blocking(move || {
                    let Ok(mut session) = link.lock() else {
                        return Err(Failure {
                            message: String::from("The connection ended."),
                            detail: String::from("Nothing is holding it open any more."),
                        });
                    };
                    let mut note = |done: u64, total: u64| {
                        moved.done.store(done, Ordering::Relaxed);
                        moved.total.store(total, Ordering::Relaxed);
                    };
                    match way {
                        Way::Up => session
                            .put(&local, &remote, &mut note)
                            .ok_or_else(|| failed(&session)),
                        Way::Down => {
                            // Written beside where it is going and moved onto
                            // it when it is whole, which is how the editor
                            // saves: a transfer that dies leaves nothing under
                            // a name something else would open.
                            let part = local.with_file_name(partial(&local));
                            session
                                .get(&remote, &part, &mut note)
                                .ok_or_else(|| failed(&session))?;
                            std::fs::rename(&part, &local).map_err(|error| {
                                let _ = std::fs::remove_file(&part);
                                Failure {
                                    message: format!("Couldn't finish {}.", local.display()),
                                    detail: error.to_string(),
                                }
                            })
                        }
                    }
                })
                .await;

                this.imp().moving.replace(None);
                if let Some(bar) = this.imp().progress.borrow().as_ref() {
                    bar.set_visible(false);
                }
                if generation != this.imp().generation.get() {
                    return;
                }
                match moved {
                    Ok(Err(failure)) => {
                        // What was behind it goes no further. A connection that
                        // died on the first file is going to die on the other
                        // eleven, and eleven more dialogs is not eleven more
                        // pieces of news.
                        let waiting = this.imp().queue.borrow_mut().drain(..).count();
                        this.report(&stopped(&failure, waiting));
                    }
                    // The directory has something in it that it did not have,
                    // and nothing remote will say so on its own.
                    Ok(Ok(())) => {
                        if let (Way::Up, Some(directory)) = (way, directory) {
                            this.list(directory, false);
                        }
                    }
                    Err(_) => {}
                }
                this.pump();
            }
        ));
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

    /// Deletes one file, or one directory that has nothing in it.
    fn delete(&self, item: &Item) {
        let Some(directory) = item.path.parent().map(Path::to_path_buf) else {
            return;
        };
        let path = item.path.to_string_lossy().into_owned();
        let is_directory = item.is_directory;
        let body = if is_directory {
            "There is no trash on the other machine, so this is gone for good. A \
             directory with anything in it is refused."
        } else {
            "There is no trash on the other machine, so this is gone for good."
        };
        let dialog = adw::AlertDialog::new(Some(&format!("Delete “{}”?", item.name)), Some(body));
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
                    let path = path.clone();
                    this.change(directory.clone(), false, move |session| {
                        if is_directory {
                            session.rmdir(&path)
                        } else {
                            session.remove(&path)
                        }
                    });
                }
            ),
        );
        dialog.present(Some(self));
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
                        return Err(Failure {
                            message: String::from("The connection ended."),
                            detail: String::from("Nothing is holding it open any more."),
                        });
                    };
                    apply(&mut session).ok_or_else(|| failed(&session))
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
                    Err(failure) => this.report(&failure),
                }
            }
        ));
    }

    /// Draws a transfer while it runs: a local timer over two numbers, not a
    /// poll of anything on the other machine.
    fn watch(&self, label: &str) {
        let Some(bar) = self.imp().progress.borrow().clone() else {
            return;
        };
        bar.set_text(Some(label));
        bar.set_fraction(0.0);
        bar.set_visible(true);
        glib::timeout_add_local(
            std::time::Duration::from_millis(100),
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    let Some(moved) = this.imp().moving.borrow().clone() else {
                        return glib::ControlFlow::Break;
                    };
                    let done = moved.done.load(Ordering::Relaxed);
                    let total = moved.total.load(Ordering::Relaxed);
                    if let Some(bar) = this.imp().progress.borrow().as_ref() {
                        // A server that did not say how big a file is leaves
                        // nothing to draw a fraction from, so the bar moves
                        // without claiming to know how far along it is.
                        if total == 0 {
                            bar.pulse();
                        } else {
                            bar.set_fraction(done as f64 / total as f64);
                        }
                    }
                    glib::ControlFlow::Continue
                }
            ),
        );
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
        let imp = self.imp();
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

    fn popup_menu(&self, widget: &gtk::Box, item: &Item, position: u32, x: f64, y: f64) {
        let imp = self.imp();
        let (Some(menu), Some(list)) = (imp.menu.borrow().clone(), imp.list.borrow().clone())
        else {
            return;
        };
        if let Some(selection) = list.model().and_downcast::<gtk::SingleSelection>() {
            selection.set_selected(position);
        }
        imp.target.replace(Some(item.clone()));
        let model = gio::Menu::new();
        let move_files = gio::Menu::new();
        if item.is_directory {
            move_files.append(Some("Upload File Here…"), Some("sftp.upload"));
        } else {
            move_files.append(Some("Download…"), Some("sftp.download"));
        }
        if item.is_directory {
            move_files.append(Some("New Folder…"), Some("sftp.new-folder"));
        }
        move_files.append(Some("Copy Path"), Some("sftp.copy-path"));
        model.append_section(None, &move_files);

        let edit = gio::Menu::new();
        edit.append(Some("Rename…"), Some("sftp.rename"));
        edit.append(Some("Delete…"), Some("sftp.delete"));
        model.append_section(None, &edit);
        menu.set_menu_model(Some(&model));

        let point = widget
            .compute_point(&list, &gtk::graphene::Point::new(x as f32, y as f32))
            .unwrap_or_else(|| gtk::graphene::Point::new(x as f32, y as f32));
        crate::menu::popup_at(&menu, point);
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

/// Which way a transfer goes. Everything else about the two is the same.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Way {
    Up,
    Down,
}

/// A name beside the file being written, and the reason it is a dotfile with a
/// process id in it is the one [`tuni_core::editor::save`] gives: two windows
/// fetching the same name at once must not share the temporary.
fn partial(local: &Path) -> String {
    let name = local
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    format!(".{name}.tuni-{}", std::process::id())
}

/// A failure that took a queue down with it, since what stopped the file being
/// moved is what the ones behind it would have run into.
fn stopped(failure: &Failure, waiting: usize) -> Failure {
    if waiting == 0 {
        return failure.clone();
    }
    let rest = if waiting == 1 {
        String::from("The one behind it was left where it is.")
    } else {
        format!("The {waiting} behind it were left where they are.")
    };
    Failure {
        message: failure.message.clone(),
        detail: format!("{} {rest}", failure.detail),
    }
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
            }
        })
        .collect();
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
