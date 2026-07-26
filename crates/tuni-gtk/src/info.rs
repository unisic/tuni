//! The Info panel: what the shell in the focused pane is actually doing.
//!
//! The directory it is working in and the one the other two pages anchor to,
//! the processes running under it, and the TCP ports those processes are
//! listening on — which is the question a dev server makes people ask, and
//! which otherwise costs an `ss` and a `ps` in another window.
//!
//! A pane on another machine gets two more headings, because the first question
//! about a connection is whether it is still there and the second is which
//! ports it is carrying.
//!
//! The reading is [`tuni_core::info`], off the main loop because it walks
//! `/proc`, with a generation stamp so a read that lands after the focus moved
//! is dropped rather than drawn.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::gdk;
use gtk::gio;
use gtk::glib;

use tuni_core::info::{self, Port, Process, Snapshot};
use tuni_core::ssh::{self, Direction, Forward, Notes, Source};
use tuni_core::usage::{self, Agent};

/// How many rows a section draws. A build spawning a compiler per core makes a
/// long list, and past this many the count in the heading is the useful part.
const MAX_ROWS: usize = 200;

/// How rarely the panel asks whether a connection is still answering.
///
/// Every ask is a subprocess, and the two-second poll the rest of this page
/// runs on is the wrong rate for a question whose answer changes when a laptop
/// closes. Nothing waits on it: an operation that fails is a liveness check
/// with better provenance than any timer.
const CHECK_INTERVAL: Duration = Duration::from_secs(30);

mod imp {
    use super::{
        Cell, HashMap, Instant, Link, PathBuf, RefCell, Snapshot, Tunnels, gio, glib, usage,
    };
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

        /// The host the pane is connected to, when it is a connection rather
        /// than a shell of its own, and what asking about it found.
        pub host: RefCell<Option<String>>,
        pub link: RefCell<Link>,
        /// When the connection was last asked about, which is what keeps the
        /// asking down to a heartbeat.
        pub checked: Cell<Option<Instant>>,

        /// The forwarded ports on that host, and which of them are answering.
        pub forwards: RefCell<Tunnels>,
        /// The remote forwards tuni opened this run, by the spec they were
        /// asked for and the port they were given. Nothing on this machine can
        /// see a listener at the far end, so what tuni was told is all there is
        /// to go on, and the port matters because a remote forward may ask the
        /// far end to pick one.
        pub opened: RefCell<HashMap<String, u16>>,
        /// Whether a forward is being opened or closed, which is what stops two
        /// clicks turning into two requests.
        pub busy: Cell<bool>,

        /// The last snapshot drawn, so a poll that finds nothing new redraws
        /// nothing — the panel polls every couple of seconds.
        pub snapshot: RefCell<Snapshot>,
        pub generation: Cell<u64>,
        pub loading: Cell<bool>,

        /// What the coding agent in the pane has spent, and the reader that
        /// works it out. The reader holds how far into each of the agent's logs
        /// it has read, so it travels to the worker thread and back rather than
        /// starting again every poll.
        pub usage: RefCell<usage::Snapshot>,
        pub reader: RefCell<usage::Reader>,

        /// The row a context menu was opened over.
        pub menu_pid: Cell<u32>,
        pub menu_port: Cell<u16>,
        pub menu_executable: RefCell<String>,
        /// Which of tuni's own forwards the row menu belongs to.
        pub menu_tunnel: Cell<usize>,

        pub title: RefCell<Option<gtk::Label>>,
        pub subtitle: RefCell<Option<gtk::Label>>,
        pub connection: RefCell<Option<super::Connection>>,
        pub tunnels: RefCell<Option<super::Section>>,
        pub problem: RefCell<Option<gtk::Label>>,
        pub cwd_group: RefCell<Option<super::Directory>>,
        pub root_group: RefCell<Option<super::Directory>>,
        pub agent: RefCell<Option<super::AgentSection>>,
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

/// What asking about a pane's connection found: where it actually goes, and
/// how long the shared connection carrying it has been up.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Link {
    /// `deploy@10.0.0.1:2222`, as `ssh` resolves the alias rather than as the
    /// block for it happens to be written.
    address: String,
    /// The host this one is reached through, empty for one reached directly.
    jump: String,
    /// Seconds the master has been running, or nothing when none is: a
    /// connection nobody is sharing has no age to report.
    uptime: Option<u64>,
}

