//! The Debug page of the panel: a program, its breakpoints, and where it is.
//!
//! The protocol work is [`tuni_core::dap`]'s; this widget is the session. One
//! at a time, because the page shows one stack and one set of variables, and
//! a second debuggee would need a second page to mean anything. The adapter's
//! frames arrive over the same thread-to-main-loop channel the language
//! servers use, and everything drawn here (the stack, the locals, the output)
//! is a parsed answer, never a guess.
//!
//! Breakpoints live in a registry here rather than in the editors, one record
//! for one thing: the gutter draws what the registry says, the session sends
//! what the registry says, and toggling from a file that is closed and
//! reopened keeps the dot because the record never went anywhere.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use tuni_core::dap;
use tuni_core::dap::Value;

// --- breakpoints -------------------------------------------------------------

thread_local! {
    /// Every breakpoint in the workspace, by file, in one-based lines.
    static BREAKPOINTS: RefCell<HashMap<PathBuf, BTreeSet<usize>>> =
        RefCell::new(HashMap::new());
    /// The running session, if one is, for a toggle to report to.
    static ACTIVE: RefCell<Option<glib::WeakRef<TuniDebugger>>> = const { RefCell::new(None) };
}

/// Flips one line's breakpoint and says whether it is now set. The editor
/// redraws its gutter from [`lines`] afterwards, and a running session hears
/// about it immediately, which is what makes a breakpoint added mid-run land.
pub fn toggle_breakpoint(path: &Path, line: usize) -> bool {
    let set = BREAKPOINTS.with(|breakpoints| {
        let mut breakpoints = breakpoints.borrow_mut();
        let lines = breakpoints.entry(path.to_path_buf()).or_default();
        if lines.remove(&line) {
            if lines.is_empty() {
                breakpoints.remove(path);
            }
            false
        } else {
            lines.insert(line);
            true
        }
    });
    ACTIVE.with(|active| {
        if let Some(session) = active.borrow().as_ref().and_then(glib::WeakRef::upgrade) {
            session.push_breakpoints(path);
        }
    });
    set
}

/// The breakpoints in one file, for the gutter to draw.
#[must_use]
pub fn lines(path: &Path) -> Vec<usize> {
    BREAKPOINTS.with(|breakpoints| {
        breakpoints
            .borrow()
            .get(path)
            .map(|lines| lines.iter().copied().collect())
            .unwrap_or_default()
    })
}

fn all_breakpoints() -> Vec<(PathBuf, Vec<usize>)> {
    BREAKPOINTS.with(|breakpoints| {
        breakpoints
            .borrow()
            .iter()
            .map(|(path, lines)| (path.clone(), lines.iter().copied().collect()))
            .collect()
    })
}

// --- the page ----------------------------------------------------------------

mod imp {
    use super::{Cell, RefCell, dap, glib};
    use adw::subclass::prelude::*;

    #[derive(Default)]
    pub struct TuniDebugger {
        pub client: RefCell<Option<dap::Client>>,
        pub adapter: Cell<Option<&'static dap::Debugger>>,
        /// The thread the last stop pointed at, which is the one every step
        /// and continue is about.
        pub thread: Cell<Option<i64>>,
        pub frames: RefCell<Vec<dap::Frame>>,

