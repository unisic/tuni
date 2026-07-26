//! The terminal widget: a `GtkWidget` subclass that owns one VT state machine
//! and one PTY, and paints the viewport with Pango.
//!
//! Rendering follows Ghostty-on-Linux practice rather than a GPU atlas: Pango
//! plus fontconfig gives subpixel antialiasing, system font fallback, and IME
//! for free, and text quality is the thing a terminal is judged on. Whether
//! this keeps up under a firehose is exactly what the Etap 0 benchmark decides.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use gtk::gdk;
use gtk::glib;
use gtk::graphene;
use gtk::pango;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use unicode_width::UnicodeWidthStr;

use tuni_core::TerminalConfig;
use tuni_core::theme::Theme;
use tuni_pty::{Pty, PtyConfig, PtyEvent};
use tuni_vt::{
    ClipboardTarget, Colors, CursorShape, Geometry, KeyAction, KeyInput, Mods, MouseAction,
    MouseButton, MouseInput, Rgb, ScrollPosition,
};

use crate::keymap;

/// Width of the strip along the trailing edge that the scrollbar owns. Wider
/// than the thumb, so grabbing it does not demand pixel precision.
const SCROLLBAR_STRIP: f32 = 14.0;
const SCROLLBAR_INSET: f32 = 3.0;
const SCROLLBAR_MIN_THUMB: f32 = 24.0;
/// How long the thumb stays up after the last scroll, and how long it takes to
/// fade afterwards. Both from the reference implementation.
const SCROLLBAR_LINGER: Duration = Duration::from_millis(1100);
const SCROLLBAR_FADE: Duration = Duration::from_millis(400);
const SCROLLBAR_REVEAL: Duration = Duration::from_millis(100);

/// What the pointer is doing between press and release. A drag either paints a
/// selection or is reported to the application; never both.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Pointer {
    #[default]
    Idle,
    Selecting,
    Reporting,
}

/// The overlay scrollbar's thumb, in widget pixels.
#[derive(Clone, Copy, Debug)]
struct Thumb {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl Thumb {
    fn contains(&self, y: f32) -> bool {
        y >= self.y && y <= self.y + self.height
    }
}

/// One step of the font-size shortcut, in points. Ghostty's step, and the one
/// every terminal on this desktop uses.
const FONT_STEP: f64 = 1.0;

/// The sample the cell width is measured from: digits and letters of a width a
/// monospace face is obliged to share. Long enough that rounding in a single
/// advance cannot skew the average.
const WIDTH_SAMPLE: &str = "0123456789abcdefghijklmnopqrstuvwxyz";

/// Cell geometry derived from the font. Everything on screen is placed off
/// these numbers, so a terminal stays a grid even when the font lies about
/// being monospace.
#[derive(Clone, Copy, Debug)]
struct Metrics {
    cell_width: f32,
    cell_height: f32,
    /// Distance from the top of a cell to the baseline. Runs that fall back to
    /// another face are aligned on this rather than on their own box, so a
    /// missing glyph does not lift or drop the line it lands in.
    ascent: f32,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            cell_width: 8.0,
            cell_height: 16.0,
            ascent: 12.0,
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
        /// Colors in effect. Kept here as well as pushed into the VT, because
        /// the widget's own chrome — the scrollbar, the empty background before
        /// a shell starts — is painted from the theme rather than from CSS.
        pub(super) theme: RefCell<Theme>,
        pub(super) font: RefCell<pango::FontDescription>,
        /// Point size in effect, which the zoom shortcuts move away from the
        /// configured one and put back.
        pub(super) font_size: Cell<f64>,
        pub(super) metrics: Cell<Metrics>,
        /// Grid size last pushed to the VT and the PTY, so a resize that does
        /// not cross a cell boundary costs nothing. The cell's pixel size goes
        /// with it, because zooming changes that without changing the grid.
        pub(super) grid_size: Cell<(u16, u16)>,
        pub(super) cell_size: Cell<(u16, u16)>,
        pub(super) im: RefCell<Option<gtk::IMMulticontext>>,
        /// Text the input method committed during the current key press.
        pub(super) pending_commit: RefCell<Option<String>>,
        pub(super) title: RefCell<Option<String>>,
        /// Working directory as last reported by OSC 7. Etap 2 infers a
        /// project's directory from this; for now it names the window.
        pub(super) cwd: RefCell<Option<String>>,
        pub(super) pointer: Cell<Pointer>,
        /// How many mouse buttons are down, which mouse reporting needs in
        /// order to decide whether motion is worth sending.
        pub(super) buttons_down: Cell<u8>,
        /// Last known pointer position, because scroll events carry no
        /// coordinates but the application still expects them in the report.
        pub(super) pointer_pos: Cell<(f64, f64)>,
        /// Scroll state as of the last feed. Upstream raises no notification
        /// when the viewport moves, so this is polled and diffed.
        pub(super) scroll: Cell<ScrollPosition>,
        /// Overlay scrollbar: opacity, when the fade may begin, and the grab
        /// offset inside the thumb while it is being dragged.
        pub(super) bar_alpha: Cell<f32>,
        pub(super) bar_until: Cell<Option<Instant>>,
        pub(super) bar_hover: Cell<bool>,
        pub(super) bar_drag: Cell<Option<f32>>,
        pub(super) bar_tick: RefCell<Option<gtk::TickCallbackId>>,
        /// Frame-clock time of the previous fade step, in microseconds.
        pub(super) bar_frame: Cell<i64>,
        /// Cursor blink phase, its timer, and whether the cursor drawn last
        /// frame asked to blink at all.
        pub(super) blink_on: Cell<bool>,
        pub(super) blink_source: RefCell<Option<glib::SourceId>>,
        pub(super) blink_wanted: Cell<bool>,
        /// When the user last typed. GTK stops blinking a while after input,
        /// so an idle terminal does not animate forever.
        pub(super) last_input: Cell<Option<Instant>>,
        /// Draw timings, collected only when TUNI_DEBUG_FRAME_TIME is set.
        /// This is the measurement that decides whether Pango can keep up.
        pub(super) frame_timing: bool,
        pub(super) frame_times: RefCell<Vec<std::time::Duration>>,
    }

