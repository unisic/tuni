//! Language servers behind the editor panes.
//!
//! One server per project root and language, shared by every editor showing a
//! file of that kind, because that is the unit a server thinks in: the
//! `rust-analyzer` for this checkout knows every crate in it, and a second one
//! for a second pane would burn a core learning the same workspace. The pool
//! is keyed accordingly and lives at module scope: servers outlive tabs and
//! belong to no window.
//!
//! The protocol work (spawning, framing, positions, parsing) is
//! [`tuni_core::lsp`]'s. This file is the traffic: a reader thread hands
//! frames to the main loop over an async channel the way PTY output arrives,
//! and the answers are routed to sourceview's own completion and hover
//! machinery, which already knows how to draw a popup, filter it as the word
//! grows, and put an edit back in the buffer.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use tuni_core::lsp;
use tuni_core::lsp::Value;

use crate::editor::TuniEditor;

/// How long after the last keystroke the server hears about the buffer. Long
/// enough to sit out a burst of typing, short enough that diagnostics answer
/// the line just written rather than the one before it.
const SYNC_DELAY: Duration = Duration::from_millis(300);

/// How many completions are worth carrying to the popup. The servers rank
/// their answers, so what the cap drops is the tail nobody scrolls to.
const COMPLETION_CAP: usize = 200;

