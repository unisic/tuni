//! The Files panel: the project's directory tree, beside the terminal.
//!
//! The rows come from [`tuni_core::files::Tree`], which is a flat list already,
//! so this is a `GtkListView` over a store rebuilt whenever that list changes —
//! and only when it changes, because the panel re-reads the disk on a timer and
//! most of those reads find nothing new.
//!
//! What the tree cannot do it hands off: the trash, the clipboard, and the
//! desktop's own file manager and default applications are all reached through
//! GIO, which routes them through the portal in a sandbox and through the
//! session bus outside one.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::gdk;
use gtk::gio;
use gtk::glib;

use tuni_core::files::{Disk, Failure, Item, Tree};

/// Pixels of indent per level of depth.
const INDENT: i32 = 12;

/// The two buttons under a thumb, which every file manager and every browser
/// spends on the history. GDK names the first three and numbers the rest, and
/// these are the numbers X11 gave them and Wayland kept.
pub(crate) const BUTTON_BACK: u32 = 8;
pub(crate) const BUTTON_FORWARD: u32 = 9;

/// One row of the list, wrapped so a `GListModel` can hold it.
pub(crate) mod row {
    use std::cell::{Cell, RefCell};

    use gtk::glib;
    use gtk::subclass::prelude::*;

    use tuni_core::files::Item;

    mod imp {
        use super::{Cell, Item, RefCell, glib};
        use gtk::subclass::prelude::*;

        #[derive(Default)]
        pub struct Row {
            pub item: RefCell<Option<Item>>,
            pub expanded: Cell<bool>,
        }

        #[glib::object_subclass]
        impl ObjectSubclass for Row {
            const NAME: &'static str = "TuniFileRow";
            type Type = super::Row;
        }

        impl ObjectImpl for Row {}
    }

    glib::wrapper! {
        pub struct Row(ObjectSubclass<imp::Row>);
    }

    impl Row {
        pub fn new(item: Item, expanded: bool) -> Self {
            let row: Self = glib::Object::new();
            row.imp().item.replace(Some(item));
            row.imp().expanded.set(expanded);
            row
        }

        pub fn item(&self) -> Option<Item> {
            self.imp().item.borrow().clone()
        }

        pub fn is_expanded(&self) -> bool {
            self.imp().expanded.get()
        }
    }
}

use row::Row;

mod imp {
    use super::{Item, Rc, RefCell, Tree, gio, glib};
    use adw::prelude::*;
    use adw::subclass::prelude::*;

    pub type Handler = Rc<dyn Fn(Message)>;

    /// What the panel cannot do itself, on its way to the window.
    pub enum Message {
        /// Type a `cd` into the terminal that has the keyboard.
        Cd(std::path::PathBuf),
        /// Open a file in a tab of its own.
        Open(std::path::PathBuf),
        /// Open a file beside the pane being worked in.
        OpenToSide(std::path::PathBuf),
    }

