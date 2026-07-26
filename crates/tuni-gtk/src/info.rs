//! The Info panel: what the shell in the focused pane is actually doing.
//!
//! The directory it is working in and the one the other two pages anchor to,
//! the processes running under it, and the TCP ports those processes are
//! listening on — which is the question a dev server makes people ask, and
//! which otherwise costs an `ss` and a `ps` in another window.
//!
//! The reading is [`tuni_core::info`], off the main loop because it walks
//! `/proc`, with a generation stamp so a read that lands after the focus moved
//! is dropped rather than drawn.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::gdk;
use gtk::gio;
use gtk::glib;

use tuni_core::info::{self, Port, Process, Snapshot};

/// How many rows a section draws. A build spawning a compiler per core makes a
/// long list, and past this many the count in the heading is the useful part.
const MAX_ROWS: usize = 200;

mod imp {
    use super::{Cell, PathBuf, RefCell, Snapshot, gio, glib};
    use adw::prelude::*;
    use adw::subclass::prelude::*;

    #[derive(Default)]
    pub struct TuniInfo {
        /// The shell the panel is looking at, and where it is working.
        pub shell_pid: Cell<u32>,
        pub cwd: RefCell<PathBuf>,
        /// What Files and Git are anchored to, and whether that was pinned by
        /// hand or worked out from the shell's directory.
        pub root: RefCell<PathBuf>,
        pub automatic: Cell<bool>,

        /// The last snapshot drawn, so a poll that finds nothing new redraws
        /// nothing — the panel polls every couple of seconds.
        pub snapshot: RefCell<Snapshot>,
        pub generation: Cell<u64>,
        pub loading: Cell<bool>,

        /// The row a context menu was opened over.
        pub menu_pid: Cell<u32>,
        pub menu_port: Cell<u16>,
        pub menu_executable: RefCell<String>,

