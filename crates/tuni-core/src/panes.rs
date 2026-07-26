//! A tab's layout of panes, arranged the way niri arranges windows.
//!
//! A tab is a row of columns; a column is a stack of panes. Nothing nests
//! further, which is the whole point of the scheme: every pane is reachable by
//! a column index and a row index, so "focus left" and "move this one below
//! that one" are ordinary array operations rather than a walk down a tree of
//! splits. A tab that was never split is a layout with one column holding one
//! pane, and behaves exactly as a tab did before panes existed.
//!
//! Sizes are relative weights rather than pixels: the view hands out the space
//! it has in proportion to them, so a window resize needs no model change at
//! all, and a saved layout restores into a window of any size.

use crate::workspace::Id;

/// The smallest share of an axis a single tile may be shrunk to. Below this a
/// terminal is too narrow to be worth keeping on screen, and the divider it
/// hangs on becomes hard to grab back.
pub const MIN_SHARE: f64 = 0.1;

/// How much of an axis one keyboard resize step moves.
const RESIZE_STEP: f64 = 0.05;

/// Which side of a pane something goes on: the direction a split opens in, the
/// direction focus moves, and the edge a dragged pane was dropped against.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Edge {
    Left,
    Right,
    Up,
    Down,
}

impl Edge {
    /// Whether this edge works on columns rather than on the panes inside one.
    #[must_use]
    pub fn is_horizontal(self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }

    /// -1 towards the start of the axis, +1 towards its end.
    #[must_use]
    fn direction(self) -> isize {
        match self {
            Self::Left | Self::Up => -1,
            Self::Right | Self::Down => 1,
        }
    }
}

/// One tile: a terminal, and what the shell inside it last said about itself.
#[derive(Clone, Debug)]
pub struct Pane {
    id: Id,
    /// What the shell last said through OSC 0/2.
    pub title: Option<String>,
    /// Where the shell last said it is, through OSC 7.
    pub directory: Option<String>,
    /// Share of its column's height.
    pub weight: f64,
}

impl Pane {
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: Id::next(),
            title: None,
            directory: None,
            weight: 1.0,
        }
    }

    #[must_use]
    pub fn id(&self) -> Id {
        self.id
    }
}

impl Default for Pane {
    fn default() -> Self {
        Self::new()
    }
}

/// A vertical stack of panes. Columns tile left to right across the tab.
#[derive(Clone, Debug)]
pub struct Column {
    panes: Vec<Pane>,
    /// Share of the tab's width.
    pub weight: f64,
}

impl Column {
    fn new(pane: Pane) -> Self {
        Self {
            panes: vec![pane],
            weight: 1.0,
        }
    }

    /// A column read back from a saved layout. Nothing when it held no panes,
    /// which a hand-edited or truncated snapshot can say and the rest of the
    /// model may not see.
    #[must_use]
    pub fn from_panes(panes: Vec<Pane>, weight: f64) -> Option<Self> {
        if panes.is_empty() {
            return None;
        }
        Some(Self {
            panes,
            weight: sane(weight),
        })
    }

    #[must_use]
    pub fn panes(&self) -> &[Pane] {
        &self.panes
    }
}

/// A weight that came from outside: anything not a usable positive number
/// falls back to an even share.
fn sane(weight: f64) -> f64 {
    if weight.is_finite() && weight > 0.0 {
        weight
    } else {
        1.0
    }
}

/// Every pane in a tab, and which one has the keyboard.
#[derive(Clone, Debug)]
pub struct Layout {
    columns: Vec<Column>,
    focused: Id,
    zoomed: bool,
}

impl Layout {
    /// A layout holding one pane, which is what a fresh tab is.
    #[must_use]
    pub fn new(pane: Pane) -> Self {
        let focused = pane.id();
        Self {
            columns: vec![Column::new(pane)],
            focused,
            zoomed: false,
        }
    }

