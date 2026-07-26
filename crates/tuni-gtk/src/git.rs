//! The Git panel: what the repository beside the terminal is holding.
//!
//! Every read and every action is a `git` process, and a process is slow
//! enough that running one on the main loop would stop the terminal from
//! drawing. So the work goes to GIO's blocking pool and comes back as a
//! future on the main context, and each result carries the generation it was
//! asked for — a status that finishes after the shell has moved to another
//! repository is thrown away rather than drawn.
//!
//! What the panel decides — whether a commit is possible, which paths a
//! discard has to touch — is decided in [`tuni_core::git`], where it is
//! tested. This file draws the answer and runs what it is handed.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::gio;
use gtk::glib;

use tuni_core::git::{self, Entry, Load, Status, Task};

/// How many rows a section draws before it stops. A repository mid-rebase can
/// report thousands of changed files, and a list that long is neither useful
/// nor cheap; the count in the section header still tells the truth.
const MAX_ROWS: usize = 200;

mod imp {
    use super::{Cell, PathBuf, Rc, RefCell, Status, gio, glib};
    use adw::subclass::prelude::*;

    /// Told how many files have changed, so whatever holds the panel can put
    /// the number where it is visible without the panel showing.
    pub type Handler = Rc<dyn Fn(usize)>;
    /// The window's, called with the file a row names.
    pub type Opener = Rc<dyn Fn(std::path::PathBuf)>;

    #[derive(Default)]
    pub struct TuniGit {
        /// The directory to look at — the shell's, or the project's when one
        /// is pinned. The repository root is resolved from it.
        pub directory: RefCell<PathBuf>,
        /// The last status that was drawn, kept so a poll that finds nothing
        /// new redraws nothing.
        pub status: RefCell<Option<Status>>,
        /// Bumped whenever the directory moves or an action runs, so a read
        /// that was already in flight cannot overwrite what replaced it.
        pub generation: Cell<u64>,
        /// A read is in flight.
        pub loading: Cell<bool>,
        /// An action the user asked for is running. Only one at a time: two
        /// git processes writing the same index is how a repository ends up
        /// with a stale lock.
        pub busy: Cell<bool>,

        pub stack: RefCell<Option<gtk::Stack>>,
        pub banner: RefCell<Option<adw::Banner>>,
        pub branch: RefCell<Option<gtk::MenuButton>>,
        pub distance: RefCell<Option<gtk::Label>>,
        pub message: RefCell<Option<gtk::TextView>>,
        pub amend: RefCell<Option<gtk::CheckButton>>,
        pub commit: RefCell<Option<gtk::Button>>,
        pub sections: RefCell<Vec<super::Section>>,
        pub history: RefCell<Option<gtk::ListBox>>,
        pub stash_pop: RefCell<Option<gtk::Button>>,
        pub placeholder: RefCell<Option<adw::StatusPage>>,
        pub actions: RefCell<Option<gio::SimpleActionGroup>>,
        pub changed: RefCell<Option<Handler>>,
        pub open: RefCell<Option<Opener>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TuniGit {
        const NAME: &'static str = "TuniGit";
        type Type = super::TuniGit;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for TuniGit {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build();
        }
    }