    impl Default for TuniTerminal {
        fn default() -> Self {
            Self {
                session: RefCell::new(None),
                theme: RefCell::new(
                    TerminalConfig::default().theme(adw::StyleManager::default().is_dark()),
                ),
                font_size: Cell::new(TerminalConfig::default().font_size),
                config: RefCell::new(TerminalConfig::default()),
                font: RefCell::new(pango::FontDescription::new()),
                metrics: Cell::new(Metrics::default()),
                grid_size: Cell::new((0, 0)),
                cell_size: Cell::new((0, 0)),
                im: RefCell::new(None),
                pending_commit: RefCell::new(None),
                title: RefCell::new(None),
                cwd: RefCell::new(None),
                pointer: Cell::new(Pointer::default()),
                buttons_down: Cell::new(0),
                pointer_pos: Cell::new((0.0, 0.0)),
                scroll: Cell::new(ScrollPosition::default()),
                bar_alpha: Cell::new(0.0),
                bar_until: Cell::new(None),
                bar_hover: Cell::new(false),
                bar_drag: Cell::new(None),
                bar_tick: RefCell::new(None),
                bar_frame: Cell::new(0),
                blink_on: Cell::new(true),
                blink_source: RefCell::new(None),
                blink_wanted: Cell::new(false),
                last_input: Cell::new(None),
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
                vec![
                    glib::ParamSpecString::builder("title").read_only().build(),
                    glib::ParamSpecString::builder("cwd").read_only().build(),
                ]
            })
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "title" => self.title.borrow().to_value(),
                "cwd" => self.cwd.borrow().to_value(),
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
            if let Some(source) = self.blink_source.take() {
                source.remove();
            }
            if let Some(tick) = self.bar_tick.take() {
                tick.remove();
            }
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

    /// The shell's working directory, as last reported by OSC 7. `None` until
    /// a shell that sends it does.
    #[must_use]
    pub fn cwd(&self) -> Option<String> {
        self.imp().cwd.borrow().clone()
    }

    // --- setup -------------------------------------------------------------

    fn setup_font(&self) {
        let imp = self.imp();
        let stack = imp.config.borrow().font_stack();

        let mut font = pango::FontDescription::new();
        font.set_family(&stack);
        font.set_size((imp.font_size.get() * f64::from(pango::SCALE)).round() as i32);

        imp.font.replace(font);
        self.update_metrics();
    }

