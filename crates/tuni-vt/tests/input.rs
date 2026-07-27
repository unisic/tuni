//! The input behaviors this crate promises the widget, each of them a piece
//! of Ghostty the C API does not hand over directly: XTSHIFTESCAPE, the
//! alternate screen's wheel-as-arrows, what a right click selects, and the
//! shapes of the click gesture.

use std::time::Duration;

use tuni_vt::{Geometry, Terminal};

const CELL_W: u32 = 8;
const CELL_H: u32 = 16;

fn geometry(cols: u16, rows: u16) -> Geometry {
    Geometry {
        cols,
        rows,
        cell_width_px: CELL_W,
        cell_height_px: CELL_H,
        screen_width_px: u32::from(cols) * CELL_W,
        screen_height_px: u32::from(rows) * CELL_H,
    }
}

/// Center of a cell in surface pixels, which is what the gesture expects.
fn at(col: u16, row: u16) -> (f64, f64) {
    (
        f64::from(col) * f64::from(CELL_W) + f64::from(CELL_W) / 2.0,
        f64::from(row) * f64::from(CELL_H) + f64::from(CELL_H) / 2.0,
    )
}

#[test]
fn an_application_asks_for_shift_with_xtshiftescape() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    assert!(!terminal.captures_shift(), "shift is the user's by default");

    terminal.feed(b"\x1b[>1s");
    assert!(terminal.captures_shift());

    terminal.feed(b"\x1b[>0s");
    assert!(!terminal.captures_shift());

    // A parameterless ask means the same as zero.
    terminal.feed(b"\x1b[>1s");
    terminal.feed(b"\x1b[>s");
    assert!(!terminal.captures_shift());
}

#[test]
fn a_full_reset_takes_the_shift_request_back() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"\x1b[>1s");
    assert!(terminal.captures_shift());

    terminal.feed(b"\x1bc");
    assert!(!terminal.captures_shift());
}

#[test]
fn a_sequence_split_across_feeds_still_lands() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"\x1b[>");
    terminal.feed(b"1s");
    assert!(terminal.captures_shift());
}

#[test]
fn a_passthrough_wrapped_request_is_not_ours_to_hear() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    // A DCS body is some other terminal's input, tmux passthrough being the
    // usual carrier.
    terminal.feed(b"\x1bP\x1b[>1s\x1b\\");
    assert!(!terminal.captures_shift());
}

#[test]
fn the_wheel_becomes_arrows_on_the_alternate_screen() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    assert_eq!(
        terminal.encode_alternate_scroll(1).expect("encode"),
        b"",
        "the primary screen scrolls a viewport instead"
    );

    terminal.feed(b"\x1b[?1049h");
    assert_eq!(
        terminal.encode_alternate_scroll(1).expect("encode"),
        b"\x1b[B"
    );
    assert_eq!(
        terminal.encode_alternate_scroll(-2).expect("encode"),
        b"\x1b[A\x1b[A"
    );
    assert_eq!(terminal.encode_alternate_scroll(0).expect("encode"), b"");
}

#[test]
fn alternate_scroll_honors_cursor_key_application_mode() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"\x1b[?1049h\x1b[?1h");
    assert_eq!(
        terminal.encode_alternate_scroll(1).expect("encode"),
        b"\x1bOB"
    );
}

#[test]
fn a_tracking_application_hears_the_wheel_as_buttons_not_arrows() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"\x1b[?1049h\x1b[?1000h");
    assert_eq!(terminal.encode_alternate_scroll(1).expect("encode"), b"");

    terminal.feed(b"\x1b[?1000l");
    assert_eq!(
        terminal.encode_alternate_scroll(1).expect("encode"),
        b"\x1b[B"
    );
}

#[test]
fn an_application_can_refuse_alternate_scroll() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"\x1b[?1049h\x1b[?1007l");
    assert_eq!(terminal.encode_alternate_scroll(1).expect("encode"), b"");
}

#[test]
fn a_right_click_selects_the_word_under_it() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"hello world");
    let geometry = geometry(20, 5);

    let (x, y) = at(1, 0);
    assert!(terminal.right_click_select(x, y, geometry).expect("select"));
    assert_eq!(
        terminal.selection_text().expect("text").as_deref(),
        Some("hello")
    );
}

