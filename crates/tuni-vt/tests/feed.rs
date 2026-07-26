//! Feed known VT sequences and assert what lands in the grid.
//!
//! These pin the behavior this crate promises to the widget above it, so an
//! upstream API change that silently alters the meaning of a snapshot shows up
//! here rather than as a rendering mystery.

use tuni_vt::{CursorShape, Key, KeyAction, KeyInput, Mods, Rgb, Terminal};

fn row_text(terminal: &mut Terminal, row: u16) -> String {
    let grid = terminal.snapshot().expect("snapshot");
    grid.row(row)
        .iter()
        .map(|cell| {
            if cell.text.is_empty() {
                " "
            } else {
                cell.text.as_str()
            }
        })
        .collect::<String>()
        .trim_end()
        .to_owned()
}

#[test]
fn plain_text_lands_in_the_first_row() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"hello");
    assert_eq!(row_text(&mut terminal, 0), "hello");
}

#[test]
fn newline_and_carriage_return_move_to_the_next_row() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"first\r\nsecond");
    assert_eq!(row_text(&mut terminal, 0), "first");
    assert_eq!(row_text(&mut terminal, 1), "second");
}

#[test]
fn sgr_sets_color_and_attributes() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"\x1b[1;38;2;255;0;0mR\x1b[0mp");

    let grid = terminal.snapshot().expect("snapshot");
    let styled = grid.cell(0, 0).expect("cell 0");
    assert_eq!(styled.text, "R");
    assert_eq!(styled.fg, Rgb { r: 255, g: 0, b: 0 });
    assert!(styled.bold);

    let plain = grid.cell(1, 0).expect("cell 1");
    assert_eq!(plain.text, "p");
    assert!(!plain.bold);
}

#[test]
fn inverse_video_swaps_foreground_and_background() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"\x1b[7mx");

    let grid = terminal.snapshot().expect("snapshot");
    let cell = grid.cell(0, 0).expect("cell");
    assert_eq!(cell.fg, grid.bg, "inverse should paint text in the background color");
    assert_eq!(cell.bg, Some(grid.fg));
}

#[test]
fn cursor_follows_position_and_shape_sequences() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    // Move to row 3, column 5 (CUP is 1-based), then ask for a bar cursor.
    terminal.feed(b"\x1b[3;5H\x1b[6 q");

    let grid = terminal.snapshot().expect("snapshot");
    let cursor = grid.cursor.expect("cursor should be visible");
    assert_eq!((cursor.col, cursor.row), (4, 2));
    assert_eq!(cursor.shape, CursorShape::Bar);
}

#[test]
fn resize_reflows_a_wrapped_line() {
    let mut terminal = Terminal::new(10, 5, 100).expect("terminal");
    terminal.feed(b"abcdefghijklmno");
    assert_eq!(row_text(&mut terminal, 0), "abcdefghij");
    assert_eq!(row_text(&mut terminal, 1), "klmno");

    terminal.resize(20, 5, 8, 16).expect("resize");
    assert_eq!(row_text(&mut terminal, 0), "abcdefghijklmno");
}

#[test]
fn osc_two_sets_the_title() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"\x1b]2;a title\x07");

    let effects = terminal.take_effects();
    assert!(effects.title_changed);
    assert_eq!(terminal.title(), Some("a title"));
}

#[test]
fn bel_raises_the_bell_once_per_drain() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"\x07");
    assert!(terminal.take_effects().bell);
    assert!(!terminal.take_effects().bell);
}

#[test]
fn device_status_report_is_answered_back_to_the_pty() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"\x1b[6n");

    let effects = terminal.take_effects();
    assert_eq!(effects.pty_write, b"\x1b[1;1R");
}

#[test]
fn scrolling_back_shows_lines_that_left_the_viewport() {
    let mut terminal = Terminal::new(20, 3, 100).expect("terminal");
    terminal.feed(b"one\r\ntwo\r\nthree\r\nfour");
    assert_eq!(row_text(&mut terminal, 0), "two");

    terminal.scroll_lines(-1);
    assert_eq!(row_text(&mut terminal, 0), "one");

    terminal.scroll_to_bottom();
    assert_eq!(row_text(&mut terminal, 0), "two");
}

// --- key encoding ---------------------------------------------------------

fn press(terminal: &mut Terminal, key: Key, mods: Mods, text: Option<&str>) -> Vec<u8> {
    let consumed = if text.is_some() && mods.contains(Mods::SHIFT) {
        Mods::SHIFT
    } else {
        Mods::empty()
    };
    terminal
        .encode_key(&KeyInput {
            action: KeyAction::Press,
            key,
            mods,
            consumed_mods: consumed,
            text,
        })
        .expect("encode")
        .to_vec()
}

#[test]
fn printable_keys_encode_to_their_text() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    assert_eq!(press(&mut terminal, Key::A, Mods::empty(), Some("a")), b"a");
    assert_eq!(
        press(&mut terminal, Key::A, Mods::SHIFT, Some("A")),
        b"A",
        "shift is consumed by the toolkit, so it must not be reported again"
    );
}

#[test]
fn control_keys_encode_to_their_c0_bytes() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    assert_eq!(press(&mut terminal, Key::C, Mods::CTRL, None), b"\x03");
    assert_eq!(press(&mut terminal, Key::D, Mods::CTRL, None), b"\x04");
    assert_eq!(press(&mut terminal, Key::Enter, Mods::empty(), None), b"\r");
    assert_eq!(
        press(&mut terminal, Key::Backspace, Mods::empty(), None),
        b"\x7f"
    );
    assert_eq!(press(&mut terminal, Key::Tab, Mods::empty(), None), b"\t");
}

#[test]
fn arrows_follow_the_cursor_key_mode() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    assert_eq!(
        press(&mut terminal, Key::ArrowUp, Mods::empty(), None),
        b"\x1b[A"
    );

    // DECCKM: an application like vi switches the arrows to SS3 form.
    terminal.feed(b"\x1b[?1h");
    assert_eq!(
        press(&mut terminal, Key::ArrowUp, Mods::empty(), None),
        b"\x1bOA"
    );
}

#[test]
fn bare_modifiers_produce_nothing() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    assert!(press(&mut terminal, Key::ShiftLeft, Mods::SHIFT, None).is_empty());
}