/// The forwarded ports on a pane's host.
///
/// Two lists rather than one, because they are two different things. A forward
/// the host's own block declares is brought up by `ssh` with the connection and
/// there is nothing here to start or stop. One tuni is keeping is opened and
/// closed against a connection that is already running, which is what the
/// switch on its row does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tunnels {
    declared: Vec<Forward>,
    own: Vec<Forward>,
    /// Which of the ports above something on this machine is listening on. Only
    /// the ones that listen here: a remote forward listens at the far end.
    listening: HashSet<u16>,
}

/// The host a pane is on, with a dot for whether anything is still answering.
#[derive(Clone)]
pub struct Connection {
    container: gtk::Box,
    name: gtk::Label,
    dot: gtk::Image,
    state: gtk::Label,
    address: gtk::Label,
    jump: gtk::Label,
}

/// A path with the two things anyone wants to do to one.
#[derive(Clone)]
pub struct Directory {
    container: gtk::Box,
    heading: gtk::Label,
    path: gtk::Label,
}

/// What the coding agent running under the shell has spent: a heading naming
/// it and the model it is on, and a row per reading.
#[derive(Clone)]
pub struct AgentSection {
    container: gtk::Box,
    model: gtk::Label,
    list: gtk::ListBox,
}

/// A heading with a count and a list under it.
#[derive(Clone)]
pub struct Section {
    container: gtk::Box,
    /// The heading's own row, for a section with a button in it.
    heading: gtk::Box,
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

        let connection = self.connection_section();
        let cwd_group = self.directory("Current directory", "info.open-cwd", "info.copy-cwd");
        let root_group = self.directory("Project directory", "info.open-root", "info.copy-root");
        root_group.container.set_tooltip_text(Some(
            "Files and Git anchor to this directory. Automatic means the \
             closest repository above the shell's own directory; a directory \
             pinned from the project menu is used as it stands.",
        ));

