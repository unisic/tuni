//! Feed known VT sequences and assert what lands in the grid.
//!
//! These pin the behavior this crate promises to the widget above it, so an
//! upstream API change that silently alters the meaning of a snapshot shows up
//! here rather than as a rendering mystery.

use std::time::Duration;

use tuni_vt::{
    Colors, CursorShape, Geometry, Key, KeyAction, KeyInput, Layer, Mods, MouseAction, MouseButton,
    MouseInput, Rgb, Terminal,
};

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
fn faint_text_draws_halfway_to_the_background() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"\x1b[2mdim\x1b[0mfull");

    let grid = terminal.snapshot().expect("snapshot");
    let dim = grid.cell(0, 0).expect("cell 0");
    let full = grid.cell(3, 0).expect("cell 3");
    let expected = grid.fg.blend(grid.bg, 0.5);
    assert_eq!(dim.fg, expected, "faint halves the way to the background");
    assert_eq!(full.fg, grid.fg, "reset restores full strength");
}

#[test]
fn inverse_video_swaps_foreground_and_background() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"\x1b[7mx");

    let grid = terminal.snapshot().expect("snapshot");
    let cell = grid.cell(0, 0).expect("cell");
    assert_eq!(
        cell.fg, grid.bg,
        "inverse should paint text in the background color"
    );
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

#[test]
fn scroll_position_describes_the_scrollable_area() {
    let mut terminal = Terminal::new(20, 3, 100).expect("terminal");
    assert!(!terminal.scroll_position().is_scrollable());

    terminal.feed(b"one\r\ntwo\r\nthree\r\nfour");
    let bottom = terminal.scroll_position();
    assert_eq!(bottom.len, 3);
    assert_eq!(bottom.total, 4);
    assert_eq!(bottom.offset, 1);
    assert!(bottom.is_scrollable());
    // Pinned to the bottom of a four-row area showing three of them.
    assert_eq!(bottom.fraction(), 1.0);

    terminal.scroll_lines(-1);
    let top = terminal.scroll_position();
    assert_eq!(top.offset, 0);
    assert_eq!(top.fraction(), 0.0);
    assert_eq!(top.proportion(), 0.75);
}

#[test]
fn scroll_to_row_round_trips_through_a_fraction() {
    let mut terminal = Terminal::new(20, 4, 100).expect("terminal");
    for line in 0..24 {
        terminal.feed(format!("line{line}\r\n").as_bytes());
    }

    // What a scrollbar thumb dragged to its middle asks for.
    let row = terminal.scroll_position().row_at(0.5);
    terminal.scroll_to_row(row);

    let position = terminal.scroll_position();
    assert_eq!(position.offset, row);
    assert!((position.fraction() - 0.5).abs() < 0.05);

    terminal.scroll_to_top();
    assert_eq!(terminal.scroll_position().offset, 0);
    assert_eq!(row_text(&mut terminal, 0), "line0");
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
            unshifted_codepoint: None,
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

/// On a Cyrillic layout the key that is physically C types "с", and the Kitty
/// protocol reports what it types with no modifier. The escape has to name the
/// Latin letter regardless, which is what the unshifted codepoint is for.
#[test]
fn the_unshifted_codepoint_names_the_key_under_the_kitty_protocol() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    // Kitty progressive enhancement: disambiguate, report alternates.
    terminal.feed(b"\x1b[>5u");

    let escape = terminal
        .encode_key(&KeyInput {
            action: KeyAction::Press,
            key: Key::C,
            mods: Mods::CTRL,
            consumed_mods: Mods::empty(),
            text: None,
            unshifted_codepoint: Some('c'),
        })
        .expect("encode")
        .to_vec();
    assert_eq!(escape, b"\x1b[99;5u");
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

// --- selection and paste --------------------------------------------------

const CELL_W: u32 = 10;
const CELL_H: u32 = 20;

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
fn dragging_selects_the_dragged_over_text() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"hello world");

    let geometry = geometry(20, 5);
    let (x, y) = at(0, 0);
    terminal
        .select_press(x, y, geometry, Duration::from_millis(0), false)
        .expect("press");
    // Past the middle of the fifth cell, so that cell is included.
    let (x, y) = at(4, 0);
    terminal
        .select_drag(x + 4.0, y, geometry, false)
        .expect("drag");
    terminal.select_release().expect("release");

    assert!(terminal.has_selection());
    assert_eq!(
        terminal.selection_text().expect("text").as_deref(),
        Some("hello")
    );
}