        pub title: RefCell<Option<gtk::Label>>,
        pub subtitle: RefCell<Option<gtk::Label>>,
        pub cwd_group: RefCell<Option<super::Directory>>,
        pub root_group: RefCell<Option<super::Directory>>,
        pub processes: RefCell<Option<super::Section>>,
        pub ports: RefCell<Option<super::Section>>,
        pub menu: RefCell<Option<gtk::PopoverMenu>>,
        pub actions: RefCell<Option<gio::SimpleActionGroup>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TuniInfo {
        const NAME: &'static str = "TuniInfo";
        type Type = super::TuniInfo;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for TuniInfo {
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

    impl WidgetImpl for TuniInfo {}
    impl BinImpl for TuniInfo {}
}

/// A path with the two things anyone wants to do to one.
#[derive(Clone)]
pub struct Directory {
    container: gtk::Box,
    heading: gtk::Label,
    path: gtk::Label,
}

/// A heading with a count and a list under it.
#[derive(Clone)]
pub struct Section {
    container: gtk::Box,
    count: gtk::Label,
    list: gtk::ListBox,
    empty: gtk::Label,
    overflow: gtk::Label,
}

glib::wrapper! {
    pub struct TuniInfo(ObjectSubclass<imp::TuniInfo>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for TuniInfo {
    fn default() -> Self {
        Self::new()
    }
}

impl TuniInfo {
    #[must_use]
    pub fn new() -> Self {
        glib::Object::new()
    }

    // --- construction ------------------------------------------------------

    fn build(&self) {
        let imp = self.imp();

        let title = gtk::Label::builder().label("Session").xalign(0.0).build();
        title.add_css_class("heading");
        let subtitle = gtk::Label::builder().xalign(0.0).build();
        subtitle.add_css_class("caption");
        subtitle.add_css_class("dim-label");
        let heading = gtk::Box::new(gtk::Orientation::Vertical, 0);
        heading.set_hexpand(true);
        heading.append(&title);
        heading.append(&subtitle);

        let refresh = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Refresh")
            .action_name("info.refresh")
            .build();
        refresh.add_css_class("flat");

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.set_margin_start(12);
        header.set_margin_end(6);
        header.set_margin_top(6);
        header.set_margin_bottom(6);
        header.append(&heading);
        header.append(&refresh);

        let cwd_group = self.directory("Current directory", "info.open-cwd", "info.copy-cwd");
        let root_group = self.directory("Project directory", "info.open-root", "info.copy-root");
        root_group.container.set_tooltip_text(Some(
            "Files and Git anchor to this directory. Automatic means the \
             closest repository above the shell's own directory; a directory \
             pinned from the project menu is used as it stands.",
        ));

        let processes = self.section("Processes", "Nothing is running under this shell");
        let ports = self.section("Ports", "Nothing is listening");

        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_bottom(12);
        content.append(&cwd_group.container);
        content.append(&root_group.container);
        content.append(&processes.container);
        content.append(&ports.container);

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&content)
            .build();

        let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        outer.append(&header);
        outer.append(&scroller);
        self.set_child(Some(&outer));

        let menu = gtk::PopoverMenu::from_model(None::<&gio::Menu>);
        menu.set_has_arrow(false);
        menu.set_halign(gtk::Align::Start);
        menu.set_parent(self);

        self.install_actions();

        imp.title.replace(Some(title));
        imp.subtitle.replace(Some(subtitle));
        imp.cwd_group.replace(Some(cwd_group));
        imp.root_group.replace(Some(root_group));
        imp.processes.replace(Some(processes));
        imp.ports.replace(Some(ports));
        imp.menu.replace(Some(menu));
    }

    fn directory(&self, title: &str, open: &str, copy: &str) -> Directory {
        let heading = gtk::Label::builder().label(title).xalign(0.0).build();
        heading.add_css_class("heading");

        let path = gtk::Label::builder()
            .xalign(0.0)
            .selectable(true)
            .wrap(false)
            .ellipsize(gtk::pango::EllipsizeMode::Start)
            .build();
        path.add_css_class("monospace");
        path.add_css_class("caption");
        path.add_css_class("dim-label");

        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        buttons.set_homogeneous(true);
        let show = gtk::Button::builder()
            .label("Show in Files")
            .action_name(open)
            .build();
        show.add_css_class("flat");
        let copy = gtk::Button::builder()
            .label("Copy Path")
            .action_name(copy)
            .build();
        copy.add_css_class("flat");
        buttons.append(&show);
        buttons.append(&copy);

        let container = gtk::Box::new(gtk::Orientation::Vertical, 6);
        container.set_margin_start(12);
        container.set_margin_end(12);
        container.set_margin_top(6);
        container.append(&heading);
        container.append(&path);
        container.append(&buttons);

        Directory {
            container,
            heading,
            path,
        }
    }

    fn section(&self, title: &str, empty_text: &str) -> Section {
        let name = gtk::Label::builder().label(title).xalign(0.0).build();
        name.add_css_class("heading");
        let count = gtk::Label::builder().hexpand(true).xalign(0.0).build();
        count.add_css_class("caption");
        count.add_css_class("dim-label");

        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        heading.append(&name);
        heading.append(&count);

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.add_css_class("boxed-list");

        let empty = gtk::Label::builder().label(empty_text).xalign(0.0).build();
        empty.add_css_class("caption");
        empty.add_css_class("dim-label");

        let overflow = gtk::Label::builder().xalign(0.0).build();
        overflow.add_css_class("caption");
        overflow.add_css_class("dim-label");
        overflow.set_visible(false);

        let container = gtk::Box::new(gtk::Orientation::Vertical, 6);
        container.set_margin_start(12);
        container.set_margin_end(12);
        container.append(&heading);
        container.append(&list);
        container.append(&empty);
        container.append(&overflow);

        Section {
            container,
            count,
            list,
            empty,
            overflow,
        }
    }

    // --- what the panel is looking at --------------------------------------

    /// Points the panel at a pane: the shell running in it, where it is
    /// working, and what the other pages are anchored to.
    pub fn sync(&self, shell_pid: Option<u32>, cwd: &Path, root: &Path, automatic: bool) {
        let imp = self.imp();
        let pid = shell_pid.unwrap_or_default();
        let moved = imp.shell_pid.get() != pid
            || imp.cwd.borrow().as_path() != cwd
            || imp.root.borrow().as_path() != root
            || imp.automatic.get() != automatic;
        if !moved {
            return;
        }

        imp.shell_pid.set(pid);
        imp.cwd.replace(cwd.to_path_buf());
        imp.root.replace(root.to_path_buf());
        imp.automatic.set(automatic);
        // A read for the pane that was focused a moment ago is no longer an
        // answer to anything.
        imp.generation.set(imp.generation.get().wrapping_add(1));
        imp.snapshot.replace(Snapshot::default());
        self.draw_directories();
        self.reload();
    }

    /// The timer's re-read: nothing above says when a build finishes or a
    /// server binds a port.
    pub fn poll(&self) {
        self.reload();
    }

    fn reload(&self) {
        let imp = self.imp();
        let pid = imp.shell_pid.get();
        if pid == 0 {
            self.draw(&Snapshot::default());
            return;
        }
        if imp.loading.get() {
            return;
        }

        let generation = imp.generation.get();
        imp.loading.set(true);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let snapshot = gio::spawn_blocking(move || info::snapshot(pid)).await;
                this.imp().loading.set(false);
                if this.imp().generation.get() != generation {
                    return;
                }
                if let Ok(snapshot) = snapshot {
                    if *this.imp().snapshot.borrow() == snapshot {
                        return;
                    }
                    this.imp().snapshot.replace(snapshot.clone());
                    this.draw(&snapshot);
                }
            }
        ));
    }