        let tunnels = self.tunnel_section();
        let agent = self.agent_section();
        let processes = self.section("Processes", "Nothing is running under this shell");
        let ports = self.section("Ports", "Nothing is listening");

        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_bottom(12);
        content.append(&connection.container);
        content.append(&tunnels.container);
        content.append(&cwd_group.container);
        content.append(&root_group.container);
        content.append(&agent.container);
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
        imp.connection.replace(Some(connection));
        imp.tunnels.replace(Some(tunnels));
        imp.cwd_group.replace(Some(cwd_group));
        imp.root_group.replace(Some(root_group));
        imp.agent.replace(Some(agent));
        imp.processes.replace(Some(processes));
        imp.ports.replace(Some(ports));
        imp.menu.replace(Some(menu));
    }

    fn connection_section(&self) -> Connection {
        let name = gtk::Label::builder().xalign(0.0).build();
        name.add_css_class("heading");
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);

        // Emptied rather than hidden when nothing is answering, so the heading
        // does not shift sideways every time a link drops.
        let dot = gtk::Image::from_icon_name("media-record-symbolic");
        dot.set_pixel_size(8);
        dot.add_css_class("success");
        let state = gtk::Label::builder().hexpand(true).xalign(0.0).build();
        state.add_css_class("caption");
        state.add_css_class("dim-label");

        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        heading.append(&name);
        heading.append(&dot);
        heading.append(&state);

        let address = gtk::Label::builder()
            .xalign(0.0)
            .selectable(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        address.add_css_class("monospace");
        address.add_css_class("caption");
        address.add_css_class("dim-label");

        let jump = gtk::Label::builder().xalign(0.0).build();
        jump.add_css_class("caption");
        jump.add_css_class("dim-label");
        jump.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let container = gtk::Box::new(gtk::Orientation::Vertical, 6);
        container.set_margin_start(12);
        container.set_margin_end(12);
        container.set_margin_top(6);
        container.set_visible(false);
        container.set_tooltip_text(Some(
            "Tuni shares one authenticated connection per host, so a second \
             pane costs no second login. The dot is whether that shared \
             connection is still answering, which is not the same question as \
             whether this pane's own shell is alive.",
        ));
        container.append(&heading);
        container.append(&address);
        container.append(&jump);

        Connection {
            container,
            name,
            dot,
            state,
            address,
            jump,
        }
    }

    /// The ports section for the machine at the other end, which is the Ports
    /// section's question asked of a connection rather than of a shell.
    fn tunnel_section(&self) -> Section {
        let section = self.section("Tunnels", "No forwarded ports");

        let add = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("Forward a port")
            .action_name("info.add-tunnel")
            .valign(gtk::Align::Center)
            .build();
        add.add_css_class("flat");
        section.heading.append(&add);

        // Where a refused port lands. The message names the process holding it,
        // and a dialog for something the next click can fix would be in the way.
        let problem = gtk::Label::builder().xalign(0.0).wrap(true).build();
        problem.add_css_class("caption");
        problem.add_css_class("error");
        problem.set_visible(false);
        section.container.append(&problem);

        section.container.set_visible(false);
        section.container.set_tooltip_text(Some(
            "A forward the host's own configuration declares comes up with the \
             connection and stays up. One added here is tuni's: it is kept with \
             the host and opened against the connection already running, and \
             Make Permanent moves it into the host's block.",
        ));

        self.imp().problem.replace(Some(problem));
        section
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

        // An icon and a word each, with the whole sentence in the tooltip: two
        // spelled-out labels are wider than the panel is meant to be.
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        buttons.set_homogeneous(true);
        let show = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("folder-symbolic")
                    .label("Files")
                    .build(),
            )
            .tooltip_text("Show in Files")
            .action_name(open)
            .build();
        show.add_css_class("flat");
        let copy = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("edit-copy-symbolic")
                    .label("Copy")
                    .build(),
            )
            .tooltip_text("Copy Path")
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

    fn agent_section(&self) -> AgentSection {
        let name = gtk::Label::builder().label("Agent").xalign(0.0).build();
        name.add_css_class("heading");
        let model = gtk::Label::builder().hexpand(true).xalign(0.0).build();
        model.add_css_class("caption");
        model.add_css_class("dim-label");
        model.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        heading.append(&name);
        heading.append(&model);

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.add_css_class("boxed-list");

        let container = gtk::Box::new(gtk::Orientation::Vertical, 6);
        container.set_margin_start(12);
        container.set_margin_end(12);
        container.set_visible(false);
        container.set_tooltip_text(Some(
            "Read from the agent's own session logs, except the plan bars: \
             Codex writes its own into the log, and Claude Code's are asked \
             of the account's usage page with the login the agent already \
             keeps. Nothing here signs in anywhere.",
        ));
        container.append(&heading);
        container.append(&list);

        AgentSection {
            container,
            model,
            list,
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

        let empty = gtk::Label::builder()
            .label(empty_text)
            .xalign(0.0)
            .wrap(true)
            .build();
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
            heading,
            count,
            list,
            empty,
            overflow,
        }
    }

    // --- what the panel is looking at --------------------------------------

    /// Points the panel at a pane: the shell running in it, where it is
    /// working, what the other pages are anchored to, and the host it is on
    /// when it is on one.
    pub fn sync(
        &self,
        shell_pid: Option<u32>,
        cwd: &Path,
        root: &Path,
        automatic: bool,
        host: Option<&str>,
    ) {
        let imp = self.imp();
        let pid = shell_pid.unwrap_or_default();
        let switched = imp.host.borrow().as_deref() != host;
        let moved = switched
            || imp.shell_pid.get() != pid
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
        imp.usage.replace(usage::Snapshot::default());
        self.draw_agent(&usage::Snapshot::default());
        self.draw_directories();
        self.reload();
        // Only when the pane changed host. Asking again costs `ssh -G`, which
        // runs whatever the user's `Match exec` blocks run, and a `cd` is not a
        // reason to run somebody's script.
        if switched {
            imp.host.replace(host.map(str::to_owned));
            imp.link.replace(Link::default());
            imp.forwards.replace(Tunnels::default());
            imp.opened.borrow_mut().clear();
            // A port that would not open on the host before this one is not
            // news about this one.
            if let Some(problem) = imp.problem.borrow().as_ref() {
                problem.set_visible(false);
            }
            self.draw_connection();
            self.draw_tunnels();
            self.reload_connection(true);
        }
    }

    /// The timer's re-read: nothing above says when a build finishes or a
    /// server binds a port.
    pub fn poll(&self) {
        self.reload();

        if self.imp().host.borrow().is_none() {
            return;
        }
        // A window on another workspace is not being read, and a connection
        // does not change because time passed.
        if !self.window().is_some_and(|window| window.is_active()) {
            return;
        }
        let due = self
            .imp()
            .checked
            .get()
            .is_none_or(|at| at.elapsed() >= CHECK_INTERVAL);
        if due {
            self.reload_connection(false);
        }
    }

    fn reload(&self) {
        let imp = self.imp();
        let pid = imp.shell_pid.get();
        if pid == 0 {
            imp.usage.replace(usage::Snapshot::default());
            self.draw_agent(&usage::Snapshot::default());
            self.draw(&Snapshot::default());
            return;
        }
        if imp.loading.get() {
            return;
        }

        let generation = imp.generation.get();
        let cwd = imp.cwd.borrow().clone();
        let reader = imp.reader.take();
        imp.loading.set(true);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let read = gio::spawn_blocking(move || {
                    let snapshot = info::snapshot(pid);
                    // Whether an agent is running is a question the process
                    // list has just been walked for anyway.
                    let mut reader = reader;
                    let usage = Agent::running(&snapshot.processes)
                        .map(|agent| reader.read(agent, &cwd))
                        .unwrap_or_default();
                    (snapshot, usage, reader)
                })
                .await;
                let imp = this.imp();
                imp.loading.set(false);
                let Ok((snapshot, usage, reader)) = read else {
                    return;
                };
                // Back it goes whatever happens next, or the next poll reads
                // every log from the top again.
                imp.reader.replace(reader);
                if imp.generation.get() != generation {
                    return;
                }
                if *imp.usage.borrow() != usage {
                    imp.usage.replace(usage.clone());
                    this.draw_agent(&usage);
                }
                if *imp.snapshot.borrow() == snapshot {
                    return;
                }
                imp.snapshot.replace(snapshot.clone());
                this.draw(&snapshot);
            }
        ));
    }

    /// Asks about the pane's connection: where the alias goes, and whether a
    /// shared connection to it is still answering.
    ///
    /// `resolved` says whether the address has to be worked out again. It costs
    /// an `ssh -G`, which applies `Match`, `Include` and canonicalisation and
    /// therefore runs the user's own `Match exec` commands, so it happens when
    /// the pane changes host and when somebody asks for a refresh. Never on the
    /// timer, which only asks who is answering.
    fn reload_connection(&self, resolved: bool) {
        let imp = self.imp();
        let Some(host) = imp.host.borrow().clone() else {
            return;
        };
        let known = (!resolved).then(|| imp.link.borrow().clone());
        let generation = imp.generation.get();
        imp.checked.set(Some(Instant::now()));
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let read = gio::spawn_blocking(move || {
                    let mut link = known.unwrap_or_else(|| resolve(&host));
                    // The master is not a child of this process, so how long it
                    // has been up is a question for `/proc` rather than for
                    // `ssh`, which reports only that it is there.
                    link.uptime = crate::hosts::control()
                        .check(&host)
                        .and_then(tuni_core::info::age);
                    (link, tunnels(&host))
                })
                .await;
                let imp = this.imp();
                let Ok((link, forwards)) = read else { return };
                if imp.generation.get() != generation {
                    return;
                }
                let reconnected = *imp.link.borrow() != link;
                if reconnected {
                    imp.link.replace(link);
                    this.draw_connection();
                }
                let moved = *imp.forwards.borrow() != forwards;
                if moved {
                    imp.forwards.replace(forwards);
                }
                // A master that came or went changes the rows too: nothing can
                // be opened or closed on a connection that is not there.
                if reconnected || moved {
                    this.draw_tunnels();
                }
            }
        ));
    }

    // --- drawing -----------------------------------------------------------

    fn draw_connection(&self) {
        let Some(section) = self.imp().connection.borrow().clone() else {
            return;
        };
        let Some(host) = self.imp().host.borrow().clone() else {
            section.container.set_visible(false);
            return;
        };
        let link = self.imp().link.borrow().clone();

        section.container.set_visible(true);
        section.name.set_text(&host);
        section.address.set_text(&link.address);
        section.address.set_tooltip_text(Some(&link.address));
        match link.uptime {
            Some(seconds) => {
                section.dot.set_icon_name(Some("media-record-symbolic"));
                section
                    .state
                    .set_text(&format!("connected for {}", uptime_label(seconds)));
            }
            None => {
                section.dot.set_icon_name(None);
                // What was actually measured, and not "disconnected": a pane
                // whose shell is perfectly alive has no master behind it when
                // the configuration turns sharing off.
                section.state.set_text("no shared connection");
            }
        }
        section.jump.set_visible(!link.jump.is_empty());
        section.jump.set_text(&format!("through {}", link.jump));
    }

    fn draw_tunnels(&self) {
        let Some(section) = self.imp().tunnels.borrow().clone() else {
            return;
        };
        if self.imp().host.borrow().is_none() {
            section.container.set_visible(false);
            return;
        }
        section.container.set_visible(true);

        let forwards = self.imp().forwards.borrow().clone();
        let master = self.imp().link.borrow().uptime.is_some();
        let count = forwards.declared.len() + forwards.own.len();
        self.fill(&section, count, |list| {
            for forward in &forwards.declared {
                list.append(&self.tunnel_row(forward, None, self.is_open(forward), master));
            }
            for (index, forward) in forwards.own.iter().enumerate() {
                list.append(&self.tunnel_row(forward, Some(index), self.is_open(forward), master));
            }
        });
    }

    /// Whether a forward is carrying anything.
    ///
    /// Read from the listening sockets on this machine, which is the honest
    /// answer for the two directions that listen here. A remote forward listens
    /// at the far end, so the answer for one is what the master said when it was
    /// asked to open it, and nothing more.
    fn is_open(&self, forward: &Forward) -> bool {
        if forward.direction == Direction::Remote {
            return self.imp().opened.borrow().contains_key(&forward.spec());
        }
        self.imp()
            .forwards
            .borrow()
            .listening
            .contains(&forward.listen_port)
    }

    fn tunnel_row(
        &self,
        forward: &Forward,
        own: Option<usize>,
        open: bool,
        master: bool,
    ) -> gtk::ListBoxRow {
        let dot = gtk::Image::from_icon_name("media-record-symbolic");
        dot.set_pixel_size(8);
        dot.add_css_class("success");
        if !open {
            dot.set_icon_name(None);
        }

        let (what, written) = crate::forward_editor::describe(forward);
        let name = gtk::Label::builder()
            .label(forward.title())
            .xalign(0.0)
            .hexpand(true)
            .build();
        name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        if forward.label.is_empty() {
            name.add_css_class("monospace");
        }
        // Under it rather than beside it: the panel is a column, and a spec and
        // a sentence and a switch do not fit across one of those.
        let detail = gtk::Label::builder().label(&what).xalign(0.0).build();
        detail.add_css_class("caption");
        detail.add_css_class("dim-label");
        detail.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        line.append(&dot);
        line.append(&name);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
        content.set_margin_start(12);
        content.set_margin_end(6);
        content.set_margin_top(4);
        content.set_margin_bottom(4);
        content.append(&line);
        content.append(&detail);

        match own {
            Some(index) => {
                let switch = gtk::Switch::builder()
                    .valign(gtk::Align::Center)
                    .active(open)
                    .state(open)
                    .sensitive(master && !self.imp().busy.get())
                    .build();
                if !master {
                    switch.set_tooltip_text(Some(
                        "There is no shared connection to open a port on. \
                         Connect to the host first.",
                    ));
                }
                // Connected after the state is set, or drawing a row would ask
                // ssh to do again whatever it has already done.
                let forward = forward.clone();
                switch.connect_state_set(glib::clone!(
                    #[weak(rename_to = this)]
                    self,
                    #[upgrade_or]
                    glib::Propagation::Proceed,
                    move |_, wanted| {
                        this.set_tunnel(&forward, wanted);
                        // The switch follows what ssh answered rather than what
                        // the click asked for, so the row is redrawn from the
                        // result instead of moving here.
                        glib::Propagation::Stop
                    }
                ));

                let more = gtk::MenuButton::builder()
                    .icon_name("view-more-symbolic")
                    .menu_model(&tunnel_menu())
                    .valign(gtk::Align::Center)
                    .build();
                more.add_css_class("flat");
                // Which row it belongs to is remembered rather than passed: the
                // actions the menu names are the group's and fire later.
                more.set_create_popup_func(glib::clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |_: &gtk::MenuButton| this.imp().menu_tunnel.set(index)
                ));

                line.append(&switch);
                line.append(&more);
            }
            None => {
                let held = gtk::Label::new(Some("always on"));
                held.add_css_class("caption");
                held.add_css_class("dim-label");
                line.append(&held);
            }
        }

        let row = gtk::ListBoxRow::builder().child(&content).build();
        row.set_tooltip_text(Some(&match own {
            Some(_) => written,
            None => {
                format!("{written}\nDeclared by the host, so ssh brings it up with the connection")
            }
        }));
        row
    }

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

    /// Draws what the agent has spent, and hides the section when there is no
    /// agent in the pane, which is most panes most of the time.
    fn draw_agent(&self, usage: &usage::Snapshot) {
        let Some(section) = self.imp().agent.borrow().clone() else {
            return;
        };
        let Some(agent) = usage.agent.filter(|_| !usage.is_empty()) else {
            section.container.set_visible(false);
            return;
        };

        section.container.set_visible(true);
        section.model.set_text(&match usage.model.as_deref() {
            Some(model) => format!("{} · {model}", agent.label()),
            None => agent.label().to_owned(),
        });

        while let Some(child) = section.list.first_child() {
            section.list.remove(&child);
        }
        section.list.append(&self.agent_row(
            "This session",
            &tokens_label(usage.session.total()),
            Some(&breakdown(usage.session)),
            None,
        ));
        if let Some(context) = usage.context {
            let row = self.agent_row(
                "Context",
                &format!("{:.0}%", context.percent()),
                Some(&format!(
                    "{} of {}",
                    tokens_label(context.used),
                    tokens_label(context.total)
                )),
                Some(context.percent() / 100.0),
            );
            row.set_tooltip_text(Some(
                "How much of the model's window the last turn carried.",
            ));
            section.list.append(&row);
        }
        for limit in &usage.limits {
            section.list.append(&self.agent_row(
                &format!("{} limit", limit.window),
                &format!("{:.0}%", limit.used_percent),
                limit.resets_at.and_then(resets_label).as_deref(),
                Some(limit.used_percent / 100.0),
            ));
        }
    }

    /// A reading: what it is on the left, the number on the right, and under
    /// them the detail and, for the readings that are a share of something, a
    /// bar.
    fn agent_row(
        &self,
        title: &str,
        value: &str,
        detail: Option<&str>,
        fraction: Option<f64>,
    ) -> gtk::ListBoxRow {
        let name = gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .hexpand(true)
            .build();
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let amount = gtk::Label::builder().label(value).xalign(1.0).build();
        amount.add_css_class("monospace");
        amount.add_css_class("numeric");

        let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        line.append(&name);
        line.append(&amount);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.set_margin_top(6);
        content.set_margin_bottom(6);
        content.append(&line);

        if let Some(fraction) = fraction {
            let bar = gtk::ProgressBar::new();
            bar.set_fraction(fraction.clamp(0.0, 1.0));
            content.append(&bar);
        }
        if let Some(detail) = detail {
            let caption = gtk::Label::builder().label(detail).xalign(0.0).build();
            caption.add_css_class("caption");
            caption.add_css_class("dim-label");
            caption.set_ellipsize(gtk::pango::EllipsizeMode::End);
            content.append(&caption);
        }

        gtk::ListBoxRow::builder().child(&content).build()
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
        crate::menu::popup_at(&menu, point);
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
                this.reload_connection(true);
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
            entry("add-tunnel", self, |this| {
                if this.imp().host.borrow().is_none() {
                    return;
                }
                crate::forward_editor::present(
                    this,
                    None,
                    glib::clone!(
                        #[weak]
                        this,
                        move |forward| this.keep_tunnel(forward)
                    ),
                );
            }),
            entry("pin-tunnel", self, |this| this.pin_tunnel()),
            entry("remove-tunnel", self, |this| this.remove_tunnel()),
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

    // --- forwarded ports ---------------------------------------------------

    /// Asks the master to open or close a forward.
    ///
    /// A port that something else is already listening on is caught before ssh
    /// is asked, because the mux client's own answer for that is `Error: remote
    /// port forwarding failed` with neither the port nor the process in it.
    fn set_tunnel(&self, forward: &Forward, open: bool) {
        let imp = self.imp();
        let Some(host) = imp.host.borrow().clone() else {
            return;
        };
        if imp.busy.get() {
            return;
        }
        imp.busy.set(true);

        // Closing one goes back as the port that ended up listening, which is
        // not what was asked for when the far end picked it: the master matches
        // a cancel against what it recorded.
        let mut request = forward.clone();
        if !open
            && request.listen_port == 0
            && let Some(port) = imp.opened.borrow().get(&forward.spec())
        {
            request.listen_port = *port;
        }

        let forward = forward.clone();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let done = gio::spawn_blocking(move || {
                    let control = crate::hosts::control();
                    if !open {
                        return control.cancel(&host, &request).map(|()| 0);
                    }
                    ssh::check_port(&request)?;
                    control.add(&host, &request)
                })
                .await;
                let imp = this.imp();
                imp.busy.set(false);
                let outcome = done.unwrap_or_else(|_| Err("The request did not finish".to_owned()));
                match outcome {
                    Ok(port) if open => {
                        imp.opened.borrow_mut().insert(forward.spec(), port);
                    }
                    Ok(_) => {
                        imp.opened.borrow_mut().remove(&forward.spec());
                    }
                    Err(_) => {}
                }
                this.report_tunnel(outcome.map(|_| ()));
            }
        ));
    }

    /// Files a forward with the host, and opens it when there is a connection
    /// to open it on. Kept either way: a host nobody is connected to yet is
    /// still a host with a forward waiting on it.
    fn keep_tunnel(&self, forward: Forward) {
        let Some(host) = self.imp().host.borrow().clone() else {
            return;
        };
        let live = self.imp().link.borrow().uptime.is_some();
        let kept = forward.clone();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let written = gio::spawn_blocking(move || remember(&host, &kept))
                    .await
                    .unwrap_or_else(|_| Err("The write did not finish".to_owned()));
                if written.is_ok() && live {
                    this.set_tunnel(&forward, true);
                    return;
                }
                this.report_tunnel(written);
            }
        ));
    }

    fn pin_tunnel(&self) {
        let Some(forward) = self.tunnel() else {
            return;
        };
        let Some(host) = self.imp().host.borrow().clone() else {
            return;
        };
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let moved = gio::spawn_blocking(move || pin(&host, &forward))
                    .await
                    .unwrap_or_else(|_| Err("The write did not finish".to_owned()));
                this.report_tunnel(moved);
            }
        ));
    }

    /// Takes a forward off the host, closing it first when it is carrying
    /// something. A forward left open on a master nothing lists any more is a
    /// port nobody can find again.
    fn remove_tunnel(&self) {
        let Some(forward) = self.tunnel() else {
            return;
        };
        let Some(host) = self.imp().host.borrow().clone() else {
            return;
        };
        let open = self.is_open(&forward);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let spec = forward.spec();
                let removed = gio::spawn_blocking(move || {
                    let closed = if open {
                        crate::hosts::control().cancel(&host, &forward)
                    } else {
                        Ok(())
                    };
                    forget(&host, &forward)?;
                    closed
                })
                .await
                .unwrap_or_else(|_| Err("The write did not finish".to_owned()));
                this.imp().opened.borrow_mut().remove(&spec);
                this.report_tunnel(removed);
            }
        ));
    }

    /// The forward a row menu was opened over.
    fn tunnel(&self) -> Option<Forward> {
        let index = self.imp().menu_tunnel.get();
        self.imp().forwards.borrow().own.get(index).cloned()
    }

    fn report_tunnel(&self, outcome: Result<(), String>) {
        if let Some(problem) = self.imp().problem.borrow().as_ref() {
            problem.set_visible(outcome.is_err());
            if let Err(message) = &outcome {
                problem.set_label(message);
            }
        }
        // What is listening now is a question for the reader either way: a
        // request that failed may still have changed something, and one that
        // worked says nothing about the rest of the list.
        self.reload_connection(false);
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

fn tunnel_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    menu.append(Some("Make Permanent"), Some("info.pin-tunnel"));
    menu.append(Some("Remove"), Some("info.remove-tunnel"));
    menu
}

