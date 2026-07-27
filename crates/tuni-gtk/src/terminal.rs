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
use gtk::gio;
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
    ClipboardTarget, Colors, CursorShape, Geometry, Key, KeyAction, KeyInput, Layer, Mods,
    MouseAction, MouseButton, MouseInput, Progress, Rgb, ScrollPosition,
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

/// The progress bar an application draws by reporting OSC 9;4, and how long a
/// report stands before it is treated as abandoned. A shell killed mid-build
/// never sends the report that clears the bar, so the bar clears itself.
const PROGRESS_HEIGHT: f32 = 2.0;
const PROGRESS_STALE: Duration = Duration::from_secs(15);

/// How long the PTY drain loop may hold the main loop before handing it back.
/// Under a frame's worth, so output that arrives faster than the VT can parse
/// it costs throughput rather than the window's ability to answer.
const FEED_BUDGET: Duration = Duration::from_millis(8);

/// What a pane's session runs. `Launch::default()` is the configured shell in
/// no particular directory, which is what every pane was before a pane could
/// run anything else.
#[derive(Clone, Debug, Default)]
pub struct Launch {
    pub cwd: Option<std::path::PathBuf>,
    /// An exact argv, or empty for the configured command and then the login
    /// shell.
    pub argv: Vec<String>,
    /// Extra environment, folded over the session's own. An ssh pane overrides
    /// `TERM` here, because the terminfo this terminal describes itself with
    /// is not on the machine at the far end.
    pub env: Vec<(String, String)>,
}

/// What the pointer is doing between press and release. A drag either paints a
/// selection or is reported to the application; never both.
#[derive(Clone, Copy, Default)]
enum Pointer {
    #[default]
    Idle,
    Selecting,
    Reporting,
    /// Pressed on a hyperlink. Nothing happens until the release, which is
    /// where Ghostty opens it and where a press that slid off can still be
    /// taken back.
    Link,
    /// Pressed where an application listens for clicks but not for motion. The
    /// press waits: leaving the cell makes it a selection, lifting inside the
    /// cell makes it the click the application was listening for.
    Held {
        x: f64,
        y: f64,
        time: u32,
    },
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

/// What the cell width is measured over: every printable ASCII character,
/// because a face that calls itself monospace is not obliged to prove it and
/// the widest of them is what has to fit.
const WIDTH_SAMPLE: std::ops::RangeInclusive<char> = ' '..='~';

/// Cell geometry derived from the font. Everything on screen is placed off
/// these numbers, so a terminal stays a grid even when the font lies about
/// being monospace.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    cell_width: f32,
    cell_height: f32,
    /// Distance from the top of a cell to the baseline. Runs that fall back to
    /// another face are aligned on this rather than on their own box, so a
    /// missing glyph does not lift or drop the line it lands in.
    ascent: f32,
    /// How thick a line the face draws, which is the thickness the box drawing
    /// characters are built from.
    thickness: f32,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            cell_width: 8.0,
            cell_height: 16.0,
            ascent: 12.0,
            thickness: 1.0,
        }
    }
}

struct Session {
    term: tuni_vt::Terminal,
    /// Gone once the program in front of it has exited. The screen it left
    /// behind stays: the last thing a session printed is usually the reason it
    /// ended, and for a connection that pane is the only place the reason is
    /// written down.
    pty: Option<Pty>,
}

/// Types at the program in front of a session, if there still is one. Free
/// rather than a method, so the caller can hold the terminal state at the same
/// time: encoding a key needs it, and the two are separate fields.
fn send(pty: &mut Option<Pty>, bytes: &[u8]) {
    if let Some(pty) = pty {
        let _ = pty.write(bytes);
    }
}

/// How many inline images are kept as textures at once. An image is uploaded
/// again the moment it is drawn again, so this only decides how much scrolling
/// past a picture costs; it is not a limit on how many a terminal may hold.
const TEXTURE_CACHE: usize = 16;

/// How often a widget that is still being resized passes its size on, and how
/// often it is looked at to see whether it has stopped. Two quiet ticks end the
/// wait, so the shell hears about the size a drag finished on between one and
/// two of these after the last frame. Twenty resizes a second is enough to read
/// as text moving with the pointer and few enough that a shell has time to
/// answer each one; kitty's own debounce is in the same range.
const RESIZE_SETTLE: Duration = Duration::from_millis(50);

/// Uploaded inline images, keyed by which image and which version of it.
///
/// The key carries the storage's generation, so a plot redrawn under the same
/// image id lands as a new texture rather than as the previous frame.
#[derive(Default)]
struct Textures {
    map: std::collections::HashMap<tuni_vt::ImageKey, gdk::MemoryTexture>,
    /// Insertion order, for evicting the oldest when the map is full.
    order: std::collections::VecDeque<tuni_vt::ImageKey>,
}

impl Textures {
    fn get(&self, key: &tuni_vt::ImageKey) -> Option<&gdk::MemoryTexture> {
        self.map.get(key)
    }

    fn insert(&mut self, key: tuni_vt::ImageKey, pixels: &tuni_vt::Pixels) {
        let stride = pixels.width as usize * 4;
        let bytes = glib::Bytes::from(&pixels.rgba[..]);
        let texture = gdk::MemoryTexture::new(
            pixels.width as i32,
            pixels.height as i32,
            // Straight alpha, which is what the protocol transmits.
            gdk::MemoryFormat::R8g8b8a8,
            &bytes,
            stride,
        );
        while self.order.len() >= TEXTURE_CACHE {
            if let Some(oldest) = self.order.pop_front() {
                self.map.remove(&oldest);
            }
        }
        self.order.push_back(key);
        self.map.insert(key, texture);
    }
}

/// What the find bar asked for and what the terminal answered.
///
/// Empty needle means the bar is closed or has nothing typed in it, which is
/// the same thing as far as the drawing is concerned: nothing is highlighted.
#[derive(Default)]
struct Find {
    needle: String,
    hits: Vec<tuni_vt::Hit>,
    /// Which hit the viewport was moved to, as an index into `hits`. `None`
    /// until the first step, so opening the bar counts matches without
    /// dragging the viewport away from what the user was looking at.
    current: Option<usize>,
}

impl Find {
    fn status(&self) -> FindStatus {
        FindStatus {
            total: self.hits.len(),
            // Shown to a person, so counted from one.
            current: self.current.map(|index| index + 1),
        }
    }
}

/// What the find bar puts beside its entry: how many matches there are, and
/// which of them the viewport was stepped to, if any.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FindStatus {
    pub total: usize,
    pub current: Option<usize>,
}