    impl WidgetImpl for TuniGit {}
    impl BinImpl for TuniGit {}
}

/// One list of files with a heading of its own.
#[derive(Clone)]
pub struct Section {
    kind: Kind,
    container: gtk::Box,
    count: gtk::Label,
    list: gtk::ListBox,
    overflow: gtk::Label,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Kind {
    Conflicts,
    Staged,
    Changed,
}

glib::wrapper! {
    pub struct TuniGit(ObjectSubclass<imp::TuniGit>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for TuniGit {
    fn default() -> Self {
        Self::new()
    }
}

impl TuniGit {
    #[must_use]
    pub fn new() -> Self {
        glib::Object::new()
    }

    // --- construction ------------------------------------------------------

    fn build(&self) {
        let imp = self.imp();

        let branch = gtk::MenuButton::builder()
            .label("no repository")
            .tooltip_text("Branch")
            .build();
        branch.add_css_class("flat");
        let distance = gtk::Label::builder()
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .hexpand(true)
            .xalign(0.0)
            .build();
        distance.add_css_class("caption");
        distance.add_css_class("dim-label");

        let refresh = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Refresh")
            .action_name("git.refresh")
            .build();
        refresh.add_css_class("flat");

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.set_margin_start(6);
        header.set_margin_end(6);
        header.set_margin_top(6);
        header.append(&branch);
        header.append(&distance);
        header.append(&refresh);

        let banner = adw::Banner::builder().button_label("Dismiss").build();
        banner.connect_button_clicked(|banner| banner.set_revealed(false));

        // --- the commit box

        let message = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(6)
            .bottom_margin(6)
            .left_margin(6)
            .right_margin(6)
            .accessible_role(gtk::AccessibleRole::TextBox)
            .build();
        message.update_property(&[gtk::accessible::Property::Label("Commit message")]);
        let message_frame = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(64)
            .max_content_height(160)
            .child(&message)
            .build();
        message_frame.add_css_class("card");

        let amend = gtk::CheckButton::with_label("Amend the last commit");
        amend.add_css_class("caption");
        let commit = gtk::Button::builder()
            .label("Commit")
            .action_name("git.commit")
            .hexpand(true)
            .build();
        commit.add_css_class("suggested-action");
        let commit_all = gtk::Button::builder()
            .label("Stage all and commit")
            .action_name("git.commit-all")
            .build();

        let commit_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        commit_row.append(&commit);
        commit_row.append(&commit_all);

        let commit_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        commit_box.set_margin_start(6);
        commit_box.set_margin_end(6);
        commit_box.set_margin_top(6);
        commit_box.append(&message_frame);
        commit_box.append(&amend);
        commit_box.append(&commit_row);

        // The button is what says whether a commit is possible, so it follows
        // the message rather than waiting for a click to complain.
        message.buffer().connect_changed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.refresh_commit_button()
        ));

        let sections = vec![
            self.section(Kind::Conflicts, "Conflicts", &[]),
            self.section(
                Kind::Staged,
                "Staged",
                &[("Unstage all", "git.unstage-all")],
            ),
            self.section(
                Kind::Changed,
                "Changes",
                &[
                    ("Stage all", "git.stage-all"),
                    ("Discard all", "git.discard-all"),
                ],
            ),
        ];

        let history = gtk::ListBox::new();
        history.set_selection_mode(gtk::SelectionMode::None);
        history.add_css_class("boxed-list");
        let history_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        history_box.set_margin_start(6);
        history_box.set_margin_end(6);
        history_box.set_margin_top(12);
        history_box.set_margin_bottom(6);
        let history_title = gtk::Label::builder().label("History").xalign(0.0).build();
        history_title.add_css_class("heading");
        history_box.append(&history_title);
        history_box.append(&history);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.append(&commit_box);
        for section in &sections {
            content.append(&section.container);
        }
        content.append(&history_box);

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&content)
            .build();

        // --- what the repository can be told to do

        let stash = gtk::Button::builder()
            .label("Stash")
            .tooltip_text("Stash every change, untracked files included")
            .action_name("git.stash")
            .build();
        let stash_pop = gtk::Button::builder()
            .label("Pop")
            .tooltip_text("Restore the newest stash")
            .action_name("git.stash-pop")
            .build();
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        toolbar.set_margin_start(6);
        toolbar.set_margin_end(6);
        toolbar.set_margin_top(6);
        toolbar.set_margin_bottom(6);
        toolbar.set_homogeneous(true);
        for (label, action, tooltip) in [
            ("Fetch", "git.fetch", "Fetch every remote"),
            ("Pull", "git.pull", "Pull, fast-forward only"),
            ("Push", "git.push", "Push the current branch"),
        ] {
            let button = gtk::Button::builder()
                .label(label)
                .tooltip_text(tooltip)
                .action_name(action)
                .build();
            toolbar.append(&button);
        }
        toolbar.append(&stash);
        toolbar.append(&stash_pop);

        let repository = gtk::Box::new(gtk::Orientation::Vertical, 0);
        repository.append(&banner);
        repository.append(&scroller);
        repository.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        repository.append(&toolbar);

        // A directory that is not a repository is not a failure, so it gets an
        // offer rather than an error.
        let placeholder = adw::StatusPage::builder()
            .icon_name("folder-symbolic")
            .title("Not a repository")
            .build();
        let start = gtk::Button::builder()
            .label("Start a Repository Here")
            .action_name("git.init")
            .halign(gtk::Align::Center)
            .build();
        start.add_css_class("pill");
        start.add_css_class("suggested-action");
        placeholder.set_child(Some(&start));

        let stack = gtk::Stack::new();
        stack.add_named(&repository, Some("repository"));
        stack.add_named(&placeholder, Some("placeholder"));
        stack.set_visible_child_name("placeholder");

        let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        outer.append(&header);
        outer.append(&stack);
        self.set_child(Some(&outer));

        self.install_actions();

        imp.stack.replace(Some(stack));
        imp.banner.replace(Some(banner));
        imp.branch.replace(Some(branch));
        imp.distance.replace(Some(distance));
        imp.message.replace(Some(message));
        imp.amend.replace(Some(amend));
        imp.commit.replace(Some(commit));
        imp.sections.replace(sections);
        imp.history.replace(Some(history));
        imp.stash_pop.replace(Some(stash_pop));
        imp.placeholder.replace(Some(placeholder));

        self.refresh_commit_button();
    }

