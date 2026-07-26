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

use std::cell::RefCell;
use std::rc::Rc;

use libghostty_vt::render::{CellIterator, CursorVisualStyle, RenderState, RowIterator};
use libghostty_vt::terminal::{ScrollViewport, Terminal as VtTerminal, TerminalOptions};
use libghostty_vt::{key, mouse};

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
    /// Position in cells from the top-left of the viewport.
    pub col: u16,
    pub row: u16,
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
    effects: Rc<RefCell<Effects>>,
    grid: Grid,
    /// Scratch buffer for encoder output, reused across events.
    encoded: Vec<u8>,
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

        Ok(Self {
            inner,
            render: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            key_encoder: key::Encoder::new()?,
            key_event: key::Event::new()?,
            mouse_encoder: mouse::Encoder::new()?,
            mouse_event: mouse::Event::new()?,
            effects,
            grid: Grid::default(),
            encoded: Vec::with_capacity(64),
        })
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

    /// Take everything the terminal raised since the last call.
    pub fn take_effects(&mut self) -> Effects {
        std::mem::take(&mut *self.effects.borrow_mut())
    }

    pub fn title(&self) -> Option<&str> {
        self.inner.title().ok().filter(|t| !t.is_empty())
    }

    pub fn pwd(&self) -> Option<&str> {
        self.inner.pwd().ok().filter(|p| !p.is_empty())
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
    pub fn encode_mouse(&mut self, input: &MouseInput) -> Result<&[u8]> {
        self.mouse_encoder.set_options_from_terminal(&self.inner);
        self.mouse_event
            .set_action(input.action)
            .set_button(input.button)
            .set_mods(input.mods)
            .set_position(mouse::Position {
                x: f64::from(input.col),
                y: f64::from(input.row),
            });

        self.encoded.clear();
        self.mouse_encoder
            .encode_to_vec(&self.mouse_event, &mut self.encoded)?;
        Ok(&self.encoded)
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
                    let swapped = bg.unwrap_or(colors.background);
                    bg = Some(fg);
                    fg = swapped;
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