    #[derive(Default)]
    pub struct TuniFiles {
        pub tree: RefCell<Tree>,
        /// The directory the window last pointed the panel at. Browsing --
        /// stepping up, typing a path -- moves the tree without moving this,
        /// and the window saying the same root again is not a reason to snap
        /// back; a root that actually changed is.
        pub given: RefCell<std::path::PathBuf>,
        /// Where browsing has been, and where it was called back from. Two
        /// stacks rather than one list with a cursor, because that is what
        /// back and forward are: everything ahead is thrown away the moment a
        /// step is taken somewhere else.
        pub back: RefCell<Vec<std::path::PathBuf>>,
        pub forward: RefCell<Vec<std::path::PathBuf>>,
        pub rows: RefCell<Option<gio::ListStore>>,
        pub list: RefCell<Option<gtk::ListView>>,
        pub title: RefCell<Option<gtk::Label>>,
        pub subtitle: RefCell<Option<gtk::Label>>,
        pub location: RefCell<Option<gtk::Entry>>,
        pub location_bar: RefCell<Option<gtk::Revealer>>,
        pub menu: RefCell<Option<gtk::PopoverMenu>>,
        /// The row whose context menu is open, and so the one every action in
        /// it acts on.
        pub target: RefCell<Option<Item>>,
        pub message: RefCell<Option<Handler>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TuniFiles {
        const NAME: &'static str = "TuniFiles";
        type Type = super::TuniFiles;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for TuniFiles {
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

    impl WidgetImpl for TuniFiles {}
    impl BinImpl for TuniFiles {}
}

pub use imp::Message;

glib::wrapper! {
    pub struct TuniFiles(ObjectSubclass<imp::TuniFiles>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for TuniFiles {
    fn default() -> Self {
        Self::new()
    }
}

impl TuniFiles {
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

    // --- construction ------------------------------------------------------

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
            .action_name("files.up")
            .valign(gtk::Align::Center)
            .build();
        up.add_css_class("flat");

        let jump = gtk::Button::builder()
            .icon_name("folder-open-symbolic")
            .tooltip_text("Go to Directory")
            .action_name("files.location")
            .valign(gtk::Align::Center)
            .build();
        jump.add_css_class("flat");

        let add = gtk::MenuButton::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("New File or Folder")
            .menu_model(&root_menu())
            .valign(gtk::Align::Center)
            .build();
        add.add_css_class("flat");

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.set_margin_start(12);
        header.set_margin_end(6);
        header.set_margin_top(8);
        header.set_margin_bottom(8);
        header.append(&names);
        header.append(&up);
        header.append(&jump);
        header.append(&add);

        // A place to say where to go, shown when asked for and typed into the
        // way a shell would be: Enter goes, Escape thinks better of it.
        let location = gtk::Entry::builder()
            .placeholder_text("Directory path")
            .margin_start(12)
            .margin_end(6)
            .margin_bottom(8)
            .build();
        location.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |entry| this.go_to(&entry.text())
        ));
        // A path that led nowhere reddens the entry; typing again is the start
        // of a new answer, not more of the wrong one.
        location.connect_changed(|entry| entry.remove_css_class("error"));
        let escape = gtk::EventControllerKey::new();
        escape.connect_key_pressed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, _| {
                if key == gdk::Key::Escape {
                    this.show_location(false);
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        ));
        location.add_controller(escape);
        let location_bar = gtk::Revealer::builder().child(&location).build();

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
                if let Some(item) = row.and_then(|row| row.item()) {
                    this.activate(&item);
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

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header);
        content.append(&location_bar);
        content.append(&scroller);
        self.set_child(Some(&content));

        self.add_controller(history(self, "files"));
        self.install_actions();

        imp.rows.replace(Some(rows));
        imp.list.replace(Some(list));
        imp.title.replace(Some(title));
        imp.subtitle.replace(Some(subtitle));
        imp.location.replace(Some(location));
        imp.location_bar.replace(Some(location_bar));
        imp.menu.replace(Some(menu));
    }

    /// One row's widgets. Built once and handed back a row at a time, so
    /// everything that depends on which row it is happens in [`bind_row`] —
    /// except the gestures, which read the list item they were given when they
    /// fire rather than when they were attached.
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

        // One click on the chevron opens or closes a directory, which is
        // quicker than the double click the row itself asks for.
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

        list_item.set_child(Some(&content));
    }

    /// The context menu's actions. Every one of them acts on the row the menu
    /// was opened over, except the three the header's own menu uses, which act
    /// on the root.
    fn install_actions(&self) {
        let actions = gio::SimpleActionGroup::new();
        actions.add_action_entries([
            entry("open", self, |files| {
                if let Some(item) = files.target() {
                    files.activate(&item);
                }
            }),
            entry("open-to-side", self, |files| {
                if let Some(item) = files.target() {
                    files.send(Message::OpenToSide(item.path));
                }
            }),
            entry("reveal", self, |files| {
                if let Some(item) = files.target() {
                    files.reveal(&item.path);
                }
            }),
            entry("copy-path", self, |files| {
                if let Some(item) = files.target() {
                    files.clipboard().set_text(&item.path.to_string_lossy());
                }
            }),
            entry("cd", self, |files| {
                if let Some(item) = files.target() {
                    files.send(Message::Cd(item.path));
                }
            }),
            entry("new-file", self, |files| {
                if let Some(item) = files.target() {
                    files.create_in(&item.path, false);
                }
            }),
            entry("new-folder", self, |files| {
                if let Some(item) = files.target() {
                    files.create_in(&item.path, true);
                }
            }),
            entry("new-file-here", self, |files| {
                let root = files.imp().tree.borrow().root().to_path_buf();
                files.create_in(&root, false);
            }),
            entry("new-folder-here", self, |files| {
                let root = files.imp().tree.borrow().root().to_path_buf();
                files.create_in(&root, true);
            }),
            entry("reveal-here", self, |files| {
                let root = files.imp().tree.borrow().root().to_path_buf();
                files.reveal(&root);
            }),
            entry("up", self, |files| {
                let parent = files
                    .imp()
                    .tree
                    .borrow()
                    .root()
                    .parent()
                    .map(Path::to_path_buf);
                if let Some(parent) = parent {
                    files.browse(&parent);
                }
            }),
            entry("back", self, |files| files.walk(false)),
            entry("forward", self, |files| files.walk(true)),
            entry("location", self, |files| {
                let bar = files.imp().location_bar.borrow().clone();
                let open = bar.is_some_and(|bar| bar.reveals_child());
                files.show_location(!open);
            }),
            entry("rename", self, |files| {
                if let Some(item) = files.target() {
                    files.rename(&item);
                }
            }),
            entry("trash", self, |files| {
                if let Some(item) = files.target() {
                    files.trash(&item);
                }
            }),
        ]);
        self.insert_action_group("files", Some(&actions));
    }