    fn section(&self, kind: Kind, title: &str, actions: &[(&str, &str)]) -> Section {
        let name = gtk::Label::builder().label(title).xalign(0.0).build();
        name.add_css_class("heading");
        let count = gtk::Label::builder().hexpand(true).xalign(0.0).build();
        count.add_css_class("caption");
        count.add_css_class("dim-label");

        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        heading.append(&name);
        heading.append(&count);
        for (label, action) in actions {
            let button = gtk::Button::builder()
                .label(*label)
                .action_name(*action)
                .build();
            button.add_css_class("flat");
            button.add_css_class("caption");
            heading.append(&button);
        }

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.add_css_class("boxed-list");

        let overflow = gtk::Label::builder().xalign(0.0).visible(false).build();
        overflow.add_css_class("caption");
        overflow.add_css_class("dim-label");

        let container = gtk::Box::new(gtk::Orientation::Vertical, 6);
        container.set_margin_start(6);
        container.set_margin_end(6);
        container.set_margin_top(12);
        container.set_visible(false);
        container.append(&heading);
        container.append(&list);
        container.append(&overflow);

        Section {
            kind,
            container,
            count,
            list,
            overflow,
        }
    }

    fn install_actions(&self) {
        let actions = gio::SimpleActionGroup::new();
        actions.add_action_entries([
            entry("refresh", self, |git| git.reload(true)),
            entry("commit", self, |git| git.commit(false)),
            entry("commit-all", self, |git| git.commit(true)),
            entry("stage-all", self, |git| git.run(git::stage_all())),
            entry("unstage-all", self, |git| {
                let has_head = git.status().is_none_or(|status| status.has_head);
                git.run(git::unstage_all(has_head));
            }),
            entry("discard-all", self, TuniGit::discard_all),
            entry("fetch", self, |git| git.attempt(git::fetch)),
            entry("pull", self, |git| git.attempt(git::pull)),
            entry("push", self, |git| git.attempt(git::push)),
            entry("stash", self, |git| {
                git.attempt(|status| git::stash_push(status, true));
            }),
            entry("stash-pop", self, |git| git.attempt(git::stash_pop)),
            entry("init", self, |git| git.run(git::init())),
            entry("new-branch", self, TuniGit::create_branch),
        ]);

        // Switching branches is one action carrying the name, so the menu can
        // be built from whatever the repository happens to have.
        let switch = gio::ActionEntry::builder("switch")
            .parameter_type(Some(&String::static_variant_type()))
            .activate(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_: &gio::SimpleActionGroup, _, parameter| {
                    let Some(name) = parameter.and_then(glib::Variant::str) else {
                        return;
                    };
                    this.run(git::switch_branch(name));
                }
            ))
            .build();
        actions.add_action_entries([switch]);

        self.insert_action_group("git", Some(&actions));
        self.imp().actions.replace(Some(actions));
    }

    // --- what the repository says ------------------------------------------

    fn status(&self) -> Option<Status> {
        self.imp().status.borrow().clone()
    }