#[test]
fn a_double_click_selects_a_word() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"hello world");

    let geometry = geometry(20, 5);
    let (x, y) = at(7, 0);
    terminal
        .select_press(x, y, geometry, Duration::from_millis(0), false)
        .expect("first press");
    terminal.select_release().expect("release");
    terminal
        .select_press(x, y, geometry, Duration::from_millis(100), false)
        .expect("second press");

    assert_eq!(
        terminal.selection_text().expect("text").as_deref(),
        Some("world")
    );
}

#[test]
fn selection_is_marked_on_the_grid_and_can_be_cleared() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"hello");

    let geometry = geometry(20, 5);
    let (x, y) = at(0, 0);
    terminal
        .select_press(x, y, geometry, Duration::from_millis(0), false)
        .expect("press");
    let (x, y) = at(2, 0);
    terminal.select_drag(x, y, geometry, false).expect("drag");

    // Selected cells render inverted, so the first cell's background is now the
    // page foreground.
    let grid = terminal.snapshot().expect("snapshot");
    assert_eq!(grid.cell(0, 0).expect("cell").bg, Some(grid.fg));

    terminal.clear_selection().expect("clear");
    assert!(!terminal.has_selection());
    assert_eq!(terminal.selection_text().expect("text"), None);
}

#[test]
fn select_all_covers_the_scrollback() {
    let mut terminal = Terminal::new(20, 2, 100).expect("terminal");
    terminal.feed(b"one\r\ntwo\r\nthree");

    terminal.select_all().expect("select all");
    let text = terminal.selection_text().expect("text").expect("some text");
    assert!(
        text.contains("one"),
        "scrolled-off line should be selected: {text:?}"
    );
    assert!(text.contains("three"));
}

#[test]
fn mouse_is_reported_only_when_the_application_asks() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    let geometry = geometry(20, 5);
    let (x, y) = at(2, 1);
    let press = MouseInput {
        action: MouseAction::Press,
        button: Some(MouseButton::Left),
        mods: Mods::empty(),
        x,
        y,
        any_button_pressed: true,
    };

    assert!(!terminal.is_mouse_tracking());
    assert!(
        terminal
            .encode_mouse(&press, geometry)
            .expect("encode")
            .is_empty()
    );

    // Normal tracking plus SGR reporting, the combination every modern
    // application enables.
    terminal.feed(b"\x1b[?1000h\x1b[?1006h");
    assert!(terminal.is_mouse_tracking());
    assert_eq!(
        terminal.encode_mouse(&press, geometry).expect("encode"),
        b"\x1b[<0;3;2M",
        "SGR reports 1-based cell coordinates"
    );

    // The wheel rides the same path as buttons four and five.
    let wheel_up = MouseInput {
        button: Some(MouseButton::Four),
        any_button_pressed: false,
        ..press
    };
    assert_eq!(
        terminal.encode_mouse(&wheel_up, geometry).expect("encode"),
        b"\x1b[<64;3;2M"
    );
}

#[test]
fn focus_is_reported_only_when_the_application_asks() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    assert!(terminal.encode_focus(true).expect("encode").is_empty());

    terminal.feed(b"\x1b[?1004h");
    assert_eq!(terminal.encode_focus(true).expect("encode"), b"\x1b[I");
    assert_eq!(terminal.encode_focus(false).expect("encode"), b"\x1b[O");

    terminal.feed(b"\x1b[?1004l");
    assert!(terminal.encode_focus(false).expect("encode").is_empty());
}