thread_local! {
    /// The running servers, by project root and language id. Main thread only,
    /// like everything else that touches a [`Server`].
    static POOL: RefCell<HashMap<(PathBuf, &'static str), Rc<Server>>> =
        RefCell::new(HashMap::new());
}

/// One diagnostic, converted to buffer terms: lines and character offsets.
#[derive(Clone, Debug)]
pub struct Shown {
    pub start: (usize, usize),
    pub end: (usize, usize),
    pub severity: lsp::Severity,
    pub message: String,
}

/// What an editor holds while a server knows about its file. Dropping it says
/// goodbye: the document closes, and a server left with no documents is shut
/// down rather than kept warm for a tab that may never reopen.
pub struct Attachment {
    server: Rc<Server>,
    path: PathBuf,
}

impl Drop for Attachment {
    fn drop(&mut self) {
        self.server.release(&self.path);
    }
}

#[derive(Clone)]
pub(crate) struct Wired {
    server: Rc<Server>,
    path: PathBuf,
}

struct Server {
    key: (PathBuf, &'static str),
    connection: RefCell<lsp::Connection>,
    /// False until `initialize` answers; opens sent meanwhile wait in
    /// `queued`, because a server must not hear about a file before it has
    /// agreed on how positions are counted.
    ready: std::cell::Cell<bool>,
    queued: RefCell<Vec<(PathBuf, String)>>,
    /// Which editor shows which URI, for routing diagnostics. Weak: a closed
    /// pane is not kept alive by the server that was helping it.
    editors: RefCell<HashMap<String, glib::WeakRef<TuniEditor>>>,
    /// Futures waiting for an answer, by request id. A waiter that has been
    /// dropped, the popup closed or the pointer moved on, just loses interest,
    /// and the answer is dropped with it.
    waiters: RefCell<HashMap<i64, async_channel::Sender<Value>>>,
}

// --- what the editor calls ---------------------------------------------------

/// Puts a file under a server's care, if any server covers it. Safe to call
/// for every file that opens: a README simply finds no language in the table.
pub fn attach(editor: &TuniEditor, path: &Path, text: &str) {
    detach(editor);
    let Some(language) = lsp::language_for_path(path) else {
        return;
    };
    let Some(server) = server_for(path, language) else {
        return;
    };

    if server.ready.get() {
        server.connection.borrow_mut().open(path, language.id, text);
    } else {
        server
            .queued
            .borrow_mut()
            .push((path.to_path_buf(), text.to_owned()));
    }

    let uri = lsp::uri_from_path(path);
    let weak = glib::WeakRef::new();
    weak.set(Some(editor));
    server.editors.borrow_mut().insert(uri, weak);

    let wired = Wired {
        server: Rc::clone(&server),
        path: path.to_path_buf(),
    };
    if let Some(source) = editor.imp().lsp_completion.borrow().as_ref() {
        source.imp().wired.replace(Some(wired.clone()));
    }
    if let Some(source) = editor.imp().lsp_hover.borrow().as_ref() {
        source.imp().wired.replace(Some(wired));
    }
    editor.imp().lsp.replace(Some(Attachment {
        server,
        path: path.to_path_buf(),
    }));
}

/// Takes the file back out of the server's world. The editor keeps working;
/// it just stops being told things.
pub fn detach(editor: &TuniEditor) {
    let imp = editor.imp();
    if let Some(pending) = imp.lsp_sync.take() {
        pending.remove();
    }
    if let Some(source) = imp.lsp_completion.borrow().as_ref() {
        source.imp().wired.replace(None);
    }
    if let Some(source) = imp.lsp_hover.borrow().as_ref() {
        source.imp().wired.replace(None);
    }
    if imp.lsp.take().is_some() {
        editor.show_diagnostics(Vec::new());
    }
}

/// Called on every buffer edit. The actual send waits [`SYNC_DELAY`] behind
/// the newest keystroke, so a burst of typing is one message, not thirty.
pub fn changed(editor: &TuniEditor) {
    let imp = editor.imp();
    if imp.filling.get() || imp.lsp.borrow().is_none() {
        return;
    }
    if let Some(previous) = imp.lsp_sync.take() {
        previous.remove();
    }
    let weak = editor.downgrade();
    let source = glib::timeout_add_local_once(SYNC_DELAY, move || {
        let Some(editor) = weak.upgrade() else {
            return;
        };
        editor.imp().lsp_sync.replace(None);
        push_text(&editor);
    });
    imp.lsp_sync.replace(Some(source));
}

/// Called after a successful save. The buffer goes over first, since the
/// debounce may still be holding the last edits, so the server never hears "saved"
/// about text it has not seen.
pub fn saved(editor: &TuniEditor) {
    let imp = editor.imp();
    if let Some(pending) = imp.lsp_sync.take() {
        pending.remove();
    }
    let Some(wired) = wired(editor) else {
        return;
    };
    let Some(text) = editor.text() else {
        return;
    };
    let mut connection = wired.server.connection.borrow_mut();
    connection.change(&wired.path, &text);
    connection.saved(&wired.path);
}

/// Asks where the thing at a position is defined and goes there: another file
/// opens in a pane, the same file just moves its cursor. Nothing happens on a
/// position the server has no answer for, which is most of them.
pub fn definition(editor: &TuniEditor, line: usize, character: usize) {
    let Some(wired) = wired(editor) else {
        return;
    };
    let weak = editor.downgrade();
    glib::spawn_future_local(async move {
        let Some((target, line)) = Rc::clone(&wired.server)
            .definition(wired.path.clone(), line, character)
            .await
        else {
            return;
        };
        let Some(editor) = weak.upgrade() else {
            return;
        };
        if target == wired.path {
            editor.set_line(line + 1);
            return;
        }
        if let Some(window) = editor.root().and_downcast::<crate::window::TuniWindow>() {
            window.open_file_at(&target, line + 1);
        }
    });
}

fn push_text(editor: &TuniEditor) {
    let Some(wired) = wired(editor) else {
        return;
    };
    let Some(text) = editor.text() else {
        return;
    };
    wired
        .server
        .connection
        .borrow_mut()
        .change(&wired.path, &text);
}

fn wired(editor: &TuniEditor) -> Option<Wired> {
    editor.imp().lsp.borrow().as_ref().map(|attachment| Wired {
        server: Rc::clone(&attachment.server),
        path: attachment.path.clone(),
    })
}

/// A clamped iterator at a line and character offset. Positions come from a
/// server whose copy of the file may trail the buffer by a keystroke, so
/// anything past an end lands on the end instead of panicking GTK.
pub fn place(buffer: &impl IsA<gtk::TextBuffer>, line: usize, character: usize) -> gtk::TextIter {
    let line = i32::try_from(line).unwrap_or(i32::MAX);
    let Some(mut iter) = buffer.iter_at_line(line) else {
        return buffer.end_iter();
    };
    let mut remaining = character;
    while remaining > 0 && !iter.ends_line() {
        if !iter.forward_char() {
            break;
        }
        remaining -= 1;
    }
    iter
}

// --- the pool ----------------------------------------------------------------

fn server_for(path: &Path, language: &'static lsp::Language) -> Option<Rc<Server>> {
    let root = lsp::find_root(path, language);
    let key = (root.clone(), language.id);
    if let Some(server) = POOL.with(|pool| pool.borrow().get(&key).cloned()) {
        return Some(server);
    }

    let command = lsp::available_command(language)?;
    let mut connection = lsp::Connection::spawn(command, &root).ok()?;
    connection.initialize(&root);

    let server = Rc::new(Server {
        key: key.clone(),
        connection: RefCell::new(connection),
        ready: std::cell::Cell::new(false),
        queued: RefCell::new(Vec::new()),
        editors: RefCell::new(HashMap::new()),
        waiters: RefCell::new(HashMap::new()),
    });

    // The reader thread must not touch GTK, so it only feeds the channel; the
    // local future on the other end does the routing on the main thread.
    let (sender, receiver) = async_channel::unbounded();
    server.connection.borrow_mut().start_reader(move |event| {
        let _ = sender.send_blocking(event);
    });
    let routed = Rc::clone(&server);
    glib::spawn_future_local(async move {
        while let Ok(event) = receiver.recv().await {
            let exited = matches!(event, lsp::Event::Exited);
            routed.dispatch(event);
            if exited {
                break;
            }
        }
    });

    POOL.with(|pool| pool.borrow_mut().insert(key, Rc::clone(&server)));
    Some(server)
}

impl Server {
    fn dispatch(self: &Rc<Self>, event: lsp::Event) {
        let message = match event {
            lsp::Event::Message(message) => message,
            lsp::Event::Exited => {
                // A crashed server takes its promises with it: every waiting
                // future gets its "no answer", every mark comes off the
                // screen, and the pool forgets the key so the next file can
                // try a fresh start.
                for (_, waiter) in self.waiters.borrow_mut().drain() {
                    let _ = waiter.try_send(Value::Null);
                }
                for editor in self.editors.borrow().values() {
                    if let Some(editor) = editor.upgrade() {
                        editor.show_diagnostics(Vec::new());
                    }
                }
                self.forget();
                return;
            }
        };
        match lsp::parse_incoming(&message) {
            lsp::Incoming::Response { id, result, error } => {
                let pending = self.connection.borrow_mut().take_pending(id);
                match pending {
                    Some(lsp::Pending::Initialize) => {
                        if error.is_some() {
                            self.forget();
                            return;
                        }
                        self.connection.borrow_mut().finish_initialize(&result);
                        self.ready.set(true);
                        let queued = std::mem::take(&mut *self.queued.borrow_mut());
                        for (path, text) in queued {
                            self.connection.borrow_mut().open(&path, self.key.1, &text);
                        }
                    }
                    Some(_) => {
                        if let Some(waiter) = self.waiters.borrow_mut().remove(&id) {
                            let answer = if error.is_some() { Value::Null } else { result };
                            let _ = waiter.try_send(answer);
                        }
                    }
                    None => {}
                }
            }
            lsp::Incoming::Diagnostics { uri, diagnostics } => {
                let editor = self
                    .editors
                    .borrow()
                    .get(&uri)
                    .and_then(glib::WeakRef::upgrade);
                let Some(editor) = editor else {
                    return;
                };
                let shown = {
                    let connection = self.connection.borrow();
                    diagnostics
                        .iter()
                        .filter_map(|diagnostic| {
                            Some(Shown {
                                start: connection.to_char(&uri, diagnostic.start)?,
                                end: connection.to_char(&uri, diagnostic.end)?,
                                severity: diagnostic.severity,
                                message: match &diagnostic.source {
                                    Some(source) => {
                                        format!("{source}: {}", diagnostic.message)
                                    }
                                    None => diagnostic.message.clone(),
                                },
                            })
                        })
                        .collect()
                };
                editor.show_diagnostics(shown);
            }
            lsp::Incoming::Message { .. } | lsp::Incoming::Other => {}
        }
    }

    fn release(&self, path: &Path) {
        let uri = lsp::uri_from_path(path);
        self.editors.borrow_mut().remove(&uri);
        let mut connection = self.connection.borrow_mut();
        connection.close(path);
        if connection.is_idle() {
            connection.shutdown();
            drop(connection);
            self.forget();
        }
    }

    /// Takes this server out of the pool, if it is still the one in it.
    fn forget(&self) {
        POOL.with(|pool| {
            let mut pool = pool.borrow_mut();
            if pool
                .get(&self.key)
                .is_some_and(|entry| std::ptr::eq(entry.as_ref(), self))
            {
                pool.remove(&self.key);
            }
        });
    }

    /// Waits for the answer to one request. Resolves to `Null`, meaning "no
    /// answer", if the server dies first.
    async fn response(self: Rc<Self>, id: i64) -> Value {
        let (sender, receiver) = async_channel::bounded(1);
        self.waiters.borrow_mut().insert(id, sender);
        receiver.recv().await.unwrap_or(Value::Null)
    }

    async fn hover(self: Rc<Self>, path: PathBuf, line: usize, character: usize) -> Option<String> {
        let id = self.connection.borrow_mut().hover(&path, line, character)?;
        let result = Rc::clone(&self).response(id).await;
        lsp::hover_text(&result)
    }

    async fn completions(
        self: Rc<Self>,
        path: PathBuf,
        line: usize,
        character: usize,
    ) -> Vec<Item> {
        let id = self
            .connection
            .borrow_mut()
            .completion(&path, line, character);
        let Some(id) = id else {
            return Vec::new();
        };
        let result = Rc::clone(&self).response(id).await;
        let uri = lsp::uri_from_path(&path);
        let connection = self.connection.borrow();
        let mut items: Vec<Item> = lsp::parse_completions(&result)
            .into_iter()
            .map(|completion| Item {
                start: completion
                    .start
                    .and_then(|position| connection.to_char(&uri, position)),
                end: completion
                    .end
                    .and_then(|position| connection.to_char(&uri, position)),
                label: completion.label,
                insert: completion.insert,
                detail: completion.detail,
                sort: completion.sort,
                filter: completion.filter,
            })
            .collect();
        drop(connection);
        items.sort_by(|a, b| a.sort.cmp(&b.sort));
        items.truncate(COMPLETION_CAP);
        items
    }

    async fn definition(
        self: Rc<Self>,
        path: PathBuf,
        line: usize,
        character: usize,
    ) -> Option<(PathBuf, usize)> {
        let id = self
            .connection
            .borrow_mut()
            .definition(&path, line, character)?;
        let result = Rc::clone(&self).response(id).await;
        lsp::parse_definition(&result)
            .into_iter()
            .find_map(|location| {
                let target = lsp::path_from_uri(&location.uri)?;
                Some((target, location.line))
            })
    }
}

/// One completion after position conversion: everything in buffer terms.
#[derive(Clone, Debug)]
pub struct Item {
    label: String,
    insert: String,
    detail: Option<String>,
    start: Option<(usize, usize)>,
    end: Option<(usize, usize)>,
    sort: String,
    filter: String,
}

// --- sourceview integration --------------------------------------------------

mod imp {
    use super::glib::subclass::prelude::*;
    use super::{Rc, RefCell, Wired, glib};
    use gtk::prelude::*;
    use sourceview5::subclass::prelude::*;

    /// A proposal is a GObject because the popup's list model needs one; the
    /// object is just the [`super::Item`] in a coat.
    #[derive(Default)]
    pub struct LspProposal {
        pub item: RefCell<Option<super::Item>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LspProposal {
        const NAME: &'static str = "TuniLspProposal";
        type Type = super::LspProposal;
        type Interfaces = (sourceview5::CompletionProposal,);
    }

    impl ObjectImpl for LspProposal {}
    impl CompletionProposalImpl for LspProposal {}

    #[derive(Default)]
    pub struct CompletionSource {
        pub(super) wired: RefCell<Option<Wired>>,
        /// What the user has typed of the word, lowercased; the filter closure
        /// reads it through the shared cell so refiltering is a nudge, not a
        /// new model.
        pub word: Rc<RefCell<String>>,
        pub filter: RefCell<Option<gtk::CustomFilter>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CompletionSource {
        const NAME: &'static str = "TuniLspCompletionSource";
        type Type = super::CompletionSource;
        type Interfaces = (sourceview5::CompletionProvider,);
    }

    impl ObjectImpl for CompletionSource {}

    impl CompletionProviderImpl for CompletionSource {
        fn populate_future(
            &self,
            context: &sourceview5::CompletionContext,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<gtk::gio::ListModel, glib::Error>>>,
        > {
            let wired = self.wired.borrow().clone();
            // The position is the end of what was typed: that is where the
            // cursor sits, and where the server expects to complete.
            let position = context.bounds().map(|(_, end)| {
                (
                    end.line().max(0) as usize,
                    end.line_offset().max(0) as usize,
                )
            });
            let word = Rc::clone(&self.word);
            *word.borrow_mut() = context.word().to_lowercase();
            let filter = gtk::CustomFilter::new({
                let word = Rc::clone(&word);
                move |object| {
                    let word = word.borrow();
                    if word.is_empty() {
                        return true;
                    }
                    object
                        .downcast_ref::<super::LspProposal>()
                        .and_then(|proposal| proposal.imp().item.borrow().clone())
                        .is_some_and(|item| item.filter.to_lowercase().starts_with(&*word))
                }
            });
            self.filter.replace(Some(filter.clone()));

            Box::pin(async move {
                let store = gtk::gio::ListStore::new::<super::LspProposal>();
                if let (Some(wired), Some((line, character))) = (wired, position) {
                    let items = Rc::clone(&wired.server)
                        .completions(wired.path.clone(), line, character)
                        .await;
                    for item in items {
                        store.append(&super::LspProposal::with(item));
                    }
                }
                let model = gtk::FilterListModel::new(Some(store), Some(filter));
                Ok(model.upcast())
            })
        }

        /// Typing while the popup is open narrows it here, without asking the
        /// server again: the answer it already gave covers the longer word.
        fn refilter(&self, context: &sourceview5::CompletionContext, _model: &gtk::gio::ListModel) {
            *self.word.borrow_mut() = context.word().to_lowercase();
            if let Some(filter) = self.filter.borrow().as_ref() {
                filter.changed(gtk::FilterChange::Different);
            }
        }

        fn display(
            &self,
            _context: &sourceview5::CompletionContext,
            proposal: &sourceview5::CompletionProposal,
            cell: &sourceview5::CompletionCell,
        ) {
            let Some(proposal) = proposal.downcast_ref::<super::LspProposal>() else {
                return;
            };
            let item = proposal.imp().item.borrow();
            let Some(item) = item.as_ref() else {
                return;
            };
            match cell.column() {
                sourceview5::CompletionColumn::TypedText => cell.set_text(Some(&item.label)),
                sourceview5::CompletionColumn::Comment => cell.set_text(item.detail.as_deref()),
                _ => cell.set_text(None),
            }
        }

        fn activate(
            &self,
            context: &sourceview5::CompletionContext,
            proposal: &sourceview5::CompletionProposal,
        ) {
            let Some(proposal) = proposal.downcast_ref::<super::LspProposal>() else {
                return;
            };
            let Some(item) = proposal.imp().item.borrow().clone() else {
                return;
            };
            let Some(buffer) = context.buffer() else {
                return;
            };
            // The server's replace range when it gave one, which is how
            // completing "push_" mid-word replaces the whole word, and the
            // typed word's bounds when it did not.
            let (mut start, mut end) = match (item.start, item.end) {
                (Some(from), Some(to)) => (
                    super::place(&buffer, from.0, from.1),
                    super::place(&buffer, to.0, to.1),
                ),
                _ => match context.bounds() {
                    Some(bounds) => bounds,
                    None => {
                        let iter = buffer.iter_at_mark(&buffer.get_insert());
                        (iter, iter)
                    }
                },
            };
            buffer.begin_user_action();
            buffer.delete(&mut start, &mut end);
            buffer.insert(&mut start, &item.insert);
            buffer.end_user_action();
        }

        /// The characters that ask for completion by themselves: a member
        /// access or a path separator is a question even without Ctrl+Space.
        fn is_trigger(&self, iter: &gtk::TextIter, c: char) -> bool {
            if c == '.' {
                return true;
            }
            let mut before = *iter;
            if !before.backward_char() {
                return false;
            }
            // The iterator sits after the inserted character; step past it to
            // see what came before.
            if before.char() == c && !before.backward_char() {
                return false;
            }
            matches!((before.char(), c), (':', ':') | ('-', '>'))
        }
    }

    #[derive(Default)]
    pub struct HoverSource {
        pub(super) wired: RefCell<Option<Wired>>,
        /// The editor whose diagnostics share the popover, weakly: the hover
        /// belongs to the view, not the other way around.
        pub editor: glib::WeakRef<super::TuniEditor>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for HoverSource {
        const NAME: &'static str = "TuniLspHoverSource";
        type Type = super::HoverSource;
        type Interfaces = (sourceview5::HoverProvider,);
    }

    impl ObjectImpl for HoverSource {}

    impl HoverProviderImpl for HoverSource {
        fn populate_future(
            &self,
            context: &sourceview5::HoverContext,
            display: &sourceview5::HoverDisplay,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), glib::Error>>>> {
            let wired = self.wired.borrow().clone();
            let editor = self.editor.upgrade();
            let position = context.bounds().map(|(start, _)| {
                (
                    start.line().max(0) as usize,
                    start.line_offset().max(0) as usize,
                )
            });
            let display = display.clone();
            Box::pin(async move {
                let Some((line, character)) = position else {
                    return Ok(());
                };
                // What is wrong with this spot first, then what it is. The
                // diagnostics are already here; the explanation takes a trip
                // to the server.
                if let Some(editor) = editor {
                    for diagnostic in editor.diagnostics_at(line, character) {
                        display.append(&super::hover_label(
                            &diagnostic.message,
                            match diagnostic.severity {
                                tuni_core::lsp::Severity::Error => "error",
                                tuni_core::lsp::Severity::Warning => "warning",
                                _ => "dim-label",
                            },
                        ));
                    }
                }
                if let Some(wired) = wired
                    && let Some(text) = Rc::clone(&wired.server)
                        .hover(wired.path.clone(), line, character)
                        .await
                {
                    let text = super::plain(&text);
                    if !text.is_empty() {
                        display.append(&super::hover_label(&text, "monospace"));
                    }
                }
                Ok(())
            })
        }
    }
}

glib::wrapper! {
    pub struct LspProposal(ObjectSubclass<imp::LspProposal>)
        @implements sourceview5::CompletionProposal;
}

impl LspProposal {
    fn with(item: Item) -> Self {
        let proposal: Self = glib::Object::new();
        proposal.imp().item.replace(Some(item));
        proposal
    }
}

glib::wrapper! {
    pub struct CompletionSource(ObjectSubclass<imp::CompletionSource>)
        @implements sourceview5::CompletionProvider;
}

impl Default for CompletionSource {
    fn default() -> Self {
        glib::Object::new()
    }
}

glib::wrapper! {
    pub struct HoverSource(ObjectSubclass<imp::HoverSource>)
        @implements sourceview5::HoverProvider;
}

impl HoverSource {
    pub fn for_editor(editor: &TuniEditor) -> Self {
        let source: Self = glib::Object::new();
        source.imp().editor.set(Some(editor));
        source
    }
}

/// One block of the hover popover.
fn hover_label(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .wrap(true)
        .max_width_chars(80)
        .xalign(0.0)
        .build();
    label.add_css_class(class);
    label
}

/// Markdown reduced to something one label can say: fences drop, since the
/// text between them is already code, and blank runs collapse. A tooltip is not
/// the place to render a document, but it should not show the scaffolding
/// either.
fn plain(markdown: &str) -> String {
    let mut out = String::new();
    let mut blank = true;
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            continue;
        }
        if line.trim().is_empty() {
            if !blank {
                out.push('\n');
            }
            blank = true;
            continue;
        }
        if blank && !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line.trim_end());
        out.push('\n');
        blank = false;
    }
    let out = out.trim_end();
    // A hover that scrolls is a document; cut it where a reader stops.
    match out.char_indices().nth(1500) {
        Some((cut, _)) => format!("{}…", &out[..cut]),
        None => out.to_owned(),
    }
}