    /// Called with the number of changed files whenever that number could
    /// have moved.
    pub fn connect_changed<F: Fn(usize) + 'static>(&self, callback: F) {
        self.imp().changed.replace(Some(Rc::new(callback)));
    }

    /// Called with the file a row names when the row is opened. Where it is
    /// shown is the window's decision, not the panel's.
    pub fn connect_open<F: Fn(std::path::PathBuf) + 'static>(&self, callback: F) {
        self.imp().open.replace(Some(Rc::new(callback)));
    }

    fn announce(&self, count: usize) {
        let callback = self.imp().changed.borrow().clone();
        if let Some(callback) = callback {
            callback(count);
        }
    }

    /// Points the panel at a directory. Reads it again from scratch when it
    /// has moved, which is also when the branch list and the history are
    /// worth re-reading.
    pub fn sync(&self, directory: &Path) {
        let imp = self.imp();
        if imp.directory.borrow().as_path() == directory {
            return;
        }
        imp.directory.replace(directory.to_path_buf());
        imp.status.replace(None);
        self.reload(true);
    }

    /// The timer's read: the working tree changes as the shell beside it runs,
    /// but the branch list and the history do not change on that timescale.
    pub fn poll(&self) {
        self.reload(false);
    }

    fn reload(&self, details: bool) {
        let imp = self.imp();
        let directory = imp.directory.borrow().clone();
        if directory.as_os_str().is_empty() || imp.loading.get() {
            return;
        }

        let generation = imp.generation.get();
        imp.loading.set(true);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let load = gio::spawn_blocking(move || git::load(&directory, details)).await;
                this.imp().loading.set(false);
                // The shell moved, or an action ran, while this was in flight.
                if this.imp().generation.get() != generation {
                    return;
                }
                match load {
                    Ok(load) => this.apply(load),
                    Err(_) => this.report("Read the repository", "The git process failed."),
                }
            }
        ));
    }

    fn apply(&self, load: Load) {
        let imp = self.imp();
        match load {
            Load::NotRepository => {
                imp.status.replace(None);
                self.show_placeholder("Not a repository", None);
                self.announce(0);
            }
            Load::Failed(message) => {
                imp.status.replace(None);
                self.show_placeholder("Can't read the repository", Some(&message));
                self.announce(0);
            }
            Load::Repository(status) => {
                let mut status = *status;
                if let Some(previous) = imp.status.borrow().as_ref() {
                    status.keep_details_from(previous);
                    if *previous == status {
                        return;
                    }
                }
                imp.status.replace(Some(status.clone()));
                self.draw(&status);
                self.announce(status.change_count());
            }
        }
    }

    fn show_placeholder(&self, title: &str, detail: Option<&str>) {
        let imp = self.imp();
        if let Some(placeholder) = imp.placeholder.borrow().as_ref() {
            placeholder.set_title(title);
            placeholder.set_description(detail);
            // Offering to start a repository on top of metadata that could not
            // be read would make a bad situation worse.
            placeholder.set_icon_name(Some(if detail.is_some() {
                "dialog-warning-symbolic"
            } else {
                "folder-symbolic"
            }));
            if let Some(child) = placeholder.child() {
                child.set_visible(detail.is_none());
            }
        }
        if let Some(stack) = imp.stack.borrow().as_ref() {
            stack.set_visible_child_name("placeholder");
        }
        if let Some(branch) = imp.branch.borrow().as_ref() {
            branch.set_label("no repository");
            branch.set_menu_model(None::<&gio::Menu>);
        }
        if let Some(distance) = imp.distance.borrow().as_ref() {
            distance.set_text("");
        }
        self.refresh_commit_button();
    }

    fn draw(&self, status: &Status) {
        let imp = self.imp();
        if let Some(stack) = imp.stack.borrow().as_ref() {
            stack.set_visible_child_name("repository");
        }

        if let Some(branch) = imp.branch.borrow().as_ref() {
            branch.set_label(status.branch_name());
            branch.set_menu_model(Some(&branch_menu(status)));
        }
        if let Some(distance) = imp.distance.borrow().as_ref() {
            distance.set_text(&distance_text(status));
        }
        if let Some(banner) = imp.banner.borrow().as_ref()
            && let Some(operation) = status.operation.as_deref()
        {
            banner.set_title(operation);
            banner.set_revealed(true);
        }
        if let Some(button) = imp.stash_pop.borrow().as_ref() {
            button.set_sensitive(status.stashes > 0);
            button.set_label(&if status.stashes > 0 {
                format!("Pop ({})", status.stashes)
            } else {
                "Pop".to_owned()
            });
        }

        for section in imp.sections.borrow().iter() {
            let entries = match section.kind {
                Kind::Conflicts => &status.conflicts,
                Kind::Staged => &status.staged,
                Kind::Changed => &status.changed,
            };
            self.fill(section, entries, status);
        }

        if let Some(history) = imp.history.borrow().as_ref() {
            clear(history);
            for commit in &status.commits {
                history.append(&commit_row(commit));
            }
            history.set_visible(!status.commits.is_empty());
        }

        self.refresh_commit_button();
    }

    fn fill(&self, section: &Section, entries: &[Entry], status: &Status) {
        section.container.set_visible(!entries.is_empty());
        section.count.set_text(&count_text(entries.len()));
        clear(&section.list);
        for entry in entries.iter().take(MAX_ROWS) {
            section.list.append(&self.row(section.kind, entry, status));
        }
        let hidden = entries.len().saturating_sub(MAX_ROWS);
        section.overflow.set_visible(hidden > 0);
        if hidden > 0 {
            section
                .overflow
                .set_text(&format!("… and {hidden} more, not shown"));
        }
    }

    fn row(&self, kind: Kind, entry: &Entry, status: &Status) -> gtk::ListBoxRow {
        // The letters git itself uses, spelled out in the tooltip: the state
        // has to be readable without telling the colors apart.
        let letters = gtk::Label::new(Some(&format!("{}{}", entry.staged, entry.unstaged)));
        letters.add_css_class("monospace");
        letters.add_css_class("dim-label");
        letters.set_tooltip_text(Some(&entry.summary()));
        letters.set_width_chars(2);

        let name = gtk::Label::builder()
            .label(entry.file_name())
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .xalign(0.0)
            .build();
        let directory = gtk::Label::builder()
            .label(entry.directory())
            .ellipsize(gtk::pango::EllipsizeMode::Start)
            .hexpand(true)
            .xalign(0.0)
            .build();
        directory.add_css_class("caption");
        directory.add_css_class("dim-label");

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        content.set_margin_start(8);
        content.set_margin_end(4);
        content.set_margin_top(4);
        content.set_margin_bottom(4);
        content.append(&letters);
        content.append(&name);
        content.append(&directory);

        match kind {
            Kind::Staged => {
                let has_head = status.has_head;
                content.append(&self.button(
                    "list-remove-symbolic",
                    "Unstage",
                    entry,
                    move |git, entry| git.run(git::unstage(entry, has_head)),
                ));
            }
            Kind::Changed | Kind::Conflicts => {
                let (icon, tooltip) = if kind == Kind::Conflicts {
                    ("object-select-symbolic", "Mark as resolved")
                } else {
                    ("list-add-symbolic", "Stage")
                };
                content.append(&self.button(icon, tooltip, entry, |git, entry| {
                    git.run(git::stage(entry));
                }));
                if kind == Kind::Changed {
                    let (icon, tooltip) = if entry.is_untracked() {
                        ("user-trash-symbolic", "Move to the trash")
                    } else {
                        ("edit-undo-symbolic", "Discard the changes")
                    };
                    content.append(&self.button(icon, tooltip, entry, TuniGit::confirm_discard));
                }
            }
        }

        let row = gtk::ListBoxRow::builder()
            .child(&content)
            .activatable(true)
            .tooltip_text(&entry.path)
            .build();
        let entry = entry.clone();
        row.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.open(&entry)
        ));
        row
    }

    fn button<F>(&self, icon: &str, tooltip: &str, entry: &Entry, action: F) -> gtk::Button
    where
        F: Fn(&TuniGit, &Entry) + 'static,
    {
        let button = gtk::Button::builder()
            .icon_name(icon)
            .tooltip_text(tooltip)
            .valign(gtk::Align::Center)
            .build();
        button.add_css_class("flat");
        let entry = entry.clone();
        button.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| action(&this, &entry)
        ));
        button
    }

    /// Opens the file the row names, in a pane. The diff viewer takes this
    /// over when there is one.
    fn open(&self, entry: &Entry) {
        let Some(status) = self.status() else {
            return;
        };
        let path = status.root.join(&entry.path);
        // A deleted file has a row and no bytes behind it; there is nothing to
        // open until the diff viewer can show what was in it.
        if !path.exists() {
            return;
        }
        let handler = self.imp().open.borrow().clone();
        if let Some(handler) = handler {
            handler(path);
        }
    }

    // --- acting on the repository ------------------------------------------

    /// Runs what a constructor decided was possible, or says why it was not.
    fn attempt<F>(&self, build: F)
    where
        F: Fn(&Status) -> Result<Task, String>,
    {
        let Some(status) = self.status() else {
            return;
        };
        match build(&status) {
            Ok(task) => self.run(task),
            Err(message) => self.report("That can't be done yet", &message),
        }
    }

    fn commit(&self, include_all: bool) {
        let imp = self.imp();
        let Some(status) = self.status() else {
            return;
        };
        let Some(message) = imp.message.borrow().clone() else {
            return;
        };
        let amend = imp
            .amend
            .borrow()
            .as_ref()
            .is_some_and(gtk::prelude::CheckButtonExt::is_active);
        let buffer = message.buffer();
        let text = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string();

        match git::commit(&text, include_all, amend, &status) {
            Ok(task) => {
                self.run(task);
                // Cleared here rather than when the commit lands: the text is
                // in the task already, and a message left in the box after a
                // successful commit is the next commit's message by accident.
                buffer.set_text("");
                if let Some(amend) = imp.amend.borrow().as_ref() {
                    amend.set_active(false);
                }
            }
            Err(message) => self.report("Nothing was committed", &message),
        }
    }

    fn confirm_discard(&self, entry: &Entry) {
        let name = entry.file_name().to_owned();
        let body = if entry.is_untracked() {
            format!("“{name}” will be moved to the trash.")
        } else {
            format!("The changes in “{name}” will be lost.")
        };
        let entry = entry.clone();
        self.confirm("Discard the changes?", &body, "Discard", move |git| {
            git.run(git::discard(&entry));
        });
    }

    fn discard_all(&self) {
        let Some(status) = self.status() else {
            return;
        };
        if status.changed.is_empty() {
            return;
        }
        let trashed = status.changed.iter().filter(|e| e.is_untracked()).count();
        let body = match trashed {
            0 => format!(
                "The changes in {} will be lost.",
                count_text(status.changed.len())
            ),
            _ => format!(
                "The changes in {} will be lost, and {trashed} untracked \
                 will be moved to the trash.",
                count_text(status.changed.len())
            ),
        };
        // Over the list the user was shown, not over whatever the working tree
        // holds by the time they answer: a build running beside the dialog
        // must not have its output swept up by an answer given before it
        // existed.
        let entries = status.changed.clone();
        self.confirm("Discard every change?", &body, "Discard All", move |git| {
            git.run(git::discard_all(&entries));
        });
    }

    fn create_branch(&self) {
        let entry = gtk::Entry::builder().activates_default(true).build();
        let dialog = adw::AlertDialog::new(Some("New Branch"), Some("Branch off the current one"));
        dialog.set_extra_child(Some(&entry));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("create", "Create");
        dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("create"));
        dialog.set_close_response("cancel");
        dialog.connect_response(
            None,
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_, response| {
                    if response != "create" {
                        return;
                    }
                    match git::create_branch(&entry.text()) {
                        Ok(task) => this.run(task),
                        Err(message) => this.report("No branch was created", &message),
                    }
                }
            ),
        );
        dialog.present(Some(self));
    }

    /// Runs a task off the main loop, then reads the repository again.
    ///
    /// One at a time: two git processes writing the same index is how a
    /// repository ends up holding a lock nobody owns.
    fn run(&self, task: Task) {
        let imp = self.imp();
        if imp.busy.get() {
            return;
        }
        let Some(root) = self.status().map(|status| status.root.clone()).or_else(|| {
            // Starting a repository is the one action there is no status for.
            let directory = imp.directory.borrow().clone();
            (!directory.as_os_str().is_empty()).then_some(directory)
        }) else {
            return;
        };

        imp.busy.set(true);
        imp.generation.set(imp.generation.get().wrapping_add(1));
        self.set_sensitive(false);

        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let running = task.clone();
                let directory = root.clone();
                let outcome =
                    gio::spawn_blocking(move || git::run_task(&running, &directory)).await;

                match outcome {
                    Ok(Ok(_)) => this.trash(&task.trash, &root),
                    Ok(Err(message)) => this.report(&task.label, &message),
                    Err(_) => this.report(&task.label, "The git process failed."),
                }

                this.imp().busy.set(false);
                this.set_sensitive(true);
                // A read from before the action is worthless, and the details
                // may have moved with it: a commit changes the history, a
                // switch changes the branch.
                this.imp().status.replace(None);
                this.reload(true);
            }
        ));
    }

    /// Moves what git cannot restore to the desktop's trash, which can.
    fn trash(&self, paths: &[String], root: &Path) {
        for path in paths {
            let file = gio::File::for_path(root.join(path));
            if let Err(error) = file.trash(gio::Cancellable::NONE) {
                self.report(&format!("Couldn't trash {path}"), &error.to_string());
            }
        }
    }

    // --- talking back ------------------------------------------------------

    fn confirm<F>(&self, heading: &str, body: &str, confirm: &str, apply: F)
    where
        F: Fn(&TuniGit) + 'static,
    {
        let dialog = adw::AlertDialog::new(Some(heading), Some(body));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("confirm", confirm);
        dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.connect_response(
            None,
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_, response| {
                    if response == "confirm" {
                        apply(&this);
                    }
                }
            ),
        );
        dialog.present(Some(self));
    }

    /// What went wrong, in the panel rather than in a dialog: a failed fetch
    /// is not worth a modal, and the message is usually git's own first line.
    fn report(&self, label: &str, message: &str) {
        if let Some(banner) = self.imp().banner.borrow().as_ref() {
            banner.set_title(&format!("{label}: {message}"));
            banner.set_revealed(true);
        }
    }

    fn refresh_commit_button(&self) {
        let imp = self.imp();
        let Some(button) = imp.commit.borrow().clone() else {
            return;
        };
        let has_message = imp.message.borrow().as_ref().is_some_and(|message| {
            let buffer = message.buffer();
            !buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .trim()
                .is_empty()
        });
        let staged = self
            .status()
            .is_some_and(|status| !status.staged.is_empty());
        button.set_sensitive(has_message && staged);
        button.set_tooltip_text(Some(if !has_message {
            "Write a commit message first"
        } else if staged {
            "Commit what is staged"
        } else {
            "Stage something first"
        }));
    }
}

