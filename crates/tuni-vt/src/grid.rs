//! Owned snapshot of the terminal viewport.
//!
//! The libghostty render state is a borrow-chained iteration (snapshot → rows →
//! cells) whose lifetimes cannot cross a widget boundary. So one frame is
//! flattened into this owned grid, whose buffers are reused between frames —
//! `String::clear` keeps the per-cell allocation, so a steady-state redraw
//! allocates nothing.

/// 24-bit color. Mirrors `libghostty_vt::style::RgbColor` so callers above this
/// crate never name an upstream type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl From<libghostty_vt::style::RgbColor> for Rgb {
    fn from(c: libghostty_vt::style::RgbColor) -> Self {
        Self {
            r: c.r,
            g: c.g,
            b: c.b,
        }
    }
}

impl From<Rgb> for libghostty_vt::style::RgbColor {
    fn from(c: Rgb) -> Self {
        Self {
            r: c.r,
            g: c.g,
            b: c.b,
        }
    }
}

/// One grid cell. `text` is empty for a blank cell and holds a full grapheme
/// cluster otherwise, so combining marks stay attached to their base.
#[derive(Clone, Debug, Default)]
pub struct Cell {
    pub text: String,
    pub fg: Rgb,
    /// `None` means "use the grid default" — worth distinguishing, because a
    /// cell explicitly painted with the default background still has to be
    /// filled when it sits under a differently-colored region.
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
}

impl Cell {
    pub(crate) fn reset(&mut self) {
        self.text.clear();
        self.bg = None;
        self.bold = false;
        self.italic = false;
        self.underline = false;
        self.strikethrough = false;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    BlockHollow,
    Bar,
    Underline,
}

#[derive(Clone, Copy, Debug)]
pub struct Cursor {
    pub col: u16,
    pub row: u16,
    pub shape: CursorShape,
    pub blinking: bool,
    pub color: Option<Rgb>,
    /// The glyph the cursor covers, when the theme names a color for it.
    /// `None` means paint it in the cell's own background, so the cursor reads
    /// as an inversion.
    pub text_color: Option<Rgb>,
}

/// A viewport snapshot: `rows * cols` cells in row-major order.
#[derive(Clone, Debug, Default)]
pub struct Grid {
    pub cols: u16,
    pub rows: u16,
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Option<Cursor>,
    pub(crate) cells: Vec<Cell>,
}

impl Grid {
    /// Grow or shrink to `cols * rows` and blank every cell, keeping the
    /// existing per-cell string allocations.
    pub(crate) fn resize_and_clear(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        self.cells.resize(usize::from(cols) * usize::from(rows), Cell::default());
        for cell in &mut self.cells {
            cell.reset();
        }
    }

    pub(crate) fn cell_mut(&mut self, col: u16, row: u16) -> Option<&mut Cell> {
        if col >= self.cols || row >= self.rows {
            return None;
        }
        let idx = usize::from(row) * usize::from(self.cols) + usize::from(col);
        self.cells.get_mut(idx)
    }

    #[must_use]
    pub fn row(&self, row: u16) -> &[Cell] {
        if row >= self.rows {
            return &[];
        }
        let start = usize::from(row) * usize::from(self.cols);
        &self.cells[start..start + usize::from(self.cols)]
    }

    #[must_use]
    pub fn cell(&self, col: u16, row: u16) -> Option<&Cell> {
        if col >= self.cols || row >= self.rows {
            return None;
        }
        self.cells
            .get(usize::from(row) * usize::from(self.cols) + usize::from(col))
    }
}
