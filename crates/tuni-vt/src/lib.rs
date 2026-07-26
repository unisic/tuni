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

pub use grid::{Cell, Cursor, CursorShape, Grid, Rgb};
pub use libghostty_vt::Error;
pub use libghostty_vt::key::{Action as KeyAction, Key, Mods};
pub use libghostty_vt::mouse::{Action as MouseAction, Button as MouseButton};

use libghostty_vt::mouse::EncoderSize;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use libghostty_vt::fmt::Format;
use libghostty_vt::render::{CellIterator, CursorVisualStyle, RenderState, RowIterator};
use libghostty_vt::selection::FormatOptions;
use libghostty_vt::selection::gesture::{
    DragEvent, Geometry as GestureGeometry, Gesture, PressEvent, ReleaseEvent,
};
use libghostty_vt::terminal::{
    ClipboardLocation, Mode, Options as TerminalOptions, Point, PointCoordinate, ScrollViewport,
    Terminal as VtTerminal,
};
use libghostty_vt::{key, mouse, paste};

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
    pub key: Key,
    pub mods: Mods,
    /// Mods already baked into `text` — Shift is consumed when the toolkit
    /// already produced the shifted character, otherwise the Kitty protocol
    /// would report it twice.
    pub consumed_mods: Mods,
    /// Committed text for this event, when the key produced any.
    pub text: Option<&'a str>,
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
    effects: Rc<RefCell<Effects>>,
    grid: Grid,
    /// Scratch buffer for encoder output, reused across events.
    encoded: Vec<u8>,
    /// Scratch buffer for formatted selection text.
    selection_buf: Vec<u8>,
    /// Selection colors, which the library does not own: selection is drawn by
    /// us, over cells the library only marks.
    selection_colors: Option<(Option<Rgb>, Option<Rgb>)>,
    /// The theme's cursor text color, applied to the cell under a block cursor.
    cursor_text: Option<Rgb>,
}

impl Terminal {
    pub fn new(cols: u16, rows: u16, max_scrollback: usize) -> Result<Self> {
        let mut inner = VtTerminal::new(TerminalOptions {
            cols: cols.max(1),
            rows: rows.max(1),
            max_scrollback,
        })?;

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

        // Click repeat is untimed until told otherwise, which would leave
        // double- and triple-click dead. These are the GTK defaults
        // (gtk-double-click-time, gtk-double-click-distance).
        let mut press_event = PressEvent::new()?;
        press_event
            .set_repeat_interval(Duration::from_millis(400))?
            .set_repeat_distance(5.0)?;

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
            effects,
            grid: Grid::default(),
            encoded: Vec::with_capacity(64),
            selection_buf: Vec::new(),
            selection_colors: None,
            cursor_text: None,
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
        self.inner.set_default_fg_color(Some(colors.foreground.into()))?;
        self.inner.set_default_bg_color(Some(colors.background.into()))?;
        self.inner.set_default_cursor_color(colors.cursor.map(Into::into))?;

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
    }

    pub fn resize(&mut self, cols: u16, rows: u16, cell_width_px: u32, cell_height_px: u32) -> Result<()> {
        self.inner
            .resize(cols.max(1), rows.max(1), cell_width_px, cell_height_px)
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

    /// Encode a key event into the bytes to write to the PTY. Returns an empty
    /// slice for keys that produce no output, such as a bare modifier.
    pub fn encode_key(&mut self, input: &KeyInput<'_>) -> Result<&[u8]> {
        self.key_encoder.set_options_from_terminal(&self.inner);
        self.key_event
            .set_action(input.action)
            .set_key(input.key)
            .set_mods(input.mods)
            .set_consumed_mods(input.consumed_mods)
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

    // --- selection ---------------------------------------------------------

    /// Begin a selection at a surface-space position.
    ///
    /// Click counting lives in the gesture state machine, so the second and
    /// third click at the same spot select a word and a line without the widget
    /// having to know anything about word boundaries. `time` is a monotonic
    /// event timestamp; without it only single clicks are possible.
    pub fn select_press(&mut self, x: f64, y: f64, geometry: Geometry, time: Duration) -> Result<()> {
        let grid_ref = self.inner.grid_ref(geometry.point(x, y))?;
        self.press_event.set_position(x, y)?.set_time(time)?;
        let selection = self
            .press_event
            .apply(&mut self.gesture, &self.inner, grid_ref)?;
        // A plain single click yields no selection: it only drops the anchor,
        // and any previous selection goes away.
        self.inner.set_selection(selection.as_ref())?;
        Ok(())
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
        self.drag_event.set_position(x, y)?.set_rectangle(rectangle)?;
        let selection = self.drag_event.apply(
            &mut self.gesture,
            &self.inner,
            grid_ref,
            geometry.into(),
        )?;
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
            match self.inner.format_selection_buf(options, &mut self.selection_buf) {
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
            let mut cell_it = self.cells.update(row)?;
            let mut x: u16 = 0;
            while let Some(cell) = cell_it.next() {
                let Some(out) = self.grid.cell_mut(x, y) else {
                    break;
                };

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