    // --- drawing -----------------------------------------------------------

    fn draw_directories(&self) {
        let imp = self.imp();
        let cwd = imp.cwd.borrow().clone();
        let root = imp.root.borrow().clone();

        if let Some(group) = imp.cwd_group.borrow().as_ref() {
            // It earns a row once it differs from what the panels anchor to;
            // saying the same path twice tells nobody anything.
            group
                .container
                .set_visible(!cwd.as_os_str().is_empty() && cwd != root);
            group.path.set_text(&cwd.to_string_lossy());
            group.path.set_tooltip_text(Some(&cwd.to_string_lossy()));
        }
        if let Some(group) = imp.root_group.borrow().as_ref() {
            group.container.set_visible(!root.as_os_str().is_empty());
            group.heading.set_text(if imp.automatic.get() {
                "Project directory (automatic)"
            } else {
                "Project directory (pinned)"
            });
            group.path.set_text(&root.to_string_lossy());
            group.path.set_tooltip_text(Some(&root.to_string_lossy()));
        }
    }

    fn draw(&self, snapshot: &Snapshot) {
        let imp = self.imp();

        if let Some(title) = imp.title.borrow().as_ref() {
            title.set_text(if snapshot.shell.is_empty() {
                "Session"
            } else {
                &snapshot.shell
            });
        }
        if let Some(subtitle) = imp.subtitle.borrow().as_ref() {
            let pid = imp.shell_pid.get();
            subtitle.set_text(&if pid > 0 {
                format!("pid {pid}")
            } else {
                "no shell".to_owned()
            });
        }

        if let Some(section) = imp.processes.borrow().as_ref() {
            self.fill(section, snapshot.processes.len(), |list| {
                for process in snapshot.processes.iter().take(MAX_ROWS) {
                    list.append(&self.process_row(process));
                }
            });
        }
        if let Some(section) = imp.ports.borrow().as_ref() {
            self.fill(section, snapshot.ports.len(), |list| {
                for port in snapshot.ports.iter().take(MAX_ROWS) {
                    list.append(&self.port_row(port));
                }
            });
        }
    }

    fn fill(&self, section: &Section, count: usize, rows: impl FnOnce(&gtk::ListBox)) {
        while let Some(child) = section.list.first_child() {
            section.list.remove(&child);
        }
        section.count.set_text(&count.to_string());
        section.list.set_visible(count > 0);
        section.empty.set_visible(count == 0);
        rows(&section.list);

        let hidden = count.saturating_sub(MAX_ROWS);
        section.overflow.set_visible(hidden > 0);
        if hidden > 0 {
            section.overflow.set_text(&format!("and {hidden} more"));
        }
    }

