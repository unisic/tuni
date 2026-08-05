//! Facade over [`libghostty_vt`].
//!
//! Upstream is pre-1.0 and its C API is documented as expected to change, so
//! nothing above this crate imports `libghostty_vt` directly. When upstream
//! moves, this crate is the only thing that has to follow.
//!
//! The terminal owns thread-local C state and is therefore `!Send`/`!Sync`.
//! It lives on the GTK main thread; PTY reads arrive from a worker thread as
//! byte buffers over a channel.

mod grid;
mod image;
mod osc;

pub use grid::{Cell, Cursor, CursorShape, Grid, LinkHover, Rgb};
pub use image::{ImageKey, Layer, Pixels, Placement};
pub use libghostty_vt::Error;
pub use libghostty_vt::key::{Action as KeyAction, Key, Mods};
pub use libghostty_vt::mouse::{Action as MouseAction, Button as MouseButton};
pub use libghostty_vt::terminal::CursorStyle;
pub use osc::{Notification, Progress};

use libghostty_vt::mouse::EncoderSize;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use libghostty_vt::fmt::Format;
use libghostty_vt::render::{CellIterator, CursorVisualStyle, RenderState, RowIterator};
use libghostty_vt::screen::RowSemanticPrompt;
use libghostty_vt::selection::gesture::{
    Autoscroll, AutoscrollTickEvent, Behavior, Behaviors, DragEvent, Geometry as GestureGeometry,
    Gesture, PressEvent, ReleaseEvent,
};
use libghostty_vt::selection::{FormatOptions, SelectWordOptions};
use libghostty_vt::terminal::{
    ClipboardLocation, Mode, Options as TerminalOptions, Point, PointCoordinate, ScrollViewport,
    Terminal as VtTerminal,
};
use libghostty_vt::{focus, key, mouse, paste};
use unicode_width::UnicodeWidthChar;

pub type Result<T> = std::result::Result<T, Error>;

/// Side effects the terminal raised while processing input. Drained by the
/// widget after each feed so it can update the title, ring the bell, and write
/// query responses back to the PTY.
#[derive(Debug, Default)]
pub struct Effects {
    /// Bytes the terminal wants written back to the PTY (DSR, DA, DECRQM …).
    pub pty_write: Vec<u8>,
    pub bell: bool,
    pub title_changed: bool,
    pub pwd_changed: bool,
    /// Clipboard writes requested by the application (OSC 52, OSC 1337 Copy).
    /// Reads are refused upstream and never reach us.
    pub clipboard_writes: Vec<ClipboardRequest>,
    /// Desktop notifications the application asked for (OSC 9, OSC 99,
    /// OSC 777), in the order they were asked for.
    pub notifications: Vec<Notification>,
    /// The last progress report (OSC 9;4). Only the last matters: a build that
    /// counted from 1 to 40 while the widget was busy shows 40.
    pub progress: Option<Progress>,
}

/// Which clipboard an application asked to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardTarget {
    Standard,
    Selection,
    Primary,
}

#[derive(Debug, Clone)]
pub struct ClipboardRequest {
    pub target: ClipboardTarget,
    pub text: String,
}

/// Where the viewport sits in the scrollable area, in rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScrollPosition {
    /// Rows in the whole scrollable area, scrollback included.
    pub total: usize,
    /// First visible row, counted from the top of the scrollback.
    pub offset: usize,
    /// Rows the viewport shows.
    pub len: usize,
}

impl ScrollPosition {
    /// Whether there is anything above the viewport to scroll to.
    #[must_use]
    pub fn is_scrollable(&self) -> bool {
        self.total > self.len
    }

    /// How far down the scrollable area the viewport sits: 0 at the top of the
    /// scrollback, 1 at the bottom.
    #[must_use]
    pub fn fraction(&self) -> f64 {
        let travel = self.total.saturating_sub(self.len);
        if travel == 0 {
            return 1.0;
        }
        (self.offset as f64 / travel as f64).clamp(0.0, 1.0)
    }

    /// Visible share of the scrollable area — the scrollbar thumb's length.
    #[must_use]
    pub fn proportion(&self) -> f64 {
        if self.total == 0 {
            return 1.0;
        }
        (self.len as f64 / self.total as f64).clamp(0.0, 1.0)
    }

    /// The row that puts the viewport at `fraction` down the scrollable area.
    #[must_use]
    pub fn row_at(&self, fraction: f64) -> usize {
        let travel = self.total.saturating_sub(self.len);
        (fraction.clamp(0.0, 1.0) * travel as f64).round() as usize
    }
}

/// Where one search match sits: the row it is on, counted from the top of the
/// scrollback the way [`ScrollPosition::offset`] counts, and the cells it
/// covers on that row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hit {
    pub row: usize,
    pub col: u16,
    pub len: u16,
}

/// Records every non-overlapping, case-insensitive occurrence of `needle` in
/// one row of text, in cells rather than in characters.
fn find_in_line(line: &str, needle: &str, row: usize, hits: &mut Vec<Hit>) {
    let mut byte = 0;
    let mut col = 0usize;
    while byte < line.len() {
        let rest = &line[byte..];
        if let Some(len) = match_at(rest, needle) {
            let width = cells(&rest[..len]);
            hits.push(Hit {
                row,
                col: u16::try_from(col).unwrap_or(u16::MAX),
                len: u16::try_from(width).unwrap_or(u16::MAX),
            });
            byte += len;
            col += width;
            continue;
        }
        let Some(next) = rest.chars().next() else {
            break;
        };
        byte += next.len_utf8();
        col += cells(next.encode_utf8(&mut [0u8; 4]));
    }
}

/// The length in bytes of `needle` matching at the front of `haystack`,
/// ignoring case. The two can differ in length — a character whose lowercase
/// form is a different width still matches.
fn match_at(haystack: &str, needle: &str) -> Option<usize> {
    let mut chars = haystack.char_indices();
    let mut end = 0;
    for wanted in needle.chars() {
        let (index, found) = chars.next()?;
        if found != wanted && !found.to_lowercase().eq(wanted.to_lowercase()) {
            return None;
        }
        end = index + found.len_utf8();
    }
    Some(end)
}

/// How many cells a piece of text occupies. A combining mark is part of the
/// cell before it and adds nothing.
fn cells(text: &str) -> usize {
    text.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

/// Pixel geometry of the rendered grid.
///
/// Pointer positions are in surface pixels for both selection and mouse
/// reporting, so the widget has to describe how that space maps onto cells.
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    pub cols: u16,
    pub rows: u16,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    pub screen_width_px: u32,
    pub screen_height_px: u32,
}

