//! The panel on the far side of the terminals: what the shell is running, the
//! files beside it, the repository they belong to, and a debugger for the
//! program being worked on.
//!
//! Pages of one panel rather than a panel each, because they answer the
//! same question — what is going on where the shell is working — and a window
//! only has so many sides.

use std::cell::RefCell;
use std::path::Path;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use tuni_core::settings::Settings;

use crate::debugger::TuniDebugger;
use crate::files::TuniFiles;
use crate::git::TuniGit;
use crate::info::TuniInfo;
use crate::sftp::TuniSftp;

/// The names the pages are addressed by, in the switcher and in `session.json`.
pub const FILES: &str = "files";
pub const SFTP: &str = "sftp";
pub const GIT: &str = "git";
pub const INFO: &str = "info";
pub const DEBUG: &str = "debug";

mod imp {
    use super::{RefCell, TuniDebugger, TuniFiles, TuniGit, TuniInfo, TuniSftp, glib};
    use adw::subclass::prelude::*;

    #[derive(Default)]
    pub struct TuniPanel {
        pub stack: RefCell<Option<adw::ViewStack>>,
        pub info: RefCell<Option<TuniInfo>>,
        pub files: RefCell<Option<TuniFiles>>,
        pub git: RefCell<Option<TuniGit>>,
        pub files_page: RefCell<Option<adw::ViewStackPage>>,
        pub git_page: RefCell<Option<adw::ViewStackPage>>,
        pub info_page: RefCell<Option<adw::ViewStackPage>>,
        pub debug_page: RefCell<Option<adw::ViewStackPage>>,
        pub sftp: RefCell<Option<TuniSftp>>,
        pub debugger: RefCell<Option<TuniDebugger>>,
        /// Held so the page can be taken off the switcher: there is nothing to
        /// browse until a pane is connected to something.
        pub sftp_page: RefCell<Option<adw::ViewStackPage>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TuniPanel {
        const NAME: &'static str = "TuniPanel";
        type Type = super::TuniPanel;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for TuniPanel {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build();
        }
    }

