//! Box drawing and block characters, drawn rather than typeset.
//!
//! A font's `█` is a glyph like any other: as tall as the designer drew it, and
//! placed in a cell as tall as the line height says. The two rarely agree, so a
//! column of full blocks comes out striped, and a frame drawn with `│` and `─`
//! has holes where its corners should meet. Every terminal that cares answers
//! this the same way, which Ghostty calls its sprite font: draw U+2500 to
//! U+259F from the cell's own measurements instead, so a line ends exactly on
//! the edge the next cell starts at. The geometry here is Ghostty's, ported.

use gtk::gdk;
use gtk::graphene;
use gtk::gsk;
use gtk::prelude::*;

/// The cell one of these is drawn into.
#[derive(Clone, Copy)]
pub struct Cell {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// How thick a light line is, in pixels. The font's underline thickness,
    /// which is where Ghostty takes it from too: it is the one measurement a
    /// face states about the lines drawn across it.
    pub thickness: f32,
}

/// The character this text is drawn as, when it is one of these at all.
pub fn glyph(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let glyph = chars
        .next()
        .filter(|c| matches!(c, '\u{2500}'..='\u{259f}'))?;
    chars.next().is_none().then_some(glyph)
}

pub fn draw(snapshot: &gtk::Snapshot, glyph: char, cell: Cell, color: gdk::RGBA) {
    let pen = Pen {
        snapshot,
        width: cell.width.round() as i32,
        height: cell.height.round() as i32,
        light: (cell.thickness.round() as i32).max(1),
        color,
    };

    // Everything below is in cell coordinates, which keeps the arithmetic to
    // whole pixels off a corner rather than off the widget.
    snapshot.save();
    snapshot.translate(&graphene::Point::new(cell.x, cell.y));
    let codepoint = glyph as u32;
    if codepoint < 0x2580 {
        pen.line_glyph(codepoint);
    } else {
        pen.block(codepoint);
    }
    snapshot.restore();
}

struct Pen<'a> {
    snapshot: &'a gtk::Snapshot,
    width: i32,
    height: i32,
    /// The thickness of a light line. A heavy one is twice it, and a double one
    /// is two light lines with a light line's worth of gap between them.
    light: i32,
    color: gdk::RGBA,
}

/// An arm of an intersection character: none, light, heavy, double.
const NONE: u8 = b'.';
const LIGHT: u8 = b'L';
const HEAVY: u8 = b'H';
const DOUBLE: u8 = b'D';