    fn process_row(&self, process: &Process) -> gtk::ListBoxRow {
        let dot = gtk::Image::from_icon_name("media-record-symbolic");
        dot.set_pixel_size(8);
        dot.add_css_class("success");

        let name = gtk::Label::builder()
            .label(&process.name)
            .xalign(0.0)
            .build();
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        if !process.executable.is_empty() {
            name.set_tooltip_text(Some(&process.executable));
        }

        let pid = gtk::Label::builder()
            .label(process.pid.to_string())
            .xalign(0.0)
            .hexpand(true)
            .build();
        pid.add_css_class("monospace");
        pid.add_css_class("caption");
        pid.add_css_class("dim-label");

        let usage = gtk::Label::new(Some(&usage_text(process)));
        usage.add_css_class("caption");
        usage.add_css_class("dim-label");

        let terminate = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Terminate")
            .build();
        terminate.add_css_class("flat");
        let pid_number = process.pid;
        terminate.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.terminate(pid_number, false)
        ));

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        content.set_margin_start(12);
        content.set_margin_end(6);
        content.set_margin_top(4);
        content.set_margin_bottom(4);
        content.append(&dot);
        content.append(&name);
        content.append(&pid);
        content.append(&usage);
        content.append(&terminate);

        let row = gtk::ListBoxRow::builder().child(&content).build();
        self.attach_menu(&row, process.pid, 0, &process.executable);
        row
    }

    fn port_row(&self, port: &Port) -> gtk::ListBoxRow {
        let icon = gtk::Image::from_icon_name("network-transmit-receive-symbolic");
        icon.set_pixel_size(12);

        let number = gtk::Label::builder()
            .label(port.port.to_string())
            .xalign(0.0)
            .build();
        number.add_css_class("monospace");

        let process = gtk::Label::builder()
            .label(&port.process)
            .xalign(0.0)
            .hexpand(true)
            .build();
        process.add_css_class("caption");
        process.add_css_class("dim-label");
        process.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let open = gtk::Button::builder()
            .icon_name("external-link-symbolic")
            .tooltip_text(format!("Open {}", port.url()))
            .build();
        open.add_css_class("flat");
        let url = port.url();
        open.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.open_url(&url)
        ));

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        content.set_margin_start(12);
        content.set_margin_end(6);
        content.set_margin_top(4);
        content.set_margin_bottom(4);
        content.append(&icon);
        content.append(&number);
        content.append(&process);
        content.append(&open);

        // The whole row opens it, not only the button on the end: the port is
        // the link, and kero's row is a button for the same reason.
        let row = gtk::ListBoxRow::builder()
            .child(&content)
            .activatable(true)
            .build();
        row.set_tooltip_text(Some(&port.url()));
        let target = port.url();
        row.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.open_url(&target)
        ));
        self.attach_menu(&row, port.pid, port.port, "");
        row
    }

    /// A right click anywhere on a row opens the menu for it. Which row that
    /// was is remembered rather than passed, because the actions the menu
    /// names are the group's and fire long after the click.
    fn attach_menu(&self, row: &gtk::ListBoxRow, pid: u32, port: u16, executable: &str) {
        let press = gtk::GestureClick::new();
        press.set_button(gdk::BUTTON_SECONDARY);
        let executable = executable.to_owned();
        press.connect_pressed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            row,
            move |_, _, x, y| {
                this.popup_menu(&row, pid, port, &executable, x, y);
            }
        ));
        row.add_controller(press);
    }

    fn popup_menu(
        &self,
        over: &gtk::ListBoxRow,
        pid: u32,
        port: u16,
        executable: &str,
        x: f64,
        y: f64,
    ) {
        let imp = self.imp();
        imp.menu_pid.set(pid);
        imp.menu_port.set(port);
        imp.menu_executable.replace(executable.to_owned());

        let Some(menu) = imp.menu.borrow().clone() else {
            return;
        };
        menu.set_menu_model(Some(&if port > 0 {
            port_menu(port)
        } else {
            process_menu()
        }));

        // The menu hangs off the panel, and the click arrived in the row's own
        // coordinates.
        let point = gtk::graphene::Point::new(x as f32, y as f32);
        let point = over.compute_point(self, &point).unwrap_or(point);
        let point = gdk::Rectangle::new(point.x() as i32, point.y() as i32, 1, 1);
        menu.set_pointing_to(Some(&point));
        menu.popup();
    }

    // --- what the rows can be told to do -----------------------------------

    fn install_actions(&self) {
        let actions = gio::SimpleActionGroup::new();
        actions.add_action_entries([
            entry("refresh", self, |this| {
                // A refresh asked for by hand is not the timer's poll: it has
                // to run even if the last read found nothing new.
                this.imp().snapshot.replace(Snapshot::default());
                this.reload();
            }),
            entry("open-cwd", self, |this| {
                let path = this.imp().cwd.borrow().clone();
                this.show_in_files(&path);
            }),
            entry("copy-cwd", self, |this| {
                let path = this.imp().cwd.borrow().clone();
                this.copy(&path.to_string_lossy());
            }),
            entry("open-root", self, |this| {
                let path = this.imp().root.borrow().clone();
                this.show_in_files(&path);
            }),
            entry("copy-root", self, |this| {
                let path = this.imp().root.borrow().clone();
                this.copy(&path.to_string_lossy());
            }),
            entry("terminate", self, |this| {
                this.terminate(this.imp().menu_pid.get(), false);
            }),
            entry("kill", self, |this| {
                this.terminate(this.imp().menu_pid.get(), true);
            }),
            entry("copy-pid", self, |this| {
                this.copy(&this.imp().menu_pid.get().to_string());
            }),
            entry("copy-executable", self, |this| {
                let executable = this.imp().menu_executable.borrow().clone();
                this.copy(&executable);
            }),
            entry("open-port", self, |this| {
                let port = this.imp().menu_port.get();
                this.open_url(&format!("http://localhost:{port}"));
            }),
            entry("copy-url", self, |this| {
                let port = this.imp().menu_port.get();
                this.copy(&format!("http://localhost:{port}"));
            }),
        ]);
        self.insert_action_group("info", Some(&actions));
        self.imp().actions.replace(Some(actions));
    }

    /// Signals the process, then reads again shortly after, so the row leaves
    /// once it is actually gone rather than at the next tick of the timer.
    fn terminate(&self, pid: u32, force: bool) {
        if pid == 0 {
            return;
        }
        info::terminate(pid, force);
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(300),
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move || this.reload()
            ),
        );
    }

    fn show_in_files(&self, path: &Path) {
        if path.as_os_str().is_empty() {
            return;
        }
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(path)));
        launcher.open_containing_folder(self.window().as_ref(), gio::Cancellable::NONE, |_| ());
    }

    fn open_url(&self, url: &str) {
        let launcher = gtk::UriLauncher::new(url);
        launcher.launch(self.window().as_ref(), gio::Cancellable::NONE, |_| ());
    }

    fn copy(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.clipboard().set_text(text);
    }

    fn window(&self) -> Option<gtk::Window> {
        self.root().and_downcast::<gtk::Window>()
    }
}

