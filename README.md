# Tuni

A native terminal workspace for Linux: terminal panes, projects, a file tree, a
git panel, an editor, and a diff viewer in one window.

Tuni is a ground-up Linux implementation of the workspace that
[egoist/kero](https://github.com/egoist/kero) built for macOS. Kero's Swift
source is read as a specification of behavior, not translated; the macOS build
is SwiftUI/AppKit over the libghostty embed API, which exists only for macOS and
iOS. Kero is GPLv3 and so is Tuni.

## Status

One window, one terminal: keyboard input, Pango rendering, mouse selection with
word and line clicks, clipboard and bracketed paste, and SGR mouse reporting for
applications that ask for it. `ls`, `vi`, and `top` all render correctly. Still
missing before it is a daily terminal: scrollbar, cursor blinking, themes, font
configuration, hyperlinks, tabs.

| Shortcut | Action |
| --- | --- |
| `Ctrl+Shift+C` / `Ctrl+Shift+V` | Copy selection, paste |
| Middle click | Paste the primary selection |
| `Ctrl+Shift+A` | Select everything, scrollback included |
| Drag, double click, triple click | Select by character, word, line |
| `Alt`+drag | Block selection |
| `Shift`+click | Select even while an application is tracking the mouse |

From here the work runs to full parity: a complete standalone terminal, then
projects and tabs, the niri-style pane layout, session persistence, the file
tree, the git panel, the editor, the diff viewer, the command palette, and
packaging.

Consuming a 200 MiB stream, measured with `scripts/throughput.sh` on this
machine:

| Terminal | Throughput |
| --- | --- |
| Tuni | 117 MiB/s |
| Ghostty | 98 MiB/s |
| Konsole | 45 MiB/s |

Drawing a full 120x37 viewport costs 516 µs at the median and 1.0 ms at the
95th percentile, against a 16.7 ms frame budget. That answers the question the
spike existed to answer: Pango is not the bottleneck, and there is no reason to
reach for a GPU glyph atlas.

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
terminal is judged on.

## Building

Requires a Rust toolchain, GTK4 and libadwaita development headers, and Zig —
`libghostty-vt` is Zig source that is compiled during the build.

Fedora ships Zig 0.16, which is too new; fetch 0.15.2 from ziglang.org and put
it on `PATH`.

```sh
sudo dnf install rustup gtk4-devel libadwaita-devel gtksourceview5-devel
rustup-init -y

cargo run --release
```

Debugging aids, all off unless set: `TUNI_DEBUG_FRAME_TIME` prints draw-time
percentiles, `TUNI_DEBUG_PTY_WRITE` logs what the terminal answers back to the
shell, and `TUNI_CAPTURE_PNG` renders the widget to a file and exits — useful
on compositors with no screenshot protocol.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