    /// A layout read back from a saved one, focused at (`column`, `row`).
    ///
    /// Nothing when the snapshot holds no panes at all; a focus position that
    /// no longer names a pane falls to the first one rather than rejecting the
    /// whole layout, because losing a tab's contents is worse than losing which
    /// half of it had the keyboard.
    #[must_use]
    pub fn from_columns(columns: Vec<Column>, column: usize, row: usize) -> Option<Self> {
        let first = columns.first()?.panes.first()?.id;
        let focused = columns
            .get(column)
            .and_then(|column| column.panes.get(row))
            .map_or(first, |pane| pane.id);
        Some(Self {
            columns,
            focused,
            zoomed: false,
        })
    }

    #[must_use]
    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    /// Every pane in layout order: columns left to right, top to bottom within
    /// each. The order `focus-next` walks and the order a saved layout keeps.
    pub fn panes(&self) -> impl Iterator<Item = &Pane> {
        self.columns.iter().flat_map(|column| column.panes.iter())
    }

    #[must_use]
    pub fn count(&self) -> usize {
        self.columns.iter().map(|column| column.panes.len()).sum()
    }

    /// Whether the tab holds more than one pane — what decides if focus rings,
    /// dividers and the zoom command mean anything.
    #[must_use]
    pub fn is_split(&self) -> bool {
        self.count() > 1
    }

    #[must_use]
    pub fn pane(&self, id: Id) -> Option<&Pane> {
        self.panes().find(|pane| pane.id == id)
    }

    pub fn pane_mut(&mut self, id: Id) -> Option<&mut Pane> {
        self.columns
            .iter_mut()
            .flat_map(|column| column.panes.iter_mut())
            .find(|pane| pane.id == id)
    }

    #[must_use]
    pub fn focused(&self) -> Id {
        self.focused
    }

    #[must_use]
    pub fn focused_pane(&self) -> Option<&Pane> {
        self.pane(self.focused)
    }

    /// (column, row) of a pane.
    #[must_use]
    pub fn location(&self, id: Id) -> Option<(usize, usize)> {
        self.columns.iter().enumerate().find_map(|(col, column)| {
            column
                .panes
                .iter()
                .position(|pane| pane.id == id)
                .map(|row| (col, row))
        })
    }

    /// Gives a pane the keyboard. Ignores an id this layout does not hold, and
    /// leaves zoom alone: a click can only land on a pane that is on screen.
    pub fn focus(&mut self, id: Id) {
        if self.pane(id).is_some() {
            self.focused = id;
        }
    }

    // Navigation. Each of these leaves zoom, so a command always lands on a
    // pane the window is actually showing.

    /// Moves focus one tile towards `edge`, stopping at the edge of the layout
    /// rather than wrapping — a direction key that wrapped would move focus to
    /// the far side of the screen, which is not what the key says it does.
    pub fn focus_toward(&mut self, edge: Edge) {
        self.unzoom();
        let Some((col, row)) = self.location(self.focused) else {
            return;
        };
        let step = edge.direction();
        if edge.is_horizontal() {
            let Some(next) = index_after(col, step, self.columns.len()) else {
                return;
            };
            // Land on the pane nearest the height focus was already at.
            let panes = &self.columns[next].panes;
            self.focused = panes[row.min(panes.len() - 1)].id;
        } else {
            let panes = &self.columns[col].panes;
            let Some(next) = index_after(row, step, panes.len()) else {
                return;
            };
            self.focused = panes[next].id;
        }
    }

    /// Cycles through every pane in layout order, wrapping at the ends.
    pub fn focus_next(&mut self) {
        self.cycle(1);
    }

    pub fn focus_previous(&mut self) {
        self.cycle(-1);
    }

    fn cycle(&mut self, offset: isize) {
        self.unzoom();
        let ids: Vec<Id> = self.panes().map(Pane::id).collect();
        if ids.len() < 2 {
            return;
        }
        let Some(index) = ids.iter().position(|id| *id == self.focused) else {
            return;
        };
        let next = (index as isize + offset).rem_euclid(ids.len() as isize) as usize;
        self.focused = ids[next];
    }

    // Zoom.

    #[must_use]
    pub fn is_zoomed(&self) -> bool {
        self.zoomed
    }

