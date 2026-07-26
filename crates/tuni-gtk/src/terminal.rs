//! The terminal widget: a `GtkWidget` subclass that owns one VT state machine
//! and one PTY, and paints the viewport with Pango.
//!
//! Rendering follows Ghostty-on-Linux practice rather than a GPU atlas: Pango
//! plus fontconfig gives subpixel antialiasing, system font fallback, and IME
//! for free, and text quality is the thing a terminal is judged on. Whether
//! this keeps up under a firehose is exactly what the Etap 0 benchmark decides.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use gtk::gdk;
use gtk::glib;
use gtk::graphene;
use gtk::pango;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use unicode_width::UnicodeWidthStr;

use tuni_core::TerminalConfig;
use tuni_pty::{Pty, PtyConfig, PtyEvent};
use tuni_vt::{
    ClipboardTarget, CursorShape, Geometry, KeyAction, KeyInput, Mods, MouseAction, MouseButton,
    MouseInput, Rgb,
};

use crate::keymap;

/// What the pointer is doing between press and release. A drag either paints a
/// selection or is reported to the application; never both.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Pointer {
    #[default]
    Idle,
    Selecting,
    Reporting,
}

/// Cell geometry derived from the font. Everything on screen is placed off
/// these two numbers, so a terminal stays a grid even when the font lies about
/// being monospace.
#[derive(Clone, Copy, Debug)]
struct Metrics {
    cell_width: f32,
    cell_height: f32,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            cell_width: 8.0,
            cell_height: 16.0,
        }
    }
}

struct Session {
    term: tuni_vt::Terminal,
    pty: Pty,
}

mod imp {
    use super::*;

    pub struct TuniTerminal {
        pub(super) session: RefCell<Option<Session>>,
        pub(super) config: RefCell<TerminalConfig>,
        pub(super) font: RefCell<pango::FontDescription>,
        pub(super) metrics: Cell<Metrics>,
        /// Grid size last pushed to the VT and the PTY, so a resize that does
        /// not cross a cell boundary costs nothing.
        pub(super) grid_size: Cell<(u16, u16)>,
        pub(super) im: RefCell<Option<gtk::IMMulticontext>>,
        /// Text the input method committed during the current key press.
        pub(super) pending_commit: RefCell<Option<String>>,
        pub(super) title: RefCell<Option<String>>,
        pub(super) pointer: Cell<Pointer>,
        /// How many mouse buttons are down, which mouse reporting needs in
        /// order to decide whether motion is worth sending.
        pub(super) buttons_down: Cell<u8>,
        /// Last known pointer position, because scroll events carry no
        /// coordinates but the application still expects them in the report.
        pub(super) pointer_pos: Cell<(f64, f64)>,
        /// Draw timings, collected only when TUNI_DEBUG_FRAME_TIME is set.
        /// This is the measurement that decides whether Pango can keep up.
        pub(super) frame_timing: bool,
        pub(super) frame_times: RefCell<Vec<std::time::Duration>>,
    }

    impl Default for TuniTerminal {
        fn default() -> Self {
            Self {
                session: RefCell::new(None),
                config: RefCell::new(TerminalConfig::default()),
                font: RefCell::new(pango::FontDescription::new()),
                metrics: Cell::new(Metrics::default()),
                grid_size: Cell::new((0, 0)),
                im: RefCell::new(None),
                pending_commit: RefCell::new(None),
                title: RefCell::new(None),
                pointer: Cell::new(Pointer::default()),
                buttons_down: Cell::new(0),
                pointer_pos: Cell::new((0.0, 0.0)),
                frame_timing: std::env::var_os("TUNI_DEBUG_FRAME_TIME").is_some(),
                frame_times: RefCell::new(Vec::with_capacity(120)),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TuniTerminal {
        const NAME: &'static str = "TuniTerminal";
        type Type = super::TuniTerminal;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for TuniTerminal {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPERTIES: std::sync::OnceLock<Vec<glib::ParamSpec>> =
                std::sync::OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![glib::ParamSpecString::builder("title").read_only().build()]
            })
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "title" => self.title.borrow().to_value(),
                other => unimplemented!("unknown property {other}"),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_focusable(true);
            obj.set_can_focus(true);
            obj.set_focus_on_click(true);
            obj.setup_font();
            obj.setup_input();
        }

        fn dispose(&self) {
            // Drop the session before the widget so the reader thread's channel
            // closes and the shell gets its SIGHUP.
            self.session.replace(None);
        }
    }