/// A key press translated from a GTK event, ready to be encoded.
pub struct KeyInput<'a> {
    pub action: KeyAction,
    /// The key by position, in W3C `KeyboardEvent.code` terms, not by what it
    /// types. On a Dvorak layout the key a US keyboard calls S is `Key::S`
    /// however the layout maps it.
    pub key: Key,
    pub mods: Mods,
    /// Mods already baked into `text` — Shift is consumed when the toolkit
    /// already produced the shifted character, otherwise the Kitty protocol
    /// would report it twice.
    pub consumed_mods: Mods,
    /// Committed text for this event, when the key produced any.
    pub text: Option<&'a str>,
    /// What this key types with no modifier at all on the layout in force. The
    /// Kitty protocol reports it, and it is what lets `Ctrl+С` on a Cyrillic
    /// layout arrive as `Ctrl+C`. `None` when the key types nothing.
    pub unshifted_codepoint: Option<char>,
}

pub struct MouseInput {
    pub action: MouseAction,
    pub button: Option<MouseButton>,
    pub mods: Mods,
    /// Position in surface pixels, which is what the encoder wants: pixel
    /// reporting modes need the sub-cell part.
    pub x: f64,
    pub y: f64,
    /// Whether any button is down, which decides whether motion is reported in
    /// button-event mode.
    pub any_button_pressed: bool,
}

impl Geometry {
    /// The viewport cell under a surface-space position, clamped to the grid so
    /// a drag past the edge still selects the edge cell.
    fn point(&self, x: f64, y: f64) -> Point {
        let cell_width = f64::from(self.cell_width_px.max(1));
        let cell_height = f64::from(self.cell_height_px.max(1));
        let col = (x / cell_width).floor();
        let row = (y / cell_height).floor();

        Point::Viewport(PointCoordinate {
            x: col.clamp(0.0, f64::from(self.cols.saturating_sub(1))) as u16,
            y: row.clamp(0.0, f64::from(self.rows.saturating_sub(1))) as u32,
        })
    }

    /// The viewport cell under a surface-space position, or `None` when the
    /// position falls outside the grid.
    ///
    /// Unlike selection this does not clamp: a pointer past the last column is
    /// over nothing, and clamping would light up a hyperlink that is not under
    /// it.
    #[must_use]
    pub fn cell_at(&self, x: f64, y: f64) -> Option<(u16, u16)> {
        if x < 0.0 || y < 0.0 {
            return None;
        }
        let col = (x / f64::from(self.cell_width_px.max(1))).floor();
        let row = (y / f64::from(self.cell_height_px.max(1))).floor();
        if col >= f64::from(self.cols) || row >= f64::from(self.rows) {
            return None;
        }
        Some((col as u16, row as u16))
    }
}

impl From<Geometry> for GestureGeometry {
    fn from(value: Geometry) -> Self {
        Self {
            columns: u32::from(value.cols.max(1)),
            cell_width: value.cell_width_px.max(1),
            padding_left: 0,
            screen_height: value.screen_height_px.max(1),
        }
    }
}

impl From<Geometry> for EncoderSize {
    fn from(value: Geometry) -> Self {
        Self {
            screen_width: value.screen_width_px.max(1),
            screen_height: value.screen_height_px.max(1),
            cell_width: value.cell_width_px.max(1),
            cell_height: value.cell_height_px.max(1),
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        }
    }
}

/// The colors a terminal draws with, as a theme names them.
///
/// The sixteen ANSI slots are theme data; the remaining 240 slots of the
/// 256-color palette are the standard cube and grayscale ramp, which the
/// terminal already knows and no theme overrides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colors {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Option<Rgb>,
    /// Text under a block cursor. `None` inverts the cell instead.
    pub cursor_text: Option<Rgb>,
    /// Selected text. `None` inverts the cells instead, as before a theme.
    pub selection_background: Option<Rgb>,
    pub selection_foreground: Option<Rgb>,
    pub palette: [Rgb; 16],
}

pub struct Terminal {
    inner: VtTerminal<'static, 'static>,
    render: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    key_encoder: key::Encoder<'static>,
    key_event: key::Event<'static>,
    mouse_encoder: mouse::Encoder<'static>,
    mouse_event: mouse::Event<'static>,
    gesture: Gesture<'static>,
    press_event: PressEvent<'static>,
    drag_event: DragEvent<'static>,
    release_event: ReleaseEvent<'static>,
    autoscroll_event: AutoscrollTickEvent<'static>,
    /// What XTSHIFTESCAPE last asked, `None` until an application asks.
    /// Upstream parses the sequence into a flag its C API never exposes, so
    /// the sniffer keeps this copy.
    shift_capture: Option<bool>,
    effects: Rc<RefCell<Effects>>,
    grid: Grid,
    /// Scratch buffer for encoder output, reused across events.
    encoded: Vec<u8>,
    /// Scratch buffer for formatted selection text.
    selection_buf: Vec<u8>,
    /// Scratch buffer for a hyperlink URI, reused so hovering a link does not
    /// allocate once per cell of the viewport.
    link_buf: Vec<u8>,
    /// Scratch buffer for the whole screen as text, reused because a find bar
    /// searches it again on every keystroke.
    search_buf: Vec<u8>,
    /// Selection colors, which the library does not own: selection is drawn by
    /// us, over cells the library only marks.
    selection_colors: Option<(Option<Rgb>, Option<Rgb>)>,
    /// The theme's cursor text color, applied to the cell under a block cursor.
    cursor_text: Option<Rgb>,
    /// Reads the stream for the OSCs upstream parses but never reports.
    sniffer: osc::Sniffer,
    /// Scratch buffer for what the sniffer found, reused across feeds.
    sniffed: Vec<osc::Event>,
}