#[test]
fn a_right_click_inside_the_selection_keeps_it() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"hello world");
    let geometry = geometry(20, 5);

    let (x, y) = at(1, 0);
    terminal
        .right_click_select(x, y, geometry)
        .expect("first click");
    let (x, y) = at(3, 0);
    assert!(
        !terminal.right_click_select(x, y, geometry).expect("second"),
        "a click inside the selection is a menu over it, not a new one"
    );
    assert_eq!(
        terminal.selection_text().expect("text").as_deref(),
        Some("hello")
    );

    let (x, y) = at(8, 0);
    assert!(terminal.right_click_select(x, y, geometry).expect("third"));
    assert_eq!(
        terminal.selection_text().expect("text").as_deref(),
        Some("world")
    );
}

#[test]
fn a_right_click_on_nothing_clears() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"hello");
    let geometry = geometry(20, 5);

    let (x, y) = at(1, 0);
    terminal.right_click_select(x, y, geometry).expect("word");
    assert!(terminal.has_selection());

    let (x, y) = at(4, 3);
    terminal.right_click_select(x, y, geometry).expect("blank");
    assert!(!terminal.has_selection());
}

#[test]
fn a_triple_click_selects_the_line() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"alpha beta");
    let geometry = geometry(20, 5);

    let (x, y) = at(7, 0);
    for time in [0u64, 100, 200] {
        terminal
            .select_press(x, y, geometry, Duration::from_millis(time), false)
            .expect("press");
        terminal.select_release().expect("release");
    }
    assert_eq!(terminal.click_count(), 3);
    assert_eq!(
        terminal.selection_text().expect("text").as_deref(),
        Some("alpha beta")
    );
}

#[test]
fn ctrl_turns_the_third_click_into_the_command_output() {
    let mut terminal = Terminal::new(20, 8, 100).expect("terminal");
    // A shell that marks its prompts with OSC 133: a prompt, a command, two
    // rows of output, the next prompt.
    terminal.feed(b"\x1b]133;A\x07$ make\r\n");
    terminal.feed(b"\x1b]133;C\x07one\r\ntwo\r\n");
    terminal.feed(b"\x1b]133;D;0\x07\x1b]133;A\x07$ ");
    let geometry = geometry(20, 8);

    let (x, y) = at(1, 1);
    for time in [0u64, 100, 200] {
        terminal
            .select_press(x, y, geometry, Duration::from_millis(time), true)
            .expect("press");
        terminal.select_release().expect("release");
    }
    let text = terminal
        .selection_text()
        .expect("text")
        .expect("a selection");
    assert!(
        text.contains("one") && text.contains("two") && !text.contains("make"),
        "the output, not the command: {text:?}"
    );
}

#[test]
fn a_wrapped_line_reads_back_as_one() {
    let mut terminal = Terminal::new(10, 5, 100).expect("terminal");
    terminal.feed(b"0123456789abcd\r\nplain");
    // The line is read off the flattened grid, which a draw keeps current;
    // here the snapshot stands in for the draw.
    terminal.snapshot().expect("snapshot");

    let (line, first) = terminal.line_text(1).expect("line");
    assert_eq!(first, 0, "the run starts on its first row");
    assert_eq!(line.len(), 20, "one character per cell, both rows");
    assert!(line.starts_with("0123456789abcd"));

    let (line, first) = terminal.line_text(2).expect("line");
    assert_eq!(first, 2, "an unwrapped row is its own line");
    assert!(line.starts_with("plain"));
}

#[test]
fn a_drag_pinned_to_the_edge_asks_for_autoscroll() {
    let mut terminal = Terminal::new(20, 3, 100).expect("terminal");
    terminal.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
    terminal.scroll_to_top();
    let geometry = geometry(20, 3);

    let (x, y) = at(1, 0);
    terminal
        .select_press(x, y, geometry, Duration::from_millis(0), false)
        .expect("press");
    assert_eq!(terminal.selection_autoscroll(), None);

    // Against the bottom edge: past `screen height - 1`, Ghostty's margin.
    let edge = f64::from(3 * CELL_H) - 0.5;
    terminal
        .select_drag(x, edge, geometry, false)
        .expect("drag");
    assert_eq!(terminal.selection_autoscroll(), Some(false));

    let before = terminal.scroll_position().offset;
    assert!(
        terminal
            .autoscroll_tick(x, edge, geometry, false)
            .expect("tick")
    );
    assert_eq!(
        terminal.scroll_position().offset,
        before + 1,
        "one row per tick"
    );
    assert!(terminal.has_selection());

    // Released, the gesture stops asking and a tick reports it is over.
    terminal.select_release().expect("release");
    assert_eq!(terminal.selection_autoscroll(), None);
}
