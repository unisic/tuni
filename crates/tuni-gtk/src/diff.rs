//! A pane holding what changed in a file.
//!
//! The text is git's own: `git diff` for the working tree, `git diff --cached`
//! for the index, read as it prints and parsed by [`tuni_core::diff`]. Nothing
//! here re-implements what a diff is, and nothing here writes one — staging a
//! hunk cuts the hunk back out of the text that was parsed and hands it to
//! `git apply`, so what is staged is what was on screen.
//!
//! Two shapes, as kero has: everything in one column in the order git wrote it,
//! or the two sides beside each other. The choice is per pane, because a wide
//! pane and a narrow one want different answers.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::gio;
use gtk::glib;

use tuni_core::diff::{Diff, Hunk, Kind, Line, Span};
use tuni_core::theme::{Rgb, Theme};
use tuni_core::{diff as model, git};

/// The pages of the stack, by the names they are added under.
const DIFF: &str = "diff";
const MESSAGE: &str = "message";

/// How many lines are drawn before the rest is summed up instead.
///
/// Every line is a handful of widgets, and a diff of a generated file runs to
/// tens of thousands of them. The cut is per pane rather than per hunk so that
/// a diff of many small hunks is treated the same as one of a single large one.
const MAX_LINES: usize = 4000;

/// How far the line's background is tinted toward the palette color, and how
/// far the words that actually changed are tinted past it.
const ROW_TINT: f64 = 0.16;
const SPAN_TINT: f64 = 0.38;

/// How often a diff on screen re-reads its file. The poll belongs to the pane
/// rather than to a window-wide timer, so a diff nobody is looking at — its
/// tab unmapped — costs no wakeup at all.
const POLL_SECONDS: u32 = 2;

mod imp {
    use super::{Cell, Colors, Diff, PathBuf, Rc, RefCell, glib};
    use adw::subclass::prelude::*;

    pub type Handler = Rc<dyn Fn()>;

    #[derive(Default)]
    pub struct TuniDiff {
        pub path: RefCell<PathBuf>,
        /// Which comparison is shown: the index against HEAD, or the working
        /// tree against the index.
        pub staged: Cell<bool>,
        /// Whether the two sides are drawn beside each other.
        pub split: Cell<bool>,
        /// The repository the last read came from — where a patch has to be
        /// applied, which is not the directory the file is in.
        pub root: RefCell<PathBuf>,
        pub diff: RefCell<Diff>,
        /// The patch the drawing on screen came from. A re-read that comes
        /// back with the same text is not worth redrawing over, and the timer
        /// re-reads every couple of seconds.
        pub text: RefCell<String>,
        /// Whether `text` is what is on screen, rather than a message about a
        /// read that failed.
        pub loaded: Cell<bool>,
        /// Set while a read or an apply is in flight, so a second click does
        /// not start a second one.
        pub busy: Cell<bool>,
        /// Which file the pane is pointed at, counted rather than named. A read
        /// runs on a worker and answers about the path it was started with, so
        /// a pane pointed somewhere else in the meantime has to be able to tell
        /// that the answer is about the file before.
        pub generation: Cell<u64>,
        pub colors: RefCell<Colors>,
        /// The re-read poll, alive only while the pane is mapped.
        pub timer: RefCell<Option<glib::SourceId>>,

        pub name: RefCell<Option<gtk::Label>>,
        pub place: RefCell<Option<gtk::Label>>,
        pub summary: RefCell<Option<gtk::Label>>,
        pub sides: RefCell<Option<gtk::ToggleButton>>,
        pub banner: RefCell<Option<adw::Banner>>,
        pub stack: RefCell<Option<gtk::Stack>>,
        pub hunks: RefCell<Option<gtk::Box>>,
        pub status: RefCell<Option<adw::StatusPage>>,