impl Terminal {
    pub fn new(cols: u16, rows: u16, scrollback_lines: usize) -> Result<Self> {
        // Upstream's `max_scrollback` is a byte budget, whatever the binding's
        // doc comment says about lines — Screen.zig spells it out, and pages
        // are allocated against it. Handing it the line count directly is how
        // "10,000 lines" of scrollback silently became one 64 KiB page: about
        // 900 rows at 80 columns, 430 at 200. The budget below is sixteen
        // bytes per cell — a cell is a packed u64, and the doubling covers row
        // metadata, style pages and the rounding up to whole pages — measured
        // to retain the full count at both 80 and 200 columns (see git history
        // for the probe). A pane made narrow and resized wide keeps the budget
        // it was born with, so it may retain proportionally fewer lines than
        // asked; a budget can be exact or cheap to compute, not both. Zero
        // passes through: upstream reads zero as "no scrollback at all",
        // which is also what zero means in the preferences.
        let max_scrollback = scrollback_lines.saturating_mul(usize::from(cols.max(80)) * 16);
        let mut inner = VtTerminal::new(TerminalOptions {
            cols: cols.max(1),
            rows: rows.max(1),
            max_scrollback,
        })?;

        // The Kitty graphics protocol is off until a storage limit says
        // otherwise, and a PNG transmission is refused until a decoder is
        // installed. Both are the embedder's to decide, so both are decided
        // here rather than left at a default that draws nothing.
        image::install_png_decoder();
        inner.set_kitty_image_storage_limit(image::STORAGE_LIMIT)?;

        let effects = Rc::new(RefCell::new(Effects::default()));

        let sink = Rc::clone(&effects);
        inner.on_pty_write(move |_term, data: &[u8]| {
            sink.borrow_mut().pty_write.extend_from_slice(data);
        })?;

        let sink = Rc::clone(&effects);
        inner.on_bell(move |_term| {
            sink.borrow_mut().bell = true;
        })?;

        let sink = Rc::clone(&effects);
        inner.on_title_changed(move |_term| {
            sink.borrow_mut().title_changed = true;
        })?;

        let sink = Rc::clone(&effects);
        inner.on_pwd_changed(move |_term| {
            sink.borrow_mut().pwd_changed = true;
        })?;

        let sink = Rc::clone(&effects);
        inner.on_clipboard_write(move |_term, write| {
            let target = match write.location() {
                ClipboardLocation::Selection => ClipboardTarget::Selection,
                ClipboardLocation::Primary => ClipboardTarget::Primary,
                _ => ClipboardTarget::Standard,
            };
            // A write carries one or more MIME representations. GTK clipboards
            // take text, so prefer text/plain and fall back to the first
            // representation offered.
            let mut contents = write.contents();
            let text = contents
                .clone()
                .find(|c| c.mime.starts_with("text/plain"))
                .or_else(|| contents.next())
                .map(|c| c.data.to_owned());
            if let Some(text) = text {
                sink.borrow_mut()
                    .clipboard_writes
                    .push(ClipboardRequest { target, text });
            }
            Ok(())
        })?;

        // Say, once, that the shell redraws its own prompt.
        //
        // A resize reflows the rows it is given, and a row a shell wrote to the
        // full old width becomes two rows at a narrower one. A shell that draws
        // a prompt then repaints it on SIGWINCH — fish, zsh, anything with a
        // right-hand prompt — counts the rows it wrote, moves up that many, and
        // overwrites. Reflow has already turned one of those rows into two, so
        // the move lands one row low and the head of the old prompt is left
        // behind. One resize leaves one stranded prompt; a drag leaves a
        // screenful.
        //
        // Ghostty answers this by clearing the prompt rows instead of reflowing
        // them, which is what its `shell_redraws_prompt` does and what it
        // defaults to. libghostty-vt turns that default off, because an
        // embedder cannot assume the shell marks its prompts at all — the
        // clearing only ever fires for a shell that emits OSC 133 in the first
        // place. This is that opt-in, in the one form the C API exposes: the
        // `redraw=1` option of a semantic-prompt start.
        //
        // The `C` behind it hands the cursor back to output, so the marker sets
        // the flag and leaves no prompt region of its own for the resize to
        // find. Both are written into an empty screen with the cursor at the
        // origin, where a fresh-line is a no-op, so nothing on screen moves.
        inner.vt_write(b"\x1b]133;A;redraw=1\x1b\\\x1b]133;C\x1b\\");

        // Click repeat is untimed until told otherwise, which would leave
        // double- and triple-click dead. 500ms is what Ghostty resolves its
        // `click-repeat-interval` to on Linux; the distance allowance is one
        // cell width, set per press because the cell is sized per press.
        let mut press_event = PressEvent::new()?;
        press_event.set_repeat_interval(Duration::from_millis(500))?;

        Ok(Self {
            inner,
            render: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            key_encoder: key::Encoder::new()?,
            key_event: key::Event::new()?,
            mouse_encoder: mouse::Encoder::new()?,
            mouse_event: mouse::Event::new()?,
            gesture: Gesture::new()?,
            press_event,
            drag_event: DragEvent::new()?,
            release_event: ReleaseEvent::new()?,
            autoscroll_event: AutoscrollTickEvent::new()?,
            shift_capture: None,
            effects,
            grid: Grid::default(),
            encoded: Vec::with_capacity(64),
            selection_buf: Vec::new(),
            link_buf: Vec::new(),
            search_buf: Vec::new(),
            selection_colors: None,
            cursor_text: None,
            sniffer: osc::Sniffer::default(),
            sniffed: Vec::new(),
        })
    }

    /// Repaint the terminal in a theme's colors.
    ///
    /// The default foreground, background, cursor, and ANSI palette belong to
    /// the library, which keeps its own overrides from OSC 4/10/11/12 layered
    /// on top; setting the *defaults* means an application that changed a color
    /// keeps its change, and one that reset it lands on the new theme. The
    /// selection and cursor-text colors are ours, because selection and cursor
    /// drawing are ours.
    pub fn set_colors(&mut self, colors: &Colors) -> Result<()> {
        self.inner
            .set_default_fg_color(Some(colors.foreground.into()))?;
        self.inner
            .set_default_bg_color(Some(colors.background.into()))?;
        self.inner
            .set_default_cursor_color(colors.cursor.map(Into::into))?;

        // Only the first sixteen slots are theme data; the rest of the cube has
        // to survive, so the palette is read back and patched rather than built.
        let mut palette = self.inner.default_color_palette()?;
        for (slot, color) in palette.0.iter_mut().zip(colors.palette) {
            *slot = color.into();
        }
        self.inner.set_default_color_palette(Some(palette))?;

        self.selection_colors = Some((colors.selection_background, colors.selection_foreground));
        self.cursor_text = colors.cursor_text;
        Ok(())
    }

    /// Feed PTY output into the VT parser. Never fails — malformed input is
    /// logged upstream rather than propagated, by design.
    pub fn feed(&mut self, data: &[u8]) {
        self.inner.vt_write(data);

        // Read the same bytes for what the parser above swallows: notifications
        // and progress reports, which it recognises and then keeps.
        self.sniffed.clear();
        self.sniffer.feed(data, &mut self.sniffed);
        if self.sniffed.is_empty() {
            return;
        }
        let mut effects = self.effects.borrow_mut();
        for event in self.sniffed.drain(..) {
            match event {
                osc::Event::Notify(notification) => effects.notifications.push(notification),
                osc::Event::Progress(progress) => effects.progress = Some(progress),
                osc::Event::ShiftCapture(wanted) => self.shift_capture = Some(wanted),
                osc::Event::Reset => self.shift_capture = None,
            }
        }
    }

    /// Whether the application asked to see Shift on mouse events, with
    /// XTSHIFTESCAPE. Until one asks, Shift belongs to the user, which is how
    /// Ghostty resolves its `mouse-shift-capture = false` default.
    #[must_use]
    pub fn captures_shift(&self) -> bool {
        self.shift_capture.unwrap_or(false)
    }