/// How far a match's background is tinted toward the theme's yellow, and how
/// far the one the viewport is on goes past it.
const MATCH_TINT: f64 = 0.55;
const CURRENT_TINT: f64 = 1.0;

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
        /// The context text is measured and drawn through, and the serial of
        /// the widget's own context when it was made. See `text_context`.
        pub(super) pango: RefCell<Option<pango::Context>>,
        pub(super) pango_serial: Cell<u32>,
        /// Grid size last pushed to the VT, so a resize that does not cross a
        /// cell boundary costs nothing. The cell's pixel size goes with it,
        /// because zooming changes that without changing the grid.
        pub(super) grid_size: Cell<(u16, u16)>,
        pub(super) cell_size: Cell<(u16, u16)>,
        /// The same two as the shell was last told, which is not the same
        /// thing: a resize in progress reflows the screen every tick and hands
        /// the shell the size it finished on. See `apply_size`.
        pub(super) shell_size: Cell<((u16, u16), (u16, u16))>,
        /// A size the widget has been given but the shell has not been told
        /// about yet, and the timer that will tell it once the widget stops
        /// moving. See `apply_size`.
        pub(super) pending_size: Cell<Option<(i32, i32)>>,
        pub(super) resize_timer: RefCell<Option<glib::SourceId>>,
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
        /// Modifiers as of the last event, so pressing Ctrl with a stationary
        /// pointer can light up the hyperlink under it.
        pub(super) mods: Cell<Mods>,
        /// The hyperlink under the pointer, the cell it was found on, and
        /// whether that answer still stands. Output, scrolling, and resizing
        /// all move what a viewport coordinate means, so each of them retires
        /// the answer rather than the highlight.
        pub(super) link_hover: RefCell<Option<tuni_vt::LinkHover>>,
        pub(super) link_probe: Cell<Option<(u16, u16)>>,
        pub(super) link_valid: Cell<bool>,
        /// The context menu, its actions, and the hyperlink it was opened over.
        /// What the menu holds depends on where the pointer was, so the model
        /// is built per press and the link the first section acts on is kept
        /// here until the menu is done with it.
        pub(super) menu: RefCell<Option<gtk::PopoverMenu>>,
        pub(super) menu_actions: RefCell<Option<gio::SimpleActionGroup>>,
        pub(super) menu_link: RefCell<Option<String>>,
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
        /// What the find bar is looking for, where it is, and which of those is
        /// the one the viewport was moved to, plus whether a re-search over new
        /// output is already queued.
        pub(super) find: RefCell<Find>,
        pub(super) find_pending: Cell<bool>,
        /// The progress report in effect (OSC 9;4), the percentage last given
        /// — states that report none carry the one before — and the timer that
        /// retires a report nothing has refreshed.
        pub(super) progress: Cell<Option<Progress>>,
        pub(super) progress_value: Cell<u8>,
        pub(super) progress_stale: RefCell<Option<glib::SourceId>>,
        /// Inline images: where this frame's are, and the textures they were
        /// last uploaded as. The geometry is rebuilt every frame because
        /// scrolling moves it; the pixels are not, because they are the
        /// expensive half.
        pub(super) images: RefCell<Vec<tuni_vt::Placement>>,
        pub(super) textures: RefCell<Textures>,
        /// Draw timings, collected only when TUNI_DEBUG_FRAME_TIME is set.
        /// This is the measurement that decides whether Pango can keep up. The
        /// value is how many frames go into one report, so a scenario that
        /// draws a handful of times can still be measured; zero is off.
        pub(super) frame_batch: usize,
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
                pango: RefCell::new(None),
                pango_serial: Cell::new(0),
                grid_size: Cell::new((0, 0)),
                cell_size: Cell::new((0, 0)),
                shell_size: Cell::new(((0, 0), (0, 0))),
                pending_size: Cell::new(None),
                resize_timer: RefCell::new(None),
                im: RefCell::new(None),
                pending_commit: RefCell::new(None),
                title: RefCell::new(None),
                cwd: RefCell::new(None),
                pointer: Cell::new(Pointer::default()),
                buttons_down: Cell::new(0),
                pointer_pos: Cell::new((0.0, 0.0)),
                mods: Cell::new(Mods::empty()),
                link_hover: RefCell::new(None),
                link_probe: Cell::new(None),
                link_valid: Cell::new(false),
                menu: RefCell::new(None),
                menu_actions: RefCell::new(None),
                menu_link: RefCell::new(None),
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
                find: RefCell::new(Find::default()),
                find_pending: Cell::new(false),
                progress: Cell::new(None),
                progress_value: Cell::new(0),
                progress_stale: RefCell::new(None),
                images: RefCell::new(Vec::new()),
                textures: RefCell::new(Textures::default()),
                frame_batch: match std::env::var("TUNI_DEBUG_FRAME_TIME") {
                    Ok(value) => value.trim().parse().unwrap_or(120).max(2),
                    Err(_) => 0,
                },
                frame_times: RefCell::new(Vec::new()),
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

        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: std::sync::OnceLock<Vec<glib::subclass::Signal>> =
                std::sync::OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    // The shell is gone. Whoever placed this widget decides what
                    // that means — the tab strip closes the tab.
                    glib::subclass::Signal::builder("exited").build(),
                    // The application rang. A tab that is not on screen shows it
                    // as an attention mark rather than as a sound.
                    glib::subclass::Signal::builder("bell").build(),
                    // The application asked the desktop to show something
                    // (OSC 9, OSC 99, OSC 777): a title and a body, either of
                    // which may be empty. Not called "notify", which is
                    // GObject's own.
                    glib::subclass::Signal::builder("desktop-notify")
                        .param_types([String::static_type(), String::static_type()])
                        .build(),
                    // Output changed what the live search finds. The find bar
                    // reads the tally back rather than being handed it, because
                    // by the time it runs the terminal may have moved on again.
                    glib::subclass::Signal::builder("find-changed").build(),
                ]
            })
        }

        fn constructed(&self) {
            self.parent_constructed();
            crate::debug::born("TuniTerminal");
            let obj = self.obj();
            obj.set_focusable(true);
            obj.set_can_focus(true);
            obj.set_focus_on_click(true);
            obj.setup_font();
            obj.setup_input();
            obj.setup_menu();
            obj.watch_display();
        }

        fn dispose(&self) {
            if let Some(source) = self.blink_source.take() {
                source.remove();
            }
            if let Some(tick) = self.bar_tick.take() {
                tick.remove();
            }
            if let Some(source) = self.progress_stale.take() {
                source.remove();
            }
            if let Some(source) = self.resize_timer.take() {
                source.remove();
            }
            // Parented rather than packed, so it has to be taken off by hand.
            if let Some(menu) = self.menu.take() {
                menu.unparent();
            }
            // Drop the session before the widget so the reader thread's channel
            // closes and the shell gets its SIGHUP.
            self.session.replace(None);
        }
    }

    impl Drop for TuniTerminal {
        fn drop(&mut self) {
            crate::debug::died("TuniTerminal");
        }
    }

    impl WidgetImpl for TuniTerminal {
        fn root(&self) {
            self.parent_root();
            if let Some(im) = self.im.borrow().as_ref() {
                im.set_client_widget(Some(&*self.obj()));
            }
        }

        fn unroot(&self) {
            // Breaks the reference the input method holds on this widget. A
            // widget with no window under it has no input method to talk to
            // anyway, and this is the point at which a closed pane's terminal
            // becomes garbage rather than furniture.
            if let Some(im) = self.im.borrow().as_ref() {
                im.set_client_widget(gtk::Widget::NONE);
            }
            self.parent_unroot();
        }

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
            if self.frame_batch == 0 {
                self.obj().draw(snapshot);
                return;
            }

            let started = std::time::Instant::now();
            self.obj().draw(snapshot);
            let elapsed = started.elapsed();

            let mut frames = self.frame_times.borrow_mut();
            frames.push(elapsed);
            if frames.len() >= self.frame_batch {
                frames.sort_unstable();
                let last = frames.len() - 1;
                eprintln!(
                    "frame: n {}  p50 {:?}  p95 {:?}  max {:?}",
                    frames.len(),
                    frames[last / 2],
                    frames[last * 95 / 100],
                    frames[last]
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
            send(&mut session.pty, text.as_bytes());
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

    /// The shell's process id. `None` before it starts and after it exits,
    /// which is what the Info panel shows an empty page for.
    #[must_use]
    pub fn shell_pid(&self) -> Option<u32> {
        self.imp()
            .session
            .borrow()
            .as_ref()
            .and_then(|session| session.pty.as_ref()?.shell_pid())
    }

    /// Whether anything is running in here. False before the first session
    /// starts and again after one ends, when what is on screen is the last
    /// thing the dead one printed and nothing can be typed at it.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.imp()
            .session
            .borrow()
            .as_ref()
            .is_some_and(|session| session.pty.is_some())
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

    /// The Pango context every measurement and every run is drawn through.
    ///
    /// Not the widget's own: GTK leaves glyph positions unrounded there when
    /// the desktop asks for unhinted text, and a face that then advances
    /// 8.7998 px per cell would put the eightieth column two pixels away from
    /// the background that was filled for it. A terminal is a grid, so this
    /// context rounds. The widget's context still decides everything else —
    /// DPI, font options, text direction — so a fresh copy is taken whenever
    /// its serial says any of that moved.
    fn text_context(&self) -> pango::Context {
        let imp = self.imp();
        let serial = self.pango_context().serial();
        let mut cached = imp.pango.borrow_mut();
        if cached.is_none() || imp.pango_serial.get() != serial {
            let context = self.create_pango_context();
            context.set_round_glyph_positions(true);
            imp.pango_serial.set(serial);
            *cached = Some(context);
        }
        cached.clone().expect("context was just put there")
    }

    /// Watch what the desktop can change under a running terminal: the monitor
    /// scale, the font DPI, and how the desktop asks for text to be rendered.
    /// All three change what a point is worth in pixels, and the grid is built
    /// out of pixels.
    fn watch_display(&self) {
        self.connect_scale_factor_notify(|this| this.remeasure());

        let Some(settings) = gtk::Settings::default() else {
            return;
        };
        for property in ["gtk-xft-dpi", "gtk-font-rendering", "gtk-xft-antialias"] {
            settings.connect_notify_local(
                Some(property),
                glib::clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |_, _| this.remeasure()
                ),
            );
        }
    }

    fn update_metrics(&self) {
        let imp = self.imp();
        let context = self.text_context();
        let font = imp.font.borrow();
        let metrics = context.metrics(Some(&font), None);

        let scale = pango::SCALE as f32;
        let ascent = metrics.ascent() as f32 / scale;
        let descent = metrics.descent() as f32 / scale;
        // The font's own line height where it states one, which is ascent and
        // descent plus the leading the designer asked for. Pango answers zero
        // for a face that states none, and then the two halves are all there
        // is to go on.
        let line = match metrics.height() {
            0 => ascent + descent,
            height => height as f32 / scale,
        };

        // Measured rather than taken from `approximate_char_width`: that number
        // is a hint the font supplies, and a face whose hint disagrees with its
        // own advances would draw a grid that drifts a fraction of a pixel per
        // column. Laying out real glyphs asks the question the renderer will.
        //
        // The widest of them, not the average: a face that calls itself
        // monospace can still advance one character wider than the rest, and a
        // cell built on the average would let that one spill into its
        // neighbour. Ghostty measures the same way.
        let layout = pango::Layout::new(&context);
        layout.set_font_description(Some(&font));
        let mut widest = 0;
        let mut sample = [0u8; 4];
        for ch in WIDTH_SAMPLE {
            layout.set_text(ch.encode_utf8(&mut sample));
            widest = widest.max(layout.size().0);
        }
        let measured = widest as f32 / scale;
        let cell_width = if measured >= 1.0 {
            measured
        } else {
            metrics.approximate_char_width() as f32 / scale
        };
        drop(font);

        // Whole pixels, because every column and every row is placed off these
        // two numbers: a cell a fraction of a pixel wide would have the
        // eightieth column drawn where the seventy-ninth was filled.
        let extra = imp.config.borrow().line_height_extra as f32;
        let cell_width = cell_width.max(1.0).ceil();
        let cell_height = (line + extra).max(1.0).ceil();
        imp.metrics.set(Metrics {
            cell_width,
            cell_height,
            // Whatever the rounding and the configured extra leading added
            // goes half above the text and half below, so a line sits in the
            // middle of its cell rather than riding the top of it.
            ascent: (ascent + (cell_height - ascent - descent) / 2.0).round(),
            // The underline is the only stroke a face states a thickness for,
            // and it is the one Ghostty draws its box characters at.
            thickness: (metrics.underline_thickness() as f32 / scale)
                .ceil()
                .max(1.0),
        });
    }

    /// Take the font's measurements again and rebuild the grid from them.
    /// The desktop changing its scale factor or its font settings changes what
    /// a point is worth in pixels, and every cell on screen is placed off that.
    fn remeasure(&self) {
        let before = self.imp().metrics.get();
        self.update_metrics();
        if self.imp().metrics.get() != before {
            self.apply_size(self.width(), self.height());
            self.queue_draw();
        }
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
        // An I-beam over text, so the hand over a hyperlink is a change the eye
        // catches rather than the only cursor the widget ever shows.
        self.set_cursor_from_name(Some("text"));

        let im = gtk::IMMulticontext::new();
        // The client widget is handed over in `root` and taken back in
        // `unroot`, not here: the platform input methods hold a strong
        // reference to it — GtkIMContextWayland keeps one — and a terminal that
        // handed itself over for good would be its own last reference, never
        // reach zero, never be disposed, and keep its VT, its scrollback and
        // its timers for as long as the process ran.

        im.connect_commit(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, text| {
                this.imp().pending_commit.replace(Some(text.to_owned()));
            }
        ));
        self.imp().im.replace(Some(im));

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |controller, keyval, keycode, state| this
                .on_key(controller, keyval, keycode, state)
        ));
        keys.connect_modifiers(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, state| {
                // Ctrl pressed under a stationary pointer arms the hyperlink
                // beneath it, and releasing Ctrl puts it out again.
                let mods = keymap::mods_from_state(state);
                this.imp().mods.set(mods);
                this.refresh_links(mods);
                glib::Propagation::Proceed
            }
        ));
        keys.connect_key_released(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |controller, keyval, keycode, state| {
                this.on_key_release(controller, keyval, keycode, state);
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
                this.report_focus(true);
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
                this.report_focus(false);
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
        motion.connect_leave(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                // A pointer that leaves while it was over the scrollbar strip
                // is no longer hovering it, and only motion inside the widget
                // ever said otherwise — so without this the flag stays set,
                // `fade_scrollbar` reads it as held, the alpha never reaches
                // zero, and the tick callback repaints the whole viewport at
                // the frame clock for as long as the pane exists.
                this.imp().bar_hover.set(false);
                this.clear_links();
            }
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

    /// The context menu and the actions its items name.
    ///
    /// The menu is made once and filled per press, because what belongs in it
    /// depends on where the pointer was: a hyperlink under it adds a section,
    /// and nothing selected greys Copy out rather than hiding it.
    fn setup_menu(&self) {
        let actions = gio::SimpleActionGroup::new();
        actions.add_action_entries([
            action("copy", self, |terminal| {
                terminal.copy_selection();
            }),
            action("paste", self, TuniTerminal::paste_clipboard),
            action("select-all", self, TuniTerminal::select_all),
            action("open-link", self, |terminal| {
                let uri = terminal.imp().menu_link.borrow().clone();
                if let Some(uri) = uri {
                    terminal.open_uri(&uri);
                }
            }),
            action("copy-link", self, |terminal| {
                if let Some(uri) = terminal.imp().menu_link.borrow().as_ref() {
                    terminal.clipboard().set_text(uri);
                }
            }),
        ]);
        self.insert_action_group("term", Some(&actions));

        let menu = gtk::PopoverMenu::from_model(None::<&gio::Menu>);
        menu.set_has_arrow(false);
        menu.set_halign(gtk::Align::Start);
        menu.set_parent(self);

        let imp = self.imp();
        imp.menu_actions.replace(Some(actions));
        imp.menu.replace(Some(menu));
    }

    /// Puts the context menu up over the cell that was clicked.
    fn popup_menu(&self, x: f64, y: f64) {
        let imp = self.imp();
        let Some(menu) = imp.menu.borrow().clone() else {
            return;
        };

        let link = self.link_at(x, y);
        if let Some(action) = imp
            .menu_actions
            .borrow()
            .as_ref()
            .and_then(|actions| actions.lookup_action("copy"))
            .and_downcast::<gio::SimpleAction>()
        {
            action.set_enabled(self.selection().is_some());
        }

        let model = gio::Menu::new();
        if let Some(uri) = link.as_deref() {
            let links = gio::Menu::new();
            // A link the desktop would not be handed is still a link worth
            // copying, so only the opening half waits on `can_open`.
            if can_open(uri) {
                links.append(Some("Open Link"), Some("term.open-link"));
            }
            links.append(Some("Copy Link Address"), Some("term.copy-link"));
            model.append_section(None, &links);
        }
        imp.menu_link.replace(link);

        let clipboard = gio::Menu::new();
        clipboard.append(Some("Copy"), Some("term.copy"));
        clipboard.append(Some("Paste"), Some("term.paste"));
        clipboard.append(Some("Select All"), Some("term.select-all"));
        model.append_section(None, &clipboard);

        let screen = gio::Menu::new();
        screen.append(Some("Find…"), Some("win.find"));
        screen.append(Some("Clear"), Some("win.clear-terminal"));
        model.append_section(None, &screen);

        let panes = gio::Menu::new();
        panes.append(Some("Split Right"), Some("win.split-right"));
        panes.append(Some("Split Down"), Some("win.split-down"));
        panes.append(Some("Close Pane"), Some("win.close-pane"));
        model.append_section(None, &panes);

        menu.set_menu_model(Some(&model));
        crate::menu::popup_at(&menu, graphene::Point::new(x as f32, y as f32));
    }

    /// The hyperlink on the cell at a surface position, asked for without
    /// disturbing the highlight: Ctrl is what arms a hover, and a menu is
    /// opened without it.
    fn link_at(&self, x: f64, y: f64) -> Option<String> {
        let (col, row) = self.geometry().cell_at(x, y)?;
        let mut guard = self.imp().session.borrow_mut();
        let hover = guard
            .as_mut()?
            .term
            .hyperlink_hover(col, row)
            .ok()
            .flatten()?;
        Some(hover.uri)
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

        // The cursor is the one thing here a running screen already holds a
        // copy of, so a session that is open is told again rather than left
        // showing what it was started with.
        if let Some(session) = imp.session.borrow_mut().as_mut() {
            let _ = session
                .term
                .set_default_cursor_blink(Some(config.cursor_blink));
            let _ = session
                .term
                .set_default_cursor_style(Some(cursor_style(config.cursor_style)));
        }

        // Margin rather than an offset inside the widget: every coordinate in
        // here is the grid's own, from a click to a selection to an image
        // placement, and an origin that was not 0,0 would have to be taken back
        // out of each of them.
        let (across, down) = (config.padding_x as i32, config.padding_y as i32);
        self.set_margin_start(across);
        self.set_margin_end(across);
        self.set_margin_top(down);
        self.set_margin_bottom(down);

        self.setup_font();
        self.apply_size(self.width(), self.height());
        self.queue_resize();
        self.queue_draw();
    }

    // --- session lifecycle -------------------------------------------------

    /// Start the session. Safe to call once the widget has a size; before that
    /// the grid falls back to 80x24 and is corrected on the first allocation.
    pub fn start(&self, launch: &Launch) -> Result<(), String> {
        let imp = self.imp();
        let m = imp.metrics.get();
        let (cols, rows) = {
            let (c, r) = imp.grid_size.get();
            if c == 0 || r == 0 { (80, 24) } else { (c, r) }
        };

        let mut term = tuni_vt::Terminal::new(cols, rows, imp.config.borrow().scrollback_lines)
            .map_err(|e| e.to_string())?;
        let _ = term.set_default_cursor_blink(Some(imp.config.borrow().cursor_blink));
        let _ = term.set_default_cursor_style(Some(cursor_style(imp.config.borrow().cursor_style)));
        let _ = term.set_colors(&colors(&imp.theme.borrow()));

        // A configured shell this machine has no program for falls back to the
        // login shell rather than failing to open a terminal at all. The
        // settings window says so where the name was typed. An argv of its own
        // names the program it runs, so the configured shell means nothing to
        // it.
        let shell = launch
            .argv
            .is_empty()
            .then(|| tuni_pty::resolve_shell(&imp.config.borrow().command))
            .flatten();
        let mut config = PtyConfig {
            shell,
            argv: launch.argv.clone(),
            cwd: launch.cwd.clone(),
            cols,
            rows,
            cell_width_px: m.cell_width.round() as u16,
            cell_height_px: m.cell_height.round() as u16,
            ..PtyConfig::default()
        };
        config.env.extend(launch.env.iter().cloned());
        let pty = Pty::spawn(&config).map_err(|e| e.to_string())?;

        let events = pty.events();
        imp.session.replace(Some(Session {
            term,
            pty: Some(pty),
        }));
        let cell = (m.cell_width.round() as u16, m.cell_height.round() as u16);
        imp.grid_size.set((cols, rows));
        imp.cell_size.set(cell);
        // The shell was started at this size, so it already knows it.
        imp.shell_size.set(((cols, rows), cell));

        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                // `recv()` completes without suspending whenever a chunk is
                // already queued, so this loop is one `poll` for as long as the
                // reader thread can keep the channel full — and the reader wins
                // whenever the shell emits escapes faster than the VT parses
                // them. Nothing else on the main loop runs in the meantime: not
                // the frame clock, not the key handler, not the timer that was
                // supposed to end it. Yielding on a budget rather than on every
                // chunk keeps the main loop answering within a frame without
                // paying a round trip through the loop for each 64 KiB.
                let mut served = Instant::now();
                while let Ok(event) = events.recv().await {
                    match event {
                        PtyEvent::Output(bytes) => {
                            this.feed(&bytes);
                            if served.elapsed() >= FEED_BUDGET {
                                // Below the frame clock's redraw idle, not at
                                // the default priority a plain yield uses:
                                // a ready source at priority 0 outranks the
                                // redraw at 120, so yielding there hands the
                                // loop straight back to this future and the
                                // window still never paints.
                                glib::timeout_future_with_priority(
                                    glib::Priority::DEFAULT_IDLE,
                                    Duration::ZERO,
                                )
                                .await;
                                served = Instant::now();
                            }
                        }
                        PtyEvent::Exited => {
                            // The pty goes and the screen stays, so whatever
                            // the session said on its way out is still there to
                            // read. Scoped, because emitting reaches back in.
                            if let Some(session) = this.imp().session.borrow_mut().as_mut() {
                                session.pty = None;
                            }
                            this.queue_draw();
                            this.emit_by_name::<()>("exited", &[]);
                            break;
                        }
                    }
                }
                crate::debug::note("feed future ended");
            }
        ));

        Ok(())
    }

    /// Hang up: drop the PTY, which closes the master and sends the shell its
    /// SIGHUP — the same death a closed terminal window deals.
    ///
    /// Dropping the widget does this too, but a closed tab may outlive its own
    /// close for as long as an animation holds a reference, and the shell
    /// should not.
    pub fn shutdown(&self) {
        self.imp().session.replace(None);
    }

    /// Throw away what is on screen and the scrollback behind it, then ask the
    /// program in front for its prompt again.
    ///
    /// The erase is fed to our own parser rather than typed at the shell, so it
    /// works while something is running and does not depend on `clear` being
    /// installed. `Ctrl+L` afterwards is what makes a shell — or a full-screen
    /// program, which reads it as "redraw" — put its prompt back on the blank
    /// screen instead of leaving the eye nothing to look at.
    pub fn clear(&self) {
        if !self.is_running() {
            return;
        }
        // Home, erase the screen, erase the scrollback: what `clear` sends on a
        // terminal whose terminfo carries `E3`.
        self.feed(b"\x1b[H\x1b[2J\x1b[3J");
        self.send_text("\x0c");
    }

    /// The selected text, if there is a selection. What a search for the
    /// selection looks for.
    #[must_use]
    pub fn selection(&self) -> Option<String> {
        let mut guard = self.imp().session.borrow_mut();
        guard
            .as_mut()
            .and_then(|session| session.term.selection_text().ok().flatten())
            .filter(|text| !text.is_empty())
    }

    /// What this terminal is holding, as the VT bytes that reproduce it, for
    /// the session file. `None` for a terminal that never ran or never
    /// printed anything.
    pub fn history(&self, max_lines: usize) -> Option<String> {
        let guard = self.imp().session.borrow();
        let session = guard.as_ref()?;
        session.term.dump_history(max_lines).ok().flatten()
    }

    /// Replays a saved terminal above the prompt of the shell that just
    /// started.
    ///
    /// Safe to call right after [`Self::start`]: the shell's own output comes
    /// through a channel and so lands after this, however fast it prints.
    ///
    /// The rule is not just to restore output but to be honest about it. What
    /// is on screen was not printed by the shell now running — no command in it
    /// can be re-run by pressing up, and nothing it says about the working
    /// directory is still true. The divider says where the old session ends,
    /// the way kero's does.
    pub fn restore_history(&self, text: &str) {
        if !self.is_running() {
            return;
        }
        let mut bytes = Vec::with_capacity(text.len() + 64);
        bytes.extend_from_slice(text.as_bytes());
        if !text.ends_with('\n') {
            bytes.extend_from_slice(b"\r\n");
        }
        // Dim, on its own line, and not styled by anything the restored output
        // left switched on.
        bytes.extend_from_slice(
            "\x1b[0m\x1b[2m── Session Contents Restored ──\x1b[0m\r\n".as_bytes(),
        );
        self.feed(&bytes);
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
                eprintln!(
                    "pty_write: {:?}",
                    String::from_utf8_lossy(&effects.pty_write)
                );
            }
            send(&mut session.pty, &effects.pty_write);
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
        // A silenced bell is silenced everywhere it would have been heard: the
        // widget's alert, the tab's mark, and the notification a background tab
        // raises all hang off this.
        if effects.bell && imp.config.borrow().bell {
            self.error_bell();
            self.emit_by_name::<()>("bell", &[]);
        }
        for notification in &effects.notifications {
            self.emit_by_name::<()>("desktop-notify", &[&notification.title, &notification.body]);
        }
        if let Some(progress) = effects.progress {
            self.apply_progress(progress);
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
        self.invalidate_links();
        self.schedule_refind();
        self.queue_draw();
    }

    /// Takes note of the widget's new size: reshapes the screen to it while it
    /// is still moving, and tells the shell once it has stopped.
    ///
    /// A resize is worth following rather than waiting out — text that rewraps
    /// under the pointer is what dragging an edge is for, and a terminal still
    /// showing the old shape until the edge is let go looks stuck — but only
    /// the terminal's half of it can be followed. Every SIGWINCH is a prompt
    /// the shell draws again, and a shell draws it by counting the rows it
    /// wrote and moving back over them, which the reflow has just rewrapped
    /// into a different number: the move lands short and the head of the old
    /// prompt stays on screen. One resize leaves one; twenty a second leave a
    /// screenful. So the reflow runs at the tick rate below and the SIGWINCH
    /// waits for the size to settle, which is the one thing that has to happen
    /// exactly once.
    fn apply_size(&self, width: i32, height: i32) {
        let imp = self.imp();
        // Nothing is running: the first allocation decides what size the shell
        // is started at, and a screen nobody is writing to has no reflow to
        // debounce.
        if !self.is_running() {
            self.commit_size(width, height, true);
            return;
        }

        imp.pending_size.set(Some((width, height)));
        if imp.resize_timer.borrow().is_some() {
            return;
        }
        // What the previous tick saw. Two ticks that read the same size mean
        // nothing moved in between, which is the end of the resize.
        let seen: Cell<Option<(i32, i32)>> = Cell::new(None);
        let id = glib::timeout_add_local(
            RESIZE_SETTLE,
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    let imp = this.imp();
                    let now = imp.pending_size.get();
                    let settled = now == seen.get();
                    seen.set(now);
                    if settled {
                        imp.resize_timer.replace(None);
                        imp.pending_size.set(None);
                    }
                    // Reshaped on the way as well as at the end, and the shell
                    // told only at the end. The settling tick has nothing new
                    // to reflow by definition, and reflowing it is free: a size
                    // the grid is already at is dropped.
                    if let Some((width, height)) = now {
                        this.commit_size(width, height, settled);
                    }
                    if settled {
                        glib::ControlFlow::Break
                    } else {
                        glib::ControlFlow::Continue
                    }
                }
            ),
        );
        imp.resize_timer.replace(Some(id));
    }

    /// Reshapes the screen, and hands the shell the same shape when
    /// `tell_shell` says the resize is over.
    ///
    /// The two are separate because they answer to different clocks. Reflowing
    /// costs a pass over the scrollback and shows on screen, so it happens
    /// while the pointer is still moving. A SIGWINCH costs whatever the program
    /// on the other end does about it, which for a shell is drawing its prompt
    /// again from a cursor it moved by counting the rows it wrote at the *old*
    /// width — one row out for every row the reflow rewrapped, and the head of
    /// the old prompt left on screen per resize. One SIGWINCH at the end is one
    /// redraw, and one redraw cannot land in the wrong place.
    fn commit_size(&self, width: i32, height: i32, tell_shell: bool) {
        let imp = self.imp();
        let m = imp.metrics.get();
        let cols =
            ((width as f32 / m.cell_width).floor() as i32).clamp(1, i32::from(u16::MAX)) as u16;
        let rows =
            ((height as f32 / m.cell_height).floor() as i32).clamp(1, i32::from(u16::MAX)) as u16;
        let cell = (
            (m.cell_width.round() as i32).clamp(1, i32::from(u16::MAX)) as u16,
            (m.cell_height.round() as i32).clamp(1, i32::from(u16::MAX)) as u16,
        );

        // Zooming can leave the grid the same shape while changing what a cell
        // measures, and an application that draws with sixels or images needs
        // the pixel size to be right either way.
        let reshaped = imp.grid_size.get() != (cols, rows) || imp.cell_size.get() != cell;
        let tell_shell = tell_shell && imp.shell_size.get() != ((cols, rows), cell);
        if !reshaped && !tell_shell {
            return;
        }
        imp.grid_size.set((cols, rows));
        imp.cell_size.set(cell);
        if tell_shell {
            imp.shell_size.set(((cols, rows), cell));
        }

        {
            let mut guard = imp.session.borrow_mut();
            if let Some(session) = guard.as_mut() {
                if reshaped {
                    let _ = session
                        .term
                        .resize(cols, rows, u32::from(cell.0), u32::from(cell.1));
                }
                if tell_shell && let Some(pty) = &session.pty {
                    let _ = pty.resize(cols, rows, cell.0, cell.1);
                }
            }
        }
        if !reshaped {
            return;
        }
        // Reflow moves every viewport coordinate, the hover's cells among them,
        // and every row a match was found on.
        self.invalidate_links();
        self.schedule_refind();
    }

    // --- cursor blinking -----------------------------------------------------

    /// Desktop blink preferences: whether to blink at all, the full cycle, and
    /// how long after the last input blinking stops. Following GtkSettings
    /// rather than a private default keeps the terminal's cursor in step with
    /// every text field on the desktop.
    fn blink_settings(&self) -> (bool, Duration, Option<Duration>) {
        let settings = gtk::Settings::for_display(&self.display());
        let cycle =
            Duration::from_millis(u64::from(settings.gtk_cursor_blink_time().max(100) as u32));
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
        // The blink timeout is measured from the last input, and a terminal
        // that has been focused but never typed into has none — which left
        // `idle` below permanently false and the cursor blinking for as long as
        // the window kept focus. Focusing is what restarts the blink in every
        // other text cursor on the desktop, so it is what starts the clock.
        self.imp().last_input.set(Some(Instant::now()));

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
            // Waking twice a second to decide not to draw is the whole cost of
            // an idle terminal, so the timer stops rather than idles — but only
            // for the two conditions with a way back: focus returns through
            // `start_blink`, and a keystroke through `note_input`, which
            // re-arms when the slot is empty. A cursor that merely asked to
            // stay solid has no such path, so it keeps the timer until the
            // idle timeout above retires it. Taking the id without removing it
            // is what `Break` needs: `SourceId` is not a guard, and the source
            // is gone the moment this returns.
            if idle || !self.has_focus() {
                let _ = imp.blink_source.take();
                return glib::ControlFlow::Break;
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
        let height =
            (track * scroll.proportion() as f32).clamp(SCROLLBAR_MIN_THUMB.min(track), track);
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
            || imp
                .bar_until
                .get()
                .is_some_and(|until| Instant::now() < until);
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

        self.invalidate_links();
        self.reveal_scrollbar();
        self.queue_draw();
    }

    /// What is on screen as plain text, the last `lines` of it, for the card
    /// the tab switcher draws.
    ///
    /// The tail rather than the head: the bottom of a terminal is where the
    /// work is, and a card showing the top of a full screen would be a picture
    /// of whatever scrolled past minutes ago.
    #[must_use]
    pub fn preview(&self, lines: usize) -> String {
        let mut guard = self.imp().session.borrow_mut();
        let Some(session) = guard.as_mut() else {
            return String::new();
        };
        let Ok(grid) = session.term.snapshot() else {
            return String::new();
        };
        let mut rows: Vec<String> = (0..grid.rows)
            .map(|row| {
                let text: String = grid
                    .row(row)
                    .iter()
                    .map(|cell| cell.text.as_str())
                    .collect();
                text.trim_end().to_owned()
            })
            .collect();
        while rows.last().is_some_and(String::is_empty) {
            rows.pop();
        }
        if rows.len() > lines {
            rows.drain(..rows.len() - lines);
        }
        rows.join("\n")
    }

    // --- find ----------------------------------------------------------------

    /// Look for `needle`, forget where the last search was, and report the tally
    /// the find bar shows. An empty needle is how the bar says it has nothing to
    /// look for, and clears the highlight.
    pub fn find(&self, needle: &str) -> FindStatus {
        let hits = self.search(needle);
        let imp = self.imp();
        let mut find = imp.find.borrow_mut();
        needle.clone_into(&mut find.needle);
        find.hits = hits;
        find.current = None;
        let status = find.status();
        drop(find);
        self.queue_draw();
        status
    }

    /// Move to the next match, or the previous one, scrolling it into view.
    ///
    /// The first step from a fresh search starts at the viewport rather than at
    /// the top of the scrollback, so a user who typed something visible steps to
    /// what they can already see instead of to the oldest match in the history.
    pub fn find_step(&self, forward: bool) -> FindStatus {
        let imp = self.imp();
        let mut find = imp.find.borrow_mut();
        if find.hits.is_empty() {
            return find.status();
        }

        let last = find.hits.len() - 1;
        let next = match find.current {
            Some(current) if forward => {
                if current == last {
                    0
                } else {
                    current + 1
                }
            }
            Some(current) => {
                if current == 0 {
                    last
                } else {
                    current - 1
                }
            }
            None => {
                let view = imp.scroll.get();
                let bottom = view.offset + view.len;
                if forward {
                    find.hits
                        .iter()
                        .position(|hit| hit.row >= view.offset)
                        .unwrap_or(0)
                } else {
                    find.hits
                        .iter()
                        .rposition(|hit| hit.row < bottom)
                        .unwrap_or(last)
                }
            }
        };
        find.current = Some(next);
        let row = find.hits[next].row;
        let status = find.status();
        drop(find);

        self.scroll_into_view(row);
        self.queue_draw();
        status
    }

    /// Put the find state back to nothing, which also takes the highlight off.
    pub fn find_clear(&self) {
        let imp = self.imp();
        let mut find = imp.find.borrow_mut();
        if find.needle.is_empty() && find.hits.is_empty() {
            return;
        }
        *find = Find::default();
        drop(find);
        self.queue_draw();
    }

    fn search(&self, needle: &str) -> Vec<tuni_vt::Hit> {
        let mut guard = self.imp().session.borrow_mut();
        guard
            .as_mut()
            .and_then(|session| session.term.search(needle).ok())
            .unwrap_or_default()
    }

    /// Scroll only when `row` is off screen, and then put it a third of the way
    /// down, where the line around a match is readable.
    fn scroll_into_view(&self, row: usize) {
        let imp = self.imp();
        let view = imp.scroll.get();
        if row >= view.offset && row < view.offset + view.len {
            return;
        }

        let mut guard = imp.session.borrow_mut();
        let Some(session) = guard.as_mut() else {
            return;
        };
        session.term.scroll_to_row(row.saturating_sub(view.len / 3));
        imp.scroll.set(session.term.scroll_position());
        drop(guard);

        self.invalidate_links();
        self.reveal_scrollbar();
    }

    /// The tally as it stands, for a find bar that has just been told the
    /// terminal's matches moved.
    pub fn find_status(&self) -> FindStatus {
        self.imp().find.borrow().status()
    }

    /// Ask for the live search to be run again once the current burst of output
    /// is over.
    ///
    /// A search reads the whole screen, and output arrives in chunks, so running
    /// it per chunk would search the same screen several times for one frame.
    /// The idle handler runs once per main-loop turn instead, which is the same
    /// rate the widget redraws at.
    fn schedule_refind(&self) {
        let imp = self.imp();
        if imp.find.borrow().needle.is_empty() || imp.find_pending.replace(true) {
            return;
        }
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move || {
                this.imp().find_pending.set(false);
                let before = this.find_status();
                this.refind();
                if this.find_status() != before {
                    this.emit_by_name::<()>("find-changed", &[]);
                }
                this.queue_draw();
            }
        ));
    }

    /// Run the live search again over what the terminal now holds.
    ///
    /// Output moves every row a match sits on, so the hits are re-taken rather
    /// than adjusted. The step index is kept where it was if there is still a
    /// match there, which holds Enter-Enter-Enter stepping steady under a shell
    /// that keeps printing, and the viewport is left alone either way.
    fn refind(&self) {
        let imp = self.imp();
        let needle = imp.find.borrow().needle.clone();
        if needle.is_empty() {
            return;
        }
        let hits = self.search(&needle);
        let mut find = imp.find.borrow_mut();
        find.current = find
            .current
            .filter(|_| !hits.is_empty())
            .map(|current| current.min(hits.len() - 1));
        find.hits = hits;
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
        if mods.contains(Mods::SHIFT) || !self.imp().config.borrow().mouse_reporting {
            return false;
        }
        self.imp()
            .session
            .borrow()
            .as_ref()
            .is_some_and(|session| session.term.is_mouse_tracking())
    }

    /// Whether a drag is something the application can hear at all.
    fn tracks_motion(&self) -> bool {
        self.imp()
            .session
            .borrow()
            .as_ref()
            .is_some_and(|session| session.term.tracks_mouse_motion())
    }

    fn report_mouse(
        &self,
        action: MouseAction,
        button: Option<MouseButton>,
        mods: Mods,
        x: f64,
        y: f64,
    ) {
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
            send(&mut session.pty, bytes);
        }
    }

    /// Tell an application that the keyboard arrived or left. A pane counts as
    /// its own window here, the way Ghostty reports per surface, so handing the
    /// keyboard to the split next door is a departure for the one it left.
    fn report_focus(&self, gained: bool) {
        let mut guard = self.imp().session.borrow_mut();
        let Some(session) = guard.as_mut() else {
            return;
        };
        if let Ok(bytes) = session.term.encode_focus(gained)
            && !bytes.is_empty()
        {
            send(&mut session.pty, bytes);
        }
    }

    // --- hyperlinks ----------------------------------------------------------

    /// Work out what the pointer is on and light it up.
    ///
    /// Ghostty arms hyperlinks on Ctrl and leaves the mouse to an application
    /// that is tracking it, so both gate the probe. The probe itself crosses
    /// into libghostty, which is why an answer already taken at the same cell
    /// is reused rather than asked for again.
    fn refresh_links(&self, mods: Mods) {
        let imp = self.imp();
        let armed = mods.contains(Mods::CTRL) && !self.reports_mouse(mods);
        let cell = armed
            .then(|| {
                let (x, y) = imp.pointer_pos.get();
                self.geometry().cell_at(x, y)
            })
            .flatten();

        if imp.link_valid.get() && imp.link_probe.get() == cell {
            return;
        }
        imp.link_valid.set(true);
        imp.link_probe.set(cell);

        let hover = cell.and_then(|(col, row)| {
            let mut guard = imp.session.borrow_mut();
            guard
                .as_mut()?
                .term
                .hyperlink_hover(col, row)
                .ok()
                .flatten()
        });

        if *imp.link_hover.borrow() == hover {
            return;
        }
        self.set_cursor_from_name(Some(if hover.is_some() { "pointer" } else { "text" }));
        imp.link_hover.replace(hover);
        self.queue_draw();
    }

    /// Arm and probe a hyperlink at a surface position, as Ctrl-hovering does,
    /// and answer with what was found. Public to the crate so the debug capture
    /// harness drives the same path a real pointer takes.
    pub(crate) fn hover_link(&self, x: f64, y: f64) -> Option<String> {
        let imp = self.imp();
        imp.pointer_pos.set((x, y));
        imp.mods.set(Mods::CTRL);
        imp.link_valid.set(false);
        self.refresh_links(Mods::CTRL);
        let hover = imp.link_hover.borrow();
        hover.as_ref().map(|hover| hover.uri.clone())
    }

    /// Retire the answer and take it again.
    ///
    /// A hover names viewport cells, so output, a scroll, and a resize all
    /// change what it points at even though the pointer has not moved.
    fn invalidate_links(&self) {
        let imp = self.imp();
        if imp.link_hover.borrow().is_none() && imp.link_probe.get().is_none() {
            return;
        }
        imp.link_valid.set(false);
        self.refresh_links(imp.mods.get());
    }

    /// Drop the highlight outright, for when the pointer leaves and no motion
    /// event will arrive to say where it went.
    fn clear_links(&self) {
        let imp = self.imp();
        imp.link_valid.set(false);
        imp.link_probe.set(None);
        if imp.link_hover.replace(None).is_some() {
            self.set_cursor_from_name(Some("text"));
            self.queue_draw();
        }
    }

    /// Hand a hyperlink to the desktop, if it is one worth handing over.
    fn open_uri(&self, uri: &str) {
        if !can_open(uri) {
            return;
        }

        let parent = self.root().and_downcast::<gtk::Window>();
        gtk::UriLauncher::new(uri).launch(
            parent.as_ref(),
            None::<&gtk::gio::Cancellable>,
            |result| {
                if let Err(error) = result {
                    eprintln!("could not open the link: {error}");
                }
            },
        );
    }

    fn on_pointer_press(&self, gesture: &gtk::GestureClick, x: f64, y: f64) {
        self.pointer_press(
            gesture.current_button(),
            keymap::mods_from_state(gesture.current_event_state()),
            x,
            y,
            gesture.current_event_time(),
        );
    }

    /// A button going down. Taken apart from the gesture that carries it so the
    /// debug capture harness can press where a pointer would.
    pub(crate) fn pointer_press(&self, button: u32, mods: Mods, x: f64, y: f64, time: u32) {
        self.grab_focus_self();

        let imp = self.imp();
        imp.buttons_down
            .set(imp.buttons_down.get().saturating_add(1));
        imp.pointer_pos.set((x, y));
        imp.mods.set(mods);

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

        // A press on a link neither selects nor reports: it waits for the
        // release, which is where the link is opened or taken back.
        self.refresh_links(mods);
        if button == gdk::BUTTON_PRIMARY && imp.link_hover.borrow().is_some() {
            imp.pointer.set(Pointer::Link);
            return;
        }

        if self.reports_mouse(mods) {
            // Ghostty hands every press to an application that tracks the
            // mouse, which leaves Shift as the only way to select in one. That
            // is the right answer while the application follows the pointer,
            // and a poor one while it only listens for clicks: the drag it is
            // being given is a drag it will never be told about. So a press of
            // the selecting button waits, and becomes whichever of the two the
            // pointer turns out to be doing.
            if button == gdk::BUTTON_PRIMARY && !self.tracks_motion() {
                imp.pointer.set(Pointer::Held { x, y, time });
                return;
            }
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
                self.selection_press(x, y, Duration::from_millis(u64::from(time)));
            }
            // An application tracking the mouse was handed this press further
            // up, so the menu belongs to whoever is not in one, and to anyone
            // holding Shift, which is the override everything else here takes.
            gdk::BUTTON_SECONDARY => self.popup_menu(x, y),
            _ => {}
        }
    }

    /// Anchor a selection at a surface position.
    fn selection_press(&self, x: f64, y: f64, time: Duration) {
        let geometry = self.geometry();
        let mut guard = self.imp().session.borrow_mut();
        if let Some(session) = guard.as_mut() {
            let _ = session.term.select_press(x, y, geometry, time);
        }
        drop(guard);
        self.queue_draw();
    }

    fn selection_drag(&self, x: f64, y: f64, rectangle: bool) {
        let geometry = self.geometry();
        let mut guard = self.imp().session.borrow_mut();
        if let Some(session) = guard.as_mut() {
            let _ = session.term.select_drag(x, y, geometry, rectangle);
        }
        drop(guard);
        self.queue_draw();
    }

    /// End the gesture and return what is selected.
    fn selection_finish(&self) -> Option<String> {
        let mut guard = self.imp().session.borrow_mut();
        let text = guard.as_mut().and_then(|session| {
            let _ = session.term.select_release();
            session.term.selection_text().ok().flatten()
        });
        drop(guard);
        text.filter(|text| !text.is_empty())
    }

    fn on_pointer_release(&self, gesture: &gtk::GestureClick, x: f64, y: f64) {
        self.pointer_release(
            gesture.current_button(),
            keymap::mods_from_state(gesture.current_event_state()),
            x,
            y,
        );
    }

    /// A button coming back up, and the end of whatever the press started.
    pub(crate) fn pointer_release(&self, button: u32, mods: Mods, x: f64, y: f64) {
        let imp = self.imp();
        imp.buttons_down
            .set(imp.buttons_down.get().saturating_sub(1));
        imp.pointer_pos.set((x, y));

        if imp.bar_drag.take().is_some() {
            self.reveal_scrollbar();
            self.queue_draw();
            return;
        }

        imp.mods.set(mods);
        match imp.pointer.replace(Pointer::Idle) {
            Pointer::Link => {
                // Between press and release the pointer may have slid off and
                // output may have moved the link. Opening only when the same
                // URI is still underneath covers both.
                let pressed = imp
                    .link_hover
                    .borrow()
                    .as_ref()
                    .map(|hover| hover.uri.clone());
                imp.link_valid.set(false);
                self.refresh_links(mods);
                let released = imp
                    .link_hover
                    .borrow()
                    .as_ref()
                    .map(|hover| hover.uri.clone());
                if let Some(uri) = released.filter(|uri| pressed.as_deref() == Some(uri.as_str())) {
                    self.open_uri(&uri);
                }
            }
            Pointer::Reporting => {
                self.report_mouse(MouseAction::Release, mouse_button(button), mods, x, y);
            }
            Pointer::Held {
                x: press_x,
                y: press_y,
                ..
            } => {
                // The pointer stayed in its cell, so the press was a click
                // after all and the application gets both halves of it now.
                let button = mouse_button(button);
                self.report_mouse(MouseAction::Press, button, mods, press_x, press_y);
                self.report_mouse(MouseAction::Release, button, mods, x, y);
            }
            Pointer::Selecting => {
                // Selecting fills the primary selection, so a middle click in
                // any other window pastes what was just highlighted.
                if let Some(text) = self.selection_finish() {
                    self.primary_clipboard().set_text(&text);
                    if imp.config.borrow().copy_on_select {
                        self.clipboard().set_text(&text);
                    }
                }
            }
            Pointer::Idle => {}
        }
    }

    fn on_pointer_motion(&self, controller: &gtk::EventControllerMotion, x: f64, y: f64) {
        self.pointer_motion(
            keymap::mods_from_state(controller.current_event_state()),
            x,
            y,
        );
    }

    /// The pointer moving, whether or not a button is down.
    pub(crate) fn pointer_motion(&self, mods: Mods, x: f64, y: f64) {
        let imp = self.imp();
        imp.pointer_pos.set((x, y));
        imp.mods.set(mods);

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
            Pointer::Held {
                x: press_x,
                y: press_y,
                time,
            } => {
                let geometry = self.geometry();
                if geometry.cell_at(press_x, press_y) != geometry.cell_at(x, y) {
                    imp.pointer.set(Pointer::Selecting);
                    self.selection_press(press_x, press_y, Duration::from_millis(u64::from(time)));
                    self.selection_drag(x, y, mods.contains(Mods::ALT));
                }
            }
            _ => {
                self.refresh_links(mods);
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

    /// Select everything there is, scrollback included.
    pub fn select_all(&self) {
        let mut guard = self.imp().session.borrow_mut();
        if let Some(session) = guard.as_mut() {
            let _ = session.term.select_all();
        }
        drop(guard);
        self.queue_draw();
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
            send(&mut session.pty, bytes);
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
        keycode: u32,
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

        // Every shortcut below is on the physical key, so that it stays under
        // the same finger whatever the layout puts there.
        let key = keymap::key_from_event(keyval, keycode);
        let mods = keymap::mods_for_key(state, key, KeyAction::Press, num_locked(controller));
        imp.mods.set(mods);

        // Scrollback navigation sits on plain Shift, where every terminal on
        // this desktop puts it.
        if mods.contains(Mods::SHIFT) && !mods.contains(Mods::CTRL) {
            let page = imp.grid_size.get().1.max(1) as isize;
            match key {
                Key::PageUp => {
                    self.scroll_by(-page);
                    return glib::Propagation::Stop;
                }
                Key::PageDown => {
                    self.scroll_by(page);
                    return glib::Propagation::Stop;
                }
                Key::Home | Key::End => {
                    let mut guard = imp.session.borrow_mut();
                    if let Some(session) = guard.as_mut() {
                        if key == Key::Home {
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
            match key {
                Key::Equal | Key::NumpadAdd => {
                    self.zoom(1);
                    return glib::Propagation::Stop;
                }
                Key::Minus | Key::NumpadSubtract => {
                    self.zoom(-1);
                    return glib::Propagation::Stop;
                }
                Key::Digit0 | Key::Numpad0 => {
                    self.reset_zoom();
                    return glib::Propagation::Stop;
                }
                _ => {}
            }
        }

        // Application shortcuts live on Ctrl+Shift, because Ctrl+C and Ctrl+V
        // belong to the shell.
        if mods.contains(Mods::CTRL) && mods.contains(Mods::SHIFT) {
            match key {
                Key::C => {
                    // With nothing selected, fall through so Ctrl+Shift+C still
                    // reaches the application.
                    if self.copy_selection() {
                        return glib::Propagation::Stop;
                    }
                }
                Key::V => {
                    self.paste_clipboard();
                    return glib::Propagation::Stop;
                }
                Key::A => {
                    self.select_all();
                    return glib::Propagation::Stop;
                }
                _ => {}
            }
        }

        if self.send_key(controller, keyval, key, mods, KeyAction::Press, committed) {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    }

    /// A release reaches the shell only under the Kitty protocol's
    /// event-reporting mode; every other mode encodes it as nothing at all.
    fn on_key_release(
        &self,
        controller: &gtk::EventControllerKey,
        keyval: gdk::Key,
        keycode: u32,
        state: gdk::ModifierType,
    ) {
        let imp = self.imp();

        if let Some(im) = imp.im.borrow().as_ref()
            && let Some(event) = controller.current_event()
        {
            im.filter_keypress(&event);
        }

        let key = keymap::key_from_event(keyval, keycode);
        let mods = keymap::mods_for_key(state, key, KeyAction::Release, num_locked(controller));
        imp.mods.set(mods);

        self.send_key(controller, keyval, key, mods, KeyAction::Release, None);
        // Letting go of Ctrl puts out whatever link it lit.
        self.refresh_links(mods);
    }

    /// Hand a key event to the encoder and write back whatever it answers
    /// with. Says whether anything went to the shell at all.
    fn send_key(
        &self,
        controller: &gtk::EventControllerKey,
        keyval: gdk::Key,
        key: Key,
        mods: Mods,
        action: KeyAction,
        committed: Option<String>,
    ) -> bool {
        let imp = self.imp();
        let event = controller.current_event();
        let key_event = event
            .as_ref()
            .and_then(|event| event.downcast_ref::<gdk::KeyEvent>());

        let text = committed.or_else(|| {
            keyval
                .to_unicode()
                .filter(|c| !c.is_control())
                .map(String::from)
        });

        // What the keymap already folded into that character: Shift on a
        // capital, AltGr on the third level. Reporting those again would
        // double them under the Kitty protocol, and GDK has worked out which
        // ones they were, which beats guessing at Shift.
        let consumed_mods = key_event.map_or(Mods::empty(), |event| {
            keymap::mods_from_state(event.consumed_modifiers() & gdk::MODIFIER_MASK)
        });
        let unshifted_codepoint =
            key_event.and_then(|event| keymap::unshifted_codepoint(&self.display(), event));

        let mut guard = imp.session.borrow_mut();
        let Some(session) = guard.as_mut() else {
            return false;
        };

        let input = KeyInput {
            action,
            key,
            mods,
            consumed_mods,
            text: text.as_deref(),
            unshifted_codepoint,
        };

        match session.term.encode_key(&input) {
            Ok(bytes) if !bytes.is_empty() => {
                send(&mut session.pty, bytes);
                // Typing pulls the viewport back down: the answer is about to
                // arrive at the bottom.
                session.term.scroll_to_bottom();
                imp.scroll.set(session.term.scroll_position());
                drop(guard);
                self.note_input();
                self.queue_draw();
                true
            }
            _ => false,
        }
    }

    // --- drawing -----------------------------------------------------------

    fn draw(&self, snapshot: &gtk::Snapshot) {
        let imp = self.imp();
        let m = imp.metrics.get();
        let width = self.width() as f32;
        let height = self.height() as f32;

        let mut guard = imp.session.borrow_mut();
        // Taken before the grid, which borrows the terminal for the rest of the
        // frame. Search hits are in scrollback rows and this is what turns them
        // into viewport rows.
        let view = guard
            .as_ref()
            .map_or_else(ScrollPosition::default, |session| {
                session.term.scroll_position()
            });
        // Inline images, before the grid for the same reason as the scroll
        // position: the snapshot borrows the terminal for the rest of the frame.
        // Only a cache miss reads pixels, so a program redrawing a plot every
        // frame uploads once per version of it rather than once per frame.
        {
            let mut placements = imp.images.borrow_mut();
            placements.clear();
            if let Some(session) = guard.as_ref() {
                let _ = session.term.images(&mut placements);
                let mut textures = imp.textures.borrow_mut();
                for placement in placements.iter() {
                    if textures.get(&placement.image).is_some() {
                        continue;
                    }
                    if let Ok(Some(pixels)) = session.term.image_pixels(placement.image.id) {
                        textures.insert(placement.image, &pixels);
                    }
                }
            }
        }

        let grid = guard
            .as_mut()
            .and_then(|session| session.term.snapshot().ok());

        // The page color: whatever the terminal is actually using, which is the
        // theme unless an application overrode it with OSC 11. Falling back to
        // the theme keeps the widget the right color before a shell starts and
        // after one exits, when there is no terminal to ask.
        let theme_background = theme_rgb(imp.theme.borrow().background);
        let background = grid.map_or(theme_background, |g| g.bg);
        // The window under the widget is painted in the theme's background
        // color already. Painting it again is free when the color is opaque and
        // wrong when it is not: two translucent layers of the same color leave
        // the grid more solid than the header bar beside it. So the fill is for
        // a page color the application chose itself, and for the opaque case.
        let opacity = imp.config.borrow().background_opacity;
        if opacity >= 1.0 || background != theme_background {
            let mut page = rgba(background);
            page.set_alpha(opacity as f32);
            snapshot.append_color(&page, &graphene::Rect::new(0.0, 0.0, width, height));
        }

        let Some(grid) = grid else {
            return;
        };

        let context = self.text_context();
        let font = imp.font.borrow();
        let layout = pango::Layout::new(&context);
        layout.set_font_description(Some(&font));

        // The attributes every run shares, built once a frame rather than once
        // a run: a full viewport is hundreds of runs and a fresh attribute list
        // for each of them is measurable against a 16.7 ms budget.
        let plain = pango::AttrList::new();
        if !imp.config.borrow().font_ligatures {
            // A ligature is one glyph where the terminal still counts several
            // cells, so it is off unless the configuration asks for it. These
            // four features are what a coding font joins characters with.
            plain.insert(pango::AttrFontFeatures::new(
                "liga 0, clig 0, dlig 0, calt 0",
            ));
        }

        let painter = Painter {
            snapshot,
            layout: &layout,
            font: &font,
            plain,
            m,
        };

        // The hovered link as a bitmap, built once: a link can cover the whole
        // viewport, and scanning its cell list per cell would be quadratic.
        let hovered = imp.link_hover.borrow();
        let hot: Vec<bool> = match hovered.as_ref() {
            Some(hover) => {
                let mut mask = vec![false; usize::from(grid.cols) * usize::from(grid.rows)];
                for &(col, row) in &hover.cells {
                    let index = usize::from(row) * usize::from(grid.cols) + usize::from(col);
                    if let Some(slot) = mask.get_mut(index) {
                        *slot = true;
                    }
                }
                mask
            }
            None => Vec::new(),
        };

        // Search hits as a second bitmap: 0 no match, 1 a match, 2 the one the
        // viewport was stepped to. A common needle can match thousands of times,
        // so the list is walked once here rather than once per row.
        let find = imp.find.borrow();
        let mut marks = Vec::new();
        if !find.hits.is_empty() {
            marks = vec![0u8; usize::from(grid.cols) * usize::from(grid.rows)];
            for (index, hit) in find.hits.iter().enumerate() {
                let Some(row) = hit.row.checked_sub(view.offset) else {
                    continue;
                };
                if row >= usize::from(grid.rows) {
                    continue;
                }
                let base = row * usize::from(grid.cols);
                let mark = if find.current == Some(index) { 2 } else { 1 };
                let end = usize::from(hit.col) + usize::from(hit.len);
                for col in usize::from(hit.col)..end.min(usize::from(grid.cols)) {
                    marks[base + col] = mark;
                }
            }
        }
        drop(find);

        // A match is drawn as a solid block the way a selection is, because a
        // tint under whatever colors the shell chose is not reliably legible.
        // The text on top is repainted in whichever of black or white survives.
        let page = tuni_core::theme::Rgb::new(background.r, background.g, background.b);
        let hit_bg = [
            page,
            page.blend(imp.theme.borrow().palette[3], MATCH_TINT),
            page.blend(imp.theme.borrow().palette[3], CURRENT_TINT),
        ];
        let hit_fg = hit_bg.map(|color| theme_rgb(color.contrasting()));
        let hit_bg = hit_bg.map(theme_rgb);

        let mut text = String::with_capacity(256);

        // Images stack in three layers of their own, and the cells they cover
        // are drawn around them: under the backgrounds, between the backgrounds
        // and the text, or over everything. So backgrounds and text are two
        // passes over the rows rather than one, with the images in between.
        let images = imp.images.borrow();
        let textures = imp.textures.borrow();
        self.draw_images(snapshot, &images, &textures, Layer::BelowBackground, m);

        for row in 0..grid.rows {
            let cells = grid.row(row);
            let y = row as f32 * m.cell_height;
            let base = usize::from(row) * usize::from(grid.cols);
            let mark = |col: usize| marks.get(base + col).copied().unwrap_or(0);

            // Backgrounds, batched into runs of equal color, so a full
            // reverse-video line is one rectangle rather than eighty.
            let mut run_start: Option<(usize, Rgb)> = None;
            for (col, cell) in cells.iter().enumerate() {
                let bg = match mark(col) {
                    0 => cell.bg,
                    other => Some(hit_bg[usize::from(other)]),
                };
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
        }

        self.draw_images(snapshot, &images, &textures, Layer::BelowText, m);

        for row in 0..grid.rows {
            let cells = grid.row(row);
            let y = row as f32 * m.cell_height;
            let base = usize::from(row) * usize::from(grid.cols);
            let hot = |col: usize| hot.get(base + col).copied().unwrap_or(false);
            let mark = |col: usize| marks.get(base + col).copied().unwrap_or(0);

            // Text, batched into runs sharing a style.
            let mut col = 0usize;
            while col < cells.len() {
                if cells[col].text.is_empty() {
                    col += 1;
                    continue;
                }

                let start = col;
                let style = style_key(&cells[col], hot(col), mark(col));
                text.clear();

                while col < cells.len() {
                    let cell = &cells[col];
                    if cell.text.is_empty() || style_key(cell, hot(col), mark(col)) != style {
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

                let fg = match mark(start) {
                    0 => cells[start].fg,
                    other => hit_fg[usize::from(other)],
                };
                painter.draw_run(start, y, &text, &cells[start], hot(start), fg);
            }
        }
        drop(hovered);

        let cursor = grid.cursor;
        if let Some(cursor) = cursor {
            // Hidden for this half of the blink cycle, but the cell still keeps
            // its glyph, which the row loop above already drew.
            if imp.blink_on.get() || !cursor.blinking {
                painter.draw_cursor(&cursor, grid, self.has_focus());
            }
        }

        self.draw_images(snapshot, &images, &textures, Layer::AboveText, m);
        drop(images);
        drop(textures);
        drop(font);
        drop(guard);

        // Recorded rather than acted on: the blink timer reads it, and touching
        // the widget's state from inside a snapshot would be a redraw loop.
        imp.blink_wanted
            .set(cursor.is_some_and(|cursor| cursor.blinking));

        self.draw_scrollbar(snapshot);
        self.draw_progress(snapshot);
    }

    /// The inline images of one layer.
    ///
    /// A placement names a rectangle of the image and a rectangle of the
    /// terminal to put it in; a texture node takes neither, only a rectangle to
    /// draw the whole texture into. So the whole image is placed at whatever
    /// size and offset makes the wanted part land where it belongs, and the
    /// clip cuts away the rest. That is also what handles a picture that has
    /// scrolled halfway off the top, which arrives as a negative row.
    fn draw_images(
        &self,
        snapshot: &gtk::Snapshot,
        images: &[tuni_vt::Placement],
        textures: &Textures,
        layer: Layer,
        m: Metrics,
    ) {
        let bounds = graphene::Rect::new(0.0, 0.0, self.width() as f32, self.height() as f32);
        for placement in images.iter().filter(|image| image.layer() == layer) {
            if placement.width == 0
                || placement.height == 0
                || placement.source_width == 0
                || placement.source_height == 0
            {
                continue;
            }
            let Some(texture) = textures.get(&placement.image) else {
                continue;
            };

            let x = placement.col as f32 * m.cell_width + placement.x_offset as f32;
            let y = placement.row as f32 * m.cell_height + placement.y_offset as f32;
            let clip = graphene::Rect::new(x, y, placement.width as f32, placement.height as f32);
            let Some(clip) = clip.intersection(&bounds) else {
                continue;
            };

            let scale_x = placement.width as f32 / placement.source_width as f32;
            let scale_y = placement.height as f32 / placement.source_height as f32;
            let whole = graphene::Rect::new(
                x - placement.source_x as f32 * scale_x,
                y - placement.source_y as f32 * scale_y,
                texture.width() as f32 * scale_x,
                texture.height() as f32 * scale_y,
            );

            snapshot.push_clip(&clip);
            // Trilinear rather than the default: an image is nearly always
            // asked for at a size that is not its own, and a terminal scales
            // down more often than up.
            snapshot.append_scaled_texture(texture, gtk::gsk::ScalingFilter::Trilinear, &whole);
            snapshot.pop();
        }
    }

    /// Takes a progress report (OSC 9;4) and puts the bar where it says.
    ///
    /// A state that reports no percentage keeps the last one, so a build that
    /// fails at 60% shows a red bar three fifths of the way along rather than
    /// an empty one.
    fn apply_progress(&self, progress: Progress) {
        let imp = self.imp();
        if let Some(source) = imp.progress_stale.take() {
            source.remove();
        }
        if progress == Progress::Remove {
            imp.progress.set(None);
            self.queue_draw();
            return;
        }
        match progress {
            Progress::Set(percent) => imp.progress_value.set(percent),
            Progress::Error(Some(percent)) | Progress::Pause(Some(percent)) => {
                imp.progress_value.set(percent);
            }
            _ => {}
        }
        imp.progress.set(Some(progress));

        // Nothing obliges an application to clear its own bar, and a shell that
        // was interrupted never will. Retire the report instead of leaving it
        // to sit under the terminal for the rest of the session.
        let source = glib::timeout_add_local_once(
            PROGRESS_STALE,
            glib::clone!(
                #[weak(rename_to = this)]
                self,
                move || {
                    this.imp().progress_stale.replace(None);
                    this.imp().progress.set(None);
                    this.queue_draw();
                }
            ),
        );
        imp.progress_stale.replace(Some(source));
        self.queue_draw();
    }

    /// The progress bar: a hairline along the bottom edge, in the theme's own
    /// colors so it reads against the terminal rather than against the desktop.
    fn draw_progress(&self, snapshot: &gtk::Snapshot) {
        let imp = self.imp();
        let Some(progress) = imp.progress.get() else {
            return;
        };
        let width = self.width() as f32;
        let top = self.height() as f32 - PROGRESS_HEIGHT;
        if width <= 0.0 || top < 0.0 {
            return;
        }

        let theme = imp.theme.borrow();
        // Blue for running, red for failed, yellow for waiting: the palette's,
        // not the desktop's, and each also differs in how much of the bar it
        // fills, so the state does not rest on color alone.
        let (color, fraction) = match progress {
            Progress::Remove => return,
            Progress::Set(percent) => (theme.palette[4], f32::from(percent) / 100.0),
            Progress::Error(_) => (
                theme.palette[1],
                f32::from(imp.progress_value.get()) / 100.0,
            ),
            Progress::Pause(_) => (
                theme.palette[3],
                f32::from(imp.progress_value.get()) / 100.0,
            ),
            Progress::Indeterminate => (theme.palette[4], 1.0),
        };
        let mut color = rgba(theme_rgb(color));
        if progress == Progress::Indeterminate {
            // Nothing to measure, so the bar says "running" rather than "done".
            color.set_alpha(0.45);
        }

        // The track, so a bar at 3% is still visibly a bar rather than a speck.
        let mut track = rgba(theme_rgb(theme.foreground));
        track.set_alpha(0.12);
        drop(theme);
        snapshot.append_color(
            &track,
            &graphene::Rect::new(0.0, top, width, PROGRESS_HEIGHT),
        );
        let filled = (width * fraction.clamp(0.0, 1.0)).max(1.0);
        snapshot.append_color(
            &color,
            &graphene::Rect::new(0.0, top, filled, PROGRESS_HEIGHT),
        );
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
    /// Whether the hovered hyperlink covers this cell. Part of the key so a run
    /// breaks where the highlight does.
    link: bool,
    /// Which search highlight this cell carries, for the same reason: a run has
    /// to break where the match does, because the text color changes there.
    mark: u8,
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

/// Whether Num Lock was on for the event in hand. The keyboard knows, and the
/// keypad encoding turns on it.
fn num_locked(controller: &gtk::EventControllerKey) -> bool {
    controller
        .current_event_device()
        .is_some_and(|device| device.is_num_locked())
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

fn style_key(cell: &tuni_vt::Cell, link: bool, mark: u8) -> StyleKey {
    StyleKey {
        fg: cell.fg,
        bold: cell.bold,
        italic: cell.italic,
        underline: cell.underline,
        strikethrough: cell.strikethrough,
        link,
        mark,
    }
}

/// Whether a hyperlink is one the desktop may be handed.
///
/// An OSC 8 URI is written by whatever holds the PTY, which over ssh is not the
/// person at the keyboard. So a control character anywhere in the string
/// disqualifies it, and only a short list of schemes is opened at all, with
/// `file://` among them only when it names this machine, which is what
/// `local_file_path` checks.
fn can_open(uri: &str) -> bool {
    const SCHEMES: [&str; 5] = ["http://", "https://", "mailto:", "ftp://", "ftps://"];

    if uri.is_empty() || uri.chars().any(char::is_control) {
        return false;
    }
    SCHEMES
        .iter()
        .any(|scheme| starts_with_ignore_case(uri, scheme))
        || (starts_with_ignore_case(uri, "file://") && tuni_vt::local_file_path(uri).is_some())
}

/// One menu action, holding the terminal weakly: the group is inserted into the
/// widget, so anything stronger than this would be a cycle the widget never
/// leaves.
fn action<F>(
    name: &str,
    terminal: &TuniTerminal,
    activate: F,
) -> gio::ActionEntry<gio::SimpleActionGroup>
where
    F: Fn(&TuniTerminal) + 'static,
{
    let weak = terminal.downgrade();
    gio::ActionEntry::builder(name)
        .activate(move |_: &gio::SimpleActionGroup, _, _| {
            if let Some(terminal) = weak.upgrade() {
                activate(&terminal);
            }
        })
        .build()
}

/// Case-insensitive prefix test over bytes. A URI arrives as arbitrary text, and
/// slicing a `str` at a byte offset that is not a character boundary panics.
fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len()
        && text.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
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

/// The configured cursor shape in the terms the VT understands, for the same
/// reason `colors` exists: `tuni-core` names the shapes without knowing whose
/// enum they end up in.
fn cursor_style(style: tuni_core::CursorStyle) -> tuni_vt::CursorStyle {
    match style {
        tuni_core::CursorStyle::Block => tuni_vt::CursorStyle::Block,
        tuni_core::CursorStyle::Bar => tuni_vt::CursorStyle::Bar,
        tuni_core::CursorStyle::Underline => tuni_vt::CursorStyle::Underline,
        tuni_core::CursorStyle::BlockHollow => tuni_vt::CursorStyle::BlockHollow,
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
    /// What every run carries: ligature suppression, unless the configuration
    /// wanted ligatures, in which case nothing at all.
    plain: pango::AttrList,
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
    fn draw_run(
        &self,
        start: usize,
        y: f32,
        text: &str,
        style: &tuni_vt::Cell,
        link: bool,
        fg: Rgb,
    ) {
        if let Some(glyph) = crate::sprites::glyph(text) {
            self.draw_sprite(glyph, start as f32 * self.m.cell_width, y, fg);
            return;
        }

        let mut desc = self.font.clone();
        if style.bold {
            desc.set_weight(pango::Weight::Bold);
        }
        if style.italic {
            desc.set_style(pango::Style::Italic);
        }
        self.layout.set_font_description(Some(&desc));
        self.layout.set_text(text);

        // A hovered hyperlink is underlined; over text that was underlined
        // already, the line doubles, so the highlight still reads as one.
        // Ghostty draws it the same way.
        let underline = match (style.underline, link) {
            (true, true) => Some(pango::Underline::Double),
            (true, false) | (false, true) => Some(pango::Underline::Single),
            (false, false) => None,
        };

        // Most runs are plain text and share the frame's own list; only a run
        // that is lined through or under pays for a list of its own.
        let attrs = if underline.is_none() && !style.strikethrough {
            self.plain.clone()
        } else {
            let attrs = self.plain.copy().unwrap_or_default();
            if let Some(underline) = underline {
                attrs.insert(pango::AttrInt::new_underline(underline));
            }
            if style.strikethrough {
                attrs.insert(pango::AttrInt::new_strikethrough(true));
            }
            attrs
        };
        self.layout.set_attributes(Some(&attrs));

        self.snapshot.save();
        self.snapshot.translate(&graphene::Point::new(
            start as f32 * self.m.cell_width,
            y + self.baseline_offset(),
        ));
        self.snapshot.append_layout(self.layout, &rgba(fg));
        self.snapshot.restore();
    }

    /// A box drawing or block character, built from the cell rather than taken
    /// from the font, so that the halves of a frame meet and a run of blocks is
    /// one unbroken shape. Underlines are the layout's to draw and are lost
    /// here, which costs nothing: these characters are the drawing.
    fn draw_sprite(&self, glyph: char, x: f32, y: f32, fg: Rgb) {
        crate::sprites::draw(
            self.snapshot,
            glyph,
            crate::sprites::Cell {
                x,
                y,
                width: self.m.cell_width,
                height: self.m.cell_height,
                thickness: self.m.thickness,
            },
            rgba(fg),
        );
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
                    if let Some(glyph) = crate::sprites::glyph(&cell.text) {
                        self.draw_sprite(glyph, x, y, text);
                        return;
                    }
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
            CursorShape::Bar => self
                .snapshot
                .append_color(&rgba(color), &graphene::Rect::new(x, y, 2.0, m.cell_height)),
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
