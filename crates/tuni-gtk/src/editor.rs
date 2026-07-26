//! A pane holding a file instead of a shell.
//!
//! What the pane shows depends on what the file turns out to be — text in a
//! source view, a picture, or a line saying why neither — and
//! [`tuni_core::editor`] is what decides that. This is the widget around it:
//! the name and the save button along the top, the find bar under them, and the
//! text itself below.
//!
//! Saving is the model's job too, so the only thing kept here that the disk
//! does not already have is the text as it was read: the file is dirty exactly
//! when the buffer differs from it, which is how kero decides the same thing,
//! and it means an edit typed and undone leaves the file clean again.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::gio;
use gtk::glib;
use sourceview5::prelude::*;

use tuni_core::TerminalConfig;
use tuni_core::editor::{self, Document};

/// The pages of the stack, by the names they are added under.
const TEXT: &str = "text";
const IMAGE: &str = "image";
const MESSAGE: &str = "message";

mod imp {
    use super::{Cell, PathBuf, Rc, RefCell, glib};
    use adw::subclass::prelude::*;

    /// Shared rather than owned, so a handler is free to touch the editor it
    /// was called from.
    pub type Handler = Rc<dyn Fn()>;

    #[derive(Default)]
    pub struct TuniEditor {
        pub path: RefCell<PathBuf>,
        /// The bytes as they were last read or written. What "dirty" is
        /// measured against.
        pub saved: RefCell<String>,
        pub dirty: Cell<bool>,
        /// Set while the buffer is being filled, so the load does not report
        /// itself as an edit.
        pub filling: Cell<bool>,

        pub buffer: RefCell<Option<sourceview5::Buffer>>,
        pub view: RefCell<Option<sourceview5::View>>,
        pub search: RefCell<Option<sourceview5::SearchContext>>,
        pub settings: RefCell<Option<sourceview5::SearchSettings>>,

        pub stack: RefCell<Option<gtk::Stack>>,
        pub name: RefCell<Option<gtk::Label>>,
        pub place: RefCell<Option<gtk::Label>>,
        pub find: RefCell<Option<gtk::Button>>,
        pub save: RefCell<Option<gtk::Button>>,
        pub banner: RefCell<Option<adw::Banner>>,
        pub bar: RefCell<Option<gtk::SearchBar>>,
        pub query: RefCell<Option<gtk::SearchEntry>>,
        pub replacement: RefCell<Option<gtk::Entry>>,
        pub matches: RefCell<Option<gtk::Label>>,
        pub replace_row: RefCell<Option<gtk::Box>>,
        pub picture: RefCell<Option<gtk::Picture>>,
        pub status: RefCell<Option<adw::StatusPage>>,

        /// The window's, called whenever the dirty mark changes.
        pub changed: RefCell<Option<Handler>>,
        /// The window's, called when the text takes the keyboard.
        pub focused: RefCell<Option<Handler>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TuniEditor {
        const NAME: &'static str = "TuniEditor";
        type Type = super::TuniEditor;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for TuniEditor {
        fn constructed(&self) {
            self.parent_constructed();
            crate::debug::born("TuniEditor");
            self.obj().build();
        }
    }

    impl Drop for TuniEditor {
        fn drop(&mut self) {
            crate::debug::died("TuniEditor");
        }
    }

    impl WidgetImpl for TuniEditor {}
    impl BinImpl for TuniEditor {}
}

glib::wrapper! {
    pub struct TuniEditor(ObjectSubclass<imp::TuniEditor>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for TuniEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl TuniEditor {
    #[must_use]
    pub fn new() -> Self {
        glib::Object::new()
    }

    // --- construction ------------------------------------------------------

    fn build(&self) {
        let imp = self.imp();

        let buffer = sourceview5::Buffer::new(None);
        buffer.set_highlight_syntax(true);
        buffer.set_highlight_matching_brackets(true);
        buffer.set_enable_undo(true);

        let view = sourceview5::View::with_buffer(&buffer);
        view.set_monospace(true);
        view.set_show_line_numbers(true);
        view.set_highlight_current_line(true);
        view.set_auto_indent(true);
        view.set_smart_backspace(true);
        view.set_smart_home_end(sourceview5::SmartHomeEndType::Before);
        view.set_tab_width(4);
        view.set_left_margin(6);
        view.set_right_margin(6);
        // Code is written in lines, and a wrapped one hides which line it is:
        // the view scrolls sideways instead, the way every editor does.
        view.set_wrap_mode(gtk::WrapMode::None);
        view.add_css_class("tuni-editor-text");

        let scroller = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&view)
            .build();

        let picture = gtk::Picture::builder()
            .can_shrink(true)
            .content_fit(gtk::ContentFit::ScaleDown)
            .build();
        let picture_scroller = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&picture)
            .build();

        let status = adw::StatusPage::builder()
            .icon_name("text-x-generic-symbolic")
            .title("Cannot Show This File")
            .build();
        let externally = gtk::Button::with_label("Open Externally");
        externally.add_css_class("pill");
        externally.add_css_class("suggested-action");
        externally.set_halign(gtk::Align::Center);
        externally.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.open_externally()
        ));
        status.set_child(Some(&externally));