    fn update_metrics(&self) {
        let imp = self.imp();
        let context = self.pango_context();
        let font = imp.font.borrow();
        let metrics = context.metrics(Some(&font), None);

        let scale = pango::SCALE as f32;
        let ascent = metrics.ascent() as f32 / scale;
        let descent = metrics.descent() as f32 / scale;

        // Measured rather than taken from `approximate_char_width`: that number
        // is a hint the font supplies, and a face whose hint disagrees with its
        // own advances would draw a grid that drifts a fraction of a pixel per
        // column. Laying out a real run asks the question the renderer will.
        let layout = pango::Layout::new(&context);
        layout.set_font_description(Some(&font));
        layout.set_text(WIDTH_SAMPLE);
        let measured = layout.size().0 as f32 / scale / WIDTH_SAMPLE.chars().count() as f32;
        let cell_width = if measured >= 1.0 {
            measured
        } else {
            metrics.approximate_char_width() as f32 / scale
        };
        drop(font);

        let extra = imp.config.borrow().line_height_extra as f32;
        imp.metrics.set(Metrics {
            cell_width: cell_width.max(1.0),
            cell_height: (ascent + descent + extra).max(1.0),
            ascent: ascent + extra / 2.0,
        });
    }

    // --- font size -----------------------------------------------------------

    /// The point size the terminal is drawn at.
    #[must_use]
    pub fn font_size(&self) -> f64 {
        self.imp().font_size.get()
    }

    /// Draw at a different point size, re-deriving the grid from it.
    ///
    /// The window keeps its size and the grid changes shape, which is what a
    /// tiling desktop leaves room for; growing the window instead would fight
    /// the compositor.
    pub fn set_font_size(&self, size: f64) {
        let imp = self.imp();
        let size = size.clamp(tuni_core::FONT_SIZE_MIN, tuni_core::FONT_SIZE_MAX);
        if (size - imp.font_size.get()).abs() < 0.01 {
            return;
        }
        imp.font_size.set(size);

        self.setup_font();
        self.apply_size(self.width(), self.height());
        self.queue_resize();
        self.queue_draw();
    }

    /// Move the size by whole points, as `Ctrl+plus` and `Ctrl+minus` do.
    pub fn zoom(&self, steps: i32) {
        self.set_font_size(self.font_size() + f64::from(steps) * FONT_STEP);
    }