/// What is forwarded on a host, and which of it is answering.
///
/// The configuration, the host's notes and `/proc`, so it belongs off the main
/// thread with everything else here.
fn tunnels(destination: &str) -> Tunnels {
    let declared = ssh::host(destination).forwards;
    let own = Notes::load().get(destination).forwards;
    let watched: HashSet<u16> = declared
        .iter()
        .chain(own.iter())
        .filter(|forward| forward.direction != Direction::Remote && forward.listen_port != 0)
        .map(|forward| forward.listen_port)
        .collect();
    // One pair of file reads answers for the whole list, and a host with
    // nothing forwarded on it costs not even that.
    let listening = if watched.is_empty() {
        HashSet::new()
    } else {
        let mut ports = info::listening_ports();
        ports.retain(|port| watched.contains(port));
        ports
    };
    Tunnels {
        declared,
        own,
        listening,
    }
}

fn remember(destination: &str, forward: &Forward) -> Result<(), String> {
    let mut notes = Notes::load();
    let mut meta = notes.get(destination);
    if !meta.forwards.contains(forward) {
        meta.forwards.push(forward.clone());
    }
    notes.set(destination, meta);
    notes.save().map_err(|error| error.to_string())
}

fn forget(destination: &str, forward: &Forward) -> Result<(), String> {
    let mut notes = Notes::load();
    let mut meta = notes.get(destination);
    meta.forwards.retain(|kept| kept != forward);
    notes.set(destination, meta);
    notes.save().map_err(|error| error.to_string())
}

