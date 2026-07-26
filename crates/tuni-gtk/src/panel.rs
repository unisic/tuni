//! The panel on the far side of the terminals: the files beside the shell,
//! and the repository they belong to.
//!
//! Two pages of one panel rather than two panels, because they answer the same
//! question — what is in the directory the shell is working in — and a window
//! only has so many sides.

use std::cell::RefCell;
use std::path::Path;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use crate::files::TuniFiles;
use crate::git::TuniGit;

/// The names the pages are addressed by, in the switcher and in `session.json`.
pub const FILES: &str = "files";
pub const GIT: &str = "git";

mod imp {
    use super::{RefCell, TuniFiles, TuniGit, glib};
    use adw::subclass::prelude::*;

    #[derive(Default)]
    pub struct TuniPanel {
        pub stack: RefCell<Option<adw::ViewStack>>,
        pub files: RefCell<Option<TuniFiles>>,
        pub git: RefCell<Option<TuniGit>>,
        pub git_page: RefCell<Option<adw::ViewStackPage>>,
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

        let files = TuniFiles::new();
        let git = TuniGit::new();

        let stack = adw::ViewStack::new();
        stack.add_titled_with_icon(&files, Some(FILES), "Files", "folder-symbolic");
        // Adwaita has no icon for a repository, and a switcher page without one
        // draws the missing-image glyph. A list of what changed is what the
        // page is, and it is an icon the theme actually has.
        let git_page =
            stack.add_titled_with_icon(&git, Some(GIT), "Git", "view-list-bullet-symbolic");

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
        bar.set_halign(gtk::Align::Center);
        bar.append(&switcher);

        let view = adw::ToolbarView::new();
        view.add_top_bar(&bar);
        view.set_content(Some(&stack));
        self.set_child(Some(&view));

        imp.stack.replace(Some(stack));
        imp.files.replace(Some(files));
        imp.git.replace(Some(git));
        imp.git_page.replace(Some(git_page));
    }

    #[must_use]
    pub fn files(&self) -> Option<TuniFiles> {
        self.imp().files.borrow().clone()
    }

    #[must_use]
    pub fn git(&self) -> Option<TuniGit> {
        self.imp().git.borrow().clone()
    }

    /// Points both pages at a directory.
    ///
    /// Both, not only the one showing: the badge on the Git tab is part of
    /// what the Files page is telling the user, and a page that only catches
    /// up when it is looked at shows the previous directory for a frame.
    pub fn sync(&self, directory: &Path) {
        if let Some(files) = self.files() {
            files.sync(directory);
        }
        if let Some(git) = self.git() {
            git.sync(directory);
        }
    }

    /// The timer's re-read.
    pub fn poll(&self) {
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