    /// Back to the configured size.
    pub fn reset_zoom(&self) {
        self.set_font_size(self.imp().config.borrow().font_size);
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
                this.start_blink();
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
                // An unfocused terminal draws a hollow cursor, which must not
                // also blink.
                this.stop_blink();
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

    // --- colors ------------------------------------------------------------

    /// Repaint in a theme's colors.
    ///
    /// Safe to call before the shell starts: the theme is kept and handed to
    /// the session when one is created. Safe to call repeatedly, which is what
    /// happens when the desktop flips between light and dark.
    pub fn set_theme(&self, theme: &Theme) {
        let imp = self.imp();
        if *imp.theme.borrow() == *theme {
            return;
        }
        imp.theme.replace(theme.clone());

        if let Some(session) = imp.session.borrow_mut().as_mut() {
            let _ = session.term.set_colors(&colors(theme));
        }
        self.queue_draw();
    }

    /// The theme currently painting this terminal.
    pub fn theme(&self) -> Theme {
        self.imp().theme.borrow().clone()
    }

    // --- configuration -------------------------------------------------------

    /// Adopt a configuration: font, ligatures, row spacing, scrollback.
    ///
    /// The zoom goes back to the configured size, which is the only sensible
    /// reading of being handed a new one. Scrollback depth is fixed when the
    /// shell starts, so a change to it only reaches a session started after
    /// this call.
    pub fn set_config(&self, config: &TerminalConfig) {
        let imp = self.imp();
        imp.config.replace(config.clone());
        imp.font_size.set(config.font_size);

        self.setup_font();
        self.apply_size(self.width(), self.height());
        self.queue_resize();
        self.queue_draw();
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

        let mut term = tuni_vt::Terminal::new(cols, rows, imp.config.borrow().scrollback_lines)
            .map_err(|e| e.to_string())?;
        let _ = term.set_default_cursor_blink(Some(imp.config.borrow().cursor_blink));
        let _ = term.set_colors(&colors(&imp.theme.borrow()));

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
        imp.cell_size
            .set((m.cell_width.round() as u16, m.cell_height.round() as u16));

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
        let cwd = effects.pwd_changed.then(|| session.term.pwd()).flatten();
        let scroll = session.term.scroll_position();
        drop(guard);

        // Output grows the scrollback every frame, so only a change in where
        // the viewport sits counts as scroll activity worth showing a thumb
        // for. Otherwise a long build would keep the scrollbar lit throughout.
        let moved = scroll.fraction() != imp.scroll.replace(scroll).fraction();
        if moved {
            self.reveal_scrollbar();
        }

        if let Some(title) = title {
            imp.title.replace(Some(title));
            self.notify("title");
        }
        if let Some(cwd) = cwd {
            imp.cwd.replace(Some(cwd));
            self.notify("cwd");
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
        let cell = (
            (m.cell_width.round() as i32).clamp(1, i32::from(u16::MAX)) as u16,
            (m.cell_height.round() as i32).clamp(1, i32::from(u16::MAX)) as u16,
        );

        // Zooming can leave the grid the same shape while changing what a cell
        // measures, and an application that draws with sixels or images needs
        // the pixel size to be right either way.
        if imp.grid_size.get() == (cols, rows) && imp.cell_size.get() == cell {
            return;
        }
        imp.grid_size.set((cols, rows));
        imp.cell_size.set(cell);

        let mut guard = imp.session.borrow_mut();
        let Some(session) = guard.as_mut() else {
            return;
        };
        let _ = session
            .term
            .resize(cols, rows, u32::from(cell.0), u32::from(cell.1));
        let _ = session.pty.resize(cols, rows, cell.0, cell.1);
    }

    // --- cursor blinking -----------------------------------------------------

    /// Desktop blink preferences: whether to blink at all, the full cycle, and
    /// how long after the last input blinking stops. Following GtkSettings
    /// rather than a private default keeps the terminal's cursor in step with
    /// every text field on the desktop.
    fn blink_settings(&self) -> (bool, Duration, Option<Duration>) {
        let settings = gtk::Settings::for_display(&self.display());
        let cycle = Duration::from_millis(u64::from(settings.gtk_cursor_blink_time().max(100) as u32));
        let timeout = settings.gtk_cursor_blink_timeout();
        (
            settings.is_gtk_cursor_blink(),
            cycle,
            // GTK spells "never stop" as a timeout larger than any real one.
            (timeout < i32::MAX / 2).then(|| Duration::from_secs(u64::from(timeout.max(1) as u32))),
        )
    }

    fn start_blink(&self) {
        self.stop_blink();

        let (enabled, cycle, _) = self.blink_settings();
        if !enabled {
            return;
        }
        self.imp().blink_on.set(true);

        let id = glib::timeout_add_local(
            cycle / 2,
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || this.blink_tick()
            ),
        );
        self.imp().blink_source.replace(Some(id));
    }

    fn stop_blink(&self) {
        if let Some(source) = self.imp().blink_source.take() {
            source.remove();
        }
        self.imp().blink_on.set(true);
    }

    fn blink_tick(&self) -> glib::ControlFlow {
        let imp = self.imp();

        // Nothing to animate if the cursor asked to stay solid, or if the
        // terminal has been idle long enough that GTK would have stopped.
        let idle = match (imp.last_input.get(), self.blink_settings().2) {
            (Some(last), Some(timeout)) => last.elapsed() > timeout,
            _ => false,
        };
        if !imp.blink_wanted.get() || idle || !self.has_focus() {
            if !imp.blink_on.replace(true) {
                self.queue_draw();
            }
            return glib::ControlFlow::Continue;
        }

        imp.blink_on.set(!imp.blink_on.get());
        self.queue_draw();
        glib::ControlFlow::Continue
    }

    /// Restart the blink phase, so the cursor is solid while the user types.
    fn note_input(&self) {
        let imp = self.imp();
        imp.last_input.set(Some(Instant::now()));
        if !imp.blink_on.replace(true) {
            self.queue_draw();
        }
        if imp.blink_source.borrow().is_none() && self.has_focus() {
            self.start_blink();
        }
    }

    // --- overlay scrollbar ---------------------------------------------------

    /// The thumb as drawn, or `None` when there is nothing to scroll.
    ///
    /// The thumb floats over the content with no track, which is why it has to
    /// fade: a permanent one would cover a column of text.
    fn thumb(&self) -> Option<Thumb> {
        let imp = self.imp();
        let scroll = imp.scroll.get();
        if !scroll.is_scrollable() {
            return None;
        }

        let track = self.height() as f32 - 2.0 * SCROLLBAR_INSET;
        if track <= 0.0 {
            return None;
        }
        let height = (track * scroll.proportion() as f32).clamp(SCROLLBAR_MIN_THUMB.min(track), track);
        let width = if imp.bar_hover.get() || imp.bar_drag.get().is_some() {
            8.0
        } else {
            5.0
        };

        Some(Thumb {
            x: self.width() as f32 - width - SCROLLBAR_INSET,
            y: SCROLLBAR_INSET + (track - height) * scroll.fraction() as f32,
            width,
            height,
        })
    }

    /// Show the thumb and restart its fade timer.
    fn reveal_scrollbar(&self) {
        let imp = self.imp();
        if !imp.scroll.get().is_scrollable() {
            return;
        }
        imp.bar_until.set(Some(Instant::now() + SCROLLBAR_LINGER));

        if imp.bar_tick.borrow().is_some() {
            return;
        }
        let tick = self.add_tick_callback(|this, clock| this.fade_scrollbar(clock));
        imp.bar_tick.replace(Some(tick));
    }

    /// One frame of the fade animation. Runs only while the opacity is moving,
    /// and takes itself off the frame clock once the thumb is gone.
    fn fade_scrollbar(&self, clock: &gdk::FrameClock) -> glib::ControlFlow {
        let imp = self.imp();
        let held = imp.bar_hover.get()
            || imp.bar_drag.get().is_some()
            || imp.bar_until.get().is_some_and(|until| Instant::now() < until);
        let target: f32 = if held && imp.scroll.get().is_scrollable() {
            1.0
        } else {
            0.0
        };

        let interval = clock.frame_time() - imp.bar_frame.replace(clock.frame_time());
        // A missing or absurd interval (first frame, resumed clock) gets one
        // frame's worth rather than a jump.
        let dt = if (1..500_000).contains(&interval) {
            interval as f32 / 1_000_000.0
        } else {
            1.0 / 60.0
        };
        let span = if target > imp.bar_alpha.get() {
            SCROLLBAR_REVEAL
        } else {
            SCROLLBAR_FADE
        };

        let step = dt / span.as_secs_f32();
        let alpha = if target > imp.bar_alpha.get() {
            (imp.bar_alpha.get() + step).min(target)
        } else {
            (imp.bar_alpha.get() - step).max(target)
        };
        imp.bar_alpha.set(alpha);
        self.queue_draw();

        if alpha <= 0.0 && target <= 0.0 {
            imp.bar_tick.replace(None);
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    }

    /// Whether a press at `x` belongs to the scrollbar. An invisible thumb is
    /// transparent to the pointer, so a click lands in the terminal instead.
    fn scrollbar_hit(&self, x: f64) -> bool {
        let imp = self.imp();
        imp.bar_alpha.get() > 0.01
            && imp.scroll.get().is_scrollable()
            && x as f32 >= self.width() as f32 - SCROLLBAR_STRIP
    }

    /// Scroll so the thumb's top lands at `y` minus the grab offset.
    fn drag_scrollbar(&self, y: f64) {
        let imp = self.imp();
        let (Some(offset), Some(thumb)) = (imp.bar_drag.get(), self.thumb()) else {
            return;
        };
        let travel = self.height() as f32 - 2.0 * SCROLLBAR_INSET - thumb.height;
        if travel <= 0.0 {
            return;
        }

        let fraction = f64::from((y as f32 - offset - SCROLLBAR_INSET) / travel);
        let row = imp.scroll.get().row_at(fraction);

        let mut guard = imp.session.borrow_mut();
        if let Some(session) = guard.as_mut() {
            session.term.scroll_to_row(row);
            imp.scroll.set(session.term.scroll_position());
        }
        drop(guard);

        self.reveal_scrollbar();
        self.queue_draw();
    }

    /// Move the viewport and keep the cached scroll state in step. Public to
    /// the crate so the debug capture harness drives the same path the wheel
    /// does.
    pub(crate) fn scroll_lines(&self, lines: isize) {
        self.scroll_by(lines);
    }

    fn scroll_by(&self, lines: isize) {
        let imp = self.imp();
        let mut guard = imp.session.borrow_mut();
        let Some(session) = guard.as_mut() else {
            return;
        };
        session.term.scroll_lines(lines);
        imp.scroll.set(session.term.scroll_position());
        drop(guard);

        self.reveal_scrollbar();
        self.queue_draw();
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

        if button == gdk::BUTTON_PRIMARY
            && self.scrollbar_hit(x)
            && let Some(thumb) = self.thumb()
        {
            // On the thumb, keep the grab point; beside it, jump so the thumb
            // centers on the pointer and carry on as a drag.
            let offset = if thumb.contains(y as f32) {
                y as f32 - thumb.y
            } else {
                thumb.height / 2.0
            };
            imp.bar_drag.set(Some(offset));
            self.drag_scrollbar(y);
            return;
        }

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

        if imp.bar_drag.take().is_some() {
            self.reveal_scrollbar();
            self.queue_draw();
            return;
        }

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

        if imp.bar_drag.get().is_some() {
            self.drag_scrollbar(y);
            return;
        }
        // Hovering only widens a thumb that is already up; it never summons one.
        let hover = self.scrollbar_hit(x);
        if imp.bar_hover.replace(hover) != hover {
            self.reveal_scrollbar();
            self.queue_draw();
        }

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

        // Three lines per notch, the X11 convention every toolkit inherited.
        let lines = (dy * 3.0).round() as isize;
        if lines != 0 {
            self.scroll_by(lines);
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

        // Scrollback navigation sits on plain Shift, where every terminal on
        // this desktop puts it.
        if mods.contains(Mods::SHIFT) && !mods.contains(Mods::CTRL) {
            let page = imp.grid_size.get().1.max(1) as isize;
            match keyval {
                gdk::Key::Page_Up => {
                    self.scroll_by(-page);
                    return glib::Propagation::Stop;
                }
                gdk::Key::Page_Down => {
                    self.scroll_by(page);
                    return glib::Propagation::Stop;
                }
                gdk::Key::Home | gdk::Key::End => {
                    let mut guard = imp.session.borrow_mut();
                    if let Some(session) = guard.as_mut() {
                        if keyval == gdk::Key::Home {
                            session.term.scroll_to_top();
                        } else {
                            session.term.scroll_to_bottom();
                        }
                        imp.scroll.set(session.term.scroll_position());
                    }
                    drop(guard);
                    self.reveal_scrollbar();
                    self.queue_draw();
                    return glib::Propagation::Stop;
                }
                _ => {}
            }
        }

        // Font size sits on plain Ctrl, where Ghostty and every browser put it.
        // None of these three keys carries a control code a shell would miss.
        if mods.contains(Mods::CTRL) && !mods.contains(Mods::ALT) {
            match keyval {
                gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add => {
                    self.zoom(1);
                    return glib::Propagation::Stop;
                }
                gdk::Key::minus | gdk::Key::KP_Subtract => {
                    self.zoom(-1);
                    return glib::Propagation::Stop;
                }
                gdk::Key::_0 | gdk::Key::KP_0 => {
                    self.reset_zoom();
                    return glib::Propagation::Stop;
                }
                _ => {}
            }
        }

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
                // Typing pulls the viewport back down: the answer is about to
                // arrive at the bottom.
                session.term.scroll_to_bottom();
                imp.scroll.set(session.term.scroll_position());
                drop(guard);
                self.note_input();
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
        let grid = guard.as_mut().and_then(|session| session.term.snapshot().ok());

        // The page color: whatever the terminal is actually using, which is the
        // theme unless an application overrode it with OSC 11. Falling back to
        // the theme keeps the widget the right color before a shell starts and
        // after one exits, when there is no terminal to ask.
        let background = grid.map_or_else(|| theme_rgb(imp.theme.borrow().background), |g| g.bg);
        snapshot.append_color(
            &rgba(background),
            &graphene::Rect::new(0.0, 0.0, width, height),
        );

        let Some(grid) = grid else {
            return;
        };

        let context = self.pango_context();
        let font = imp.font.borrow();
        let layout = pango::Layout::new(&context);
        layout.set_font_description(Some(&font));
        let painter = Painter {
            snapshot,
            layout: &layout,
            font: &font,
            ligatures: imp.config.borrow().font_ligatures,
            m,
        };

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
                        painter.fill_run(*start, col, y, *current);
                        run_start = bg.map(|bg| (col, bg));
                    }
                    (None, Some(bg)) => run_start = Some((col, bg)),
                    (None, None) => {}
                }
            }
            if let Some((start, color)) = run_start {
                painter.fill_run(start, cells.len(), y, color);
            }

            // Then text, batched into runs sharing a style.
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
                    // Anything that is not one plain ASCII character is laid
                    // out alone, so it lands on its own column rather than
                    // wherever the run's accumulated advances put it.
                    let simple = is_simple(&cell.text);
                    if !simple && col != start {
                        break;
                    }
                    text.push_str(&cell.text);
                    // A double-width grapheme owns the following cell; the VT
                    // reports that cell as empty, and stepping over it keeps
                    // the run's column arithmetic honest.
                    col += UnicodeWidthStr::width(cell.text.as_str()).max(1);
                    if !simple {
                        break;
                    }
                }