#[test]
fn paste_follows_bracketed_paste_mode() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    assert_eq!(terminal.encode_paste("ls").expect("encode"), b"ls");

    terminal.feed(b"\x1b[?2004h");
    assert_eq!(
        terminal.encode_paste("ls").expect("encode"),
        b"\x1b[200~ls\x1b[201~"
    );
}

#[test]
fn osc_fifty_two_asks_for_a_clipboard_write() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    // base64 of "copied"
    terminal.feed(b"\x1b]52;c;Y29waWVk\x07");

    let effects = terminal.take_effects();
    let request = effects.clipboard_writes.first().expect("a clipboard write");
    assert_eq!(request.text, "copied");
}

#[test]
fn bare_modifiers_produce_nothing() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    assert!(press(&mut terminal, Key::ShiftLeft, Mods::SHIFT, None).is_empty());
}

// --- working directory ------------------------------------------------------

#[test]
fn osc_seven_reports_a_local_working_directory() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    assert_eq!(terminal.pwd(), None);

    terminal.feed(b"\x1b]7;file://localhost/home/user/my%20code\x07");
    assert!(terminal.take_effects().pwd_changed);
    assert_eq!(terminal.pwd().as_deref(), Some("/home/user/my code"));
}

#[test]
fn osc_seven_ignores_a_directory_on_another_machine() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"\x1b]7;file://somewhere-else/home/user\x07");
    // An SSH session's path is not one this machine can open.
    assert_eq!(terminal.pwd(), None);
}

// --- colors -----------------------------------------------------------------

fn test_colors() -> Colors {
    Colors {
        foreground: Rgb {
            r: 0xe0,
            g: 0xe0,
            b: 0xe0,
        },
        background: Rgb {
            r: 0x10,
            g: 0x12,
            b: 0x14,
        },
        cursor: Some(Rgb {
            r: 0xff,
            g: 0xa0,
            b: 0x00,
        }),
        cursor_text: Some(Rgb {
            r: 0x00,
            g: 0x00,
            b: 0x00,
        }),
        selection_background: Some(Rgb {
            r: 0x30,
            g: 0x40,
            b: 0x50,
        }),
        selection_foreground: Some(Rgb {
            r: 0xff,
            g: 0xff,
            b: 0xff,
        }),
        palette: [Rgb {
            r: 0x01,
            g: 0x02,
            b: 0x03,
        }; 16],
    }
}

#[test]
fn a_theme_repaints_the_page_the_palette_and_the_cursor() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    let mut colors = test_colors();
    // A distinguishable ANSI red, so the SGR below has something to prove.
    colors.palette[1] = Rgb {
        r: 0xd0,
        g: 0x20,
        b: 0x20,
    };
    terminal.set_colors(&colors).expect("set colors");

    // SGR 31 is "ANSI red", which the palette now defines.
    terminal.feed(b"\x1b[31mred");

    let grid = terminal.snapshot().expect("snapshot");
    assert_eq!(grid.bg, colors.background);
    assert_eq!(grid.fg, colors.foreground);
    assert_eq!(grid.cell(0, 0).expect("cell").fg, colors.palette[1]);

    let cursor = grid.cursor.expect("a visible cursor");
    assert_eq!(cursor.color, colors.cursor);
    assert_eq!(cursor.text_color, colors.cursor_text);
}

