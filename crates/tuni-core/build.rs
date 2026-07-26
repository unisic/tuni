//! Bakes `data/themes/` into the binary as a sorted name/contents table.
//!
//! The themes are Ghostty's own catalog, vendored verbatim. Embedding rather
//! than installing them keeps `cargo run` working from a fresh checkout and
//! keeps a Flatpak or AppImage build from needing a data directory alongside
//! the executable. Parsing stays at runtime and only touches the theme the
//! user actually selected.

use std::fmt::Write as _;
use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("../../data/themes")
        .canonicalize()
        .expect("data/themes is missing from the checkout");

    println!("cargo:rerun-if-changed={}", dir.display());

    let mut themes: Vec<(String, PathBuf)> = std::fs::read_dir(&dir)
        .expect("data/themes cannot be read")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let name = path.file_name()?.to_str()?.to_owned();
            path.is_file().then_some((name, path))
        })
        .collect();

    // Sorted so the lookup below can binary search, and so the generated file
    // is stable across filesystems that hand back directory entries unordered.
    themes.sort_unstable();

    let mut out = String::from("pub(crate) static THEMES: &[(&str, &str)] = &[\n");
    for (name, path) in &themes {
        // Theme names come from a vendored directory, not from user input, and
        // none of them contain a quote or a backslash; the check keeps that
        // true if the catalog ever changes rather than emitting broken Rust.
        assert!(
            !name.contains(['"', '\\']),
            "theme name {name:?} needs escaping"
        );
        let _ = writeln!(
            out,
            "    ({name:?}, include_str!({:?})),",
            path.to_str().expect("theme path is not UTF-8")
        );
    }
    out.push_str("];\n");

    let target = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("themes.rs");
    std::fs::write(&target, out).expect("cannot write the generated theme table");
}