    // --- what the tree says ------------------------------------------------

    /// Points the panel at a directory and draws it. Browsing away is
    /// respected until the window names a different directory: the focus
    /// moving between panes of one project says the same root over and over,
    /// and snapping back on every poll would make browsing impossible.
    pub fn sync(&self, root: &Path) {
        if self.imp().given.borrow().as_path() == root {
            return;
        }
        self.imp().given.replace(root.to_path_buf());
        // Another project's tree, so the way back through this one leads
        // somewhere the panel is no longer about.
        self.imp().back.borrow_mut().clear();
        self.imp().forward.borrow_mut().clear();
        self.show_location(false);
        let changed = self.imp().tree.borrow_mut().sync(root, &mut Disk);
        if changed {
            self.reload();
        }
        self.refresh_header();
    }

    /// Steps the tree somewhere of the user's own choosing, and remembers
    /// where it was standing.
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
    /// for. Nothing at all when that end of the history is empty, which is
    /// what a browser does with a greyed-out button.
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
        if self.imp().tree.borrow_mut().sync(directory, &mut Disk) {
            self.reload();
        }
        self.refresh_header();
        if let Some(location) = self.imp().location.borrow().as_ref() {
            location.set_text(&directory.to_string_lossy());
        }
    }

    /// The typed path, gone to if it leads anywhere. `~` is the shell's home,
    /// because a path is typed here the way it would be typed at a prompt.
    fn go_to(&self, text: &str) {
        let text = text.trim();
        let expanded = text
            .strip_prefix("~")
            .filter(|rest| rest.is_empty() || rest.starts_with('/'))
            .map(|rest| glib::home_dir().join(rest.trim_start_matches('/')))
            .unwrap_or_else(|| Path::new(text).to_path_buf());
        if !expanded.is_dir() {
            if let Some(location) = self.imp().location.borrow().as_ref() {
                location.add_css_class("error");
            }
            return;
        }
        self.browse(&expanded);
        self.show_location(false);
    }

    fn show_location(&self, show: bool) {
        let imp = self.imp();
        let (Some(bar), Some(location)) = (
            imp.location_bar.borrow().clone(),
            imp.location.borrow().clone(),
        ) else {
            return;
        };
        location.remove_css_class("error");
        bar.set_reveal_child(show);
        if show {
            location.set_text(&imp.tree.borrow().root().to_string_lossy());
            location.set_position(-1);
            location.grab_focus();
        }
    }

    /// Re-reads what is open. Draws nothing when nothing moved, which is the
    /// usual answer.
    pub fn poll(&self) {
        if self.imp().tree.borrow_mut().rebuild(&mut Disk) {
            self.reload();
        }
    }

    fn toggle(&self, path: &Path) {
        if self.imp().tree.borrow_mut().toggle(path, &mut Disk) {
            self.reload();
        }
    }

    /// A directory is stepped into; a file goes to the editor, which is the
    /// window's to place. Something the editor will not show — a picture, a
    /// binary — is still opened in a pane, and the pane is where the offer to
    /// hand it to the desktop lives.
    ///
    /// Stepping in rather than opening in place, because two clicks are what
    /// the parent button undoes and the chevron is already the one that opens a
    /// directory without leaving the one above it.
    fn activate(&self, item: &Item) {
        if item.is_directory {
            self.browse(&item.path);
            return;
        }
        self.send(Message::Open(item.path.clone()));
    }

    fn reload(&self) {
        let imp = self.imp();
        let Some(rows) = imp.rows.borrow().clone() else {
            return;
        };
        let tree = imp.tree.borrow();
        let built: Vec<Row> = tree
            .items()
            .iter()
            .map(|item| Row::new(item.clone(), tree.is_expanded(&item.path)))
            .collect();
        // Replaced in one go: a store emitting one signal per row makes the
        // list view rebind every widget below each insertion.
        rows.splice(0, rows.n_items(), &built);
    }