/// Moves a forward out of the host's notes and into its block, where `ssh`
/// brings it up with the connection instead of tuni asking for it.
///
/// Only into a block tuni owns. A host declared in `~/.ssh/config` is left
/// alone here for the reason it is left alone in the editor: that file is the
/// user's, and it is often under version control or config management.
fn pin(destination: &str, forward: &Forward) -> Result<(), String> {
    let mut hosts = ssh::saved();
    let Some(host) = hosts.iter_mut().find(|host| host.alias == destination) else {
        return Err(format!(
            "{destination} is declared outside the file tuni keeps, so tuni does not \
             write lines into it. Saving the host from the editor makes a copy tuni owns."
        ));
    };
    if !host.forwards.contains(forward) {
        host.forwards.push(forward.clone());
    }
    ssh::save(&hosts)?;
    forget(destination, forward)
}

/// Where a destination actually goes, once `ssh` has applied everything it
/// applies. Off the main thread: it reads the configuration and forks.
///
/// An address somebody typed is left alone. It carries its own user and port
/// already, and its port travels as `-p` rather than in the name, so asking
/// `ssh` about it would get an answer with the port missing from it.
fn resolve(destination: &str) -> Link {
    let host = ssh::host(destination);
    let host = if host.source == Source::Adhoc {
        host
    } else {
        ssh::resolve(&host.target()).unwrap_or(host)
    };
    Link {
        address: host.address(),
        jump: host.proxy_jump,
        uptime: None,
    }
}