    pub fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> Result<()> {
        self.inner
            .resize(cols.max(1), rows.max(1), cell_width_px, cell_height_px)
    }

    /// Fill `out` with every inline image the viewport shows, bottom of the
    /// stack first.
    ///
    /// Geometry alone: scrolling and resizing move a placement without touching
    /// the image storage, so this is recomputed per frame while the pixels
    /// behind it are fetched only when [`Terminal::image_pixels`] is asked for
    /// them. The buffer belongs to the caller so that a frame with no images
    /// allocates nothing.
    pub fn images(&self, out: &mut Vec<Placement>) -> Result<()> {
        image::placements(&self.inner, out)
    }

    /// One stored image as RGBA pixels, or `None` if it is gone.
    ///
    /// This copies the whole bitmap, so it is meant for a texture cache miss
    /// rather than for a frame. [`Placement::image`] carries the key to cache
    /// it under.
    pub fn image_pixels(&self, id: u32) -> Result<Option<Pixels>> {
        image::pixels(&self.inner, id)
    }

    pub fn scroll_lines(&mut self, delta: isize) {
        self.inner.scroll_viewport(ScrollViewport::Delta(delta));
    }

    pub fn scroll_to_bottom(&mut self) {
        self.inner.scroll_viewport(ScrollViewport::Bottom);
    }

    /// Whether the cursor blinks when the application has not asked for a
    /// particular style. Upstream's built-in default is a solid cursor; every
    /// terminal on this desktop blinks, and DECSCUSR still overrides this.
    pub fn set_default_cursor_blink(&mut self, blink: Option<bool>) -> Result<()> {
        self.inner.set_default_cursor_blink(blink)?;
        Ok(())
    }

    /// The shape it takes under the same condition, and with the same override:
    /// a program that sends DECSCUSR has said what it wants, and gets it.
    pub fn set_default_cursor_style(&mut self, style: Option<CursorStyle>) -> Result<()> {
        self.inner.set_default_cursor_style(style)?;
        Ok(())
    }

    pub fn scroll_to_top(&mut self) {
        self.inner.scroll_viewport(ScrollViewport::Top);
    }

    /// Put `row` at the top of the viewport, counted from the top of the
    /// scrollback. Same row space as [`ScrollPosition::offset`].
    pub fn scroll_to_row(&mut self, row: usize) {
        self.inner.scroll_viewport(ScrollViewport::Row(row));
    }

    /// Where the viewport sits in the scrollable area.
    ///
    /// Upstream raises no notification when this changes, so a scrollbar has to
    /// poll it once per frame and diff — which is what Ghostty's own renderer
    /// does.
    #[must_use]
    pub fn scroll_position(&self) -> ScrollPosition {
        self.inner
            .scrollbar()
            .map(|bar| ScrollPosition {
                total: bar.total as usize,
                offset: bar.offset as usize,
                len: bar.len as usize,
            })
            .unwrap_or_default()
    }

    /// Take everything the terminal raised since the last call.
    pub fn take_effects(&mut self) -> Effects {
        std::mem::take(&mut *self.effects.borrow_mut())
    }

    pub fn title(&self) -> Option<&str> {
        self.inner.title().ok().filter(|t| !t.is_empty())
    }

    /// The shell's working directory, as OSC 7 last reported it.
    ///
    /// The sequence carries a `file://` URI, so this returns the local path it
    /// names — and nothing at all when the URI names another machine, whose
    /// paths mean nothing here.
    pub fn pwd(&self) -> Option<String> {
        let raw = self.inner.pwd().ok().filter(|p| !p.is_empty())?;
        local_path(raw)
    }

    pub fn is_mouse_tracking(&self) -> bool {
        self.inner.is_mouse_tracking().unwrap_or(false)
    }

    /// Whether a full-screen application is on top: an editor, a pager, a
    /// terminal user interface. Three modes reach that state, and any of them
    /// means what is on screen is a program's own drawing rather than a shell
    /// with a prompt on it.
    #[must_use]
    pub fn is_alternate_screen(&self) -> bool {
        self.inner.mode(Mode::ALT_SCREEN_LEGACY).unwrap_or(false)
            || self.inner.mode(Mode::ALT_SCREEN).unwrap_or(false)
            || self.inner.mode(Mode::ALT_SCREEN_SAVE).unwrap_or(false)
    }

    /// Whether the application hears about the pointer moving, rather than only
    /// about the buttons.
    ///
    /// Button-event and any-event tracking are the two modes a drag reaches. In
    /// the older click-only modes a program never learns the pointer moved at
    /// all, so a drag it would be given is a drag nobody receives.
    pub fn tracks_mouse_motion(&self) -> bool {
        self.inner.mode(Mode::BUTTON_MOUSE).unwrap_or(false)
            || self.inner.mode(Mode::ANY_MOUSE).unwrap_or(false)
    }

    /// Encode a key event into the bytes to write to the PTY. Returns an empty
    /// slice for keys that produce no output, such as a bare modifier.
    pub fn encode_key(&mut self, input: &KeyInput<'_>) -> Result<&[u8]> {
        self.key_encoder.set_options_from_terminal(&self.inner);
        self.key_event
            .set_action(input.action)
            .set_key(input.key)
            .set_mods(input.mods)
            .set_consumed_mods(input.consumed_mods)
            // Set on every event: the encoder's event is reused between calls,
            // and an absent codepoint has to overwrite the last one rather than
            // leave it standing. NUL is the encoder's own word for "none".
            .set_unshifted_codepoint(input.unshifted_codepoint.unwrap_or('\0'))
            .set_utf8(input.text);

        self.encoded.clear();
        self.key_encoder
            .encode_to_vec(&self.key_event, &mut self.encoded)?;
        Ok(&self.encoded)
    }

    /// Encode a mouse event. Returns an empty slice when the application has
    /// not enabled mouse reporting for this event.
    pub fn encode_mouse(&mut self, input: &MouseInput, geometry: Geometry) -> Result<&[u8]> {
        self.mouse_encoder
            .set_options_from_terminal(&self.inner)
            .set_size(geometry.into())
            .set_any_button_pressed(input.any_button_pressed)
            // Motion within one cell is not news; without this a drag floods
            // the application with identical reports.
            .set_track_last_cell(true);
        self.mouse_event
            .set_action(input.action)
            .set_button(input.button)
            .set_mods(input.mods)
            .set_position(mouse::Position {
                x: input.x as f32,
                y: input.y as f32,
            });

        self.encoded.clear();
        self.mouse_encoder
            .encode_to_vec(&self.mouse_event, &mut self.encoded)?;
        Ok(&self.encoded)
    }