#[test]
fn a_theme_recolors_the_selection_instead_of_inverting_it() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    let colors = test_colors();
    terminal.set_colors(&colors).expect("set colors");
    terminal.feed(b"hello");

    let geometry = geometry(20, 5);
    let (x, y) = at(0, 0);
    terminal
        .select_press(x, y, geometry, Duration::from_millis(0), false)
        .expect("press");
    let (x, y) = at(2, 0);
    terminal.select_drag(x, y, geometry, false).expect("drag");

    let grid = terminal.snapshot().expect("snapshot");
    let cell = grid.cell(0, 0).expect("cell");
    assert_eq!(cell.bg, colors.selection_background);
    assert_eq!(cell.fg, colors.selection_foreground.expect("set above"));
}

#[test]
fn a_theme_with_no_selection_colors_still_inverts() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    let colors = Colors {
        selection_background: None,
        selection_foreground: None,
        ..test_colors()
    };
    terminal.set_colors(&colors).expect("set colors");
    terminal.feed(b"hello");

    let geometry = geometry(20, 5);
    let (x, y) = at(0, 0);
    terminal
        .select_press(x, y, geometry, Duration::from_millis(0), false)
        .expect("press");
    let (x, y) = at(2, 0);
    terminal.select_drag(x, y, geometry, false).expect("drag");

    let grid = terminal.snapshot().expect("snapshot");
    assert_eq!(grid.cell(0, 0).expect("cell").bg, Some(grid.fg));
}

#[test]
fn an_application_that_sets_a_color_outranks_the_theme() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.set_colors(&test_colors()).expect("set colors");

    // OSC 11 is an application asking for its own background.
    terminal.feed(b"\x1b]11;#123456\x07");
    assert_eq!(
        terminal.snapshot().expect("snapshot").bg,
        Rgb {
            r: 0x12,
            g: 0x34,
            b: 0x56
        }
    );

    // And a theme change while that override stands leaves it alone.
    let mut later = test_colors();
    later.background = Rgb {
        r: 0xff,
        g: 0xff,
        b: 0xff,
    };
    terminal.set_colors(&later).expect("set colors");
    assert_eq!(
        terminal.snapshot().expect("snapshot").bg,
        Rgb {
            r: 0x12,
            g: 0x34,
            b: 0x56
        }
    );
}

#[test]
fn osc_8_marks_the_cells_it_covers() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"a\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\b");

    let grid = terminal.snapshot().expect("snapshot");
    assert!(!grid.cell(0, 0).expect("cell").link, "the 'a' before it");
    for col in 1..5 {
        assert!(grid.cell(col, 0).expect("cell").link, "column {col}");
    }
    assert!(!grid.cell(5, 0).expect("cell").link, "the 'b' after it");
}

#[test]
fn a_hyperlink_reports_its_uri_and_its_extent() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"\x1b]8;;https://example.com/one\x1b\\link\x1b]8;;\x1b\\");
    let _ = terminal.snapshot().expect("snapshot");

    assert_eq!(
        terminal.hyperlink_at(2, 0).expect("uri"),
        Some("https://example.com/one".to_owned())
    );
    let hover = terminal
        .hyperlink_hover(2, 0)
        .expect("hover")
        .expect("a link");
    assert_eq!(hover.uri, "https://example.com/one");
    assert_eq!(hover.cells, vec![(0, 0), (1, 0), (2, 0), (3, 0)]);
}

#[test]
fn a_cell_with_no_hyperlink_reports_none() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(b"plain");
    let _ = terminal.snapshot().expect("snapshot");

    assert_eq!(terminal.hyperlink_at(0, 0).expect("uri"), None);
    assert_eq!(terminal.hyperlink_hover(0, 0).expect("hover"), None);
}

#[test]
fn two_hyperlinks_are_told_apart_by_their_uris() {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.feed(
        b"\x1b]8;;https://one.example\x1b\\aa\x1b]8;;\x1b\\ \
          \x1b]8;;https://two.example\x1b\\bb\x1b]8;;\x1b\\",
    );
    let _ = terminal.snapshot().expect("snapshot");

    let first = terminal
        .hyperlink_hover(0, 0)
        .expect("hover")
        .expect("a link");
    assert_eq!(first.uri, "https://one.example");
    assert_eq!(first.cells, vec![(0, 0), (1, 0)]);

    let second = terminal
        .hyperlink_hover(3, 0)
        .expect("hover")
        .expect("a link");
    assert_eq!(second.uri, "https://two.example");
    assert_eq!(second.cells, vec![(3, 0), (4, 0)]);
}