fn clear(list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

fn count_text(count: usize) -> String {
    if count == 1 {
        "1 file".to_owned()
    } else {
        format!("{count} files")
    }
}

/// Ahead and behind, spelled out. An arrow and a number is shorter and says
/// nothing to anyone who has not met it before.
fn distance_text(status: &Status) -> String {
    let mut parts = Vec::new();
    if status.ahead > 0 {
        parts.push(format!("{} ahead", status.ahead));
    }
    if status.behind > 0 {
        parts.push(format!("{} behind", status.behind));
    }
    match (parts.is_empty(), status.upstream.as_deref()) {
        (false, _) => parts.join(", "),
        (true, Some(upstream)) => format!("up to date with {upstream}"),
        (true, None) => "no upstream".to_owned(),
    }
}

fn branch_menu(status: &Status) -> gio::Menu {
    let menu = gio::Menu::new();

    let branches = gio::Menu::new();
    for name in status.branches.iter().filter(|name| {
        // The one it is already on is the label on the button.
        Some(name.as_str()) != status.branch.as_deref()
    }) {
        let item = gio::MenuItem::new(Some(name), None);
        item.set_action_and_target_value(Some("git.switch"), Some(&name.to_variant()));
        branches.append_item(&item);
    }
    if branches.n_items() > 0 {
        menu.append_section(Some("Switch to"), &branches);
    }

    let create = gio::Menu::new();
    create.append(Some("New Branch…"), Some("git.new-branch"));
    menu.append_section(None, &create);
    menu
}

fn commit_row(commit: &git::Commit) -> gtk::ListBoxRow {
    let subject = gtk::Label::builder()
        .label(&commit.subject)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .xalign(0.0)
        .build();
    let detail = gtk::Label::builder()
        .label(format!(
            "{} · {} · {}",
            commit.short, commit.author, commit.when
        ))
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .xalign(0.0)
        .build();
    detail.add_css_class("caption");
    detail.add_css_class("dim-label");

    let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
    content.set_margin_start(8);
    content.set_margin_end(8);
    content.set_margin_top(6);
    content.set_margin_bottom(6);
    content.append(&subject);
    content.append(&detail);

    gtk::ListBoxRow::builder()
        .child(&content)
        .activatable(false)
        .tooltip_text(&commit.hash)
        .build()
}

/// One action, holding the panel weakly: the group is inserted into the panel,
/// so anything stronger than this would be a cycle the panel never leaves.
fn entry<F>(name: &str, git: &TuniGit, activate: F) -> gio::ActionEntry<gio::SimpleActionGroup>
where
    F: Fn(&TuniGit) + 'static,
{
    let weak = git.downgrade();
    gio::ActionEntry::builder(name)
        .activate(move |_: &gio::SimpleActionGroup, _, _| {
            if let Some(git) = weak.upgrade() {
                activate(&git);
            }
        })
        .build()
}