    /// Encode the window gaining or losing the keyboard. Returns an empty slice
    /// unless the application asked to hear about it with mode 1004; editors and
    /// multiplexers use it to reload changed files or dim an idle pane.
    pub fn encode_focus(&mut self, gained: bool) -> Result<&[u8]> {
        self.encoded.clear();
        if !self.inner.mode(Mode::FOCUS_EVENT).unwrap_or(false) {
            return Ok(&self.encoded);
        }
        let event = if gained {
            focus::Event::Gained
        } else {
            focus::Event::Lost
        };
        let mut buf = [0u8; 8];
        let written = event.encode(&mut buf)?;
        self.encoded.extend_from_slice(&buf[..written]);
        Ok(&self.encoded)
    }

    /// Encode a wheel movement as arrow keys, mode 1007's bargain: an
    /// application that switched to the alternate screen scrolls by arrows,
    /// because it has no scrollback for a viewport to move over. `rows_down`
    /// is positive toward the bottom.
    ///
    /// Empty unless all three of Ghostty's conditions hold: the alternate
    /// screen is active, no mouse tracking is on (a tracking application
    /// hears the wheel as buttons instead), and mode 1007 is set, which it is
    /// by default. The arrows honor DECCKM the way real arrow keys do.
    pub fn encode_alternate_scroll(&mut self, rows_down: isize) -> Result<&[u8]> {
        self.encoded.clear();
        if rows_down == 0
            || !self.is_alternate_screen()
            || self.inner.is_mouse_tracking().unwrap_or(false)
            || !self.inner.mode(Mode::ALT_SCROLL).unwrap_or(false)
        {
            return Ok(&self.encoded);
        }
        let application = self.inner.mode(Mode::DECCKM).unwrap_or(false);
        let arrow: &[u8] = match (application, rows_down > 0) {
            (true, true) => b"\x1bOB",
            (true, false) => b"\x1bOA",
            (false, true) => b"\x1b[B",
            (false, false) => b"\x1b[A",
        };
        for _ in 0..rows_down.unsigned_abs() {
            self.encoded.extend_from_slice(arrow);
        }
        Ok(&self.encoded)
    }

    // --- selection ---------------------------------------------------------

    /// Begin a selection at a surface-space position.
    ///
    /// Click counting lives in the gesture state machine, so the second and
    /// third click at the same spot select a word and a line without the widget
    /// having to know anything about word boundaries. `time` is a monotonic
    /// event timestamp; without it only single clicks are possible.
    ///
    /// `output_on_triple` turns the third click into a selection of one
    /// command's output rather than one line, which is what it does under
    /// Ctrl in Ghostty. It only reaches as far as the shell marks its prompts
    /// with OSC 133.
    pub fn select_press(
        &mut self,
        x: f64,
        y: f64,
        geometry: Geometry,
        time: Duration,
        output_on_triple: bool,
    ) -> Result<()> {
        let grid_ref = self.inner.grid_ref(geometry.point(x, y))?;
        // A repeat click may land this far from the first one and still
        // count: one cell, which is Ghostty's allowance.
        let behaviors = Behaviors::new()
            .with_single_click_behavior(Behavior::Cell)
            .with_double_click_behavior(Behavior::Word)
            .with_triple_click_behavior(if output_on_triple {
                Behavior::Output
            } else {
                Behavior::Line
            });
        self.press_event
            .set_position(x, y)?
            .set_time(time)?
            .set_repeat_distance(f64::from(geometry.cell_width_px.max(1)))?
            .set_behaviors(&behaviors)?;
        let selection = self
            .press_event
            .apply(&mut self.gesture, &self.inner, grid_ref)?;
        // A plain single click yields no selection: it only drops the anchor,
        // and any previous selection goes away.
        self.inner.set_selection(selection.as_ref())?;
        Ok(())
    }

    /// How many clicks the gesture is at, once a press has been fed to it:
    /// 1 for a click, 2 for a double, 3 from there on.
    #[must_use]
    pub fn click_count(&self) -> u8 {
        self.gesture.click_count(&self.inner).unwrap_or(0)
    }

    /// Extend the selection to a surface-space position. `rectangle` selects a
    /// block rather than a text run.
    pub fn select_drag(
        &mut self,
        x: f64,
        y: f64,
        geometry: Geometry,
        rectangle: bool,
    ) -> Result<()> {
        let grid_ref = self.inner.grid_ref(geometry.point(x, y))?;
        self.drag_event
            .set_position(x, y)?
            .set_rectangle(rectangle)?;
        let selection =
            self.drag_event
                .apply(&mut self.gesture, &self.inner, grid_ref, geometry.into())?;
        if let Some(selection) = selection {
            self.inner.set_selection(Some(&selection))?;
        }
        Ok(())
    }

    /// End the gesture. The selection itself stays installed.
    pub fn select_release(&mut self) -> Result<()> {
        self.release_event
            .apply(&mut self.gesture, &self.inner, None)
    }

    /// Whether the drag in progress is pinned against the top or bottom edge
    /// and wants the viewport moved. `Some(true)` is upward.
    #[must_use]
    pub fn selection_autoscroll(&self) -> Option<bool> {
        match self.gesture.autoscroll(&self.inner) {
            Ok(Autoscroll::Up) => Some(true),
            Ok(Autoscroll::Down) => Some(false),
            _ => None,
        }
    }