// --- search -------------------------------------------------------------------

#[test]
fn a_match_is_reported_on_the_row_the_viewport_can_scroll_to() {
    let mut terminal = Terminal::new(20, 3, 100).expect("terminal");
    terminal.feed(b"alpha\r\nbeta\r\ngamma\r\ndelta\r\nepsilon");

    let hits = terminal.search("beta").expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].col, 0);
    assert_eq!(hits[0].len, 4);

    // The row a hit names is the row that puts it at the top of the viewport.
    terminal.scroll_to_row(hits[0].row);
    assert_eq!(row_text(&mut terminal, 0), "beta");
}

#[test]
fn a_search_ignores_case_and_finds_every_occurrence_on_a_row() {
    let mut terminal = Terminal::new(40, 4, 100).expect("terminal");
    terminal.feed(b"Error: error while erroring");

    let hits = terminal.search("ERROR").expect("search");
    assert_eq!(hits.len(), 3);
    assert!(hits.iter().all(|hit| hit.row == hits[0].row));
    assert_eq!(hits[0].col, 0);
    assert_eq!(hits[1].col, 7);
    assert_eq!(hits[2].col, 19);
}

#[test]
fn a_match_after_a_double_width_character_is_reported_in_cells() {
    let mut terminal = Terminal::new(20, 3, 100).expect("terminal");
    // Two cells for the ideograph, then the needle.
    terminal.feed("漢x".as_bytes());

    let hits = terminal.search("x").expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].col, 2);
}

#[test]
fn an_empty_needle_matches_nothing() {
    let mut terminal = Terminal::new(20, 3, 100).expect("terminal");
    terminal.feed(b"anything at all");
    assert!(terminal.search("").expect("search").is_empty());
}

#[test]
fn a_needle_that_is_not_there_is_not_found() {
    let mut terminal = Terminal::new(20, 3, 100).expect("terminal");
    terminal.feed(b"one\r\ntwo");
    assert!(terminal.search("three").expect("search").is_empty());
}

/// A 2x2 image: red, green on the first row, blue, white on the second.
const RGB_2X2: &str = "/wAAAP8AAAD/////";

/// The same image as a PNG, which is how nearly every program transmits one.
const PNG_2X2: &str = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAFElEQVR4nGP4z8DAAMIM/////w8AH+4F+7C4l8kAAAAASUVORK5CYII=";

/// A terminal with a cell size, which a placement's geometry is measured in.
fn imaging_terminal() -> Terminal {
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    terminal.resize(20, 5, 8, 16).expect("resize");
    terminal
}

#[test]
fn a_terminal_that_has_seen_no_image_reports_no_placements() {
    let mut terminal = imaging_terminal();
    terminal.feed(b"plain text");

    let mut placements = Vec::new();
    terminal.images(&mut placements).expect("images");
    assert!(placements.is_empty());
}

#[test]
fn a_transmitted_image_is_placed_where_the_cursor_was() {
    let mut terminal = imaging_terminal();
    // Row 2, column 3 (CUP is 1-based), then transmit-and-display 2x2 raw RGB.
    terminal.feed(b"\x1b[2;3H");
    terminal.feed(format!("\x1b_Ga=T,f=24,s=2,v=2,i=1,q=2;{RGB_2X2}\x1b\\").as_bytes());

    let mut placements = Vec::new();
    terminal.images(&mut placements).expect("images");
    assert_eq!(placements.len(), 1);

    let placement = placements[0];
    assert_eq!(placement.image.id, 1);
    assert_eq!((placement.col, placement.row), (2, 1));
    assert_eq!((placement.width, placement.height), (2, 2));
    assert_eq!(
        (placement.source_width, placement.source_height),
        (2, 2),
        "the whole image is drawn when no source rectangle was asked for"
    );
    assert_eq!(placement.layer(), Layer::AboveText);
}

