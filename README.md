# Tuni

A native terminal workspace for Linux: terminal panes, projects, a file tree, a
git panel, an editor, and a diff viewer in one window.

Tuni is a ground-up Linux implementation of the workspace that
[egoist/kero](https://github.com/egoist/kero) built for macOS. Kero's Swift
source is read as a specification of behavior, not translated; the macOS build
is SwiftUI/AppKit over the libghostty embed API, which exists only for macOS and
iOS. Kero is GPLv3 and so is Tuni.

## Status

Etap 0 — feasibility spike. One window, one terminal, keyboard input, Pango
rendering. Not usable yet.

The staged plan runs from here to full parity: a complete standalone terminal,
then projects and tabs, the niri-style pane layout, session persistence, the
file tree, the git panel, the editor, the diff viewer, the command palette, and
packaging. Every stage ends with an application that builds and runs.

## How it works

Ghostty's terminal emulation, not a reimplementation of it. `libghostty-vt` is
the cross-platform, zero-dependency half of Ghostty: the SIMD VT parser, the
full terminal state and grid, cell styles, reflow, scrollback, Kitty keyboard
encoding, and SGR mouse encoding. It does not do rendering or PTY management, so
those two are ours.

| Crate | Responsibility |
| --- | --- |
| `tuni-vt` | Facade over `libghostty-vt`. Nothing above this line imports it. |
| `tuni-pty` | Shell process, PTY, reader thread, window resize. |
| `tuni-core` | Portable models: config, projects, panes, session, git. No GTK. |
| `tuni-gtk` | GTK4 + libadwaita widgets, window, actions, keybindings. |

The upstream C API is explicitly pre-1.0 and expected to change, which is why
the dependency is pinned to a commit and why every call to it goes through
`tuni-vt`.

Text is drawn with Pango rather than a GPU glyph atlas. Pango brings fontconfig
fallback, subpixel antialiasing, and input methods, and text quality is what a
terminal is judged on. Whether it keeps up under load is the question the Etap 0
benchmark answers.

## Building

Requires a Rust toolchain, GTK4 and libadwaita development headers, and Zig —
`libghostty-vt` is Zig source that is compiled during the build.

```sh
# Fedora
sudo dnf install rustup gtk4-devel libadwaita-devel gtksourceview5-devel
rustup-init -y

cargo run --release
```

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