    /// One tick of drag autoscroll: scroll the viewport a row toward the
    /// pointer and carry the selection onto what just came into view.
    ///
    /// Answers whether the gesture is still alive; `false` says the timer
    /// driving these ticks should stop.
    pub fn autoscroll_tick(
        &mut self,
        x: f64,
        y: f64,
        geometry: Geometry,
        rectangle: bool,
    ) -> Result<bool> {
        let Point::Viewport(viewport) = geometry.point(x, y) else {
            return Ok(false);
        };
        self.autoscroll_event
            .set_position(x, y)?
            .set_rectangle(rectangle)?;
        let selection = self.autoscroll_event.apply(
            &mut self.gesture,
            &self.inner,
            viewport,
            geometry.into(),
        )?;
        match selection {
            Some(selection) => {
                self.inner.set_selection(Some(&selection))?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub fn select_all(&mut self) -> Result<()> {
        let selection = self.inner.select_all()?;
        self.inner.set_selection(selection.as_ref())?;
        Ok(())
    }

    pub fn clear_selection(&mut self) -> Result<()> {
        self.gesture.reset(&self.inner);
        self.inner.set_selection(None)?;
        Ok(())
    }

    /// What a right click selects before its menu opens, as Ghostty does it:
    /// a click inside the selection keeps it, a click anywhere else selects
    /// the word under the pointer, and a click on nothing clears. Answers
    /// whether the selection changed.
    ///
    /// Ghostty tries the hyperlink under the pointer before the word; a link
    /// here is a set of viewport cells rather than a range, so the word is
    /// what stands in for it, and is almost always the same run of cells.
    pub fn right_click_select(&mut self, x: f64, y: f64, geometry: Geometry) -> Result<bool> {
        let point = geometry.point(x, y);
        if let Some(selection) = self.inner.selection()?
            && selection.contains(&self.inner, point)?
        {
            return Ok(false);
        }
        let grid_ref = self.inner.grid_ref(point)?;
        let selection = self.inner.select_word(SelectWordOptions::new(grid_ref))?;
        self.inner.set_selection(selection.as_ref())?;
        Ok(true)
    }

    pub fn has_selection(&self) -> bool {
        matches!(self.inner.selection(), Ok(Some(_)))
    }

    /// The selected text, formatted the way a copy should behave: soft-wrapped
    /// lines rejoined, trailing whitespace dropped.
    pub fn selection_text(&mut self) -> Result<Option<String>> {
        if self.selection_buf.is_empty() {
            self.selection_buf.resize(4096, 0);
        }
        loop {
            let options = FormatOptions::new()
                .with_emit_format(Format::Plain)
                .with_unwrap(true)
                .with_trim(true);
            match self
                .inner
                .format_selection_buf(options, &mut self.selection_buf)
            {
                Ok(None) => return Ok(None),
                Ok(Some(len)) => {
                    let text = String::from_utf8_lossy(&self.selection_buf[..len]).into_owned();
                    return Ok(Some(text));
                }
                // Grow and retry. A required size that would not actually grow
                // the buffer would loop forever, so treat it as a failure.
                Err(Error::OutOfSpace { required }) if required > self.selection_buf.len() => {
                    self.selection_buf.resize(required, 0);
                }
                Err(err) => return Err(err),
            }
        }
    }

    // --- hyperlinks ----------------------------------------------------------

    /// Read one viewport cell's OSC 8 URI into [`Self::link_buf`], returning its
    /// length. `None` means the cell carries no hyperlink.
    ///
    /// The grid reference is built and consumed inside this call: upstream
    /// documents one as valid only until the next terminal update, so none may
    /// be held across a feed.
    fn read_hyperlink(&mut self, col: u16, row: u16) -> Result<Option<usize>> {
        if self.link_buf.is_empty() {
            self.link_buf.resize(256, 0);
        }
        loop {
            let point = Point::Viewport(PointCoordinate {
                x: col,
                y: u32::from(row),
            });
            let grid_ref = self.inner.grid_ref(point)?;
            match grid_ref.hyperlink_uri(&mut self.link_buf) {
                Ok(0) => return Ok(None),
                Ok(len) => return Ok(Some(len)),
                // Grow and retry. A required size that would not actually grow
                // the buffer would loop forever, so treat it as a failure.
                Err(Error::OutOfSpace { required }) if required > self.link_buf.len() => {
                    self.link_buf.resize(required, 0);
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// The soft-wrapped line one viewport row belongs to: its text, one
    /// character per cell, and the viewport row it starts on.
    ///
    /// One character per cell is the contract that lets a character index name
    /// a cell again: a spacer cell under a wide character contributes a space,
    /// and a cell holding a grapheme cluster contributes the cluster's first
    /// character. What that loses cannot be part of a URL anyway, which is
    /// what this is read for. A run that began above the viewport starts at
    /// the top edge here, the same place Ghostty's hover matching gives up.
    ///
    /// The text comes from the flattened grid of the last [`Self::snapshot`],
    /// the same bargain [`Self::hyperlink_hover`] strikes: a hover is only
    /// ever probed over something already drawn.
    pub fn line_text(&mut self, row: u16) -> Result<(String, u16)> {
        let row = row.min(self.grid.rows.saturating_sub(1));
        let mut first = row;
        while first > 0 && self.row_continues(first)? {
            first -= 1;
        }
        let mut last = row;
        while last + 1 < self.grid.rows && self.row_wraps(last)? {
            last += 1;
        }

        let cols = usize::from(self.grid.cols);
        let mut line = String::with_capacity(cols * usize::from(last - first + 1));
        for y in first..=last {
            for x in 0..self.grid.cols {
                let text = self
                    .grid
                    .cell(x, y)
                    .map(|cell| cell.text.as_str())
                    .unwrap_or("");
                line.push(text.chars().next().unwrap_or(' '));
            }
        }
        Ok((line, first))
    }

    fn row_wraps(&self, row: u16) -> Result<bool> {
        let point = Point::Viewport(PointCoordinate {
            x: 0,
            y: u32::from(row),
        });
        self.inner.grid_ref(point)?.row()?.is_wrapped()
    }

    fn row_continues(&self, row: u16) -> Result<bool> {
        let point = Point::Viewport(PointCoordinate {
            x: 0,
            y: u32::from(row),
        });
        self.inner.grid_ref(point)?.row()?.is_wrap_continuation()
    }

    /// The OSC 8 URI on one viewport cell, if it carries one.
    pub fn hyperlink_at(&mut self, col: u16, row: u16) -> Result<Option<String>> {
        let Some(len) = self.read_hyperlink(col, row)? else {
            return Ok(None);
        };
        Ok(Some(
            String::from_utf8_lossy(&self.link_buf[..len]).into_owned(),
        ))
    }

    /// The whole link under one cell: its URI and every visible cell that
    /// belongs to it.
    ///
    /// Cells are gathered by comparing URIs across the viewport rather than by
    /// contiguity, which is what Ghostty does — a link wrapped across lines, or
    /// split by a repaint, still highlights as one. Ghostty compares the
    /// hyperlink's identity first and its URI second; the C API exposes no
    /// identity, so two adjacent links that differ only by an `id=` parameter
    /// highlight here as one. The URI that opens is still the right one.
    ///
    /// Costs nothing on a cell with no link, which is nearly every call: the
    /// flattened grid answers that without crossing into the library.
    pub fn hyperlink_hover(&mut self, col: u16, row: u16) -> Result<Option<LinkHover>> {
        if !self.grid.cell(col, row).is_some_and(|cell| cell.link) {
            return Ok(None);
        }
        let Some(len) = self.read_hyperlink(col, row)? else {
            return Ok(None);
        };
        let uri = String::from_utf8_lossy(&self.link_buf[..len]).into_owned();

        let mut cells = Vec::new();
        for y in 0..self.grid.rows {
            for x in 0..self.grid.cols {
                if !self.grid.cell(x, y).is_some_and(|cell| cell.link) {
                    continue;
                }
                if let Some(len) = self.read_hyperlink(x, y)?
                    && self.link_buf[..len] == *uri.as_bytes()
                {
                    cells.push((x, y));
                }
            }
        }
        Ok(Some(LinkHover { uri, cells }))
    }

    /// Encode text for pasting, wrapping it in bracketed paste markers when the
    /// application asked for them. Control bytes are stripped upstream, so a
    /// paste cannot smuggle an escape sequence through.
    pub fn encode_paste(&mut self, text: &str) -> Result<&[u8]> {
        let bracketed = self.inner.mode(Mode::BRACKETED_PASTE).unwrap_or(false);
        let mut data = text.as_bytes().to_vec();

        // Markers plus room for any expansion the encoder does.
        self.encoded.clear();
        self.encoded.resize(data.len() + 32, 0);
        loop {
            match paste::encode(&mut data, bracketed, &mut self.encoded) {
                Ok(len) => {
                    self.encoded.truncate(len);
                    return Ok(&self.encoded);
                }
                Err(Error::OutOfSpace { required }) if required > self.encoded.len() => {
                    self.encoded.resize(required, 0);
                    data.copy_from_slice(text.as_bytes());
                }
                Err(err) => return Err(err),
            }
        }
    }

    // --- prompts -------------------------------------------------------------

    /// The row a shell prompt starts on, the nearest one to `from` in the
    /// direction asked for, counted from the top of the scrollback the way
    /// [`ScrollPosition::offset`] counts.
    ///
    /// Rows know this because OSC 133 told them, so a shell that does not mark
    /// its prompts has no prompt rows at all and this always answers `None`.
    /// That is the same bargain the triple click selecting a command's output
    /// strikes, and there is no guessing it from the text: a `$` in a build log
    /// is one character like any other.
    ///
    /// A prompt spanning several rows answers on its first, so walking up twice
    /// does not land inside the prompt it just left.
    ///
    /// One FFI lookup per row walked over, bounded by the scrollback, which the
    /// `terminal.scrollback-lines` setting caps. The walk stops at the first
    /// prompt, so the length that costs the full scan is the one where there is
    /// no prompt to find.
    #[must_use]
    pub fn prompt_row(&self, from: usize, up: bool) -> Option<usize> {
        if up {
            if from == 0 {
                return None;
            }
            // A prompt row whose predecessor is not one is where the prompt
            // begins, which is the whole test — carried down the walk rather
            // than looked up twice per row.
            let mut here = self.is_prompt_row(from - 1);
            for row in (0..from).rev() {
                let previous = row > 0 && self.is_prompt_row(row - 1);
                if here && !previous {
                    return Some(row);
                }
                here = previous;
            }
            return None;
        }

        let total = self.scroll_position().total;
        let mut previous = self.is_prompt_row(from);
        for row in from.saturating_add(1)..total {
            let here = self.is_prompt_row(row);
            if here && !previous {
                return Some(row);
            }
            previous = here;
        }
        None
    }

    /// Whether one row carries a shell prompt, in screen coordinates — the row
    /// space [`ScrollPosition`] counts in, scrollback included.
    fn is_prompt_row(&self, row: usize) -> bool {
        let point = Point::Screen(PointCoordinate {
            x: 0,
            y: u32::try_from(row).unwrap_or(u32::MAX),
        });
        self.inner
            .grid_ref(point)
            .and_then(|grid_ref| grid_ref.row())
            .and_then(libghostty_vt::screen::Row::semantic_prompt)
            .is_ok_and(|state| state != RowSemanticPrompt::None)
    }

    // --- search --------------------------------------------------------------

    /// Every place `needle` occurs in what the terminal is holding, screen and
    /// scrollback alike, in reading order.
    ///
    /// Upstream has no search of its own, so this is the whole screen formatted
    /// as text — one line per row, so the line a match lands on *is* its row —
    /// scanned case-insensitively. Soft-wrapped lines are left wrapped for that
    /// reason: a row has to keep its number for the viewport to be able to
    /// scroll to it, which also means a match split across a wrap is two rows
    /// and is not found. Ghostty's own search has the same limit.
    ///
    /// Matches do not overlap, and an empty needle matches nothing rather than
    /// everything.
    pub fn search(&mut self, needle: &str) -> Result<Vec<Hit>> {
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let Some(text) = self.screen_text()? else {
            return Ok(Vec::new());
        };

        let mut hits = Vec::new();
        for (row, line) in text.split('\n').enumerate() {
            let line = line.strip_suffix('\r').unwrap_or(line);
            find_in_line(line, needle, row, &mut hits);
        }
        Ok(hits)
    }

    /// The whole screen as plain text, one line per row.
    fn screen_text(&mut self) -> Result<Option<String>> {
        let Some(selection) = self.inner.select_all()? else {
            return Ok(None);
        };
        if self.search_buf.is_empty() {
            self.search_buf.resize(64 * 1024, 0);
        }
        loop {
            // Neither unwrapped nor trimmed: both would move text off the row
            // it is actually on, and a row number is what a hit is for.
            let options = FormatOptions::new()
                .with_emit_format(Format::Plain)
                .with_selection(&selection)
                .with_unwrap(false)
                .with_trim(false);
            match self
                .inner
                .format_selection_buf(options, &mut self.search_buf)
            {
                Ok(None) => return Ok(None),
                Ok(Some(len)) => {
                    return Ok(Some(
                        String::from_utf8_lossy(&self.search_buf[..len]).into_owned(),
                    ));
                }
                // Grow and retry. A required size that would not actually grow
                // the buffer would loop forever, so treat it as a failure.
                Err(Error::OutOfSpace { required }) if required > self.search_buf.len() => {
                    self.search_buf.resize(required, 0);
                }
                Err(err) => return Err(err),
            }
        }
    }

    // --- history -------------------------------------------------------------

    /// Everything the terminal is holding — scrollback and screen — as the VT
    /// bytes that reproduce it, capped to the last `max_lines` lines.
    ///
    /// Written to disk when the window closes and fed back into a fresh
    /// terminal when it opens, so what scrolled past is still there to scroll
    /// back to. Emitted in [`Format::Vt`] rather than as plain text: colors and
    /// bold are most of what makes old output readable, and the terminal that
    /// reads it back is a terminal.
    ///
    /// Soft-wrapped lines are rejoined, so a session saved from a wide window
    /// re-wraps to fit a narrow one instead of restoring with the old width's
    /// breaks baked in.
    ///
    /// `None` when there is nothing on screen worth keeping.
    pub fn dump_history(&self, max_lines: usize) -> Result<Option<String>> {
        // Not installed as the terminal's selection: this runs on a live
        // terminal, and saving the session should not take away what the user
        // had highlighted.
        let Some(selection) = self.inner.select_all()? else {
            return Ok(None);
        };

        let mut buf = vec![0u8; 64 * 1024];
        let text = loop {
            let options = FormatOptions::new()
                .with_emit_format(Format::Vt)
                .with_selection(&selection)
                .with_unwrap(true)
                .with_trim(true);
            match self.inner.format_selection_buf(options, &mut buf) {
                Ok(None) => return Ok(None),
                Ok(Some(len)) => break String::from_utf8_lossy(&buf[..len]).into_owned(),
                // Grow and retry. A required size that would not actually grow
                // the buffer would loop forever, so treat it as a failure.
                Err(Error::OutOfSpace { required }) if required > buf.len() => {
                    buf.resize(required, 0);
                }
                Err(err) => return Err(err),
            }
        };

        if text.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(last_lines(text, max_lines)))
    }

    /// Flatten the current viewport into an owned grid.
    ///
    /// The upstream snapshot is a borrow chain (snapshot → row iteration →
    /// cell iteration) whose lifetimes cannot leave this method, so the frame
    /// is copied out. Buffers are reused, so a steady-state redraw does not
    /// allocate.
    pub fn snapshot(&mut self) -> Result<&Grid> {
        let snapshot = self.render.update(&self.inner)?;
        let colors = snapshot.colors()?;
        let cols = snapshot.cols()?;
        let rows = snapshot.rows()?;

        let cursor = if snapshot.cursor_visible()? {
            snapshot.cursor_viewport()?.map(|pos| Cursor {
                col: pos.x,
                row: pos.y,
                shape: match snapshot.cursor_visual_style() {
                    Ok(CursorVisualStyle::Bar) => CursorShape::Bar,
                    Ok(CursorVisualStyle::Underline) => CursorShape::Underline,
                    Ok(CursorVisualStyle::BlockHollow) => CursorShape::BlockHollow,
                    _ => CursorShape::Block,
                },
                blinking: snapshot.cursor_blinking().unwrap_or(false),
                color: snapshot.cursor_color().ok().flatten().map(Rgb::from),
                text_color: self.cursor_text,
            })
        } else {
            None
        };

        self.grid.resize_and_clear(cols, rows);
        self.grid.fg = colors.foreground.into();
        self.grid.bg = colors.background.into();
        self.grid.cursor = cursor;

        let mut row_it = self.rows.update(&snapshot)?;
        let mut y: u16 = 0;
        while let Some(row) = row_it.next() {
            // A row-level flag, tolerant of false positives, that saves an FFI
            // call per cell on the overwhelming majority of rows, which carry
            // no hyperlink at all.
            let row_links = row
                .raw_row()
                .and_then(libghostty_vt::screen::Row::has_hyperlink)
                .unwrap_or(false);

            let mut cell_it = self.cells.update(row)?;
            let mut x: u16 = 0;
            while let Some(cell) = cell_it.next() {
                let Some(out) = self.grid.cell_mut(x, y) else {
                    break;
                };

                out.link = row_links
                    && cell
                        .raw_cell()
                        .and_then(libghostty_vt::screen::Cell::has_hyperlink)
                        .unwrap_or(false);

                let selected = cell.is_selected().unwrap_or(false);
                let mut fg = cell.fg_color()?.map_or(colors.foreground, |c| c);
                let mut bg = cell.bg_color()?;

                if cell.has_styling().unwrap_or(false) {
                    let style = cell.style()?;
                    if style.inverse {
                        let swapped = bg.unwrap_or(colors.background);
                        bg = Some(fg);
                        fg = swapped;
                    }
                    out.bold = style.bold;
                    out.italic = style.italic;
                    out.strikethrough = style.strikethrough;
                    out.underline = style.underline != libghostty_vt::style::Underline::None;
                    if style.faint {
                        // Half-way to the background, the share Ghostty's
                        // renderer gives a faint cell. Without this a faint
                        // run — a completion's placeholder, a suggestion —
                        // draws at full strength and reads as typed text.
                        fg = Rgb::from(fg)
                            .blend(Rgb::from(bg.unwrap_or(colors.background)), 0.5)
                            .into();
                    }
                }

                if selected {
                    match self.selection_colors {
                        // A theme's own selection colors. A theme that names
                        // only a background keeps the cell's own text color,
                        // which is what Ghostty does and what makes syntax
                        // highlighting survive being selected.
                        Some((background, foreground)) if background.is_some() => {
                            bg = background.map(Into::into);
                            if let Some(foreground) = foreground {
                                fg = foreground.into();
                            }
                        }
                        // No theme, or a theme with nothing to say about
                        // selection: invert, which always reads.
                        _ => {
                            let swapped = bg.unwrap_or(colors.background);
                            bg = Some(fg);
                            fg = swapped;
                        }
                    }
                }

                if cell.graphemes_len().unwrap_or(0) > 0 {
                    cell.graphemes_utf8(&mut out.text)?;
                }
                out.fg = fg.into();
                out.bg = bg.map(Rgb::from);

                x += 1;
            }
            y += 1;
        }

        Ok(&self.grid)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // The gesture holds tracked grid references. Dropping it without a
        // terminal in hand leaks them until the terminal goes away; resetting
        // here hands them back while the terminal is still alive.
        self.gesture.reset(&self.inner);
    }
}

/// The tail of `text`, at most `max_lines` lines of it.
///
/// Trimming a VT dump from the front can cut away the escape that turned a
/// color on, so what survives starts with a reset rather than with whatever
/// style the discarded lines happened to leave behind.
fn last_lines(text: String, max_lines: usize) -> String {
    if max_lines == 0 {
        return String::new();
    }
    let Some((offset, _)) = text.rmatch_indices('\n').nth(max_lines - 1) else {
        return text;
    };
    format!("\x1b[0m{}", &text[offset + 1..])
}

/// The local path a `file://` URI names, or `None` when it names another host.
///
/// Exported because a `file://` hyperlink deserves the same question OSC 7
/// asks: a path from a machine at the far end of an SSH session means nothing
/// here, and opening it would open some unrelated local file.
#[must_use]
pub fn local_file_path(uri: &str) -> Option<String> {
    local_path(uri)
}

/// The local path a `file://` URI names, or `None` if it names another host.
///
/// Shells send OSC 7 as a URI with the hostname attached, which is how a
/// terminal knows an SSH session's directory is not one it can open. Bare
/// paths are accepted too: some shells send them despite the specification.
fn local_path(raw: &str) -> Option<String> {
    let Some(rest) = raw.strip_prefix("file://") else {
        return raw.starts_with('/').then(|| raw.to_owned());
    };

    // "file://host" with no path at all says nothing useful.
    let (host, path) = rest.split_at(rest.find('/')?);
    let local = host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || hostname().is_some_and(|name| host.eq_ignore_ascii_case(&name));

    local.then(|| percent_decode(path))
}

fn hostname() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
}

fn percent_decode(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok());
                match hex {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    // Not an escape after all; a literal '%' in a file name.
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }

    String::from_utf8_lossy(&out).into_owned()
}