        /// The window's, called when the pane takes the keyboard.
        pub focused: RefCell<Option<Handler>>,
        /// The window's, called after a hunk was staged or unstaged, so the
        /// panel beside it stops showing what is no longer true.
        pub applied: RefCell<Option<Handler>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TuniDiff {
        const NAME: &'static str = "TuniDiff";
        type Type = super::TuniDiff;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for TuniDiff {
        fn constructed(&self) {
            self.parent_constructed();
            crate::debug::born("TuniDiff");
            self.obj().build();
        }
    }

    impl Drop for TuniDiff {
        fn drop(&mut self) {
            crate::debug::died("TuniDiff");
        }
    }

    impl WidgetImpl for TuniDiff {}
    impl BinImpl for TuniDiff {}
}

/// The four colors a diff is drawn in, as Pango markup wants them: the tint
/// behind an added or a removed line, and the stronger one behind the words
/// that changed within it.
#[derive(Clone, Debug)]
pub struct Colors {
    added_row: String,
    removed_row: String,
    added_span: String,
    removed_span: String,
}

impl Default for Colors {
    fn default() -> Self {
        Self::from(&tuni_core::theme::theme_or_default("", true))
    }
}

impl From<&Theme> for Colors {
    fn from(theme: &Theme) -> Self {
        let tint = |color: Rgb, amount: f64| theme.background.blend(color, amount).to_hex();
        // ANSI red and green: what every diff tool on a terminal already uses,
        // and what the theme picked for exactly this.
        let removed = theme.palette[1];
        let added = theme.palette[2];
        Self {
            added_row: tint(added, ROW_TINT),
            removed_row: tint(removed, ROW_TINT),
            added_span: tint(added, SPAN_TINT),
            removed_span: tint(removed, SPAN_TINT),
        }
    }
}

glib::wrapper! {
    pub struct TuniDiff(ObjectSubclass<imp::TuniDiff>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for TuniDiff {
    fn default() -> Self {
        Self::new()
    }
}

impl TuniDiff {
    #[must_use]
    pub fn new() -> Self {
        glib::Object::new()
    }

    // --- construction ------------------------------------------------------

    fn build(&self) {
        let imp = self.imp();

        // Coming back into view asks for the re-read the time away did not
        // do — selecting the tab again, or restoring the window — and starts
        // the poll that catches the shell beside it editing the file. Leaving
        // stops it: an unmapped pane has nothing to redraw for.
        self.connect_map(|this| {
            this.reload();
            let mut timer = this.imp().timer.borrow_mut();
            if timer.is_none() {
                *timer = Some(glib::timeout_add_seconds_local(
                    POLL_SECONDS,
                    glib::clone!(
                        #[weak]
                        this,
                        #[upgrade_or]
                        glib::ControlFlow::Break,
                        move || {
                            // A minimized or backgrounded window's diff has
                            // nobody reading it; the next tick after the
                            // window comes back is at most two seconds away,
                            // which is the staleness the poll allows anyway.
                            let active = this
                                .root()
                                .and_downcast::<gtk::Window>()
                                .is_none_or(|window| window.is_active());
                            if active {
                                this.reload();
                            }
                            glib::ControlFlow::Continue
                        }
                    ),
                ));
            }
        });
        self.connect_unmap(|this| {
            if let Some(timer) = this.imp().timer.borrow_mut().take() {
                timer.remove();
            }
        });

        let hunks = gtk::Box::new(gtk::Orientation::Vertical, 10);
        hunks.set_margin_start(8);
        hunks.set_margin_end(8);
        hunks.set_margin_top(4);
        hunks.set_margin_bottom(8);

        let scroller = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&hunks)
            .build();

        let status = adw::StatusPage::builder()
            .icon_name("edit-select-all-symbolic")
            .title("No Changes")
            .build();

        let stack = gtk::Stack::new();
        stack.add_named(&scroller, Some(DIFF));
        stack.add_named(&status, Some(MESSAGE));

        let banner = adw::Banner::new("");
        banner.set_revealed(false);

        let (header, name, place, summary, sides) = self.build_header();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header);
        content.append(&banner);
        content.append(&stack);
        self.set_child(Some(&content));

        imp.name.replace(Some(name));
        imp.place.replace(Some(place));
        imp.summary.replace(Some(summary));
        imp.sides.replace(Some(sides));
        imp.banner.replace(Some(banner));
        imp.stack.replace(Some(stack));
        imp.hunks.replace(Some(hunks));
        imp.status.replace(Some(status));

        self.install_actions();
        self.watch_focus();
    }

    /// The strip along the top: what is being compared, how much of it moved,
    /// and the two things that can be done to the view itself.
    fn build_header(
        &self,
    ) -> (
        gtk::Box,
        gtk::Label,
        gtk::Label,
        gtk::Label,
        gtk::ToggleButton,
    ) {
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

        // Not color alone: the counts say which way each number goes.
        let summary = gtk::Label::new(None);
        summary.add_css_class("numeric");
        summary.add_css_class("dim-label");
        summary.add_css_class("caption");

        let sides = gtk::ToggleButton::builder()
            .icon_name("view-dual-symbolic")
            .tooltip_text("Show Both Sides")
            .valign(gtk::Align::Center)
            .build();
        sides.add_css_class("flat");
        sides.connect_toggled(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |button| {
                this.imp().split.set(button.is_active());
                this.draw();
            }
        ));

        let refresh = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Read the Changes Again")
            .valign(gtk::Align::Center)
            .action_name("diff.refresh")
            .build();
        refresh.add_css_class("flat");

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.add_css_class("tuni-editor-bar");
        header.set_margin_start(10);
        header.set_margin_end(6);
        header.set_margin_top(4);
        header.set_margin_bottom(4);
        header.append(&titles);
        header.append(&summary);
        header.append(&sides);
        header.append(&refresh);

        (header, name, place, summary, sides)
    }