        pub program: RefCell<Option<gtk::Entry>>,
        pub arguments: RefCell<Option<gtk::Entry>>,
        pub start: RefCell<Option<gtk::Button>>,
        pub stop: RefCell<Option<gtk::Button>>,
        pub steps: RefCell<Vec<gtk::Button>>,
        pub stack: RefCell<Option<gtk::ListBox>>,
        pub variables: RefCell<Option<gtk::ListBox>>,
        pub output: RefCell<Option<gtk::TextView>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TuniDebugger {
        const NAME: &'static str = "TuniDebugger";
        type Type = super::TuniDebugger;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for TuniDebugger {
        fn constructed(&self) {
            self.parent_constructed();
            crate::debug::born("TuniDebugger");
            self.obj().build();
        }
    }

    impl Drop for TuniDebugger {
        fn drop(&mut self) {
            crate::debug::died("TuniDebugger");
        }
    }

    impl WidgetImpl for TuniDebugger {}
    impl BinImpl for TuniDebugger {}
}

glib::wrapper! {
    pub struct TuniDebugger(ObjectSubclass<imp::TuniDebugger>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for TuniDebugger {
    fn default() -> Self {
        Self::new()
    }
}

impl TuniDebugger {
    #[must_use]
    pub fn new() -> Self {
        glib::Object::new()
    }

    fn build(&self) {
        let imp = self.imp();

        let program = gtk::Entry::builder()
            .placeholder_text("Program to debug")
            .hexpand(true)
            .build();
        let arguments = gtk::Entry::builder().placeholder_text("Arguments").build();
        let start = gtk::Button::builder()
            .icon_name("media-playback-start-symbolic")
            .tooltip_text("Start Debugging")
            .build();
        start.add_css_class("suggested-action");
        start.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.start()
        ));
        program.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.start()
        ));
        let stop = gtk::Button::builder()
            .icon_name("media-playback-stop-symbolic")
            .tooltip_text("Stop Debugging")
            .sensitive(false)
            .build();
        stop.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.stop()
        ));

        let launch_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        launch_row.set_margin_start(8);
        launch_row.set_margin_end(8);
        launch_row.set_margin_top(6);
        launch_row.append(&program);
        launch_row.append(&arguments);
        launch_row.append(&start);
        launch_row.append(&stop);

        // The four verbs of a stopped program, disabled while it runs: a
        // step sent to a running debuggee is an error the adapter answers
        // with, so the buttons say what is possible instead.
        let mut steps = Vec::new();
        let step_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        step_row.set_margin_start(8);
        step_row.set_margin_end(8);
        step_row.set_margin_top(6);
        for (icon, tooltip, action) in [
            ("media-seek-forward-symbolic", "Continue", Step::Continue),
            ("go-next-symbolic", "Step Over", Step::Over),
            ("go-down-symbolic", "Step In", Step::In),
            ("go-up-symbolic", "Step Out", Step::Out),
        ] {
            let button = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tooltip)
                .sensitive(false)
                .build();
            button.connect_clicked(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_| this.step(action)
            ));
            step_row.append(&button);
            steps.push(button);
        }

        let stack = gtk::ListBox::new();
        stack.add_css_class("boxed-list");
        stack.set_selection_mode(gtk::SelectionMode::None);
        stack.connect_row_activated(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, row| this.open_frame(row.index())
        ));

        let variables = gtk::ListBox::new();
        variables.add_css_class("boxed-list");
        variables.set_selection_mode(gtk::SelectionMode::None);

        let output = gtk::TextView::builder()
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .left_margin(8)
            .right_margin(8)
            .build();

        let lists = gtk::Box::new(gtk::Orientation::Vertical, 6);
        lists.set_margin_start(8);
        lists.set_margin_end(8);
        lists.set_margin_top(6);
        lists.append(&section("Stack"));
        lists.append(&stack);
        lists.append(&section("Variables"));
        lists.append(&variables);
        lists.append(&section("Output"));
        lists.append(&output);
        let scroller = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&lists)
            .build();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&launch_row);
        content.append(&step_row);
        content.append(&scroller);
        self.set_child(Some(&content));

        imp.program.replace(Some(program));
        imp.arguments.replace(Some(arguments));
        imp.start.replace(Some(start));
        imp.stop.replace(Some(stop));
        imp.steps.replace(steps);
        imp.stack.replace(Some(stack));
        imp.variables.replace(Some(variables));
        imp.output.replace(Some(output));
    }

    // --- the session ---------------------------------------------------------

    /// Starts debugging what the entry names. Which adapter is the program's
    /// business: a Python file gets debugpy, and anything else is taken for a
    /// native binary and given lldb, because that is what a binary is
    /// whichever of the compiled languages it came from.
    fn start(&self) {
        let imp = self.imp();
        if imp.client.borrow().is_some() {
            return;
        }
        let Some(text) = imp.program.borrow().as_ref().map(|entry| entry.text()) else {
            return;
        };
        let program = PathBuf::from(text.trim());
        if program.as_os_str().is_empty() {
            self.say("Name a program to debug.");
            return;
        }
        let Ok(program) = program.canonicalize() else {
            self.say(&format!("{} does not exist.", program.display()));
            return;
        };
        let language = tuni_core::lsp::language_for_path(&program).map(|language| language.id);
        let adapter = dap::debugger_for_language(language.unwrap_or("rust"));
        let Some(adapter) = adapter else {
            self.say(&format!(
                "No debugger takes a {} file.",
                language.unwrap_or("this")
            ));
            return;
        };
        let Some(command) = dap::available_command(adapter) else {
            self.say(&format!(
                "{} is not installed; it is what debugs this program.",
                adapter.commands[0][0]
            ));
            return;
        };
        let cwd = program.parent().unwrap_or(Path::new("/")).to_path_buf();
        let mut client = match dap::Client::spawn(command, &cwd) {
            Ok(client) => client,
            Err(error) => {
                self.say(&format!("Cannot start {}: {error}", command[0]));
                return;
            }
        };

        let (sender, receiver) = async_channel::unbounded();
        client.start_reader(move |event| {
            let _ = sender.send_blocking(event);
        });
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            while let Ok(event) = receiver.recv().await {
                let Some(this) = weak.upgrade() else {
                    break;
                };
                let exited = matches!(event, dap::Event::Exited);
                this.dispatch(event, &program_key(&this));
                if exited {
                    break;
                }
            }
        });

        client.initialize(adapter.id);
        imp.client.replace(Some(client));
        imp.adapter.set(Some(adapter));
        imp.thread.set(None);
        self.clear_lists();
        self.say(&format!("Debugging {}.", program.display()));
        ACTIVE.with(|active| active.borrow_mut().replace(self.downgrade()));
        self.set_running(true);

        // What launch will need when initialize answers.
        imp.frames.borrow_mut().clear();
        self.set_launch_target(program);
    }

    /// Ends the session from this side: the adapter is asked to take the
    /// debuggee down cleanly, and the teardown happens when it reports back
    /// or dies, whichever comes first.
    fn stop(&self) {
        if let Some(client) = self.imp().client.borrow_mut().as_mut() {
            client.disconnect();
        }
    }

    fn step(&self, step: Step) {
        let imp = self.imp();
        let Some(thread) = imp.thread.get() else {
            return;
        };
        if let Some(client) = imp.client.borrow_mut().as_mut() {
            match step {
                Step::Continue => client.resume(thread),
                Step::Over => client.next(thread),
                Step::In => client.step_in(thread),
                Step::Out => client.step_out(thread),
            };
        }
        if step == Step::Continue {
            self.set_stopped(false);
        }
    }

    /// Resends one file's breakpoints, for a toggle made while running.
    pub fn push_breakpoints(&self, path: &Path) {
        if let Some(client) = self.imp().client.borrow_mut().as_mut() {
            client.set_breakpoints(path, &lines(path));
        }
    }

    fn dispatch(&self, event: dap::Event, program: &Path) {
        let message = match event {
            dap::Event::Message(message) => message,
            dap::Event::Exited => {
                self.teardown("The adapter exited.");
                return;
            }
        };
        match dap::parse_incoming(&message) {
            dap::Incoming::Response {
                request_seq,
                command,
                success,
                message,
                body,
            } => {
                let pending = self
                    .imp()
                    .client
                    .borrow_mut()
                    .as_mut()
                    .and_then(|client| client.take_pending(request_seq));
                if pending.is_none() {
                    return;
                }
                if !success {
                    // A failed step is a line in the log; a failed launch is
                    // the end of the session.
                    self.say(&message.unwrap_or_else(|| format!("{command} failed")));
                    if command == "launch" {
                        self.teardown("Nothing is being debugged.");
                    }
                    return;
                }
                self.answer(&command, &body);
            }
            dap::Incoming::Initialized => {
                if let Some(client) = self.imp().client.borrow_mut().as_mut() {
                    for (path, lines) in all_breakpoints() {
                        client.set_breakpoints(&path, &lines);
                    }
                    client.configuration_done();
                }
            }
            dap::Incoming::Stopped { thread, reason } => {
                self.say(&format!("Stopped: {reason}."));
                self.set_stopped(true);
                let mut client = self.imp().client.borrow_mut();
                let Some(client) = client.as_mut() else {
                    return;
                };
                match thread {
                    Some(thread) => {
                        self.imp().thread.set(Some(thread));
                        client.stack_trace(thread);
                    }
                    // A stop without a thread happens; the thread list says
                    // which one to ask about.
                    None => {
                        client.threads();
                    }
                }
            }
            dap::Incoming::Continued => self.set_stopped(false),
            dap::Incoming::Output { text, category } => {
                if category != "telemetry" {
                    self.append_output(&text);
                }
            }
            dap::Incoming::ProgramExited { code } => {
                self.say(&format!("{} exited with code {code}.", program.display()));
            }
            dap::Incoming::Terminated => self.teardown("Session over."),
            dap::Incoming::Other => {}
        }
    }

    /// A successful response, routed by what was asked.
    fn answer(&self, command: &str, body: &Value) {
        let imp = self.imp();
        match command {
            "initialize" => {
                let adapter = imp.adapter.get();
                let target = self.launch_target();
                let mut client = imp.client.borrow_mut();
                if let (Some(client), Some(adapter), Some(program)) =
                    (client.as_mut(), adapter, target)
                {
                    let arguments = self.arguments_list();
                    let cwd = program.parent().unwrap_or(Path::new("/")).to_path_buf();
                    client.launch(dap::launch_arguments(adapter, &program, &arguments, &cwd));
                }
            }
            "threads" => {
                if let Some(thread) = dap::first_thread(body) {
                    imp.thread.set(Some(thread));
                    if let Some(client) = imp.client.borrow_mut().as_mut() {
                        client.stack_trace(thread);
                    }
                }
            }
            "stackTrace" => {
                let frames = dap::parse_stack(body);
                self.show_stack(&frames);
                if let Some(top) = frames.first() {
                    self.jump_to(top);
                    if let Some(client) = imp.client.borrow_mut().as_mut() {
                        client.scopes(top.id);
                    }
                }
                imp.frames.replace(frames);
            }
            "scopes" => {
                // The first scope is the locals on every adapter here; the
                // registers and statics are a click away in a real debugger
                // and out of scope for a panel.
                if let Some((_, reference)) = dap::parse_scopes(body).into_iter().next()
                    && let Some(client) = imp.client.borrow_mut().as_mut()
                {
                    client.variables(reference);
                }
            }
            "variables" => self.show_variables(&dap::parse_variables(body)),
            _ => {}
        }
    }

    // --- what the session draws ----------------------------------------------

    fn show_stack(&self, frames: &[dap::Frame]) {
        let Some(list) = self.imp().stack.borrow().clone() else {
            return;
        };
        list.remove_all();
        for frame in frames {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&frame.name))
                .build();
            if let Some(path) = &frame.path {
                row.set_subtitle(&glib::markup_escape_text(&format!(
                    "{}:{}",
                    crate::window::shorten(&path.to_string_lossy()),
                    frame.line
                )));
                row.set_activatable(true);
            }
            list.append(&row);
        }
    }

    fn show_variables(&self, variables: &[dap::Variable]) {
        let Some(list) = self.imp().variables.borrow().clone() else {
            return;
        };
        list.remove_all();
        for variable in variables {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&variable.name))
                .subtitle(glib::markup_escape_text(&variable.value))
                .build();
            list.append(&row);
        }
    }

    fn open_frame(&self, index: i32) {
        let frames = self.imp().frames.borrow();
        let Some(frame) = usize::try_from(index).ok().and_then(|i| frames.get(i)) else {
            return;
        };
        self.jump_to(frame);
    }

    fn jump_to(&self, frame: &dap::Frame) {
        let Some(path) = &frame.path else {
            return;
        };
        if let Some(window) = self.root().and_downcast::<crate::window::TuniWindow>() {
            window.open_file_at(path, frame.line.max(1));
        }
    }

    fn append_output(&self, text: &str) {
        if let Some(view) = self.imp().output.borrow().as_ref() {
            let buffer = view.buffer();
            buffer.insert(&mut buffer.end_iter(), text);
        }
    }

    /// A line of the panel's own, marked off from the program's output.
    fn say(&self, text: &str) {
        self.append_output(&format!("· {text}\n"));
    }

    fn set_running(&self, running: bool) {
        let imp = self.imp();
        if let Some(start) = imp.start.borrow().as_ref() {
            start.set_sensitive(!running);
        }
        if let Some(stop) = imp.stop.borrow().as_ref() {
            stop.set_sensitive(running);
        }
        if !running {
            self.set_stopped(false);
        }
    }

    fn set_stopped(&self, stopped: bool) {
        for button in self.imp().steps.borrow().iter() {
            button.set_sensitive(stopped);
        }
        if !stopped {
            self.imp().thread.set(None);
        }
    }

    fn clear_lists(&self) {
        let imp = self.imp();
        if let Some(stack) = imp.stack.borrow().as_ref() {
            stack.remove_all();
        }
        if let Some(variables) = imp.variables.borrow().as_ref() {
            variables.remove_all();
        }
        if let Some(view) = imp.output.borrow().as_ref() {
            view.buffer().set_text("");
        }
    }

    fn teardown(&self, epitaph: &str) {
        let imp = self.imp();
        if imp.client.borrow().is_none() {
            return;
        }
        imp.client.replace(None);
        imp.thread.set(None);
        self.set_running(false);
        self.say(epitaph);
        ACTIVE.with(|active| active.borrow_mut().take());
    }

    // The launch target rides in the program entry between initialize and
    // launch, canonicalized, so the session has one truth about what it runs.
    fn set_launch_target(&self, program: PathBuf) {
        if let Some(entry) = self.imp().program.borrow().as_ref() {
            entry.set_text(&program.to_string_lossy());
        }
    }

    fn launch_target(&self) -> Option<PathBuf> {
        let text = self
            .imp()
            .program
            .borrow()
            .as_ref()
            .map(|entry| entry.text())?;
        let trimmed = text.trim();
        (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
    }

    fn arguments_list(&self) -> Vec<String> {
        self.imp()
            .arguments
            .borrow()
            .as_ref()
            .map(|entry| entry.text())
            .map(|text| text.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default()
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Step {
    Continue,
    Over,
    In,
    Out,
}

fn section(title: &str) -> gtk::Label {
    let label = gtk::Label::builder().label(title).xalign(0.0).build();
    label.add_css_class("heading");
    label.add_css_class("dim-label");
    label
}

fn program_key(debugger: &TuniDebugger) -> PathBuf {
    debugger.launch_target().unwrap_or_default()
}