fn entry<F: Fn(&TuniInfo) + 'static>(
    name: &str,
    info: &TuniInfo,
    activate: F,
) -> gio::ActionEntry<gio::SimpleActionGroup> {
    gio::ActionEntry::builder(name)
        .activate(glib::clone!(
            #[weak]
            info,
            move |_: &gio::SimpleActionGroup, _, _| activate(&info)
        ))
        .build()
}

fn process_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let signals = gio::Menu::new();
    signals.append(Some("Terminate"), Some("info.terminate"));
    signals.append(Some("Force Kill"), Some("info.kill"));
    menu.append_section(None, &signals);
    let copy = gio::Menu::new();
    copy.append(Some("Copy PID"), Some("info.copy-pid"));
    copy.append(Some("Copy Executable Path"), Some("info.copy-executable"));
    menu.append_section(None, &copy);
    menu
}

fn port_menu(port: u16) -> gio::Menu {
    let menu = gio::Menu::new();
    let open = gio::Menu::new();
    open.append(
        Some(&format!("Open localhost:{port}")),
        Some("info.open-port"),
    );
    open.append(Some("Copy URL"), Some("info.copy-url"));
    menu.append_section(None, &open);
    let signals = gio::Menu::new();
    signals.append(Some("Terminate the Process"), Some("info.terminate"));
    signals.append(Some("Force Kill"), Some("info.kill"));
    menu.append_section(None, &signals);
    menu
}

/// What `ps` would say, shortened: percent of a processor and resident memory.
fn usage_text(process: &Process) -> String {
    let memory = if process.memory_kb >= 1024 {
        format!("{:.0} MB", process.memory_kb as f64 / 1024.0)
    } else {
        format!("{} KB", process.memory_kb)
    };
    format!("{:.0}% · {memory}", process.cpu)
}
