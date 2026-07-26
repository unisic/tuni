//! What one tick of the panel's 2-second timer actually costs.
//!
//! The timer re-reads the open directories and re-anchors the repository root.
//! Whether that is free or whether it is a stall depends entirely on how much
//! of the tree is expanded, which is a property of the person using it rather
//! than of the code — so this measures it as a curve instead of asserting a
//! number: the root alone, then progressively more directories opened.
//!
//! Run: `cargo run --release -p tuni-core --example poll_cost -- [root]`

use std::path::{Path, PathBuf};
use std::time::Instant;

use tuni_core::files::Tree;

fn time<T>(label: &str, iterations: u32, mut body: impl FnMut() -> T) {
    // One untimed pass so the page cache is warm: the timer in a running
    // window is always hitting a warm cache, and a cold-cache number would
    // measure the disk rather than the poll.
    body();
    let started = Instant::now();
    for _ in 0..iterations {
        std::hint::black_box(body());
    }
    let each = started.elapsed() / iterations;
    println!("{label:<42} {each:>12?}  ({iterations} iterations)");
}

/// Every directory under `root`, breadth-first, up to `limit`.
fn directories(root: &Path, limit: usize) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut queue = vec![root.to_path_buf()];
    while let Some(dir) = queue.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.file_name().is_some_and(|n| n != ".git") {
                found.push(path.clone());
                if found.len() >= limit {
                    return found;
                }
                queue.push(path);
            }
        }
    }
    found
}

fn main() {
    let root = std::env::args()
        .nth(1)
        .map_or_else(|| std::env::current_dir().unwrap(), PathBuf::from);
    println!("root: {}\n", root.display());

    // The two things the tick does before it touches the tree at all.
    time("std::env::current_dir()", 10_000, || {
        std::env::current_dir().ok()
    });

    // The upward `.exists()` walk that re-anchors the repository. Measured from
    // a deep path, because depth is what decides how many stats it does.
    let deep = directories(&root, 400)
        .into_iter()
        .max_by_key(|p| p.components().count())
        .unwrap_or_else(|| root.clone());
    println!("deepest probe path: {}", deep.display());
    time("closest_git_repository (via panel_root)", 10_000, || {
        let mut dir = deep.as_path();
        loop {
            if dir.join(".git").exists() {
                return Some(dir.to_path_buf());
            }
            match dir.parent() {
                Some(parent) => dir = parent,
                None => return None,
            }
        }
    });

    println!();
    let all = directories(&root, 4000);
    for open in [0usize, 1, 10, 50, 200, 1000] {
        if open > all.len() {
            continue;
        }
        let mut tree = Tree::new();
        tree.sync(&root);
        for dir in all.iter().take(open) {
            tree.expand(dir);
        }
        tree.rebuild();
        let rows = tree.items().len();
        time(
            &format!("rebuild: {open} dirs open, {rows} rows"),
            if rows > 20_000 { 20 } else { 200 },
            || tree.rebuild(),
        );
    }
}
