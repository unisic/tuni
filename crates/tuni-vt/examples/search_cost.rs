//! How long one live-search pass over the scrollback takes.
//!
//! `TuniTerminal::schedule_refind` runs a whole-scrollback search once per
//! main-loop turn while the find bar holds a needle, and output is what makes
//! it run: every `feed` schedules another pass. So the cost of one pass, times
//! how many turns a second of output produces, is what the find bar adds to a
//! terminal that is printing.
//!
//! The same pass over the scrollback happens once more at closing time:
//! `save_session` calls `dump_history` per pane on the main thread, so its
//! cost times the number of panes is how long the window takes to go away.
//!
//! Run: `cargo run --release -p tuni-vt --example search_cost`

use std::time::Instant;

use tuni_vt::Terminal;

const PASSES: u32 = 20;

fn measure(lines: usize, needle: &str) {
    let mut term = Terminal::new(200, 60, lines).expect("terminal");
    // Fill the scrollback with text a search has to walk rather than skip.
    let row = b"alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu\r\n";
    for _ in 0..lines {
        term.feed(row);
    }
    // One line the needle is actually on, so the hit list is not empty.
    term.feed(b"needle-here\r\n");

    let started = Instant::now();
    let mut hits = 0;
    for _ in 0..PASSES {
        hits = term.search(needle).expect("search").len();
    }
    let each = started.elapsed() / PASSES;

    // What one pane contributes to closing the window: 500 lines is
    // `HISTORY_LINE_LIMIT`, whatever the scrollback holds behind it.
    let started = Instant::now();
    let mut bytes = 0;
    for _ in 0..PASSES {
        bytes = term
            .dump_history(500)
            .expect("dump")
            .map_or(0, |text| text.len());
    }
    let dump = started.elapsed() / PASSES;

    println!(
        "{lines:>6} lines  search {each:>10.2?}  hits={hits:<3}  \
         dump_history(500) {dump:>10.2?}  {bytes} bytes"
    );
}

fn main() {
    for lines in [1_000, 10_000, 50_000] {
        measure(lines, "needle");
    }
}