#[test]
fn image_pixels_come_back_as_rgba() {
    let mut terminal = imaging_terminal();
    terminal.feed(format!("\x1b_Ga=T,f=24,s=2,v=2,i=1,q=2;{RGB_2X2}\x1b\\").as_bytes());

    let pixels = terminal
        .image_pixels(1)
        .expect("pixels")
        .expect("image should be stored");
    assert_eq!((pixels.width, pixels.height), (2, 2));
    assert_eq!(
        pixels.rgba,
        vec![
            255, 0, 0, 255, // red
            0, 255, 0, 255, // green
            0, 0, 255, 255, // blue
            255, 255, 255, 255, // white
        ],
        "RGB gains an opaque alpha channel"
    );
}

#[test]
fn a_png_transmission_is_decoded() {
    let mut terminal = imaging_terminal();
    terminal.feed(format!("\x1b_Ga=T,f=100,i=7,q=2;{PNG_2X2}\x1b\\").as_bytes());

    let pixels = terminal
        .image_pixels(7)
        .expect("pixels")
        .expect("a PNG should decode through the installed decoder");
    assert_eq!((pixels.width, pixels.height), (2, 2));
    assert_eq!(&pixels.rgba[..4], &[255, 0, 0, 255]);
    assert_eq!(&pixels.rgba[12..], &[255, 255, 255, 255]);
}

#[test]
fn a_placement_is_sized_by_the_columns_and_rows_it_asks_for() {
    let mut terminal = imaging_terminal();
    terminal.feed(format!("\x1b_Ga=T,f=24,s=2,v=2,i=1,c=4,r=3,q=2;{RGB_2X2}\x1b\\").as_bytes());

    let mut placements = Vec::new();
    terminal.images(&mut placements).expect("images");
    assert_eq!(placements.len(), 1);
    // Four 8px columns by three 16px rows, whatever the image's own size is.
    assert_eq!((placements[0].width, placements[0].height), (32, 48));
}

#[test]
fn placements_are_ordered_by_the_layer_they_draw_in() {
    let mut terminal = imaging_terminal();
    let image = format!("\x1b_Ga=t,f=24,s=2,v=2,i=1,q=2;{RGB_2X2}\x1b\\");
    terminal.feed(image.as_bytes());
    // Three placements of the one image, each in a different layer.
    terminal.feed(b"\x1b[1;1H\x1b_Ga=p,i=1,p=1,z=5,q=2;\x1b\\");
    terminal.feed(b"\x1b[2;1H\x1b_Ga=p,i=1,p=2,z=-1,q=2;\x1b\\");
    terminal.feed(format!("\x1b[3;1H\x1b_Ga=p,i=1,p=3,z={},q=2;\x1b\\", i32::MIN).as_bytes());

    let mut placements = Vec::new();
    terminal.images(&mut placements).expect("images");
    assert_eq!(placements.len(), 3);
    let layers: Vec<_> = placements.iter().map(tuni_vt::Placement::layer).collect();
    assert_eq!(
        layers,
        vec![Layer::BelowBackground, Layer::BelowText, Layer::AboveText],
        "sorted by z, which is the order the three layers stack in"
    );
}

#[test]
fn deleting_an_image_takes_its_placements_with_it() {
    let mut terminal = imaging_terminal();
    terminal.feed(format!("\x1b_Ga=T,f=24,s=2,v=2,i=1,q=2;{RGB_2X2}\x1b\\").as_bytes());
    terminal.feed(b"\x1b_Ga=d,d=I,i=1,q=2;\x1b\\");

    let mut placements = Vec::new();
    terminal.images(&mut placements).expect("images");
    assert!(placements.is_empty());
    assert!(terminal.image_pixels(1).expect("pixels").is_none());
}