        let stack = gtk::Stack::new();
        stack.add_named(&scroller, Some(TEXT));
        stack.add_named(&picture_scroller, Some(IMAGE));
        stack.add_named(&status, Some(MESSAGE));

        let banner = adw::Banner::new("");
        banner.set_revealed(false);

        let (header, name, place, find, save) = self.build_header();
        let (bar, query, replacement, matches, replace_row) = self.build_search_bar();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header);
        content.append(&banner);
        content.append(&bar);
        content.append(&stack);
        self.set_child(Some(&content));

        imp.buffer.replace(Some(buffer.clone()));
        imp.view.replace(Some(view.clone()));
        imp.stack.replace(Some(stack));
        imp.name.replace(Some(name));
        imp.place.replace(Some(place));
        imp.find.replace(Some(find));
        imp.save.replace(Some(save));
        imp.banner.replace(Some(banner));
        imp.bar.replace(Some(bar));
        imp.query.replace(Some(query));
        imp.replacement.replace(Some(replacement));
        imp.matches.replace(Some(matches));
        imp.replace_row.replace(Some(replace_row));
        imp.picture.replace(Some(picture));
        imp.status.replace(Some(status));

        self.install_search(&buffer);
        self.install_actions();
        self.watch(&buffer, &view);
        self.set_dark(adw::StyleManager::default().is_dark());
        self.refresh_header();
    }

    /// The strip along the top: what is open, and the one button that acts on
    /// it. The tab above says the same name, and this says it again because a
    /// split tab shows several files at once.
    fn build_header(&self) -> (gtk::Box, gtk::Label, gtk::Label, gtk::Button, gtk::Button) {
        let name = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        name.add_css_class("heading");

        let place = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Start)
            .hexpand(true)
            .build();
        place.add_css_class("dim-label");
        place.add_css_class("caption");

        let titles = gtk::Box::new(gtk::Orientation::Vertical, 0);
        titles.set_hexpand(true);
        titles.set_valign(gtk::Align::Center);
        titles.append(&name);
        titles.append(&place);

        let find = gtk::Button::builder()
            .icon_name("edit-find-symbolic")
            .tooltip_text("Find and Replace (Ctrl+F)")
            .valign(gtk::Align::Center)
            .action_name("editor.find")
            .build();
        find.add_css_class("flat");

        let save = gtk::Button::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text("Save (Ctrl+S)")
            .valign(gtk::Align::Center)
            .sensitive(false)
            .action_name("editor.save")
            .build();
        save.add_css_class("flat");

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.add_css_class("tuni-editor-bar");
        header.set_margin_start(10);
        header.set_margin_end(6);
        header.set_margin_top(4);
        header.set_margin_bottom(4);
        header.append(&titles);
        header.append(&find);
        header.append(&save);

        (header, name, place, find, save)
    }

    /// Find, and replace under it. One bar with two rows rather than two bars:
    /// asking to replace is asking to find first, and the row simply appears
    /// under the one already showing.
    fn build_search_bar(
        &self,
    ) -> (
        gtk::SearchBar,
        gtk::SearchEntry,
        gtk::Entry,
        gtk::Label,
        gtk::Box,
    ) {
        let query = gtk::SearchEntry::builder()
            .placeholder_text("Find")
            .hexpand(true)
            .build();

        let matches = gtk::Label::new(None);
        matches.add_css_class("dim-label");
        matches.add_css_class("numeric");

        let previous = gtk::Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text("Previous Match")
            .action_name("editor.find-previous")
            .build();
        previous.add_css_class("flat");
        let next = gtk::Button::builder()
            .icon_name("go-down-symbolic")
            .tooltip_text("Next Match")
            .action_name("editor.find-next")
            .build();
        next.add_css_class("flat");

        let find_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        find_row.append(&query);
        find_row.append(&matches);
        find_row.append(&previous);
        find_row.append(&next);

        let replacement = gtk::Entry::builder()
            .placeholder_text("Replace")
            .hexpand(true)
            .build();
        let replace = gtk::Button::builder()
            .label("Replace")
            .action_name("editor.replace")
            .build();
        let replace_all = gtk::Button::builder()
            .label("All")
            .tooltip_text("Replace Every Match")
            .action_name("editor.replace-all")
            .build();

        let replace_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        replace_row.set_visible(false);
        replace_row.append(&replacement);
        replace_row.append(&replace);
        replace_row.append(&replace_all);

        let rows = gtk::Box::new(gtk::Orientation::Vertical, 6);
        rows.append(&find_row);
        rows.append(&replace_row);

        let bar = gtk::SearchBar::builder().child(&rows).build();
        bar.connect_entry(&query);

        (bar, query, replacement, matches, replace_row)
    }

    fn install_search(&self, buffer: &sourceview5::Buffer) {
        let imp = self.imp();
        let settings = sourceview5::SearchSettings::new();
        settings.set_wrap_around(true);
        let context = sourceview5::SearchContext::new(buffer, Some(&settings));
        context.set_highlight(true);

        context.connect_occurrences_count_notify(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.refresh_matches()
        ));

        if let Some(query) = imp.query.borrow().as_ref() {
            query.connect_search_changed(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |entry| {
                    if let Some(settings) = this.imp().settings.borrow().as_ref() {
                        settings.set_search_text(Some(entry.text().as_str()));
                    }
                    // From wherever the cursor is rather than from the top, so
                    // typing another letter keeps the match already found.
                    this.find(true, false);
                    this.refresh_matches();
                }
            ));
            query.connect_activate(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_| this.find(true, true)
            ));
            query.connect_next_match(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_| this.find(true, true)
            ));
            query.connect_previous_match(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_| this.find(false, true)
            ));
            query.connect_stop_search(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_| this.close_search()
            ));
        }
        if let Some(replacement) = imp.replacement.borrow().as_ref() {
            replacement.connect_activate(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_| this.replace_one()
            ));
        }

        imp.settings.replace(Some(settings));
        imp.search.replace(Some(context));
    }

    /// The actions the buttons and the shortcuts share.
    ///
    /// On the widget rather than on the window, and so are the keys that reach
    /// them: `Ctrl+S` is flow control to a shell and `Ctrl+F` is a search in
    /// less, so neither may be taken away from a terminal that has the
    /// keyboard. Inside an editor there is no shell to take them from.
    fn install_actions(&self) {
        let actions = gio::SimpleActionGroup::new();
        actions.add_action_entries([
            entry("save", self, |editor| editor.save()),
            entry("find", self, |editor| editor.open_search(false)),
            entry("replace", self, |editor| {
                if editor.searching() {
                    editor.replace_one();
                } else {
                    editor.open_search(true);
                }
            }),
            entry("replace-all", self, TuniEditor::replace_all),
            entry("find-next", self, |editor| editor.find(true, true)),
            entry("find-previous", self, |editor| editor.find(false, true)),
        ]);
        self.insert_action_group("editor", Some(&actions));

        let shortcuts = gtk::ShortcutController::new();
        shortcuts.set_scope(gtk::ShortcutScope::Local);
        for (keys, action) in [
            ("<Ctrl>s", "editor.save"),
            ("<Ctrl>f", "editor.find"),
            ("<Ctrl>h", "editor.replace"),
            ("<Ctrl>g", "editor.find-next"),
            ("<Ctrl><Shift>g", "editor.find-previous"),
        ] {
            shortcuts.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(keys),
                Some(gtk::NamedAction::new(action)),
            ));
        }
        self.add_controller(shortcuts);
    }

    /// What the editor watches on its own: every edit changes whether the file
    /// is dirty, and the keyboard arriving in the text is the pane being worked
    /// in.
    fn watch(&self, buffer: &sourceview5::Buffer, view: &sourceview5::View) {
        buffer.connect_changed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.refresh_dirty()
        ));
        view.connect_has_focus_notify(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |view| {
                if view.has_focus() {
                    let handler = this.imp().focused.borrow().clone();
                    if let Some(handler) = handler {
                        handler();
                    }
                }
            }
        ));
    }

    // --- what the window asks for ------------------------------------------

    /// Called whenever the file becomes dirty or stops being dirty, so the tab
    /// above can mark itself.
    pub fn connect_changed<F: Fn() + 'static>(&self, callback: F) {
        self.imp().changed.replace(Some(Rc::new(callback)));
    }

    /// Called when the text takes the keyboard: the pane it is in is the one
    /// being worked in.
    pub fn connect_focused<F: Fn() + 'static>(&self, callback: F) {
        self.imp().focused.replace(Some(Rc::new(callback)));
    }

    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.imp().path.borrow().clone()
    }

    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.imp().dirty.get()
    }

    /// The file's name, which is what the tab is called.
    #[must_use]
    pub fn name(&self) -> String {
        let path = self.imp().path.borrow();
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned())
    }

    /// Reads a file into the pane.
    pub fn open(&self, path: &Path) {
        let imp = self.imp();
        imp.path.replace(path.to_path_buf());
        self.hide_error();

        match editor::load(path) {
            Document::Text(text) => self.fill(path, &text),
            Document::Image => {
                if let Some(picture) = imp.picture.borrow().as_ref() {
                    picture.set_filename(Some(path));
                }
                self.show_page(IMAGE);
            }
            other => {
                if let Some(status) = imp.status.borrow().as_ref() {
                    status.set_description(other.message().as_deref());
                }
                self.show_page(MESSAGE);
            }
        }
        self.refresh_header();
    }

    fn fill(&self, path: &Path, text: &str) {
        let imp = self.imp();
        let Some(buffer) = imp.buffer.borrow().clone() else {
            return;
        };

        imp.filling.set(true);
        buffer.set_language(
            sourceview5::LanguageManager::default()
                .guess_language(Some(path), None)
                .as_ref(),
        );
        // Undo off around the fill, which is also what empties the undo stack:
        // the file arriving is not something Ctrl+Z should take back off the
        // screen, and neither is the file that was open before it.
        buffer.set_enable_undo(false);
        buffer.set_text(text);
        buffer.set_enable_undo(true);
        buffer.place_cursor(&buffer.start_iter());
        imp.saved.replace(text.to_owned());
        imp.filling.set(false);

        self.show_page(TEXT);
        self.refresh_dirty();
    }

    /// Writes the buffer back, or says why it could not be written.
    pub fn save(&self) {
        let imp = self.imp();
        if !self.is_editable() {
            return;
        }
        let Some(buffer) = imp.buffer.borrow().clone() else {
            return;
        };
        let text = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .to_string();
        let path = imp.path.borrow().clone();

        match editor::save(&path, &text) {
            Ok(()) => {
                imp.saved.replace(text);
                self.hide_error();
                self.refresh_dirty();
            }
            Err(error) => self.show_error(&format!("Cannot save {}: {error}", self.name())),
        }
    }

    /// Where the cursor is, in characters from the start of the file. What the
    /// session remembers, so reopening a file lands where the work was left.
    #[must_use]
    pub fn cursor(&self) -> Option<usize> {
        let buffer = self.imp().buffer.borrow().clone()?;
        if !self.is_editable() {
            return None;
        }
        usize::try_from(buffer.cursor_position()).ok()
    }

    /// Puts the cursor back, and scrolls it into view once there is a view to
    /// scroll — a restored pane is asked this before it has been allocated.
    pub fn set_cursor(&self, offset: usize) {
        let imp = self.imp();
        let (Some(buffer), Some(view)) = (imp.buffer.borrow().clone(), imp.view.borrow().clone())
        else {
            return;
        };
        let offset = i32::try_from(offset).unwrap_or(i32::MAX);
        let mut iter = buffer.iter_at_offset(offset.min(buffer.char_count()));
        buffer.place_cursor(&iter);
        glib::idle_add_local_once(move || {
            view.scroll_to_iter(&mut iter, 0.0, true, 0.0, 0.35);
        });
    }

    /// Opens the find bar with a query already in it, as Ctrl+F and typing
    /// would. The entry drives the search itself, so the matches arrive the
    /// same way they do for a reader.
    pub fn find_text(&self, query: &str) {
        self.open_search(false);
        if let Some(entry) = self.imp().query.borrow().as_ref() {
            entry.set_text(query);
        }
    }

    /// Opens the find bar, for the window's Find command reaching a file pane.
    pub fn open_find(&self) {
        self.open_search(false);
    }

    /// Opens the find bar with the replacement row showing.
    pub fn open_replace(&self) {
        self.open_search(true);
    }

    /// Walks to the next match, or the previous one.
    pub fn step_match(&self, forward: bool) {
        self.find(forward, true);
    }

    /// The selected text, if there is a selection and it is on one line — a
    /// paragraph dragged by accident is not a search term.
    #[must_use]
    pub fn selection(&self) -> Option<String> {
        let buffer = self.imp().buffer.borrow().clone()?;
        let (start, end) = buffer.selection_bounds()?;
        let text = buffer.text(&start, &end, false).to_string();
        (!text.is_empty() && !text.contains('\n')).then_some(text)
    }

    /// What the find bar is saying about the matches: "3 of 12", "No matches",
    /// or nothing while it is still counting.
    #[must_use]
    pub fn match_text(&self) -> String {
        self.imp()
            .matches
            .borrow()
            .as_ref()
            .map_or_else(String::new, |label| label.text().to_string())
    }

    /// Types text in at the cursor, the way the keyboard would.
    pub fn insert(&self, text: &str) {
        if let Some(buffer) = self.imp().buffer.borrow().as_ref()
            && self.is_editable()
        {
            buffer.insert_at_cursor(text);
        }
    }

    /// Hands the keyboard to the text, so a pane focused by a shortcut can be
    /// typed into.
    pub fn focus_text(&self) {
        if let Some(view) = self.imp().view.borrow().as_ref() {
            view.grab_focus();
        }
    }

    /// Whether a line too long for the pane wraps or scrolls sideways.
    pub fn set_wrap(&self, wrap: bool) {
        if let Some(view) = self.imp().view.borrow().as_ref() {
            view.set_wrap_mode(if wrap {
                gtk::WrapMode::WordChar
            } else {
                gtk::WrapMode::None
            });
        }
    }

    /// Follows the desktop between light and dark. The scheme is the one
    /// GtkSourceView ships to match Adwaita, so the text sits in the same
    /// palette as the window around it.
    pub fn set_dark(&self, dark: bool) {
        let Some(buffer) = self.imp().buffer.borrow().clone() else {
            return;
        };
        let manager = sourceview5::StyleSchemeManager::default();
        let scheme = if dark {
            manager
                .scheme("Adwaita-dark")
                .or_else(|| manager.scheme("classic-dark"))
        } else {
            manager
                .scheme("Adwaita")
                .or_else(|| manager.scheme("classic"))
        };
        buffer.set_style_scheme(scheme.as_ref());
    }

    // --- the state on screen -----------------------------------------------

    fn is_editable(&self) -> bool {
        self.page() == TEXT
    }

    fn page(&self) -> String {
        self.imp()
            .stack
            .borrow()
            .as_ref()
            .and_then(gtk::Stack::visible_child_name)
            .map_or_else(String::new, |name| name.to_string())
    }

    fn show_page(&self, name: &str) {
        if let Some(stack) = self.imp().stack.borrow().as_ref() {
            stack.set_visible_child_name(name);
        }
    }

    /// The buffer differs from the disk, or it does not. Everything that shows
    /// a dirty mark reads this one answer.
    fn refresh_dirty(&self) {
        let imp = self.imp();
        if imp.filling.get() {
            return;
        }
        let dirty = match imp.buffer.borrow().as_ref() {
            Some(buffer) if self.is_editable() => {
                buffer.text(&buffer.start_iter(), &buffer.end_iter(), true) != *imp.saved.borrow()
            }
            _ => false,
        };
        if dirty == imp.dirty.get() {
            return;
        }
        imp.dirty.set(dirty);
        self.refresh_header();

        let handler = imp.changed.borrow().clone();
        if let Some(handler) = handler {
            handler();
        }
    }

    fn refresh_header(&self) {
        let imp = self.imp();
        let dirty = imp.dirty.get();
        if let Some(name) = imp.name.borrow().as_ref() {
            // The dot is what marks a file as unsaved, and the tooltip says it
            // in words: color alone is not something every reader has.
            name.set_text(&if dirty {
                format!("• {}", self.name())
            } else {
                self.name()
            });
            name.set_tooltip_text(Some(if dirty {
                "This file has unsaved changes"
            } else {
                "This file is saved"
            }));
        }
        if let Some(place) = imp.place.borrow().as_ref() {
            let path = imp.path.borrow();
            let directory = path
                .parent()
                .map(|parent| parent.to_string_lossy().into_owned())
                .unwrap_or_default();
            place.set_text(&crate::window::shorten(&directory));
        }
        // Nothing to find in a picture and nothing to write back from one, so
        // neither button is there to be pressed.
        let editable = self.is_editable();
        if let Some(find) = imp.find.borrow().as_ref() {
            find.set_visible(editable);
        }
        if let Some(save) = imp.save.borrow().as_ref() {
            save.set_visible(editable);
            save.set_sensitive(dirty);
        }
    }

    fn show_error(&self, message: &str) {
        if let Some(banner) = self.imp().banner.borrow().as_ref() {
            banner.set_title(message);
            banner.set_revealed(true);
        }
    }

    fn hide_error(&self) {
        if let Some(banner) = self.imp().banner.borrow().as_ref() {
            banner.set_revealed(false);
        }
    }

    /// Hands a file the editor will not open to whatever the desktop opens it
    /// with.
    fn open_externally(&self) {
        let path = self.imp().path.borrow().clone();
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&path)));
        launcher.launch(
            self.root().and_downcast::<gtk::Window>().as_ref(),
            gio::Cancellable::NONE,
            |_| (),
        );
    }

    // --- finding and replacing ---------------------------------------------

    fn searching(&self) -> bool {
        self.imp()
            .bar
            .borrow()
            .as_ref()
            .is_some_and(gtk::SearchBar::is_search_mode)
    }

    fn open_search(&self, replacing: bool) {
        let imp = self.imp();
        if !self.is_editable() {
            return;
        }
        if let Some(row) = imp.replace_row.borrow().as_ref() {
            // Asking to replace shows the second row; asking to find leaves it
            // as it was, so a bar opened for replacing stays that way.
            if replacing {
                row.set_visible(true);
            }
        }
        if let Some(bar) = imp.bar.borrow().as_ref() {
            bar.set_search_mode(true);
        }

        // The selection is what was being looked at, so it is what the search
        // starts from — the same thing every editor does with Ctrl+F.
        if let (Some(buffer), Some(query)) =
            (imp.buffer.borrow().clone(), imp.query.borrow().clone())
        {
            if let Some((start, end)) = buffer.selection_bounds()
                && start.line() == end.line()
            {
                query.set_text(buffer.text(&start, &end, true).as_str());
            }
            query.grab_focus();
            query.select_region(0, -1);
        }
    }

    fn close_search(&self) {
        let imp = self.imp();
        if let Some(bar) = imp.bar.borrow().as_ref() {
            bar.set_search_mode(false);
        }
        if let Some(row) = imp.replace_row.borrow().as_ref() {
            row.set_visible(false);
        }
        self.focus_text();
    }

    /// Moves to the next match, or the previous one. `advance` steps past the
    /// match already selected; typing another letter does not, so the match
    /// under the cursor stays where it is while the query grows.
    fn find(&self, forward: bool, advance: bool) {
        let imp = self.imp();
        let (Some(buffer), Some(context), Some(view)) = (
            imp.buffer.borrow().clone(),
            imp.search.borrow().clone(),
            imp.view.borrow().clone(),
        ) else {
            return;
        };
        if imp
            .settings
            .borrow()
            .as_ref()
            .and_then(sourceview5::SearchSettings::search_text)
            .is_none_or(|text| text.is_empty())
        {
            return;
        }

        let cursor = buffer.iter_at_mark(&buffer.get_insert());
        let (start, end) = buffer.selection_bounds().unwrap_or((cursor, cursor));
        let found = if forward {
            context.forward(if advance { &end } else { &start })
        } else {
            context.backward(&start)
        };
        let Some((match_start, mut match_end, _)) = found else {
            return;
        };
        buffer.select_range(&match_start, &match_end);
        // Scrolled to only as far as it takes to see it, with a margin: forcing
        // an alignment would drag a line that is already on screen sideways and
        // cut the start of every other one.
        let mut match_start = match_start;
        view.scroll_to_iter(&mut match_end, 0.1, false, 0.0, 0.0);
        view.scroll_to_iter(&mut match_start, 0.1, false, 0.0, 0.0);
        self.refresh_matches();
    }

    fn replace_one(&self) {
        let imp = self.imp();
        let (Some(buffer), Some(context), Some(replacement)) = (
            imp.buffer.borrow().clone(),
            imp.search.borrow().clone(),
            imp.replacement.borrow().clone(),
        ) else {
            return;
        };
        let text = replacement.text().to_string();
        if let Some((mut start, mut end)) = buffer.selection_bounds()
            && context.occurrence_position(&start, &end) > 0
            && context.replace(&mut start, &mut end, &text).is_err()
        {
            return;
        }
        // Whether or not this one was replaced, the useful next step is the
        // match after it.
        self.find(true, true);
    }

    fn replace_all(&self) {
        let imp = self.imp();
        let (Some(context), Some(replacement)) = (
            imp.search.borrow().clone(),
            imp.replacement.borrow().clone(),
        ) else {
            return;
        };
        if let Err(error) = context.replace_all(replacement.text().as_str()) {
            self.show_error(&format!("Cannot replace: {error}"));
        }
        self.refresh_matches();
    }

    /// "3 of 12", once the whole file has been scanned. The count arrives
    /// asynchronously, which is why this is also a signal handler.
    fn refresh_matches(&self) {
        let imp = self.imp();
        let Some(label) = imp.matches.borrow().clone() else {
            return;
        };
        let (Some(buffer), Some(context)) =
            (imp.buffer.borrow().clone(), imp.search.borrow().clone())
        else {
            return;
        };
        let count = context.occurrences_count();
        if count < 0 {
            label.set_text("");
            return;
        }
        if count == 0 {
            label.set_text("No matches");
            return;
        }
        let position = buffer
            .selection_bounds()
            .map_or(0, |(start, end)| context.occurrence_position(&start, &end));
        if position > 0 {
            label.set_text(&format!("{position} of {count}"));
        } else {
            label.set_text(&format!("{count} matches"));
        }
    }
}

/// One action in the widget's own group.
fn entry<F>(
    name: &str,
    editor: &TuniEditor,
    activate: F,
) -> gio::ActionEntry<gio::SimpleActionGroup>
where
    F: Fn(&TuniEditor) + 'static,
{
    let editor = editor.downgrade();
    gio::ActionEntry::builder(name)
        .activate(move |_: &gio::SimpleActionGroup, _, _| {
            if let Some(editor) = editor.upgrade() {
                activate(&editor);
            }
        })
        .build()
}

/// Paints the editor's text in the terminal's own font.
///
/// A window of terminals with one file open in it should not be a window of two
/// fonts, and the font is a setting rather than a constant, so it is applied
/// the same way the chrome is: one provider on the display, reloaded whenever
/// the setting changes.
pub fn apply_font(config: &TerminalConfig) {
    thread_local! {
        static PROVIDER: gtk::CssProvider = gtk::CssProvider::new();
    }
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let css = format!(
        ".tuni-editor-text {{ font-family: {family}; font-size: {size}pt; }}\n",
        family = config.font_stack(),
        size = config.font_size,
    );
    PROVIDER.with(|provider| {
        provider.load_from_string(&css);
        gtk::style_context_add_provider_for_display(
            &display,
            provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
}