    /// Blows the focused pane up to fill the tab, or puts the layout back.
    ///
    /// Presentation only: the columns and their weights stay exactly as they
    /// were underneath, so zoom follows a focus change instead of hiding one.
    pub fn toggle_zoom(&mut self) {
        self.zoomed = !self.zoomed && self.is_split();
    }

    fn unzoom(&mut self) {
        self.zoomed = false;
    }

    // Structure.

    /// Puts a pane next to the focused one on the given edge, splitting the
    /// space the focused one had, and focuses it. Left and right open a new
    /// column; up and down stack inside the focused column.
    pub fn split(&mut self, pane: Pane, edge: Edge) {
        self.unzoom();
        let Some((col, row)) = self.location(self.focused) else {
            self.focused = pane.id;
            self.columns.push(Column::new(pane));
            return;
        };
        self.focused = pane.id;
        self.insert(pane, edge, col, row);
    }

    /// Moves a pane next to another one on the given edge — the drag gesture.
    /// Focus follows the pane that moved.
    pub fn move_pane(&mut self, dragged: Id, edge: Edge, target: Id) {
        if dragged == target {
            return;
        }
        let Some((from_col, from_row)) = self.location(dragged) else {
            return;
        };
        self.unzoom();
        let mut moved = self.columns[from_col].panes.remove(from_row);
        if self.columns[from_col].panes.is_empty() {
            self.columns.remove(from_col);
        }

        // The target is found again after the removal rather than before it:
        // taking the pane out shifts every index that came after it.
        self.focused = moved.id;
        match self.location(target) {
            Some((col, row)) => self.insert(moved, edge, col, row),
            None => {
                // Cannot happen while both ids come from this layout, but a
                // pane that fell through here would be lost for good.
                moved.weight = 1.0;
                self.columns.push(Column::new(moved));
            }
        }
    }

    /// Places `pane` beside the tile at (`col`, `row`), which gives up half of
    /// whatever share it had.
    fn insert(&mut self, mut pane: Pane, edge: Edge, col: usize, row: usize) {
        if edge.is_horizontal() {
            let share = self.columns[col].weight / 2.0;
            self.columns[col].weight = share;
            pane.weight = 1.0;
            let mut column = Column::new(pane);
            column.weight = share;
            let at = if edge == Edge::Left { col } else { col + 1 };
            self.columns.insert(at, column);
        } else {
            let share = self.columns[col].panes[row].weight / 2.0;
            self.columns[col].panes[row].weight = share;
            pane.weight = share;
            let at = if edge == Edge::Up { row } else { row + 1 };
            self.columns[col].panes.insert(at, pane);
        }
    }

    /// Takes a pane out, dropping its column when that empties and handing
    /// focus to the nearest survivor. False when the tab has nothing left in
    /// it, which is how the caller learns to close the tab.
    pub fn remove(&mut self, id: Id) -> bool {
        let Some((col, row)) = self.location(id) else {
            return self.count() > 0;
        };
        let was_focused = self.focused == id;
        self.columns[col].panes.remove(row);
        if self.columns[col].panes.is_empty() {
            self.columns.remove(col);
        }
        if was_focused
            && let Some(id) = self.nearest(col, row)
        {
            self.focused = id;
        }
        // Losing the zoomed pane, or the split it was hiding, drops back to the
        // layout; a pane closing out of sight underneath the zoom does not.
        if was_focused || !self.is_split() {
            self.unzoom();
        }
        self.count() > 0
    }

    /// The pane closest to a slot that no longer exists.
    fn nearest(&self, col: usize, row: usize) -> Option<Id> {
        let column = self.columns.get(col.min(self.columns.len().checked_sub(1)?))?;
        let pane = column.panes.get(row.min(column.panes.len().checked_sub(1)?))?;
        Some(pane.id())
    }

    // Sizing.