impl Pen<'_> {
    /// A rectangle in cell coordinates, right and bottom edges exclusive.
    fn fill(&self, x0: i32, y0: i32, x1: i32, y1: i32) {
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        self.snapshot.append_color(
            &self.color,
            &graphene::Rect::new(x0 as f32, y0 as f32, (x1 - x0) as f32, (y1 - y0) as f32),
        );
    }

    /// The whole cell at part strength, which is all the three shade characters
    /// are. A stipple pattern is what a bitmap font did it with; against a
    /// background of any color the wash is the same picture and stays one at
    /// every size.
    fn wash(&self, alpha: f32) {
        let mut color = self.color;
        color.set_alpha(alpha);
        self.snapshot.append_color(
            &color,
            &graphene::Rect::new(0.0, 0.0, self.width as f32, self.height as f32),
        );
    }

    fn stroke(&self, path: &gsk::PathBuilder) {
        let stroke = gsk::Stroke::new(self.light as f32);
        self.snapshot
            .append_stroke(&path.to_path(), &stroke, &self.color);
    }

    // --- blocks --------------------------------------------------------------

    fn block(&self, codepoint: u32) {
        let (w, h) = (self.width, self.height);
        // A fraction of a cell, rounded, and measured from whichever edge the
        // block is anchored to. Half of an odd number of pixels rounds up from
        // both sides that way, so an upper half and a lower half overlap by a
        // pixel instead of leaving a line of background between them.
        let part = |size: i32, fraction: f32| (size as f32 * fraction).round() as i32;
        let upper = |fraction| self.fill(0, 0, w, part(h, fraction));
        let lower = |fraction| self.fill(0, h - part(h, fraction), w, h);
        let left = |fraction| self.fill(0, 0, part(w, fraction), h);
        let right = |fraction| self.fill(w - part(w, fraction), 0, w, h);

        match codepoint {
            0x2580 => upper(0.5),              // ▀
            0x2581 => lower(0.125),            // ▁
            0x2582 => lower(0.25),             // ▂
            0x2583 => lower(0.375),            // ▃
            0x2584 => lower(0.5),              // ▄
            0x2585 => lower(0.625),            // ▅
            0x2586 => lower(0.75),             // ▆
            0x2587 => lower(0.875),            // ▇
            0x2588 => self.fill(0, 0, w, h),   // █
            0x2589 => left(0.875),             // ▉
            0x258a => left(0.75),              // ▊
            0x258b => left(0.625),             // ▋
            0x258c => left(0.5),               // ▌
            0x258d => left(0.375),             // ▍
            0x258e => left(0.25),              // ▎
            0x258f => left(0.125),             // ▏
            0x2590 => right(0.5),              // ▐
            0x2591 => self.wash(0.25),         // ░
            0x2592 => self.wash(0.5),          // ▒
            0x2593 => self.wash(0.75),         // ▓
            0x2594 => upper(0.125),            // ▔
            0x2595 => right(0.125),            // ▕
            0x2596 => self.quadrants(b"..X."), // ▖
            0x2597 => self.quadrants(b"...X"), // ▗
            0x2598 => self.quadrants(b"X..."), // ▘
            0x2599 => self.quadrants(b"X.XX"), // ▙
            0x259a => self.quadrants(b"X..X"), // ▚
            0x259b => self.quadrants(b"XXX."), // ▛
            0x259c => self.quadrants(b"XX.X"), // ▜
            0x259d => self.quadrants(b".X.."), // ▝
            0x259e => self.quadrants(b".XX."), // ▞
            0x259f => self.quadrants(b".XXX"), // ▟
            _ => {}
        }
    }

    /// The quarters of a cell that are filled, clockwise from the top left.
    fn quadrants(&self, quads: &[u8; 4]) {
        let (w, h) = (self.width, self.height);
        let (mid_x, mid_y) = (
            (w as f32 / 2.0).round() as i32,
            (h as f32 / 2.0).round() as i32,
        );
        let (from_x, from_y) = (w - mid_x, h - mid_y);
        if quads[0] == b'X' {
            self.fill(0, 0, mid_x, mid_y);
        }
        if quads[1] == b'X' {
            self.fill(from_x, 0, w, mid_y);
        }
        if quads[2] == b'X' {
            self.fill(0, from_y, mid_x, h);
        }
        if quads[3] == b'X' {
            self.fill(from_x, from_y, w, h);
        }
    }

    // --- lines ---------------------------------------------------------------

    fn line_glyph(&self, codepoint: u32) {
        if let Some((count, vertical, heavy)) = dash(codepoint) {
            self.dashes(count, vertical, heavy);
            return;
        }

        match codepoint {
            // The rounded corners, named for where their two arms point.
            0x256d => self.arc(true, true),
            0x256e => self.arc(true, false),
            0x256f => self.arc(false, false),
            0x2570 => self.arc(false, true),
            0x2571..=0x2573 => self.diagonals(codepoint),
            _ => {
                if let Some(arms) = arms(codepoint) {
                    self.lines(arms);
                }
            }
        }
    }

    /// One intersection character: an arm from each edge to the middle, in
    /// whichever weight the character calls for.
    fn lines(&self, arms: &[u8; 4]) {
        let [up, right, down, left] = *arms;
        let (w, h) = (self.width, self.height);
        let (light, heavy) = (self.light, self.light * 2);

        let h_light_top = (h - light).max(0) / 2;
        let h_light_bottom = h_light_top + light;
        let h_heavy_top = (h - heavy).max(0) / 2;
        let h_heavy_bottom = h_heavy_top + heavy;
        let h_double_top = (h_light_top - light).max(0);
        let h_double_bottom = h_light_bottom + light;

        let v_light_left = (w - light).max(0) / 2;
        let v_light_right = v_light_left + light;
        let v_heavy_left = (w - heavy).max(0) / 2;
        let v_heavy_right = v_heavy_left + heavy;
        let v_double_left = (v_light_left - light).max(0);
        let v_double_right = v_light_right + light;

        // Where an arm stops once it reaches the arms crossing it: past their
        // middle, so a corner closes; short of it, so an arm crossing a heavier
        // line does not stick out the other side of it. `across` is the pair of
        // arms it runs into, `alike` whether it and the arm opposite it match.
        let stop = |across: (u8, u8), alike: bool, on_heavy, on_double, far, near| {
            let (a, b) = across;
            if a == HEAVY || b == HEAVY {
                on_heavy
            } else if a != b || alike {
                if a == DOUBLE || b == DOUBLE {
                    on_double
                } else {
                    far
                }
            } else if a == NONE && b == NONE {
                far
            } else {
                near
            }
        };
        let up_bottom = stop(
            (left, right),
            up == down,
            h_heavy_bottom,
            h_double_bottom,
            h_light_bottom,
            h_light_top,
        );
        let down_top = stop(
            (left, right),
            up == down,
            h_heavy_top,
            h_double_top,
            h_light_top,
            h_light_bottom,
        );
        let left_right = stop(
            (up, down),
            left == right,
            v_heavy_right,
            v_double_right,
            v_light_right,
            v_light_left,
        );
        let right_left = stop(
            (up, down),
            left == right,
            v_heavy_left,
            v_double_left,
            v_light_left,
            v_light_right,
        );

        match up {
            LIGHT => self.fill(v_light_left, 0, v_light_right, up_bottom),
            HEAVY => self.fill(v_heavy_left, 0, v_heavy_right, up_bottom),
            DOUBLE => {
                // A double arm meeting another double arm stops at the near side
                // of the crossing pair, which is what leaves the little square of
                // background in the middle of `╬`.
                let outer = if left == DOUBLE {
                    h_light_top
                } else {
                    up_bottom
                };
                let inner = if right == DOUBLE {
                    h_light_top
                } else {
                    up_bottom
                };
                self.fill(v_double_left, 0, v_light_left, outer);
                self.fill(v_light_right, 0, v_double_right, inner);
            }
            _ => {}
        }

        match right {
            LIGHT => self.fill(right_left, h_light_top, w, h_light_bottom),
            HEAVY => self.fill(right_left, h_heavy_top, w, h_heavy_bottom),
            DOUBLE => {
                let upper = if up == DOUBLE {
                    v_light_right
                } else {
                    right_left
                };
                let lower = if down == DOUBLE {
                    v_light_right
                } else {
                    right_left
                };
                self.fill(upper, h_double_top, w, h_light_top);
                self.fill(lower, h_light_bottom, w, h_double_bottom);
            }
            _ => {}
        }

        match down {
            LIGHT => self.fill(v_light_left, down_top, v_light_right, h),
            HEAVY => self.fill(v_heavy_left, down_top, v_heavy_right, h),
            DOUBLE => {
                let outer = if left == DOUBLE {
                    h_light_bottom
                } else {
                    down_top
                };
                let inner = if right == DOUBLE {
                    h_light_bottom
                } else {
                    down_top
                };
                self.fill(v_double_left, outer, v_light_left, h);
                self.fill(v_light_right, inner, v_double_right, h);
            }
            _ => {}
        }

        match left {
            LIGHT => self.fill(0, h_light_top, left_right, h_light_bottom),
            HEAVY => self.fill(0, h_heavy_top, left_right, h_heavy_bottom),
            DOUBLE => {
                let upper = if up == DOUBLE {
                    v_light_left
                } else {
                    left_right
                };
                let lower = if down == DOUBLE {
                    v_light_left
                } else {
                    left_right
                };
                self.fill(0, h_double_top, upper, h_light_top);
                self.fill(0, h_light_bottom, lower, h_double_bottom);
            }
            _ => {}
        }
    }

    /// A dashed line down the middle of the cell.
    ///
    /// The gaps are laid out so that a run of these tiles into one evenly broken
    /// line: half a gap at each end across, and a whole gap left at the bottom
    /// down, where a half gap on both sides would read as a flaw rather than as
    /// a pattern.
    fn dashes(&self, count: i32, vertical: bool, heavy: bool) {
        let thick = if heavy { self.light * 2 } else { self.light };
        let (span, across) = if vertical {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        };

        // Under a pixel of dash or of gap there is no pattern left to draw, so
        // the character falls back to the solid line it is a variant of.
        if span < count * 2 {
            self.lines(if vertical { b"L.L." } else { b".L.L" });
            return;
        }

        let gap = self.light.max(4).min(span / (2 * count));
        let dashes = span - gap * count;
        let (dash, mut over) = (dashes / count, dashes % count);
        let edge = (across - thick).max(0) / 2;

        let mut at = if vertical { 0 } else { gap / 2 };
        for _ in 0..count {
            let mut end = at + dash;
            // Whatever did not divide evenly goes into the dashes rather than
            // the gaps, where a pixel more or less is the harder to notice.
            if over > 0 {
                over -= 1;
                end += 1;
            }
            if vertical {
                self.fill(edge, at, edge + thick, end);
            } else {
                self.fill(at, edge, end, edge + thick);
            }
            at = end + gap;
        }
    }

    /// A quarter turn between two arms, drawn as a curve rather than a corner.
    fn arc(&self, down: bool, right: bool) {
        let (w, h) = (self.width as f32, self.height as f32);
        let thick = self.light as f32;
        let center_x = ((self.width - self.light).max(0) / 2) as f32 + thick / 2.0;
        let center_y = ((self.height - self.light).max(0) / 2) as f32 + thick / 2.0;
        let radius = w.min(h) / 2.0;
        let (sx, sy) = (
            if right { 1.0 } else { -1.0 },
            if down { 1.0 } else { -1.0 },
        );
        let (end_x, end_y) = (if right { w } else { 0.0 }, if down { h } else { 0.0 });

        // The control points sit a quarter of the radius from the center, which
        // is what Ghostty settled on: a true quarter circle bulges more than a
        // corner should at the sizes a cell comes in.
        let pull = 0.25 * radius;
        let path = gsk::PathBuilder::new();
        path.move_to(center_x, end_y);
        path.line_to(center_x, center_y + sy * radius);
        path.cubic_to(
            center_x,
            center_y + sy * pull,
            center_x + sx * pull,
            center_y,
            center_x + sx * radius,
            center_y,
        );
        path.line_to(end_x, center_y);
        self.stroke(&path);
    }

    /// The two corner-to-corner strokes, one of them or both.
    fn diagonals(&self, codepoint: u32) {
        let (w, h) = (self.width as f32, self.height as f32);
        // Overshooting the corners keeps the slope right where the stroke is cut
        // off by the cell edge, so a diagonal joins the one in the next cell.
        let (over_x, over_y) = ((w / h).min(1.0) / 2.0, (h / w).min(1.0) / 2.0);
        let path = gsk::PathBuilder::new();
        if codepoint != 0x2571 {
            path.move_to(-over_x, -over_y);
            path.line_to(w + over_x, h + over_y);
        }
        if codepoint != 0x2572 {
            path.move_to(w + over_x, -over_y);
            path.line_to(-over_x, h + over_y);
        }
        self.stroke(&path);
    }
}