    fn install_actions(&self) {
        let actions = gio::SimpleActionGroup::new();
        actions.add_action_entries([
            entry("refresh", self, TuniDiff::reload),
            entry("sides", self, |diff| {
                let sides = diff.imp().sides.borrow().clone();
                if let Some(sides) = sides {
                    sides.set_active(!sides.is_active());
                }
            }),
        ]);
        self.insert_action_group("diff", Some(&actions));
    }

    /// The pane is focused when anything inside it is — a hunk's button as
    /// much as the text — so the ring follows a click anywhere in it.
    fn watch_focus(&self) {
        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                let handler = this.imp().focused.borrow().clone();
                if let Some(handler) = handler {
                    handler();
                }
            }
        ));
        self.add_controller(focus);
    }

    // --- what it is showing ------------------------------------------------

    /// Points the pane at a file and reads what changed in it.
    pub fn open(&self, path: &Path, staged: bool) {
        let imp = self.imp();
        imp.path.replace(path.to_path_buf());
        imp.staged.set(staged);
        imp.loaded.set(false);
        imp.generation.set(imp.generation.get().wrapping_add(1));
        self.refresh_header();
        self.reload();
    }

    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.imp().path.borrow().clone()
    }

    #[must_use]
    pub fn is_staged(&self) -> bool {
        self.imp().staged.get()
    }

    /// Reads the diff again. Everything on screen is replaced by what comes
    /// back, including the sides toggle's effect, which is kept.
    pub fn reload(&self) {
        let imp = self.imp();
        if imp.busy.get() {
            return;
        }
        let path = imp.path.borrow().clone();
        if path.as_os_str().is_empty() {
            return;
        }
        imp.busy.set(true);
        let staged = imp.staged.get();
        let generation = imp.generation.get();

        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let read = gio::spawn_blocking(move || git::diff(&path, staged)).await;
                this.imp().busy.set(false);
                // The pane was pointed at another file while this was in
                // flight. Without this the answer about the old one would be
                // parsed, drawn and left under the new one's header — and
                // `open` clears `loaded`, so the same-text check below cannot
                // catch it either.
                if this.imp().generation.get() != generation {
                    return;
                }
                match read {
                    Ok(Ok(file)) => {
                        let imp = this.imp();
                        imp.root.replace(file.root);
                        // Nothing moved, so neither should the scroll position
                        // under a user who is reading.
                        if imp.loaded.get() && *imp.text.borrow() == file.text {
                            return;
                        }
                        imp.diff.replace(Diff::parse(&file.text));
                        imp.text.replace(file.text);
                        imp.loaded.set(true);
                        this.set_message(None);
                        this.draw();
                    }
                    Ok(Err(message)) => this.fail(&message),
                    Err(_) => this.fail("Reading the changes was interrupted."),
                }
            }
        ));
    }

    /// The colors the lines are drawn in. Taken from the terminal's theme, so
    /// a pane of changes belongs to the same window as the shells around it.
    pub fn set_theme(&self, theme: &Theme) {
        self.imp().colors.replace(Colors::from(theme));
        apply_colors(theme);
        self.draw();
    }

    /// How many hunks the last read found. What a scripted staging checks.
    #[must_use]
    pub fn hunk_count(&self) -> usize {
        self.imp().diff.borrow().hunks.len()
    }

    /// Stages or unstages one hunk by number, the way its button does.
    pub fn stage_hunk(&self, index: usize) {
        self.apply_hunk(index);
    }

    pub fn connect_focused<F: Fn() + 'static>(&self, handler: F) {
        self.imp().focused.replace(Some(Rc::new(handler)));
    }

    pub fn connect_applied<F: Fn() + 'static>(&self, handler: F) {
        self.imp().applied.replace(Some(Rc::new(handler)));
    }

    // --- drawing -----------------------------------------------------------

    fn refresh_header(&self) {
        let imp = self.imp();
        let path = imp.path.borrow().clone();
        let side = if imp.staged.get() {
            "Staged changes"
        } else {
            "Changes"
        };
        if let Some(name) = imp.name.borrow().as_ref() {
            let file = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            name.set_text(&format!("{file} — {side}"));
        }
        if let Some(place) = imp.place.borrow().as_ref() {
            place.set_text(&path.to_string_lossy());
        }
    }

    /// Builds the whole view again from the diff that was last read.
    fn draw(&self) {
        let imp = self.imp();
        let Some(container) = imp.hunks.borrow().clone() else {
            return;
        };
        while let Some(child) = container.first_child() {
            container.remove(&child);
        }

        let diff = imp.diff.borrow().clone();
        let (added, removed) = diff.counts();
        if let Some(summary) = imp.summary.borrow().as_ref() {
            summary.set_text(&format!("{added} added, {removed} removed"));
            summary.set_visible(!diff.is_empty());
        }

        if diff.is_empty() {
            let message = diff.note.clone().unwrap_or_else(|| {
                if imp.staged.get() {
                    "Nothing about this file is staged."
                } else {
                    "This file matches the index."
                }
                .to_owned()
            });
            self.show_message("No Changes", &message);
            return;
        }

        let mut drawn = 0;
        for (index, hunk) in diff.hunks.iter().enumerate() {
            if drawn >= MAX_LINES {
                let left: usize = diff.hunks[index..]
                    .iter()
                    .map(|hunk| hunk.lines.len())
                    .sum();
                container.append(&overflow_row(left, diff.hunks.len() - index));
                break;
            }
            let budget = MAX_LINES - drawn;
            container.append(&self.hunk_card(index, hunk, budget));
            drawn += hunk.lines.len();
        }

        if let Some(stack) = imp.stack.borrow().as_ref() {
            stack.set_visible_child_name(DIFF);
        }
    }

    /// One hunk: where it is in the file, what it does to it, and the button
    /// that puts exactly this much into the index or takes it back out.
    fn hunk_card(&self, index: usize, hunk: &Hunk, budget: usize) -> gtk::Widget {
        let imp = self.imp();
        let staged = imp.staged.get();

        let heading = gtk::Label::builder()
            .label(hunk.header())
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        heading.add_css_class("monospace");
        heading.add_css_class("caption");
        heading.add_css_class("dim-label");

        let (added, removed) = hunk.counts();
        let counts = gtk::Label::new(Some(&format!("+{added} −{removed}")));
        counts.add_css_class("numeric");
        counts.add_css_class("caption");
        counts.add_css_class("dim-label");

        let (icon, tooltip) = if staged {
            ("list-remove-symbolic", "Unstage this hunk")
        } else {
            ("list-add-symbolic", "Stage this hunk")
        };
        let apply = gtk::Button::builder()
            .icon_name(icon)
            .tooltip_text(tooltip)
            .valign(gtk::Align::Center)
            .build();
        apply.add_css_class("flat");
        apply.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.apply_hunk(index)
        ));
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.set_margin_start(8);
        bar.set_margin_end(4);
        bar.set_margin_top(2);
        bar.set_margin_bottom(2);
        bar.append(&heading);
        bar.append(&counts);
        bar.append(&apply);

        let body = if imp.split.get() {
            self.split_body(hunk, budget)
        } else {
            self.inline_body(hunk, budget)
        };

        let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        card.add_css_class("card");
        card.append(&bar);
        card.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        card.append(&body);
        card.upcast()
    }

    /// Every line in the order git wrote it, numbered on both sides.
    fn inline_body(&self, hunk: &Hunk, budget: usize) -> gtk::Widget {
        let marked = self.marked(hunk);
        let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);

        for line in hunk.lines.iter().take(budget) {
            let spans = marked.get(&key(line)).cloned();
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            row.append(&number(line.old));
            row.append(&number(line.new));
            row.append(&self.text(line, spans.as_deref()));
            paint(&row, line.kind);
            rows.append(&row);
        }
        if hunk.lines.len() > budget {
            rows.append(&overflow_row(hunk.lines.len() - budget, 0));
        }
        rows.upcast()
    }

    /// The two sides beside each other, a line of the old file against the
    /// line of the new one that replaced it.
    fn split_body(&self, hunk: &Hunk, budget: usize) -> gtk::Widget {
        let marked = self.marked(hunk);
        let pairs = hunk.rows();
        let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);

        for row in pairs.iter().take(budget) {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            line.set_homogeneous(true);
            for (side, old_side) in [(&row.old, true), (&row.new, false)] {
                let half = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                if let Some(entry) = side {
                    let spans = marked.get(&key(entry)).cloned();
                    half.append(&number(if old_side { entry.old } else { entry.new }));
                    half.append(&self.text(entry, spans.as_deref()));
                    paint(&half, entry.kind);
                } else {
                    // Nothing on this side: the line was added or removed
                    // outright, and the gap is what says so.
                    half.append(&number(None));
                    half.append(&self.text(&blank(), None));
                }
                line.append(&half);
            }
            rows.append(&line);
        }
        if pairs.len() > budget {
            rows.append(&overflow_row(pairs.len() - budget, 0));
        }
        rows.upcast()
    }

    /// One line's text, with the words that changed marked when they are worth
    /// marking. Selectable, because the reason to look at a diff is often to
    /// copy something out of it.
    fn text(&self, line: &Line, spans: Option<&[Span]>) -> gtk::Label {
        let colors = self.imp().colors.borrow().clone();
        let background = match line.kind {
            Kind::Added => Some(&colors.added_span),
            Kind::Removed => Some(&colors.removed_span),
            Kind::Context | Kind::Note => None,
        };

        // No wrapping and no ellipsis: a line of code that runs off the pane is
        // reached by scrolling, the way it is in the editor beside it.
        let label = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .selectable(true)
            .wrap(false)
            .build();
        label.add_css_class("monospace");
        if line.kind == Kind::Note {
            label.add_css_class("dim-label");
        }

        let marker = match line.kind {
            Kind::Added => '+',
            Kind::Removed => '-',
            Kind::Note => '\\',
            Kind::Context => ' ',
        };
        match (spans, background) {
            // Only worth marking when part of the line came through unchanged.
            // A line that changed entirely is already the color it is.
            (Some(spans), Some(background)) if spans.iter().any(|span| !span.changed) => {
                let mut markup = marker.to_string();
                for span in spans {
                    let text = glib::markup_escape_text(&span.text);
                    if span.changed {
                        markup
                            .push_str(&format!("<span background=\"{background}\">{text}</span>"));
                    } else {
                        markup.push_str(&text);
                    }
                }
                label.set_markup(&markup);
            }
            _ => label.set_text(&format!("{marker}{}", line.text)),
        }
        label
    }

    /// What changed inside each pair of lines, by the line it belongs to.
    ///
    /// Only pairs: a line with nothing opposite it changed entirely, and
    /// marking all of it says no more than its color already does.
    fn marked(&self, hunk: &Hunk) -> HashMap<(bool, u32), Vec<Span>> {
        let mut marked = HashMap::new();
        for row in hunk.rows() {
            let (Some(old), Some(new)) = (row.old, row.new) else {
                continue;
            };
            if old.kind != Kind::Removed || new.kind != Kind::Added {
                continue;
            }
            let (before, after) = model::spans(&old.text, &new.text);
            if let Some(number) = old.old {
                marked.insert((false, number), before);
            }
            if let Some(number) = new.new {
                marked.insert((true, number), after);
            }
        }
        marked
    }

    // --- acting on the repository ------------------------------------------

    /// Puts one hunk into the index, or takes it back out.
    ///
    /// The patch is the hunk as it was parsed, and the direction is git's own
    /// `--reverse` rather than a patch written backwards: reversing a patch by
    /// hand is how a staging tool loses a line.
    fn apply_hunk(&self, index: usize) {
        let imp = self.imp();
        if imp.busy.get() {
            return;
        }
        let Some(patch) = imp.diff.borrow().patch(index) else {
            return;
        };
        let root = imp.root.borrow().clone();
        if root.as_os_str().is_empty() {
            return;
        }
        let name = imp
            .path
            .borrow()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Showing the working tree means the button stages; showing the index
        // means it takes back out what is already there.
        let stage = !imp.staged.get();
        let task = git::apply_hunk(patch, &name, stage);

        imp.busy.set(true);
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let done = gio::spawn_blocking(move || git::run_task(&task, &root)).await;
                this.imp().busy.set(false);
                match done {
                    Ok(Ok(_)) => {
                        this.set_message(None);
                        let handler = this.imp().applied.borrow().clone();
                        if let Some(handler) = handler {
                            handler();
                        }
                        this.reload();
                    }
                    Ok(Err(message)) => this.fail(&message),
                    Err(_) => this.fail("Applying the hunk was interrupted."),
                }
            }
        ));
    }

    // --- saying why --------------------------------------------------------

    fn fail(&self, message: &str) {
        // The banner is over content that is now only the last thing that
        // read, so the next read has to redraw even if it reads the same.
        self.imp().loaded.set(false);
        self.set_message(Some(message));
    }

    fn set_message(&self, message: Option<&str>) {
        if let Some(banner) = self.imp().banner.borrow().as_ref() {
            banner.set_title(message.unwrap_or_default());
            banner.set_revealed(message.is_some());
        }
    }

    fn show_message(&self, title: &str, detail: &str) {
        let imp = self.imp();
        if let Some(status) = imp.status.borrow().as_ref() {
            status.set_title(title);
            status.set_description(Some(detail));
        }
        if let Some(stack) = imp.stack.borrow().as_ref() {
            stack.set_visible_child_name(MESSAGE);
        }
    }
}