    impl WidgetImpl for TuniPanel {}
    impl BinImpl for TuniPanel {}
}

glib::wrapper! {
    pub struct TuniPanel(ObjectSubclass<imp::TuniPanel>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for TuniPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl TuniPanel {
    #[must_use]
    pub fn new() -> Self {
        glib::Object::new()
    }

    fn build(&self) {
        let imp = self.imp();

        let info = TuniInfo::new();
        let files = TuniFiles::new();
        let git = TuniGit::new();
        let sftp = TuniSftp::new();
        let debugger = TuniDebugger::new();

        // Files first, since it is the page the panel opens on and the one a
        // pane is opened from; Info last, as kero orders the same three.
        let stack = adw::ViewStack::new();
        let files_page =
            stack.add_titled_with_icon(&files, Some(FILES), "Files", "folder-symbolic");
        // Adwaita has no icon for a repository, so tuni ships one: the
        // commit-graph everyone draws when they say "git". Installed builds
        // find it in hicolor; a checkout run finds it via the search path
        // main.rs adds.
        let git_page = stack.add_titled_with_icon(&git, Some(GIT), "Git", "tuni-git-symbolic");
        // Beside Files, since it is the same page about another machine, and
        // hidden until there is a machine: a switcher with a page nothing can
        // fill is a dead tab.
        // A server rather than a folder: beside the Files tab, two folders that
        // differ by a small badge are two tabs nobody can tell apart at 16px.
        let sftp_page =
            stack.add_titled_with_icon(&sftp, Some(SFTP), "Remote", "network-server-symbolic");
        sftp_page.set_visible(false);
        let info_page =
            stack.add_titled_with_icon(&info, Some(INFO), "Info", "dialog-information-symbolic");
        // Last: reached on purpose, not passed through on the way somewhere.
        let debug_page =
            stack.add_titled_with_icon(&debugger, Some(DEBUG), "Debug", "system-run-symbolic");

        // How many files have changed, on the tab, so the number is readable
        // while the Files page is the one showing.
        git.connect_changed(glib::clone!(
            #[weak]
            git_page,
            move |count| {
                git_page.set_badge_number(count.try_into().unwrap_or(u32::MAX));
            }
        ));

        let switcher = adw::ViewSwitcher::builder()
            .stack(&stack)
            .policy(adw::ViewSwitcherPolicy::Wide)
            .halign(gtk::Align::Center)
            .build();
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        bar.set_margin_top(6);
        bar.set_margin_bottom(6);
        // Room at the sides too: a panel narrow enough to squeeze the
        // switcher otherwise presses the first and last page against the
        // panel's edges.
        bar.set_margin_start(12);
        bar.set_margin_end(12);
        bar.set_halign(gtk::Align::Center);
        bar.append(&switcher);

        let view = adw::ToolbarView::new();
        view.add_top_bar(&bar);
        view.set_content(Some(&stack));
        self.set_child(Some(&view));

        imp.stack.replace(Some(stack));
        imp.info.replace(Some(info));
        imp.files.replace(Some(files));
        imp.git.replace(Some(git));
        imp.git_page.replace(Some(git_page));
        imp.sftp.replace(Some(sftp));
        imp.sftp_page.replace(Some(sftp_page));
        imp.debugger.replace(Some(debugger));
        imp.files_page.replace(Some(files_page));
        imp.info_page.replace(Some(info_page));
        imp.debug_page.replace(Some(debug_page));

        // The window re-applies the settings whenever they change; what it
        // cannot do is reach a panel that has not finished building, so the
        // first read happens here.
        self.apply_settings(&Settings::load());
    }

    /// Shows and hides pages as the settings say. A page someone turned off
    /// while it was showing hands the panel to the first page still on the
    /// switcher, the same move the Remote page makes when its host goes away.
    pub fn apply_settings(&self, settings: &Settings) {
        let imp = self.imp();
        for (page, on) in [
            (&imp.files_page, settings.panel_files),
            (&imp.git_page, settings.panel_git),
            (&imp.info_page, settings.panel_info),
            (&imp.debug_page, settings.panel_debug),
        ] {
            if let Some(page) = page.borrow().as_ref() {
                page.set_visible(on);
            }
        }
        let showing = self.page();
        let hidden = [
            (FILES, settings.panel_files),
            (GIT, settings.panel_git),
            (INFO, settings.panel_info),
            (DEBUG, settings.panel_debug),
        ];
        if hidden.iter().any(|(name, on)| *name == showing && !on)
            && let Some((first, _)) = hidden.iter().find(|(_, on)| *on)
        {
            self.set_page(first);
        }
    }

    #[must_use]
    pub fn info(&self) -> Option<TuniInfo> {
        self.imp().info.borrow().clone()
    }

    #[must_use]
    pub fn files(&self) -> Option<TuniFiles> {
        self.imp().files.borrow().clone()
    }

    #[must_use]
    pub fn git(&self) -> Option<TuniGit> {
        self.imp().git.borrow().clone()
    }

    #[must_use]
    pub fn sftp(&self) -> Option<TuniSftp> {
        self.imp().sftp.borrow().clone()
    }

    #[must_use]
    pub fn debugger(&self) -> Option<TuniDebugger> {
        self.imp().debugger.borrow().clone()
    }

    /// Points every page at a directory.
    ///
    /// Every page, not only the one showing: the badge on the Git tab is part
    /// of what the Files page is telling the user, and a page that only
    /// catches up when it is looked at shows the previous directory for a
    /// frame.
    ///
    /// The Info page is told more than a directory, because what it draws is
    /// the shell rather than the tree: `cwd` is where that shell is working
    /// and `directory` is what the other two anchored to, which are the same
    /// path until a project pins one or a repository is found above it. It is
    /// also told the host, when the pane is on one, which is the one thing on
    /// that page that is not about this machine.
    pub fn sync(
        &self,
        directory: &Path,
        cwd: &Path,
        shell_pid: Option<u32>,
        automatic: bool,
        host: Option<&str>,
    ) {
        if let Some(info) = self.info() {
            info.sync(shell_pid, cwd, directory, automatic, host);
        }
        if let Some(files) = self.files() {
            files.sync(directory);
        }
        if let Some(git) = self.git() {
            git.sync(directory);
        }
        if let Some(sftp) = self.sftp() {
            sftp.sync(host);
        }
        if let Some(page) = self.imp().sftp_page.borrow().as_ref() {
            page.set_visible(host.is_some());
            // A hidden page keeps drawing if it is the one showing, so the
            // panel goes back to the files on this machine with the pane that
            // was on another.
            if host.is_none() && self.page() == SFTP {
                self.set_page(FILES);
            }
        }
    }

    /// The timer's re-read. The Info page is only read while it is showing:
    /// walking `/proc` is cheap but not free, and nothing else on screen
    /// depends on what it finds.
    pub fn poll(&self) {
        if self.page() == INFO
            && let Some(info) = self.info()
        {
            info.poll();
        }
        if let Some(files) = self.files() {
            files.poll();
        }
        if let Some(git) = self.git() {
            git.poll();
        }
    }

    /// Which page is showing, by the name it is saved under.
    #[must_use]
    pub fn page(&self) -> String {
        self.imp()
            .stack
            .borrow()
            .as_ref()
            .and_then(adw::ViewStack::visible_child_name)
            .map_or_else(|| FILES.to_owned(), |name| name.to_string())
    }

    /// Shows a page by name. An unknown name — a session file from a version
    /// with pages this one does not have — leaves the panel where it is.
    pub fn set_page(&self, name: &str) {
        if let Some(stack) = self.imp().stack.borrow().as_ref()
            && stack.child_by_name(name).is_some()
        {
            stack.set_visible_child_name(name);
        }
    }
}
