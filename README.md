# Tuni

A native terminal workspace for Linux: terminal panes, projects, a file tree, a
git panel, an editor, and a diff viewer in one window.

Tuni is a ground-up Linux implementation of the workspace that
[egoist/kero](https://github.com/egoist/kero) built for macOS. Kero's Swift
source is read as a specification of behavior, not translated; the macOS build
is SwiftUI/AppKit over the libghostty embed API, which exists only for macOS and
iOS. Kero is GPLv3 and so is Tuni.

## Status

A window of projects and tabs. The terminal itself: keyboard input, Pango
rendering, mouse selection with word and line clicks, clipboard and bracketed
paste, SGR mouse reporting for applications that ask for it, an overlay
scrollbar that fades when idle, a cursor that blinks by the desktop's own
preference, OSC 8 hyperlinks opened with `Ctrl`+click, a configurable font with
live zoom, and Ghostty's 574 color themes, which paint the window chrome as
well as the terminal. `ls`, `vi`, and `top` all render correctly.

Around it: a sidebar of projects, each with its own strip of tabs, and inside a
tab a niri-style layout of panes. A project is named by whatever its visible
shell calls itself until you rename it, and a project directory can be pinned
for the file tree and the git panel to come. A new tab starts where the visible
one is, opens next to it, and closes when its last shell exits; a project whose
tabs are all closed stays in the sidebar until it is closed on purpose.

Closing the window writes that arrangement down, and opening it again puts it
back: the projects, their tabs, the columns and panes inside each one with the
room they had, the names that were typed, and a fresh shell in each pane's last
working directory. What those shells had printed is not restored unless it is
asked for. A settings window under `Ctrl+,` edits the font, the two themes, the
scrollback, and that last decision, writing each change to
`~/.config/tuni/config.toml` as it is made.

| Shortcut | Action |
| --- | --- |
| `Ctrl+Shift+D` / `Ctrl+Shift+E` | Split right, split down |
| `Ctrl+Shift+W` | Close the pane — and the tab, when it was the last one |
| `Ctrl+Alt`+arrows | Move the focus one pane in that direction |
| `Ctrl+Shift+]` / `Ctrl+Shift+[` | Next pane, previous pane |
| `Ctrl+Shift+Enter` | Show the focused pane alone, and back |
| `Ctrl+Alt+Shift`+arrows | Grow or shrink the focused pane |
| `Ctrl+Alt+=` | Give every pane the same room |
| Drag a pane's grip onto another | Move it to that pane's left, right, top, or bottom |
| Drag the gap between panes | Move the divider |
| `Ctrl+Shift+T` | New tab |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next tab, previous tab — also `Ctrl+Page Down` / `Ctrl+Page Up` |
| `Alt+1` … `Alt+9` | Jump to a tab; `Alt+9` is the last one |
| `Ctrl+Shift+N` | New project |
| `Ctrl+Alt+Page Down` / `Ctrl+Alt+Page Up` | Next project, previous project |
| `Ctrl+Shift+1` … `Ctrl+Shift+9` | Jump to a project |
| `F9` | Show or hide the sidebar |
| `Ctrl+,` | Preferences |
| `Ctrl+Shift+C` / `Ctrl+Shift+V` | Copy selection, paste |
| Middle click | Paste the primary selection |
| `Ctrl+Shift+A` | Select everything, scrollback included |
| Drag, double click, triple click | Select by character, word, line |
| `Alt`+drag | Block selection |
| `Shift`+click | Select even while an application is tracking the mouse |
| `Ctrl`+click | Open the hyperlink under the pointer |
| `Shift+Page Up` / `Shift+Page Down` | Scroll the viewport by a page |
| `Shift+Home` / `Shift+End` | Jump to the top of the scrollback, or the bottom |
| `Ctrl+plus` / `Ctrl+minus` / `Ctrl+0` | Font a point larger, smaller, back to the configured size |

From here the work runs to full parity: the file tree, the git panel, the
editor, the diff viewer, the command palette, and packaging.

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
| `tuni-core` | Portable models: config, themes, projects, panes, session, git. No GTK. |
| `tuni-gtk` | GTK4 + libadwaita widgets, window, actions, keybindings. |

The upstream C API is explicitly pre-1.0 and expected to change, which is why
the dependency is pinned to a commit and why every call to it goes through
`tuni-vt`.

Projects and tabs are a plain model in `tuni-core`, but the tab strip is an
`AdwTabView`, and a tab strip already knows things the model would otherwise
have to be taught: that a new tab belongs next to the current one, that closing
one falls to a neighbor, that a drag ends where it was dropped. So the widget is
the record of tab order and selection, and it reports one way into the model —
never the reverse. Two records that can disagree need a guard against drift;
one record needs nothing.

Panes are the other way around, for the same reason. No GTK widget implements a
niri layout — a tab is a row of columns, a column is a stack of panes, and
nothing nests deeper than that — so the model in `tuni-core` is the record and
the widget draws whatever it says. `GtkPaned` was the alternative and it is the
wrong shape: it holds two children and nests to hold more, which turns a flat
row of four into a tree of three, and dragging one divider then moves panes that
were nowhere near the pointer. Sizes are weights rather than pixels, and the
arithmetic that turns weights into a row of tiles lives in the model beside its
tests, not in a widget where nothing can reach it.

The saved session is two files rather than one. `session.json` is the shape of
the window — projects, tabs, columns, panes, weights, focus, names — and it is
small, cheap to write, and safe to keep: a directory path and a title are what
any shell already puts in the window title. `history.json` is what the
terminals had printed, and it is a different kind of thing entirely, because a
scrollback holds whatever was on screen when the window closed. So it is
written only when it has been asked for, capped at 500 lines a pane the way
kero caps it, and deleted outright the moment the setting is turned back off.
Both are written beside themselves and renamed into place, so a crash mid-write
costs the previous session rather than both. Every field the snapshot reads is
optional and every unreadable tab is skipped, which is what lets a file written
by an older build still open — and `TUNI_SESSION=0` turns the whole mechanism
off, restore and save alike.

Ghostty's theme catalog is vendored under `data/themes` and baked into the
binary at build time, so a fresh checkout runs with all 574 and a packaged
build needs no data directory beside the executable. The same theme drives the
window: libadwaita builds its stylesheet out of named colors, so overriding
those recolors the header bar and dialogs along with the terminal.

Text is drawn with Pango rather than a GPU glyph atlas. Pango brings fontconfig
fallback, subpixel antialiasing, and input methods, and text quality is what a
terminal is judged on.

The cell width is measured from real glyphs rather than read off the font's own
`approximate_char_width` hint — the widest printable ASCII character, because a
face that calls itself monospace is not obliged to prove it — and it is rounded
to whole pixels, with glyph positions rounded to match. Both are what keeps the
eightieth column drawn where the eightieth background was filled. Every run is
placed on the row's baseline rather than in its own box, so a character that
falls back to another face lands on its column and on the line instead of near
them. Runs of plain ASCII share one layout; anything wide, combining, or
borrowed from a fallback face gets a layout of its own. Ligatures are off by
default, which is a deliberate divergence from Ghostty and kero: a ligature is
one glyph where the terminal still counts several cells. `TUNI_LIGATURES=1`
turns them back on.

A letter key is named by where it is rather than by what it types: the hardware
keycode a GDK event carries is the XKB one, which is the evdev scancode plus
eight on Wayland and X11 alike, and that is what the Kitty keyboard protocol
reports. The table is Ghostty's own, so the key where a US keyboard has Q is
`KeyQ` on an AZERTY layout too, and `Ctrl+Shift+C` is under the same finger on
every layout. Every other key is named by what the keymap says it is, because
that is the only way `caps:swapescape` and its like can work at all — XKB grants
them by handing out a different keyval for the same scancode. The line between
the two is the W3C's writing system keys, which is where Ghostty draws it. What
a key types with nothing held down travels with the event as well, so `Ctrl`+`С`
on a Cyrillic layout arrives as `Ctrl+C`, and the modifiers the keymap already
folded into the character are the ones GDK says it consumed rather than a guess
at Shift.

A hyperlink is whatever the program holding the PTY says it is, which over ssh
is not the person at the keyboard. So `Ctrl` has to be held before one lights up
at all, the press only opens on release and only if the same link is still
underneath, and the URI is handed to the desktop only when it carries no control
character and its scheme is one of `http`, `https`, `mailto`, `ftp`, `ftps`, or
a `file://` that names this machine.

## Configuration

`~/.config/tuni/config.toml`, written by the settings window and readable
without it. Only what differs from a default is written, so an empty file and a
missing one mean the same thing. The names are Ghostty's where Ghostty has one.

| Key | Default | Meaning |
| --- | --- | --- |
| `theme` | `"system"` | `system`, `light`, or `dark` — which of the two themes below is in use |
| `theme-light` | `"GitHub Light Default"` | Any of the 574 bundled themes |
| `theme-dark` | `"GitHub Dark Default"` | |
| `font-family` | `"JetBrains Mono"` | |
| `font-size` | `11` | Points |
| `font-ligatures` | `false` | |
| `line-height` | `0` | Extra pixels between rows |
| `cursor-blink` | `true` | And then only if the desktop blinks its own cursor, and only while the program running has no opinion |
| `terminal.scrollback-lines` | `10000` | Lines kept above the screen |
| `terminal.restore-history` | `false` | Whether a restored pane replays what it had printed |

The session itself lives under `~/.local/share/tuni`.

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

Debugging aids, all off unless set, and none of them written back to the
configuration file: `TUNI_THEME` names one of the bundled themes for the run
and `TUNI_FONT` a font the way Pango writes one (`"JetBrains Mono 13"`), with
`TUNI_LIGATURES=1` to let them fire; `TUNI_SESSION=0` neither restores the
saved session nor overwrites it; `TUNI_DEBUG_FRAME_TIME` prints draw-time
percentiles;
`TUNI_DEBUG_PTY_WRITE` logs what the terminal answers back to the shell; and
`TUNI_CAPTURE_PNG` renders the widget to a file and exits — useful on
compositors with no screenshot protocol.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