/// One action on the pane, weakly held so the group does not keep it alive.
fn entry<F>(name: &str, diff: &TuniDiff, activate: F) -> gio::ActionEntry<gio::SimpleActionGroup>
where
    F: Fn(&TuniDiff) + 'static,
{
    let diff = diff.downgrade();
    gio::ActionEntry::builder(name)
        .activate(move |_: &gio::SimpleActionGroup, _, _| {
            if let Some(diff) = diff.upgrade() {
                activate(&diff);
            }
        })
        .build()
}

/// A line that is not there: what fills the empty half of a split row.
fn blank() -> Line {
    Line {
        kind: Kind::Context,
        text: String::new(),
        old: None,
        new: None,
    }
}

/// Which line a set of spans belongs to: the side it is on, and its number
/// there. A line number is unique on its own side of one file.
fn key(line: &Line) -> (bool, u32) {
    match line.kind {
        Kind::Added => (true, line.new.unwrap_or_default()),
        _ => (false, line.old.unwrap_or_default()),
    }
}

/// A line number, or the space where one would be. Four columns is enough for
/// the files a person reads and wide enough not to jump about in the ones they
/// do not.
fn number(value: Option<u32>) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(value.map(|value| value.to_string()).unwrap_or_default())
        .xalign(1.0)
        .width_chars(4)
        .build();
    label.add_css_class("monospace");
    label.add_css_class("numeric");
    label.add_css_class("dim-label");
    label.set_margin_end(6);
    label
}