/// How many dashes a dashed line is broken into, whether it runs down the cell
/// rather than across it, and whether its strokes are heavy.
///
/// Read off the codepoint rather than tabulated: the block at U+2504 runs three
/// dashes then four, alternating down and across in pairs of light and heavy,
/// and the two-dash pair sits apart at U+254C in the same order.
fn dash(codepoint: u32) -> Option<(i32, bool, bool)> {
    let count = match codepoint {
        0x2504..=0x250b => (codepoint - 0x2504) / 4 + 3,
        0x254c..=0x254f => 2,
        _ => return None,
    };
    Some((count as i32, (codepoint / 2) % 2 == 1, codepoint % 2 == 1))
}

/// The four arms of an intersection character, in the order up, right, down,
/// left. Ghostty's table, and written out for the same reason: the block looks
/// regular and is not, so the exceptions cost more to derive than to state.
fn arms(codepoint: u32) -> Option<&'static [u8; 4]> {
    Some(match codepoint {
        0x2500 => b".L.L", // ─
        0x2501 => b".H.H", // ━
        0x2502 => b"L.L.", // │
        0x2503 => b"H.H.", // ┃
        0x250c => b".LL.", // ┌
        0x250d => b".HL.", // ┍
        0x250e => b".LH.", // ┎
        0x250f => b".HH.", // ┏
        0x2510 => b"..LL", // ┐
        0x2511 => b"..LH", // ┑
        0x2512 => b"..HL", // ┒
        0x2513 => b"..HH", // ┓
        0x2514 => b"LL..", // └
        0x2515 => b"LH..", // ┕
        0x2516 => b"HL..", // ┖
        0x2517 => b"HH..", // ┗
        0x2518 => b"L..L", // ┘
        0x2519 => b"L..H", // ┙
        0x251a => b"H..L", // ┚
        0x251b => b"H..H", // ┛
        0x251c => b"LLL.", // ├
        0x251d => b"LHL.", // ┝
        0x251e => b"HLL.", // ┞
        0x251f => b"LLH.", // ┟
        0x2520 => b"HLH.", // ┠
        0x2521 => b"HHL.", // ┡
        0x2522 => b"LHH.", // ┢
        0x2523 => b"HHH.", // ┣
        0x2524 => b"L.LL", // ┤
        0x2525 => b"L.LH", // ┥
        0x2526 => b"H.LL", // ┦
        0x2527 => b"L.HL", // ┧
        0x2528 => b"H.HL", // ┨
        0x2529 => b"H.LH", // ┩
        0x252a => b"L.HH", // ┪
        0x252b => b"H.HH", // ┫
        0x252c => b".LLL", // ┬
        0x252d => b".LLH", // ┭
        0x252e => b".HLL", // ┮
        0x252f => b".HLH", // ┯
        0x2530 => b".LHL", // ┰
        0x2531 => b".LHH", // ┱
        0x2532 => b".HHL", // ┲
        0x2533 => b".HHH", // ┳
        0x2534 => b"LL.L", // ┴
        0x2535 => b"LL.H", // ┵
        0x2536 => b"LH.L", // ┶
        0x2537 => b"LH.H", // ┷
        0x2538 => b"HL.L", // ┸
        0x2539 => b"HL.H", // ┹
        0x253a => b"HH.L", // ┺
        0x253b => b"HH.H", // ┻
        0x253c => b"LLLL", // ┼
        0x253d => b"LLLH", // ┽
        0x253e => b"LHLL", // ┾
        0x253f => b"LHLH", // ┿
        0x2540 => b"HLLL", // ╀
        0x2541 => b"LLHL", // ╁
        0x2542 => b"HLHL", // ╂
        0x2543 => b"HLLH", // ╃
        0x2544 => b"HHLL", // ╄
        0x2545 => b"LLHH", // ╅
        0x2546 => b"LHHL", // ╆
        0x2547 => b"HHLH", // ╇
        0x2548 => b"LHHH", // ╈
        0x2549 => b"HLHH", // ╉
        0x254a => b"HHHL", // ╊
        0x254b => b"HHHH", // ╋
        0x2550 => b".D.D", // ═
        0x2551 => b"D.D.", // ║
        0x2552 => b".DL.", // ╒
        0x2553 => b".LD.", // ╓
        0x2554 => b".DD.", // ╔
        0x2555 => b"..LD", // ╕
        0x2556 => b"..DL", // ╖
        0x2557 => b"..DD", // ╗
        0x2558 => b"LD..", // ╘
        0x2559 => b"DL..", // ╙
        0x255a => b"DD..", // ╚
        0x255b => b"L..D", // ╛
        0x255c => b"D..L", // ╜
        0x255d => b"D..D", // ╝
        0x255e => b"LDL.", // ╞
        0x255f => b"DLD.", // ╟
        0x2560 => b"DDD.", // ╠
        0x2561 => b"L.LD", // ╡
        0x2562 => b"D.DL", // ╢
        0x2563 => b"D.DD", // ╣
        0x2564 => b".DLD", // ╤
        0x2565 => b".LDL", // ╥
        0x2566 => b".DDD", // ╦
        0x2567 => b"LD.D", // ╧
        0x2568 => b"DL.L", // ╨
        0x2569 => b"DD.D", // ╩
        0x256a => b"LDLD", // ╪
        0x256b => b"DLDL", // ╫
        0x256c => b"DDDD", // ╬
        0x2574 => b"...L", // ╴
        0x2575 => b"L...", // ╵
        0x2576 => b".L..", // ╶
        0x2577 => b"..L.", // ╷
        0x2578 => b"...H", // ╸
        0x2579 => b"H...", // ╹
        0x257a => b".H..", // ╺
        0x257b => b"..H.", // ╻
        0x257c => b".H.L", // ╼
        0x257d => b"L.H.", // ╽
        0x257e => b".L.H", // ╾
        0x257f => b"H.L.", // ╿
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_lone_box_or_block_character_is_drawn_here() {
        assert_eq!(glyph("█"), Some('█'));
        assert_eq!(glyph("┼"), Some('┼'));
        assert_eq!(glyph("a"), None);
        assert_eq!(glyph("╱╲"), None);
        // Braille and the legacy computing blocks are the font's, for now.
        assert_eq!(glyph("⣿"), None);
        assert_eq!(glyph("🬀"), None);
    }

    #[test]
    fn every_box_drawing_character_is_accounted_for() {
        for codepoint in 0x2500..=0x257f {
            let drawn = arms(codepoint).is_some()
                || dash(codepoint).is_some()
                || matches!(codepoint, 0x256d..=0x2573);
            assert!(drawn, "U+{codepoint:04X} would be left to the font");
        }
    }

    #[test]
    fn a_dashed_line_is_read_off_the_codepoint() {
        assert_eq!(dash(0x2504), Some((3, false, false))); // ┄
        assert_eq!(dash(0x2507), Some((3, true, true))); // ┇
        assert_eq!(dash(0x2508), Some((4, false, false))); // ┈
        assert_eq!(dash(0x250b), Some((4, true, true))); // ┋
        assert_eq!(dash(0x254c), Some((2, false, false))); // ╌
        assert_eq!(dash(0x254f), Some((2, true, true))); // ╏
        assert_eq!(dash(0x2500), None);
    }
}
