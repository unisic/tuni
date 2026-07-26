//! How long one PTY-sized chunk takes to parse, by kind of output.
//!
//! The drain loop hands the main loop back between chunks, so the longest a
//! window can be unable to answer is the longest single `feed` — which makes
//! the per-chunk maximum, not the average throughput, the number that decides
//! whether output can freeze the UI.
//!
//! Run: `cargo run --release -p tuni-vt --example feed_cost`

use std::time::{Duration, Instant};

use tuni_vt::Terminal;

/// One PTY read, matching `tuni-pty`'s 64 KiB buffer.
const CHUNK: usize = 64 * 1024;
const TOTAL: usize = 32 * 1024 * 1024;

fn profile(name: &str, unit: &[u8]) {
    let mut chunk = Vec::with_capacity(CHUNK);
    while chunk.len() < CHUNK {
        chunk.extend_from_slice(unit);
    }
    chunk.truncate(CHUNK);

    let mut term = Terminal::new(200, 60, 10_000).expect("terminal");
    let chunks = TOTAL / CHUNK;
    let mut worst = Duration::ZERO;
    let started = Instant::now();
    for _ in 0..chunks {
        let at = Instant::now();
        term.feed(&chunk);
        worst = worst.max(at.elapsed());
    }
    let elapsed = started.elapsed();
    println!(
        "{name:<26} {:>8.1} MiB/s   mean chunk {:>9?}   worst chunk {:>9?}",
        TOTAL as f64 / 1048576.0 / elapsed.as_secs_f64(),
        elapsed / chunks as u32,
        worst
    );
}

fn main() {
    println!("{} KiB per chunk, {} MiB total\n", CHUNK / 1024, TOTAL / 1048576);
    profile("plain text", b"alpha beta gamma delta epsilon zeta eta theta\n");
    profile("sgr-dense", b"\x1b[31mab\x1b[0m");
    profile("raw esc", b"\x1b");
    profile("esc + printable", b"\x1ba");
    profile("csi, unterminated", b"\x1b[");
    profile("osc, unterminated", b"\x1b]");
}