    fn refresh_header(&self) {
        let imp = self.imp();
        let tree = imp.tree.borrow();
        if let Some(title) = imp.title.borrow().as_ref() {
            title.set_text(tree.root_name());
        }
        if let Some(subtitle) = imp.subtitle.borrow().as_ref() {
            subtitle.set_text(&tree.root().to_string_lossy());
        }
    }

    // --- the menu ----------------------------------------------------------

    fn target(&self) -> Option<Item> {
        self.imp().target.borrow().clone()
    }

    fn popup_menu(&self, widget: &gtk::Box, item: &Item, position: u32, x: f64, y: f64) {
        let imp = self.imp();
        let (Some(menu), Some(list)) = (imp.menu.borrow().clone(), imp.list.borrow().clone())
        else {
            return;
        };
        // Selecting it first, so it is plain which row the menu belongs to.
        if let Some(selection) = list.model().and_downcast::<gtk::SingleSelection>() {
            selection.set_selected(position);
        }
        imp.target.replace(Some(item.clone()));
        menu.set_menu_model(Some(&row_menu(item)));

        let point = widget
            .compute_point(&list, &gtk::graphene::Point::new(x as f32, y as f32))
            .unwrap_or_else(|| gtk::graphene::Point::new(x as f32, y as f32));
        crate::menu::popup_at(&menu, point);
    }

    // --- acting on a file --------------------------------------------------

    /// Shows a file where it lives, in whatever the desktop uses for that.
    fn reveal(&self, path: &Path) {
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(path)));
        launcher.open_containing_folder(
            self.window().as_ref(),
            gio::Cancellable::NONE,
            |_result| (),
        );
    }

    fn rename(&self, item: &Item) {
        let path = item.path.clone();
        self.ask(
            "Rename",
            &format!("Rename “{}”", item.name),
            &item.name,
            "Rename",
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |name: String| {
                    match tuni_core::files::rename(&path, &name) {
                        Ok(Some(moved)) => {
                            this.imp().tree.borrow_mut().remap(&path, &moved, &mut Disk);
                            this.reload();
                        }
                        Ok(None) => {}
                        Err(failure) => this.report(&failure),
                    }
                }
            ),
        );
    }

    fn create_in(&self, directory: &Path, folder: bool) {
        let directory = directory.to_path_buf();
        let heading = if folder { "New Folder" } else { "New File" };
        self.ask(
            heading,
            &format!("Inside “{}”", short(&directory)),
            "",
            "Create",
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |name: String| {
                    match tuni_core::files::create(&directory, &name, folder) {
                        Ok(Some(_)) => {
                            // Opened, so the thing that was just made is
                            // visible rather than hidden in a closed folder.
                            this.imp().tree.borrow_mut().expand(&directory, &mut Disk);
                            this.reload();
                        }
                        Ok(None) => {}
                        Err(failure) => this.report(&failure),
                    }
                }
            ),
        );
    }

    /// Moves a file to the desktop's trash, which is recoverable — deleting
    /// outright is not something a file tree should offer.
    fn trash(&self, item: &Item) {
        let file = gio::File::for_path(&item.path);
        match file.trash(gio::Cancellable::NONE) {
            Ok(()) => {
                self.imp().tree.borrow_mut().forget(&item.path, &mut Disk);
                self.reload();
            }
            Err(error) => self.report(&Failure {
                message: format!("Couldn't move “{}” to the trash.", item.name),
                detail: error.to_string(),
            }),
        }
    }

    // --- dialogs -----------------------------------------------------------

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

    /// What went wrong, in the two lines the model wrote it in.
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