    /// Moves the divider beside the focused tile one step towards `edge`.
    ///
    /// The divider on the side being pushed, when there is one, so the tile
    /// grows that way; against the outer edge of the layout the divider on the
    /// other side moves instead and the tile shrinks. Either way the key does
    /// something, which is what makes holding it down feel like a resize.
    pub fn resize(&mut self, edge: Edge) {
        self.unzoom();
        let Some((col, row)) = self.location(self.focused) else {
            return;
        };
        let step = edge.direction();
        if edge.is_horizontal() {
            let weights: Vec<f64> = self.columns.iter().map(|column| column.weight).collect();
            let Some((divider, first, second)) = shifted(&weights, col, step) else {
                return;
            };
            self.columns[divider].weight = first;
            self.columns[divider + 1].weight = second;
        } else {
            let weights: Vec<f64> = self.columns[col].panes.iter().map(|pane| pane.weight).collect();
            let Some((divider, first, second)) = shifted(&weights, row, step) else {
                return;
            };
            self.columns[col].panes[divider].weight = first;
            self.columns[col].panes[divider + 1].weight = second;
        }
    }

    /// Hands every column, and every pane in them, an equal share.
    pub fn equalize(&mut self) {
        self.unzoom();
        for column in &mut self.columns {
            column.weight = 1.0;
            for pane in &mut column.panes {
                pane.weight = 1.0;
            }
        }
    }

    /// Writes back the column widths a divider drag ended on. A drag that
    /// arrives against a layout that has since changed shape is dropped.
    pub fn set_column_weights(&mut self, weights: &[f64]) {
        if weights.len() != self.columns.len() {
            return;
        }
        for (column, weight) in self.columns.iter_mut().zip(weights) {
            column.weight = *weight;
        }
    }

    /// The same, for the pane heights inside one column.
    pub fn set_pane_weights(&mut self, column: usize, weights: &[f64]) {
        let Some(column) = self.columns.get_mut(column) else {
            return;
        };
        if weights.len() != column.panes.len() {
            return;
        }
        for (pane, weight) in column.panes.iter_mut().zip(weights) {
            pane.weight = *weight;
        }
    }
}

/// The neighbor of `index` in `direction`, or nothing at the end of the run.
fn index_after(index: usize, direction: isize, count: usize) -> Option<usize> {
    let next = index as isize + direction;
    (next >= 0 && (next as usize) < count).then_some(next as usize)
}

/// One resize step across a divider beside tile `index`: which divider moved
/// and the two weights either side of it afterwards. Nothing when the tile that
/// would give up the space is already at [`MIN_SHARE`], or when there is no
/// divider to move at all.
fn shifted(weights: &[f64], index: usize, direction: isize) -> Option<(usize, f64, f64)> {
    if weights.len() < 2 {
        return None;
    }
    let divider = if direction > 0 {
        if index < weights.len() - 1 { index } else { index - 1 }
    } else if index > 0 {
        index - 1
    } else {
        index
    };

    let total: f64 = weights.iter().sum();
    if total <= 0.0 {
        return None;
    }
    let donor = if direction > 0 {
        weights[divider + 1]
    } else {
        weights[divider]
    };
    let step = (total * RESIZE_STEP).min(donor - total * MIN_SHARE);
    if step <= 0.0 {
        return None;
    }
    let delta = step * direction as f64;
    Some((divider, weights[divider] + delta, weights[divider + 1] - delta))
}

/// Hands `available` out in proportion to `weights` — the one piece of layout
/// arithmetic, kept here so the model's tests cover it rather than the widget's.
#[must_use]
pub fn shares(weights: &[f64], available: f64) -> Vec<f64> {
    let total: f64 = weights.iter().sum();
    if weights.is_empty() {
        return Vec::new();
    }
    if total <= 0.0 {
        return vec![available / weights.len() as f64; weights.len()];
    }
    weights.iter().map(|weight| weight / total * available).collect()
}