#[test]
fn retransmitting_an_image_changes_its_cache_key() {
    let mut terminal = imaging_terminal();
    terminal.feed(format!("\x1b_Ga=T,f=24,s=2,v=2,i=1,q=2;{RGB_2X2}\x1b\\").as_bytes());
    let mut placements = Vec::new();
    terminal.images(&mut placements).expect("images");
    let first = placements[0].image;

    terminal.feed(b"\x1b[3;1H");
    terminal.feed(format!("\x1b_Ga=T,f=24,s=2,v=2,i=1,q=2;{RGB_2X2}\x1b\\").as_bytes());
    terminal.images(&mut placements).expect("images");
    let second = placements
        .iter()
        .map(|placement| placement.image)
        .find(|key| key.generation != first.generation);

    assert!(
        second.is_some(),
        "same id, same pixels, different picture — only the generation says so"
    );
}

#[test]
fn a_wheel_press_survives_the_pointer_resting_in_the_same_cell() {
    // The scenario every scroll starts from: any-motion tracking is on, the
    // pointer has just reported the cell it rests in, and then the wheel
    // turns without the pointer moving. The wheel press must not be eaten by
    // the motion deduplication, or scrolling only works while the mouse is
    // in flight.
    let mut terminal = Terminal::new(20, 5, 100).expect("terminal");
    let geometry = geometry(20, 5);
    let (x, y) = at(2, 1);
    terminal.feed(b"\x1b[?1000h\x1b[?1003h\x1b[?1006h");

    let motion = MouseInput {
        action: MouseAction::Motion,
        button: None,
        mods: Mods::empty(),
        x,
        y,
        any_button_pressed: false,
    };
    assert_eq!(
        terminal.encode_mouse(&motion, geometry).expect("encode"),
        b"\x1b[<35;3;2M",
        "any-motion tracking reports the resting cell first"
    );

    let wheel = MouseInput {
        action: MouseAction::Press,
        button: Some(MouseButton::Four),
        ..motion
    };
    assert_eq!(
        terminal.encode_mouse(&wheel, geometry).expect("encode"),
        b"\x1b[<64;3;2M",
        "the wheel in the same cell must still be reported"
    );
    assert_eq!(
        terminal.encode_mouse(&wheel, geometry).expect("encode"),
        b"\x1b[<64;3;2M",
        "and so must the next notch"
    );
}

#[test]
fn ten_thousand_lines_of_scrollback_hold_ten_thousand_lines() {
    // The number the app hands over is a line count, and upstream's budget is
    // bytes: this is the test that fails when one is passed off as the other.
    // Before the conversion, "10,000 lines" bought one 64 KiB page — about
    // 430 rows at this width — and everything older was silently gone.
    let mut terminal = Terminal::new(200, 60, 10_000).expect("terminal");
    for i in 0..12_000 {
        terminal.feed(format!("line-{i:06} alpha beta gamma delta epsilon\r\n").as_bytes());
    }
    let hits = terminal.search("line-002500").expect("search");
    assert_eq!(hits.len(), 1, "a line 9,500 rows back is still there");
    assert!(
        terminal.scroll_position().total > 10_000,
        "the scrollable area holds what the setting promised, got {}",
        terminal.scroll_position().total
    );
}

#[test]
fn zero_scrollback_lines_means_none_not_unlimited() {
    let mut terminal = Terminal::new(80, 24, 0).expect("terminal");
    for i in 0..5_000 {
        terminal.feed(format!("line-{i:06}\r\n").as_bytes());
    }
    let position = terminal.scroll_position();
    assert_eq!(
        position.total, position.len,
        "nothing above the viewport to scroll to"
    );
}