/// How long a connection has been up, in the largest unit that still says
/// something. A connection made in the last minute is new, and the number of
/// seconds is not the interesting part of that.
fn uptime_label(seconds: u64) -> String {
    let (count, unit) = if seconds >= 86_400 {
        (seconds / 86_400, "day")
    } else if seconds >= 3_600 {
        (seconds / 3_600, "hour")
    } else if seconds >= 60 {
        (seconds / 60, "minute")
    } else {
        return "less than a minute".to_owned();
    };
    format!("{count} {unit}{}", if count == 1 { "" } else { "s" })
}

/// A token count short enough for a row. The exact figure is out of date a
/// second after it is drawn, so thousands and millions are as far as it goes.
fn tokens_label(count: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let value = count as f64;
    if count >= 1_000_000 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", value / 1_000.0)
    } else {
        count.to_string()
    }
}

/// Where a total went. Cache reads are most of a long conversation and cost a
/// fraction of what fresh input costs, so counting them as input would make the
/// number look several times worse than it is.
fn breakdown(tokens: usage::Tokens) -> String {
    format!(
        "in {} · out {} · cache {}",
        tokens_label(tokens.input),
        tokens_label(tokens.output),
        tokens_label(tokens.cache_read + tokens.cache_write)
    )
}

/// How long a plan window has left, in the largest unit that still says
/// something. Nothing once the moment is past: the agent reports the window it
/// is actually in, and a poll or two later it will say so.
fn resets_label(at: i64) -> Option<String> {
    let left = at - glib::real_time() / 1_000_000;
    if left <= 0 {
        return None;
    }
    let (count, unit) = if left >= 2 * 86_400 {
        (left / 86_400, "days")
    } else if left >= 2 * 3_600 {
        (left / 3_600, "hours")
    } else {
        ((left / 60).max(1), "minutes")
    };
    Some(format!("resets in {count} {unit}"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_uptime_is_read_in_whichever_unit_it_is_worth_reading_in() {
        assert_eq!(uptime_label(30), "less than a minute");
        assert_eq!(uptime_label(90), "1 minute");
        assert_eq!(uptime_label(7_200), "2 hours");
        assert_eq!(uptime_label(200_000), "2 days");
    }
}