    impl WidgetImpl for TuniTerminal {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let m = self.metrics.get();
            // A terminal has no natural size worth defending; ask for a usable
            // minimum and let the window decide.
            let (min, nat) = match orientation {
                gtk::Orientation::Horizontal => (m.cell_width * 20.0, m.cell_width * 80.0),
                _ => (m.cell_height * 5.0, m.cell_height * 24.0),
            };
            (min.ceil() as i32, nat.ceil() as i32, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            self.obj().apply_size(width, height);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if !self.frame_timing {
                self.obj().draw(snapshot);
                return;
            }

            let started = std::time::Instant::now();
            self.obj().draw(snapshot);
            let elapsed = started.elapsed();

            let mut frames = self.frame_times.borrow_mut();
            frames.push(elapsed);
            if frames.len() == 120 {
                frames.sort_unstable();
                eprintln!(
                    "frame: p50 {:?}  p95 {:?}  max {:?}",
                    frames[60],
                    frames[114],
                    frames[119]
                );
                frames.clear();
            }
        }
    }
}

glib::wrapper! {
    pub struct TuniTerminal(ObjectSubclass<imp::TuniTerminal>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for TuniTerminal {
    fn default() -> Self {
        Self::new()
    }
}

impl TuniTerminal {
    #[must_use]
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Write text straight to the shell, bypassing key encoding. Paste and
    /// scripted smoke tests both need this.
    pub fn send_text(&self, text: &str) {
        let mut guard = self.imp().session.borrow_mut();
        if let Some(session) = guard.as_mut() {
            let _ = session.pty.write(text.as_bytes());
            session.term.scroll_to_bottom();
        }
    }

    #[must_use]
    pub fn title(&self) -> Option<String> {
        self.imp().title.borrow().clone()
    }

    // --- setup -------------------------------------------------------------

    fn setup_font(&self) {
        let imp = self.imp();
        let config = imp.config.borrow();

        let mut font = pango::FontDescription::new();
        // A family list, so a missing JetBrains Mono degrades to the system
        // monospace instead of to a proportional default.
        font.set_family(&format!("{}, monospace", config.font_family));
        font.set_size((config.font_size * f64::from(pango::SCALE)).round() as i32);
        drop(config);

        imp.font.replace(font);
        self.update_metrics();
    }

    fn update_metrics(&self) {
        let imp = self.imp();
        let context = self.pango_context();
        let font = imp.font.borrow();
        let metrics = context.metrics(Some(&font), None);

        let scale = f32::from(pango::SCALE as i16);
        let ascent = metrics.ascent() as f32 / scale;
        let descent = metrics.descent() as f32 / scale;
        // `approximate_char_width` is the advance of a typical character, which
        // for a monospace face is the cell width.
        let cell_width = metrics.approximate_char_width() as f32 / scale;
        let extra = imp.config.borrow().line_height_extra as f32;

        imp.metrics.set(Metrics {
            cell_width: cell_width.max(1.0),
            cell_height: (ascent + descent + extra).max(1.0),
        });
    }

    fn setup_input(&self) {
        let im = gtk::IMMulticontext::new();
        im.set_client_widget(Some(self));

        im.connect_commit(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, text| {
                this.imp()
                    .pending_commit
                    .replace(Some(text.to_owned()));
            }
        ));
        self.imp().im.replace(Some(im));

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |controller, keyval, _keycode, state| this.on_key(controller, keyval, state)
        ));
        keys.connect_key_released(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |controller, _, _, _| {
                if let Some(im) = this.imp().im.borrow().as_ref()
                    && let Some(event) = controller.current_event()
                {
                    im.filter_keypress(&event);
                }
            }
        ));
        self.add_controller(keys);

        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                if let Some(im) = this.imp().im.borrow().as_ref() {
                    im.focus_in();
                }
                this.queue_draw();
            }
        ));
        focus.connect_leave(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                if let Some(im) = this.imp().im.borrow().as_ref() {
                    im.focus_out();
                }
                this.queue_draw();
            }
        ));
        self.add_controller(focus);

        let click = gtk::GestureClick::new();
        // Every button, not just the primary one: middle pastes the primary
        // selection and applications want the others reported.
        click.set_button(0);
        click.connect_pressed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |gesture, _, x, y| this.on_pointer_press(gesture, x, y)
        ));
        click.connect_released(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |gesture, _, x, y| this.on_pointer_release(gesture, x, y)
        ));
        self.add_controller(click);

        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |controller, x, y| this.on_pointer_motion(controller, x, y)
        ));
        self.add_controller(motion);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.connect_scroll(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |controller, _, dy| {
                this.on_scroll(controller, dy);
                glib::Propagation::Stop
            }
        ));
        self.add_controller(scroll);
    }

    fn grab_focus_self(&self) {
        let _ = WidgetExt::grab_focus(self);
    }

    // --- session lifecycle -------------------------------------------------

    /// Start the shell. Safe to call once the widget has a size; before that
    /// the grid falls back to 80x24 and is corrected on the first allocation.
    pub fn start(&self, cwd: Option<std::path::PathBuf>) -> Result<(), String> {
        let imp = self.imp();
        let m = imp.metrics.get();
        let (cols, rows) = {
            let (c, r) = imp.grid_size.get();
            if c == 0 || r == 0 { (80, 24) } else { (c, r) }
        };

        let term = tuni_vt::Terminal::new(cols, rows, imp.config.borrow().scrollback_lines)
            .map_err(|e| e.to_string())?;

        let pty = Pty::spawn(&PtyConfig {
            cwd,
            cols,
            rows,
            cell_width_px: m.cell_width.round() as u16,
            cell_height_px: m.cell_height.round() as u16,
            ..PtyConfig::default()
        })
        .map_err(|e| e.to_string())?;

        let events = pty.events();
        imp.session.replace(Some(Session { term, pty }));
        imp.grid_size.set((cols, rows));

        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                while let Ok(event) = events.recv().await {
                    match event {
                        PtyEvent::Output(bytes) => this.feed(&bytes),
                        PtyEvent::Exited => {
                            this.imp().session.replace(None);
                            this.queue_draw();
                            break;
                        }
                    }
                }
            }
        ));

        Ok(())
    }

    fn feed(&self, bytes: &[u8]) {
        let imp = self.imp();
        let mut guard = imp.session.borrow_mut();
        let Some(session) = guard.as_mut() else {
            return;
        };

        session.term.feed(bytes);

        let effects = session.term.take_effects();
        if !effects.pty_write.is_empty() {
            if std::env::var_os("TUNI_DEBUG_PTY_WRITE").is_some() {
                eprintln!("pty_write: {:?}", String::from_utf8_lossy(&effects.pty_write));
            }
            let _ = session.pty.write(&effects.pty_write);
        }
        let title = effects
            .title_changed
            .then(|| session.term.title().map(str::to_owned))
            .flatten();
        drop(guard);

        if let Some(title) = title {
            imp.title.replace(Some(title));
            self.notify("title");
        }
        if effects.bell {
            self.error_bell();
        }
        for request in &effects.clipboard_writes {
            if std::env::var_os("TUNI_DEBUG_CLIPBOARD").is_some() {
                eprintln!(
                    "clipboard write: {:?} {:?}",
                    request.target,
                    request.text.chars().take(40).collect::<String>()
                );
            }
            // OSC 52. Reads are refused by the VT itself, so nothing here can
            // leak the clipboard back to a remote shell.
            match request.target {
                ClipboardTarget::Standard => self.clipboard().set_text(&request.text),
                ClipboardTarget::Selection | ClipboardTarget::Primary => {
                    self.primary_clipboard().set_text(&request.text);
                }
            }
        }
        self.queue_draw();
    }

    fn apply_size(&self, width: i32, height: i32) {
        let imp = self.imp();
        let m = imp.metrics.get();
        let cols = ((width as f32 / m.cell_width).floor() as i32).clamp(1, i32::from(u16::MAX)) as u16;
        let rows = ((height as f32 / m.cell_height).floor() as i32).clamp(1, i32::from(u16::MAX)) as u16;

        if imp.grid_size.get() == (cols, rows) {
            return;
        }
        imp.grid_size.set((cols, rows));

        let mut guard = imp.session.borrow_mut();
        let Some(session) = guard.as_mut() else {
            return;
        };
        let _ = session.term.resize(
            cols,
            rows,
            m.cell_width.round() as u32,
            m.cell_height.round() as u32,
        );
        let _ = session.pty.resize(
            cols,
            rows,
            m.cell_width.round() as u16,
            m.cell_height.round() as u16,
        );
    }

    // --- mouse ---------------------------------------------------------------

    /// Pixel geometry of the grid as drawn, which both the selection gesture and
    /// the mouse encoder work in.
    fn geometry(&self) -> Geometry {
        let imp = self.imp();
        let m = imp.metrics.get();
        let (cols, rows) = imp.grid_size.get();
        Geometry {
            cols: cols.max(1),
            rows: rows.max(1),
            cell_width_px: (m.cell_width.round() as u32).max(1),
            cell_height_px: (m.cell_height.round() as u32).max(1),
            screen_width_px: self.width().max(1) as u32,
            screen_height_px: self.height().max(1) as u32,
        }
    }

    /// Whether this event belongs to the application rather than to selection.
    /// Shift is the standard override: it takes the mouse back even while an
    /// application is tracking it.
    fn reports_mouse(&self, mods: Mods) -> bool {
        if mods.contains(Mods::SHIFT) {
            return false;
        }
        self.imp()
            .session
            .borrow()
            .as_ref()
            .is_some_and(|session| session.term.is_mouse_tracking())
    }

    fn report_mouse(&self, action: MouseAction, button: Option<MouseButton>, mods: Mods, x: f64, y: f64) {
        let geometry = self.geometry();
        let imp = self.imp();
        let any_button_pressed = imp.buttons_down.get() > 0;

        let mut guard = imp.session.borrow_mut();
        let Some(session) = guard.as_mut() else {
            return;
        };
        let input = MouseInput {
            action,
            button,
            mods,
            x,
            y,
            any_button_pressed,
        };
        if let Ok(bytes) = session.term.encode_mouse(&input, geometry)
            && !bytes.is_empty()
        {
            let _ = session.pty.write(bytes);
        }
    }

    fn on_pointer_press(&self, gesture: &gtk::GestureClick, x: f64, y: f64) {
        self.grab_focus_self();

        let imp = self.imp();
        imp.buttons_down.set(imp.buttons_down.get().saturating_add(1));
        imp.pointer_pos.set((x, y));

        let mods = keymap::mods_from_state(gesture.current_event_state());
        let button = gesture.current_button();

        if self.reports_mouse(mods) {
            imp.pointer.set(Pointer::Reporting);
            self.report_mouse(MouseAction::Press, mouse_button(button), mods, x, y);
            return;
        }

        match button {
            gdk::BUTTON_MIDDLE => {
                // The primary selection, pasted by middle click: the X11
                // convention Wayland kept.
                self.paste_from(&self.primary_clipboard());
            }
            gdk::BUTTON_PRIMARY => {
                imp.pointer.set(Pointer::Selecting);
                let time = Duration::from_millis(u64::from(gesture.current_event_time()));
                self.selection_press(x, y, time);
            }
            _ => {}
        }
    }

    /// Anchor a selection at a surface position. Public to the crate so the
    /// debug capture harness can drive the same path a real click takes.
    pub(crate) fn selection_press(&self, x: f64, y: f64, time: Duration) {
        let geometry = self.geometry();
        let mut guard = self.imp().session.borrow_mut();
        if let Some(session) = guard.as_mut() {
            let _ = session.term.select_press(x, y, geometry, time);
        }
        drop(guard);
        self.queue_draw();
    }

    pub(crate) fn selection_drag(&self, x: f64, y: f64, rectangle: bool) {
        let geometry = self.geometry();
        let mut guard = self.imp().session.borrow_mut();
        if let Some(session) = guard.as_mut() {
            let _ = session.term.select_drag(x, y, geometry, rectangle);
        }
        drop(guard);
        self.queue_draw();
    }

    /// End the gesture and return what is selected.
    pub(crate) fn selection_finish(&self) -> Option<String> {
        let mut guard = self.imp().session.borrow_mut();
        let text = guard.as_mut().and_then(|session| {
            let _ = session.term.select_release();
            session.term.selection_text().ok().flatten()
        });
        drop(guard);
        text.filter(|text| !text.is_empty())
    }

    fn on_pointer_release(&self, gesture: &gtk::GestureClick, x: f64, y: f64) {
        let imp = self.imp();
        imp.buttons_down.set(imp.buttons_down.get().saturating_sub(1));

        let mods = keymap::mods_from_state(gesture.current_event_state());
        match imp.pointer.replace(Pointer::Idle) {
            Pointer::Reporting => {
                self.report_mouse(
                    MouseAction::Release,
                    mouse_button(gesture.current_button()),
                    mods,
                    x,
                    y,
                );
            }
            Pointer::Selecting => {
                // Selecting fills the primary selection, so a middle click in
                // any other window pastes what was just highlighted.
                if let Some(text) = self.selection_finish() {
                    self.primary_clipboard().set_text(&text);
                }
            }
            Pointer::Idle => {}
        }
    }

    fn on_pointer_motion(&self, controller: &gtk::EventControllerMotion, x: f64, y: f64) {
        let imp = self.imp();
        imp.pointer_pos.set((x, y));
        let mods = keymap::mods_from_state(controller.current_event_state());

        match imp.pointer.get() {
            // Alt turns the drag into a block selection, as in Ghostty.
            Pointer::Selecting => self.selection_drag(x, y, mods.contains(Mods::ALT)),
            _ => {
                if self.reports_mouse(mods) {
                    self.report_mouse(MouseAction::Motion, None, mods, x, y);
                }
            }
        }
    }

    fn on_scroll(&self, controller: &gtk::EventControllerScroll, dy: f64) {
        let imp = self.imp();
        let mods = keymap::mods_from_state(controller.current_event_state());

        if self.reports_mouse(mods) {
            // Wheel up and down are reported as buttons four and five.
            let button = if dy < 0.0 {
                MouseButton::Four
            } else {
                MouseButton::Five
            };
            let (x, y) = imp.pointer_pos.get();
            let notches = (dy.abs().round() as u32).clamp(1, 8);
            for _ in 0..notches {
                self.report_mouse(MouseAction::Press, Some(button), mods, x, y);
            }
            return;
        }

        let mut guard = imp.session.borrow_mut();
        let Some(session) = guard.as_mut() else {
            return;
        };
        // Three lines per notch, the X11 convention every toolkit inherited.
        let lines = (dy * 3.0).round() as isize;
        if lines != 0 {
            session.term.scroll_lines(lines);
            drop(guard);
            self.queue_draw();
        }
    }

    // --- clipboard -----------------------------------------------------------

    /// Copy the selection to the system clipboard. Does nothing without one.
    pub fn copy_selection(&self) -> bool {
        let mut guard = self.imp().session.borrow_mut();
        let text = guard
            .as_mut()
            .and_then(|session| session.term.selection_text().ok().flatten());
        drop(guard);

        match text.filter(|t| !t.is_empty()) {
            Some(text) => {
                self.clipboard().set_text(&text);
                true
            }
            None => false,
        }
    }

    pub fn paste_clipboard(&self) {
        self.paste_from(&self.clipboard());
    }

    fn paste_from(&self, clipboard: &gdk::Clipboard) {
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[strong]
            clipboard,
            async move {
                if let Ok(text) = clipboard.read_text_future().await
                    && let Some(text) = text
                {
                    this.paste_text(&text);
                }
            }
        ));
    }

    /// Write pasted text to the shell, bracketed when the application asked for
    /// it so a multi-line paste cannot run itself.
    fn paste_text(&self, text: &str) {
        let mut guard = self.imp().session.borrow_mut();
        let Some(session) = guard.as_mut() else {
            return;
        };
        if let Ok(bytes) = session.term.encode_paste(text) {
            let _ = session.pty.write(bytes);
            session.term.scroll_to_bottom();
        }
        drop(guard);
        self.queue_draw();
    }

    // --- input -------------------------------------------------------------

    fn on_key(
        &self,
        controller: &gtk::EventControllerKey,
        keyval: gdk::Key,
        state: gdk::ModifierType,
    ) -> glib::Propagation {
        let imp = self.imp();

        // Give the input method first refusal, then look at what it committed.
        // A press it swallowed without committing is mid-composition and must
        // not also reach the terminal.
        let filtered = match (imp.im.borrow().as_ref(), controller.current_event()) {
            (Some(im), Some(event)) => im.filter_keypress(&event),
            _ => false,
        };
        let committed = imp.pending_commit.borrow_mut().take();
        if filtered && committed.is_none() {
            return glib::Propagation::Stop;
        }

        let mods = keymap::mods_from_state(state);

        // Application shortcuts live on Ctrl+Shift, because Ctrl+C and Ctrl+V
        // belong to the shell.
        if mods.contains(Mods::CTRL) && mods.contains(Mods::SHIFT) {
            match keyval.to_unicode().map(|c| c.to_ascii_lowercase()) {
                Some('c') => {
                    // With nothing selected, fall through so Ctrl+Shift+C still
                    // reaches the application.
                    if self.copy_selection() {
                        return glib::Propagation::Stop;
                    }
                }
                Some('v') => {
                    self.paste_clipboard();
                    return glib::Propagation::Stop;
                }
                Some('a') => {
                    let mut guard = imp.session.borrow_mut();
                    if let Some(session) = guard.as_mut() {
                        let _ = session.term.select_all();
                    }
                    drop(guard);
                    self.queue_draw();
                    return glib::Propagation::Stop;
                }
                _ => {}
            }
        }

        let key = keymap::key_from_keyval(keyval);
        let text = committed.or_else(|| {
            keyval
                .to_unicode()
                .filter(|c| !c.is_control())
                .map(String::from)
        });

        // Shift is already baked into the character the toolkit produced;
        // reporting it again would double it under the Kitty protocol.
        let consumed_mods = if text.is_some() && mods.contains(Mods::SHIFT) {
            Mods::SHIFT
        } else {
            Mods::empty()
        };

        let mut guard = imp.session.borrow_mut();
        let Some(session) = guard.as_mut() else {
            return glib::Propagation::Proceed;
        };

        let input = KeyInput {
            action: KeyAction::Press,
            key,
            mods,
            consumed_mods,
            text: text.as_deref(),
        };

        match session.term.encode_key(&input) {
            Ok(bytes) if !bytes.is_empty() => {
                let _ = session.pty.write(bytes);
                session.term.scroll_to_bottom();
                drop(guard);
                self.queue_draw();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    }

    // --- drawing -----------------------------------------------------------

    fn draw(&self, snapshot: &gtk::Snapshot) {
        let imp = self.imp();
        let m = imp.metrics.get();
        let width = self.width() as f32;
        let height = self.height() as f32;

        let mut guard = imp.session.borrow_mut();
        let Some(session) = guard.as_mut() else {
            return;
        };
        let Ok(grid) = session.term.snapshot() else {
            return;
        };

        snapshot.append_color(
            &rgba(grid.bg),
            &graphene::Rect::new(0.0, 0.0, width, height),
        );

        let context = self.pango_context();
        let font = imp.font.borrow();
        let layout = pango::Layout::new(&context);
        layout.set_font_description(Some(&font));

        let mut text = String::with_capacity(256);

        for row in 0..grid.rows {
            let cells = grid.row(row);
            let y = row as f32 * m.cell_height;

            // Backgrounds first, batched into runs of equal color, so a full
            // reverse-video line is one rectangle rather than eighty.
            let mut run_start: Option<(usize, Rgb)> = None;
            for (col, cell) in cells.iter().enumerate() {
                let bg = cell.bg;
                match (&run_start, bg) {
                    (Some((start, current)), Some(bg)) if *current == bg => {
                        let _ = start;
                    }
                    (Some((start, current)), _) => {
                        fill_run(snapshot, m, *start, col, y, *current);
                        run_start = bg.map(|bg| (col, bg));
                    }
                    (None, Some(bg)) => run_start = Some((col, bg)),
                    (None, None) => {}
                }
            }
            if let Some((start, color)) = run_start {
                fill_run(snapshot, m, start, cells.len(), y, color);
            }

            // Then text, batched into runs sharing a style. Each run is placed
            // at its own column origin, so per-run advance drift cannot
            // accumulate across the line.
            let mut col = 0usize;
            while col < cells.len() {
                if cells[col].text.is_empty() {
                    col += 1;
                    continue;
                }

                let start = col;
                let style = style_key(&cells[col]);
                text.clear();

                while col < cells.len() {
                    let cell = &cells[col];
                    if cell.text.is_empty() || style_key(cell) != style {
                        break;
                    }
                    text.push_str(&cell.text);
                    // A double-width grapheme owns the following cell; the VT
                    // reports that cell as empty, and stepping over it keeps
                    // the run's column arithmetic honest.
                    col += UnicodeWidthStr::width(cell.text.as_str()).max(1);
                }

                draw_run(
                    snapshot,
                    &layout,
                    &font,
                    m,
                    start,
                    y,
                    &text,
                    &cells[start],
                );
            }
        }

        if let Some(cursor) = grid.cursor {
            draw_cursor(snapshot, &layout, m, &cursor, grid, self.has_focus());
        }
    }
}

#[derive(PartialEq, Eq)]
struct StyleKey {
    fg: Rgb,
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
}

fn mouse_button(button: u32) -> Option<MouseButton> {
    match button {
        gdk::BUTTON_PRIMARY => Some(MouseButton::Left),
        gdk::BUTTON_MIDDLE => Some(MouseButton::Middle),
        gdk::BUTTON_SECONDARY => Some(MouseButton::Right),
        8 => Some(MouseButton::Four),
        9 => Some(MouseButton::Five),
        _ => None,
    }
}

fn style_key(cell: &tuni_vt::Cell) -> StyleKey {
    StyleKey {
        fg: cell.fg,
        bold: cell.bold,
        italic: cell.italic,
        underline: cell.underline,
        strikethrough: cell.strikethrough,
    }
}

fn rgba(color: Rgb) -> gdk::RGBA {
    gdk::RGBA::new(
        f32::from(color.r) / 255.0,
        f32::from(color.g) / 255.0,
        f32::from(color.b) / 255.0,
        1.0,
    )
}

fn fill_run(
    snapshot: &gtk::Snapshot,
    m: Metrics,
    start: usize,
    end: usize,
    y: f32,
    color: Rgb,
) {
    let x = start as f32 * m.cell_width;
    let w = (end - start) as f32 * m.cell_width;
    snapshot.append_color(&rgba(color), &graphene::Rect::new(x, y, w, m.cell_height));
}

fn draw_run(
    snapshot: &gtk::Snapshot,
    layout: &pango::Layout,
    font: &pango::FontDescription,
    m: Metrics,
    start: usize,
    y: f32,
    text: &str,
    style: &tuni_vt::Cell,
) {
    let mut desc = font.clone();
    if style.bold {
        desc.set_weight(pango::Weight::Bold);
    }
    if style.italic {
        desc.set_style(pango::Style::Italic);
    }
    layout.set_font_description(Some(&desc));
    layout.set_text(text);

    let attrs = pango::AttrList::new();
    if style.underline {
        attrs.insert(pango::AttrInt::new_underline(pango::Underline::Single));
    }
    if style.strikethrough {
        attrs.insert(pango::AttrInt::new_strikethrough(true));
    }
    layout.set_attributes(Some(&attrs));

    snapshot.save();
    snapshot.translate(&graphene::Point::new(start as f32 * m.cell_width, y));
    // `append_layout` places the layout's top-left at the origin, and Pango's
    // own ascent already puts the baseline where we want it.
    snapshot.append_layout(layout, &rgba(style.fg));
    snapshot.restore();
}

fn draw_cursor(
    snapshot: &gtk::Snapshot,
    layout: &pango::Layout,
    m: Metrics,
    cursor: &tuni_vt::Cursor,
    grid: &tuni_vt::Grid,
    focused: bool,
) {
    let x = f32::from(cursor.col) * m.cell_width;
    let y = f32::from(cursor.row) * m.cell_height;
    let color = cursor.color.unwrap_or(grid.fg);

    if !focused {
        // An unfocused terminal shows a hollow cell, the convention every
        // terminal on this desktop follows.
        draw_hollow(snapshot, m, x, y, color);
        return;
    }

    match cursor.shape {
        CursorShape::Block => {
            snapshot.append_color(
                &rgba(color),
                &graphene::Rect::new(x, y, m.cell_width, m.cell_height),
            );
            // Repaint the covered glyph in the background color so it stays
            // readable under the block.
            if let Some(cell) = grid.cell(cursor.col, cursor.row)
                && !cell.text.is_empty()
            {
                layout.set_text(&cell.text);
                layout.set_attributes(None);
                snapshot.save();
                snapshot.translate(&graphene::Point::new(x, y));
                snapshot.append_layout(layout, &rgba(cell.bg.unwrap_or(grid.bg)));
                snapshot.restore();
            }
        }
        CursorShape::BlockHollow => draw_hollow(snapshot, m, x, y, color),
        CursorShape::Bar => snapshot.append_color(
            &rgba(color),
            &graphene::Rect::new(x, y, 2.0, m.cell_height),
        ),
        CursorShape::Underline => snapshot.append_color(
            &rgba(color),
            &graphene::Rect::new(x, y + m.cell_height - 2.0, m.cell_width, 2.0),
        ),
    }
}

fn draw_hollow(snapshot: &gtk::Snapshot, m: Metrics, x: f32, y: f32, color: Rgb) {
    let c = rgba(color);
    let w = m.cell_width;
    let h = m.cell_height;
    snapshot.append_color(&c, &graphene::Rect::new(x, y, w, 1.0));
    snapshot.append_color(&c, &graphene::Rect::new(x, y + h - 1.0, w, 1.0));
    snapshot.append_color(&c, &graphene::Rect::new(x, y, 1.0, h));
    snapshot.append_color(&c, &graphene::Rect::new(x + w - 1.0, y, 1.0, h));
}