/// Draws one row: the indent, the chevron, the icon, and the name.
pub(crate) fn bind_row(object: &glib::Object) {
    let Some(list_item) = object.downcast_ref::<gtk::ListItem>() else {
        return;
    };
    let Some(row) = list_item.item().and_downcast::<Row>() else {
        return;
    };
    let Some(item) = row.item() else {
        return;
    };
    let Some(content) = list_item.child().and_downcast::<gtk::Box>() else {
        return;
    };
    let mut children = content.first_child();
    let mut widgets = Vec::new();
    while let Some(child) = children {
        children = child.next_sibling();
        widgets.push(child);
    }
    let [expander, icon, label] = widgets.as_slice() else {
        return;
    };

    content.set_margin_start(item.depth as i32 * INDENT);

    if let Some(expander) = expander.downcast_ref::<gtk::Image>() {
        // Emptied rather than hidden: a file has no chevron, but it keeps the
        // space one would take, so names at the same depth start in the same
        // column whether or not they can be opened.
        expander.set_icon_name(if item.is_directory {
            Some(if row.is_expanded() {
                "pan-down-symbolic"
            } else {
                "pan-end-symbolic"
            })
        } else {
            None
        });
    }
    if let Some(icon) = icon.downcast_ref::<gtk::Image>() {
        icon.set_icon_name(Some(if item.is_directory {
            "folder-symbolic"
        } else {
            "text-x-generic-symbolic"
        }));
    }
    if let Some(label) = label.downcast_ref::<gtk::Label>() {
        label.set_text(&item.name);
        label.set_tooltip_text(Some(&item.path.to_string_lossy()));
        // A name the shell hides is shown, but shown quietly.
        if item.is_hidden() {
            label.add_css_class("dim-label");
        } else {
            label.remove_css_class("dim-label");
        }
    }
}

/// One action, holding the panel weakly: the group is inserted into the panel,
/// so anything stronger than this would be a cycle the panel never leaves.
fn entry<F>(name: &str, files: &TuniFiles, activate: F) -> gio::ActionEntry<gio::SimpleActionGroup>
where
    F: Fn(&TuniFiles) + 'static,
{
    let weak = files.downgrade();
    gio::ActionEntry::builder(name)
        .activate(move |_: &gio::SimpleActionGroup, _, _| {
            if let Some(files) = weak.upgrade() {
                activate(&files);
            }
        })
        .build()
}

fn row_menu(item: &Item) -> gio::Menu {
    let menu = gio::Menu::new();

    let open = gio::Menu::new();
    if item.is_directory {
        open.append(Some("cd Here"), Some("files.cd"));
    } else {
        open.append(Some("Open"), Some("files.open"));
        open.append(Some("Open to the Side"), Some("files.open-to-side"));
    }
    open.append(Some("Show in Files"), Some("files.reveal"));
    open.append(Some("Copy Path"), Some("files.copy-path"));
    menu.append_section(None, &open);

    if item.is_directory {
        let create = gio::Menu::new();
        create.append(Some("New File…"), Some("files.new-file"));
        create.append(Some("New Folder…"), Some("files.new-folder"));
        menu.append_section(None, &create);
    }

    let edit = gio::Menu::new();
    edit.append(Some("Rename…"), Some("files.rename"));
    edit.append(Some("Move to Trash"), Some("files.trash"));
    menu.append_section(None, &edit);

    menu
}

/// The header's menu, which acts on the root rather than on a row.
fn root_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let create = gio::Menu::new();
    create.append(Some("New File…"), Some("files.new-file-here"));
    create.append(Some("New Folder…"), Some("files.new-folder-here"));
    menu.append_section(None, &create);
    let open = gio::Menu::new();
    open.append(Some("Show in Files"), Some("files.reveal-here"));
    menu.append_section(None, &open);
    menu
}

/// The gesture the two buttons under a thumb arrive on, walking the history of
/// whichever page it is added to. `group` is that page's action group, since
/// the remote browser has the same two actions under a name of its own.
///
/// Captured rather than bubbled: a press lands on a row before it lands on the
/// page, and a row that has already claimed the sequence would swallow it.
/// Nothing under here wants either button, so taking them early costs nothing.
pub(crate) fn history(widget: &impl IsA<gtk::Widget>, group: &'static str) -> gtk::GestureClick {
    let widget = widget.as_ref().clone();
    let gesture = gtk::GestureClick::new();
    // Every button: GDK has a constant for the first three and these are not
    // among them, so the button is read off the gesture instead.
    gesture.set_button(0);
    gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
    gesture.connect_pressed(glib::clone!(
        #[weak]
        widget,
        move |gesture, _, _, _| {
            let action = match gesture.current_button() {
                BUTTON_BACK => "back",
                BUTTON_FORWARD => "forward",
                _ => return,
            };
            gesture.set_state(gtk::EventSequenceState::Claimed);
            let _ = WidgetExt::activate_action(&widget, &format!("{group}.{action}"), None);
        }
    ));
    gesture
}

/// A directory as a dialog can afford to show it: the last two components,
/// which is enough to tell two `src` directories apart.
fn short(path: &Path) -> String {
    let mut parts: Vec<&str> = path
        .components()
        .rev()
        .take(2)
        .filter_map(|part| part.as_os_str().to_str())
        .collect();
    parts.reverse();
    if parts.is_empty() {
        return path.to_string_lossy().into_owned();
    }
    parts.join("/")
}