/// Splits a drag across the divider between tiles `index` and `index + 1` into
/// their new weights, holding both at or above [`MIN_SHARE`]. `travel` is how
/// far the pointer has moved since the drag began, in the same units as
/// `available`; `weights` are the ones it began against.
#[must_use]
pub fn dragged(weights: &[f64], index: usize, travel: f64, available: f64) -> (f64, f64) {
    let total: f64 = weights.iter().sum();
    let floor = total * MIN_SHARE;
    let delta = if available > 0.0 {
        travel / available * total
    } else {
        0.0
    };
    let mut first = weights[index] + delta;
    let mut second = weights[index + 1] - delta;
    if first < floor {
        second -= floor - first;
        first = floor;
    }
    if second < floor {
        first -= floor - second;
        second = floor;
    }
    (first, second)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A layout whose panes are laid out as `shape` describes: one entry per
    /// column, each the number of panes stacked in it. Focus is left on the
    /// first pane.
    fn layout(shape: &[usize]) -> (Layout, Vec<Vec<Id>>) {
        let mut layout = Layout::new(Pane::new());
        let mut ids = vec![vec![layout.focused()]];
        for _ in 1..shape[0] {
            let pane = Pane::new();
            ids[0].push(pane.id());
            layout.split(pane, Edge::Down);
        }
        for count in &shape[1..] {
            layout.focus(*ids.last().unwrap().last().unwrap());
            let pane = Pane::new();
            let mut column = vec![pane.id()];
            layout.split(pane, Edge::Right);
            for _ in 1..*count {
                let pane = Pane::new();
                column.push(pane.id());
                layout.split(pane, Edge::Down);
            }
            ids.push(column);
        }
        layout.focus(ids[0][0]);
        (layout, ids)
    }

    fn shape(layout: &Layout) -> Vec<usize> {
        layout.columns().iter().map(|column| column.panes().len()).collect()
    }

    #[test]
    fn a_fresh_tab_is_one_pane_and_is_not_a_split() {
        let layout = Layout::new(Pane::new());
        assert_eq!(layout.count(), 1);
        assert!(!layout.is_split());
        assert_eq!(shape(&layout), vec![1]);
    }

    #[test]
    fn splitting_sideways_opens_a_column_and_splitting_down_stacks_in_one() {
        let (layout, ids) = layout(&[2, 1]);
        assert_eq!(shape(&layout), vec![2, 1]);
        assert_eq!(layout.location(ids[0][1]), Some((0, 1)));
        assert_eq!(layout.location(ids[1][0]), Some((1, 0)));
    }

    #[test]
    fn a_split_pane_gives_up_half_of_what_it_had() {
        let mut layout = Layout::new(Pane::new());
        layout.split(Pane::new(), Edge::Right);
        let weights: Vec<f64> = layout.columns().iter().map(|column| column.weight).collect();
        assert_eq!(weights, vec![0.5, 0.5]);

        // And the new column's own half splits again rather than the tab's.
        layout.split(Pane::new(), Edge::Right);
        let weights: Vec<f64> = layout.columns().iter().map(|column| column.weight).collect();
        assert_eq!(weights, vec![0.5, 0.25, 0.25]);
    }

    #[test]
    fn a_new_pane_takes_the_keyboard() {
        let mut layout = Layout::new(Pane::new());
        let pane = Pane::new();
        let id = pane.id();
        layout.split(pane, Edge::Down);
        assert_eq!(layout.focused(), id);
    }

    #[test]
    fn focus_moves_by_direction_and_stops_at_the_edge() {
        let (mut layout, ids) = layout(&[2, 2]);
        layout.focus(ids[0][0]);

        layout.focus_toward(Edge::Down);
        assert_eq!(layout.focused(), ids[0][1]);
        layout.focus_toward(Edge::Down);
        assert_eq!(layout.focused(), ids[0][1], "the bottom of a column is the bottom");

        layout.focus_toward(Edge::Right);
        assert_eq!(layout.focused(), ids[1][1], "and it lands at the height it left");
        layout.focus_toward(Edge::Right);
        assert_eq!(layout.focused(), ids[1][1]);

        layout.focus_toward(Edge::Left);
        assert_eq!(layout.focused(), ids[0][1]);
    }

    #[test]
    fn focus_lands_on_a_shorter_column_without_falling_off_it() {
        let (mut layout, ids) = layout(&[3, 1]);
        layout.focus(ids[0][2]);
        layout.focus_toward(Edge::Right);
        assert_eq!(layout.focused(), ids[1][0]);
    }

    #[test]
    fn cycling_walks_every_pane_in_order_and_wraps() {
        let (mut layout, ids) = layout(&[2, 1]);
        let order = [ids[0][0], ids[0][1], ids[1][0]];
        layout.focus(order[0]);

        for expected in [order[1], order[2], order[0]] {
            layout.focus_next();
            assert_eq!(layout.focused(), expected);
        }
        layout.focus_previous();
        assert_eq!(layout.focused(), order[2]);
    }

    #[test]
    fn closing_a_pane_empties_its_column_and_focus_falls_to_a_neighbor() {
        let (mut layout, ids) = layout(&[1, 2]);
        layout.focus(ids[1][0]);

        assert!(layout.remove(ids[1][0]));
        assert_eq!(shape(&layout), vec![1, 1]);
        assert_eq!(layout.focused(), ids[1][1]);

        assert!(layout.remove(ids[1][1]));
        assert_eq!(shape(&layout), vec![1]);
        assert_eq!(layout.focused(), ids[0][0]);

        assert!(!layout.remove(ids[0][0]), "the last pane closing closes the tab");
    }

    #[test]
    fn closing_a_pane_nobody_is_looking_at_leaves_focus_alone() {
        let (mut layout, ids) = layout(&[2, 1]);
        layout.focus(ids[1][0]);
        layout.remove(ids[0][1]);
        assert_eq!(layout.focused(), ids[1][0]);
    }

    #[test]
    fn a_dragged_pane_lands_against_the_edge_it_was_dropped_on() {
        let (mut layout, ids) = layout(&[2, 1]);

        // Bottom of the first column, dropped on the right of the second.
        layout.move_pane(ids[0][1], Edge::Right, ids[1][0]);
        assert_eq!(shape(&layout), vec![1, 1, 1]);
        assert_eq!(layout.location(ids[0][1]), Some((2, 0)));
        assert_eq!(layout.focused(), ids[0][1], "focus follows the pane that moved");

        // And back, stacked above where it came from.
        layout.move_pane(ids[0][1], Edge::Up, ids[0][0]);
        assert_eq!(shape(&layout), vec![2, 1]);
        assert_eq!(layout.location(ids[0][1]), Some((0, 0)));
    }

    #[test]
    fn dragging_a_pane_onto_itself_changes_nothing() {
        let (mut layout, ids) = layout(&[2]);
        layout.move_pane(ids[0][0], Edge::Down, ids[0][0]);
        assert_eq!(shape(&layout), vec![2]);
        assert_eq!(layout.location(ids[0][0]), Some((0, 0)));
    }

    #[test]
    fn dragging_the_last_pane_out_of_a_column_closes_it_before_the_drop_lands() {
        let (mut layout, ids) = layout(&[1, 1, 1]);
        // The middle column empties as the pane leaves, so the target column is
        // no longer where it was when the drag began.
        layout.move_pane(ids[1][0], Edge::Right, ids[2][0]);
        assert_eq!(shape(&layout), vec![1, 1, 1]);
        assert_eq!(layout.location(ids[0][0]), Some((0, 0)));
        assert_eq!(layout.location(ids[2][0]), Some((1, 0)));
        assert_eq!(layout.location(ids[1][0]), Some((2, 0)));
    }

    #[test]
    fn zoom_needs_a_split_to_hide() {
        let (mut layout, _) = layout(&[1]);
        layout.toggle_zoom();
        assert!(!layout.is_zoomed(), "one pane is already as big as it gets");
    }

    #[test]
    fn any_move_lets_go_of_zoom() {
        let (mut layout, ids) = layout(&[2]);
        layout.focus(ids[0][0]);

        layout.toggle_zoom();
        assert!(layout.is_zoomed());
        layout.toggle_zoom();
        assert!(!layout.is_zoomed());

        // Navigation would otherwise focus a pane the window is not showing.
        layout.toggle_zoom();
        layout.focus_toward(Edge::Down);
        assert!(!layout.is_zoomed());

        layout.toggle_zoom();
        assert!(layout.is_zoomed());
        layout.remove(layout.focused());
        assert!(!layout.is_zoomed(), "the split it was hiding is gone");
    }

    #[test]
    fn a_resize_step_moves_the_divider_on_the_side_it_is_pushed() {
        let (mut layout, ids) = layout(&[1, 1]);
        layout.focus(ids[0][0]);

        layout.resize(Edge::Right);
        let weights: Vec<f64> = layout.columns().iter().map(|column| column.weight).collect();
        assert!(weights[0] > weights[1], "pushing right grows the left column");

        layout.resize(Edge::Left);
        let weights: Vec<f64> = layout.columns().iter().map(|column| column.weight).collect();
        assert!((weights[0] - weights[1]).abs() < 1e-9, "and pushing back undoes it");
    }

    #[test]
    fn against_the_outer_edge_the_other_divider_moves_instead() {
        let (mut layout, ids) = layout(&[1, 1]);
        layout.focus(ids[1][0]);

        // There is no divider to the right of the last column, so the one on
        // its left moves and the column shrinks.
        layout.resize(Edge::Right);
        let weights: Vec<f64> = layout.columns().iter().map(|column| column.weight).collect();
        assert!(weights[1] < weights[0]);
    }

    #[test]
    fn a_tile_is_never_squeezed_below_the_floor() {
        let (mut layout, ids) = layout(&[1, 1]);
        layout.focus(ids[0][0]);
        for _ in 0..100 {
            layout.resize(Edge::Right);
        }
        let weights: Vec<f64> = layout.columns().iter().map(|column| column.weight).collect();
        let total: f64 = weights.iter().sum();
        assert!(weights[1] / total >= MIN_SHARE - 1e-9);
    }

    #[test]
    fn equalizing_hands_every_tile_the_same_share() {
        let (mut layout, _) = layout(&[2, 1]);
        layout.equalize();
        for column in layout.columns() {
            assert_eq!(column.weight, 1.0);
            for pane in column.panes() {
                assert_eq!(pane.weight, 1.0);
            }
        }
    }

    #[test]
    fn space_is_handed_out_in_proportion_to_weight() {
        assert_eq!(shares(&[1.0, 1.0], 100.0), vec![50.0, 50.0]);
        assert_eq!(shares(&[3.0, 1.0], 100.0), vec![75.0, 25.0]);
        // Weights that were all zeroed still divide the space rather than
        // collapsing every tile to nothing.
        assert_eq!(shares(&[0.0, 0.0], 100.0), vec![50.0, 50.0]);
        assert!(shares(&[], 100.0).is_empty());
    }

    #[test]
    fn a_divider_drag_stops_at_the_floor_rather_than_pushing_past_it() {
        let weights = [1.0, 1.0];
        let (first, second) = dragged(&weights, 0, 25.0, 100.0);
        assert!((first - 1.5).abs() < 1e-9);
        assert!((second - 0.5).abs() < 1e-9);

        // All the way across, and the tile being squeezed keeps its floor.
        let (first, second) = dragged(&weights, 0, 500.0, 100.0);
        assert!((second - 2.0 * MIN_SHARE).abs() < 1e-9);
        assert!((first + second - 2.0).abs() < 1e-9);
    }

    #[test]
    fn weights_that_do_not_match_the_layout_are_dropped_rather_than_applied() {
        let (mut layout, _) = layout(&[1, 1]);
        layout.set_column_weights(&[9.0]);
        let weights: Vec<f64> = layout.columns().iter().map(|column| column.weight).collect();
        assert_eq!(weights, vec![0.5, 0.5]);

        layout.set_column_weights(&[3.0, 1.0]);
        let weights: Vec<f64> = layout.columns().iter().map(|column| column.weight).collect();
        assert_eq!(weights, vec![3.0, 1.0]);
    }
}