                painter.draw_run(start, y, &text, &cells[start]);
            }
        }

        let cursor = grid.cursor;
        if let Some(cursor) = cursor {
            // Hidden for this half of the blink cycle, but the cell still keeps
            // its glyph, which the row loop above already drew.
            if imp.blink_on.get() || !cursor.blinking {
                painter.draw_cursor(&cursor, grid, self.has_focus());
            }
        }
        drop(font);
        drop(guard);

        // Recorded rather than acted on: the blink timer reads it, and touching
        // the widget's state from inside a snapshot would be a redraw loop.
        imp.blink_wanted
            .set(cursor.is_some_and(|cursor| cursor.blinking));

        self.draw_scrollbar(snapshot);
    }

    /// The overlay thumb, drawn last so it floats over the text.
    fn draw_scrollbar(&self, snapshot: &gtk::Snapshot) {
        let imp = self.imp();
        let alpha = imp.bar_alpha.get();
        if alpha <= 0.01 {
            return;
        }
        let Some(thumb) = self.thumb() else {
            return;
        };

        // The theme's foreground rather than the desktop's: a dark theme in a
        // light session would otherwise draw a dark thumb on a dark page.
        let held = imp.bar_hover.get() || imp.bar_drag.get().is_some();
        let mut color = rgba(theme_rgb(imp.theme.borrow().foreground));
        color.set_alpha(alpha * if held { 0.5 } else { 0.35 });

        let rect = graphene::Rect::new(thumb.x, thumb.y, thumb.width, thumb.height);
        let radius = graphene::Size::new(thumb.width / 2.0, thumb.width / 2.0);
        let rounded = gtk::gsk::RoundedRect::new(rect, radius, radius, radius, radius);

        snapshot.push_rounded_clip(&rounded);
        snapshot.append_color(&color, &rect);
        snapshot.pop();
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

/// Whether a cell may share a layout with its neighbours.
///
/// One printable ASCII character is a glyph the primary font is certain to
/// have and to advance by exactly one cell. A wide character, a fallback to
/// another face, or a combining sequence is none of those, so it gets a layout
/// to itself and is placed on its column exactly.
fn is_simple(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(chars.next(), Some(c) if c == ' ' || c.is_ascii_graphic()) && chars.next().is_none()
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

/// A theme in the terms the VT understands. The two crates keep their own color
/// types on purpose — `tuni-core` knows nothing about libghostty — so the
/// translation lives here, where both are in scope.
fn colors(theme: &Theme) -> Colors {
    Colors {
        foreground: theme_rgb(theme.foreground),
        background: theme_rgb(theme.background),
        cursor: theme.cursor.map(theme_rgb),
        cursor_text: theme.cursor_text.map(theme_rgb),
        selection_background: theme.selection_background.map(theme_rgb),
        selection_foreground: theme.selection_foreground.map(theme_rgb),
        palette: theme.palette.map(theme_rgb),
    }
}

fn theme_rgb(color: tuni_core::theme::Rgb) -> Rgb {
    Rgb {
        r: color.r,
        g: color.g,
        b: color.b,
    }
}

/// Where a frame is painted and with what. Bundled so the row helpers take one
/// target instead of the same four parameters each.
struct Painter<'a> {
    snapshot: &'a gtk::Snapshot,
    layout: &'a pango::Layout,
    font: &'a pango::FontDescription,
    ligatures: bool,
    m: Metrics,
}

impl Painter<'_> {
    /// One background rectangle spanning the cells `start..end` of a row.
    fn fill_run(&self, start: usize, end: usize, y: f32, color: Rgb) {
        let x = start as f32 * self.m.cell_width;
        let w = (end - start) as f32 * self.m.cell_width;
        self.snapshot.append_color(
            &rgba(color),
            &graphene::Rect::new(x, y, w, self.m.cell_height),
        );
    }

    /// One run of text sharing a style, placed at its own column origin so
    /// per-run advance drift cannot accumulate across the line.
    fn draw_run(&self, start: usize, y: f32, text: &str, style: &tuni_vt::Cell) {
        let mut desc = self.font.clone();
        if style.bold {
            desc.set_weight(pango::Weight::Bold);
        }
        if style.italic {
            desc.set_style(pango::Style::Italic);
        }
        self.layout.set_font_description(Some(&desc));
        self.layout.set_text(text);

        let attrs = pango::AttrList::new();
        if !self.ligatures {
            // A ligature is one glyph where the terminal still counts several
            // cells, so it is off unless the configuration asks for it. These
            // four features are what a coding font joins characters with.
            attrs.insert(pango::AttrFontFeatures::new(
                "liga 0, clig 0, dlig 0, calt 0",
            ));
        }
        if style.underline {
            attrs.insert(pango::AttrInt::new_underline(pango::Underline::Single));
        }
        if style.strikethrough {
            attrs.insert(pango::AttrInt::new_strikethrough(true));
        }
        self.layout.set_attributes(Some(&attrs));

        self.snapshot.save();
        self.snapshot.translate(&graphene::Point::new(
            start as f32 * self.m.cell_width,
            y + self.baseline_offset(),
        ));
        self.snapshot.append_layout(self.layout, &rgba(style.fg));
        self.snapshot.restore();
    }

    /// How far to lift or drop this layout so its baseline lands on the row's.
    ///
    /// `append_layout` places the layout's top-left at the origin, so a run
    /// that fell back to a face with a taller ascent would otherwise sit lower
    /// than the run beside it.
    fn baseline_offset(&self) -> f32 {
        self.m.ascent - self.layout.baseline() as f32 / pango::SCALE as f32
    }

    fn draw_cursor(&self, cursor: &tuni_vt::Cursor, grid: &tuni_vt::Grid, focused: bool) {
        let m = self.m;
        let x = f32::from(cursor.col) * m.cell_width;
        let y = f32::from(cursor.row) * m.cell_height;
        let color = cursor.color.unwrap_or(grid.fg);

        if !focused {
            // An unfocused terminal shows a hollow cell, the convention every
            // terminal on this desktop follows.
            self.draw_hollow(x, y, color);
            return;
        }

        match cursor.shape {
            CursorShape::Block => {
                self.snapshot.append_color(
                    &rgba(color),
                    &graphene::Rect::new(x, y, m.cell_width, m.cell_height),
                );
                // Repaint the covered glyph so it stays readable under the
                // block: in the theme's cursor-text color when it names one,
                // otherwise in the cell's own background, which reads as an
                // inversion.
                if let Some(cell) = grid.cell(cursor.col, cursor.row)
                    && !cell.text.is_empty()
                {
                    let text = cursor.text_color.unwrap_or(cell.bg.unwrap_or(grid.bg));
                    // Back to the base description: the layout still carries
                    // whatever weight the last run of the frame asked for.
                    self.layout.set_font_description(Some(self.font));
                    self.layout.set_text(&cell.text);
                    self.layout.set_attributes(None);
                    self.snapshot.save();
                    self.snapshot
                        .translate(&graphene::Point::new(x, y + self.baseline_offset()));
                    self.snapshot.append_layout(self.layout, &rgba(text));
                    self.snapshot.restore();
                }
            }
            CursorShape::BlockHollow => self.draw_hollow(x, y, color),
            CursorShape::Bar => self.snapshot.append_color(
                &rgba(color),
                &graphene::Rect::new(x, y, 2.0, m.cell_height),
            ),
            CursorShape::Underline => self.snapshot.append_color(
                &rgba(color),
                &graphene::Rect::new(x, y + m.cell_height - 2.0, m.cell_width, 2.0),
            ),
        }
    }

    fn draw_hollow(&self, x: f32, y: f32, color: Rgb) {
        let c = rgba(color);
        let w = self.m.cell_width;
        let h = self.m.cell_height;
        self.snapshot
            .append_color(&c, &graphene::Rect::new(x, y, w, 1.0));
        self.snapshot
            .append_color(&c, &graphene::Rect::new(x, y + h - 1.0, w, 1.0));
        self.snapshot
            .append_color(&c, &graphene::Rect::new(x, y, 1.0, h));
        self.snapshot
            .append_color(&c, &graphene::Rect::new(x + w - 1.0, y, 1.0, h));
    }
}