/// Tints a row by what it is.
fn paint(row: &gtk::Box, kind: Kind) {
    match kind {
        Kind::Added => row.add_css_class("tuni-diff-added"),
        Kind::Removed => row.add_css_class("tuni-diff-removed"),
        Kind::Context | Kind::Note => (),
    }
}

/// Paints every diff pane's added and removed lines in the terminal's colors.
///
/// One provider on the display, reloaded when the theme changes, the same way
/// the chrome and the editor's font are handled: the tint is a setting, and
/// setting it per row would mean a stylesheet per line of every diff on screen.
pub fn apply_colors(theme: &Theme) {
    thread_local! {
        static PROVIDER: gtk::CssProvider = gtk::CssProvider::new();
    }
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let colors = Colors::from(theme);
    let css = format!(
        ".tuni-diff-added {{ background-color: {added}; }}\n\
         .tuni-diff-removed {{ background-color: {removed}; }}\n",
        added = colors.added_row,
        removed = colors.removed_row,
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

/// What was left out, said rather than silently dropped.
fn overflow_row(lines: usize, hunks: usize) -> gtk::Widget {
    let text = if hunks > 0 {
        format!("{hunks} more hunks, {lines} lines. Open the file to see the rest.")
    } else {
        format!("{lines} more lines. Open the file to see the rest.")
    };
    let label = gtk::Label::builder().label(text).xalign(0.0).build();
    label.add_css_class("dim-label");
    label.add_css_class("caption");
    label.set_margin_start(8);
    label.set_margin_top(4);
    label.set_margin_bottom(4);
    label.upcast()
}
