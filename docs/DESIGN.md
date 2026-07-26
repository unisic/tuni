# Tuni in detail

Everything the window does, why it is built the way it is, and every key it
answers to. [README.md](../README.md) is the short version.

A native terminal workspace for Linux: terminal panes, projects, a file tree, a
git panel, a session inspector, an editor, and a diff viewer in one window.

Tuni is a ground-up Linux implementation of the workspace that
[egoist/kero](https://github.com/egoist/kero) built for macOS. Kero's Swift
source is read as a specification of behavior, not translated; the macOS build
is SwiftUI/AppKit over the libghostty embed API, which exists only for macOS and
iOS. Kero is GPLv3 and so is Tuni.

## Status

A window of projects and tabs. The terminal itself: keyboard input, Pango
rendering, mouse selection with word and line clicks, clipboard and bracketed
paste, SGR mouse reporting and focus reporting for applications that ask for
them, an overlay
scrollbar that fades when idle, a cursor that blinks by the desktop's own
preference, OSC 8 hyperlinks opened with `Ctrl`+click, inline images over the
kitty graphics protocol, a configurable font with live zoom, and Ghostty's 574
color themes, which paint the window chrome as well as the terminal. `ls`,
`vi`, and `top` all render correctly.

Around it: a sidebar of projects, each with its own strip of tabs, and inside a
tab a niri-style layout of panes. A project is named by whatever its visible
shell calls itself until you rename it, and a project directory can be pinned
for the file tree and the git panel to stay on. A new tab starts where the
visible one is, opens next to it, and closes when its last shell exits; a
project whose tabs are all closed stays in the sidebar until it is closed on
purpose: by its own button, by its menu, or by a middle click anywhere on the
row, which is how a tab closes everywhere else. The sidebar and the panel are
both dragged to a width by their inner edge, and one dragged to a width keeps
it. Until then each is a fraction of the window, which is what a split view
sizes by and what an undragged one should go on doing.

On the other side, a panel under `Ctrl+Shift+B` shows the directory the focused
shell is working in, or the project's own if one is pinned, and follows it as
the focus moves. Its Files page opens directories in place and files in a pane of
their own, and a right click opens one beside what is already there, renames,
creates, copies a path, shows a file in the desktop's file manager, moves one to
the trash, or types a `cd` into the terminal that has the keyboard.

Its Git page, under `Ctrl+Shift+G`, is the repository that directory belongs to:
the branch and how far it is from its upstream, what is in conflict, what is
staged, what has changed, and the last few commits. Files stage, unstage and
discard one at a time or all at once, a message commits what is staged or
everything, and fetch, pull, push and stash are a menu on the header, where
kero keeps them: five spelled-out buttons are wider than the panel is. Every one of them
is the `git` you would have typed, run as a process, so the repository ends up
in the state the command line would have left it in — including the reflog entry
to undo it by. Discarding asks first, and what git would delete outright goes to
the desktop's trash instead. The state of a file is spelled out, never carried
by color alone: the two porcelain letters are on the row, and what they mean is
in its tooltip.

Its Info page, under `Ctrl+Shift+I`, is the session itself: the shell and its
process id, the directory it is working in, the directory the other two pages
anchor to and whether that was pinned or worked out, every process running under
that shell with its share of a core and its resident memory, and every TCP port
those processes are listening on. A port is a button that opens
`http://localhost:<port>` in the browser, which is the reason to look — a dev
server prints its port and then scrolls away. A process can be asked to quit, or
made to. Nothing here shells out: it is `/proc`, read on a worker thread while
the page is showing and no more often than that.

When a coding agent is running under that shell, the page grows an Agent section
for it. Claude Code, Codex and OpenCode each keep a record of their own turns on
disk, and that is where every figure comes from: what the session working in this
directory has spent, split into fresh input, output and what came back from the
cache, which is most of a long conversation and costs a fraction of the rest.
Codex says how full the model's context is and how much of each plan window is
gone, so those get a bar apiece; Claude Code's limits are counted over five hours
across every directory it worked in, so that total is a row of its own. Tokens,
never prices. And no account is asked anything: no login, no key, nothing over
the network.

A pane holds a file as readily as it holds a shell. Opening one from either page
puts it where a terminal would have gone — syntax highlighting for whatever
GtkSourceView recognizes, line numbers, undo, find and replace, and `Ctrl+S`.
A file with unsaved edits carries a dot in its header and on its tab, and
nothing that would throw those edits away — closing the pane, the tab, or the
window — happens without asking first. A picture opens in the pane too, and
anything that is neither, or is past the 5 MiB the editor will read, says so and
offers to hand the file to the desktop.

A row on the Git page opens what changed in that file rather than the file, in a
pane of the same kind: the working tree against the index, or — for a row that is
already staged — the index against HEAD. Each hunk is headed by its `@@` line and
how much it adds and removes, both sides' line numbers run down the margin, and
the words that actually differ within a changed line are picked out of it. One
button swaps the inline reading for the two sides beside each other. The plus on
a hunk stages that hunk alone, and on the staged side takes it back out again;
either way it is a patch handed to `git apply`, so the index ends up where the
command line would have left it. What the shell beside the pane does to the file
lands in the diff on its own.

Three ways to get somewhere. `Ctrl+Shift+F` opens a find bar over the terminal:
every match on screen and in the scrollback is highlighted as you type, the
tally counts them, and Enter walks forward while Shift+Enter walks back,
scrolling each match into view. It follows the keyboard rather than the mouse —
press it while a file pane has the focus and it is that file being searched, in
GtkSourceView's own bar — and `F3` steps whichever of the two is open. Text
already selected can be handed to the bar as the term, which is the quickest
way to look for something a command just printed. `Ctrl+Shift+P` opens the command palette —
everything the window can do, by name, with the keys that do the same thing
beside it, and under that every terminal in the workspace, so a shell in another
project is one query away rather than a hunt through tabs. Holding `Ctrl` and
pressing `Tab` brings up the tab switcher: a card per tab with a picture of what
is in it, in the order they were last worked in, so one press and release goes
back to where you just were.

Closing the window writes that arrangement down, and opening it again puts it
back: the projects, their tabs, the columns and panes inside each one with the
room they had, the names that were typed, and a fresh shell in each pane's last
working directory. What those shells had printed is not restored unless it is
asked for. A second window can be opened for a scratch project on another
screen; it starts empty and never writes the session, so the arrangement that
comes back is the one the first window was left in. A settings window under
`Ctrl+,` edits the font, the two themes, the cursor, the scrollback, the shell
to run, whether a file pane wraps its long lines, and that last decision,
writing each change to `~/.config/tuni/config.toml` as it is made. There is no
OK button; a line of terminal beside the font rows is drawn in whatever has
just been chosen, since that is the only way a font or a palette is actually
picked. The family is chosen from what this machine has, asked of the font map
rather than typed: the monospaced faces first and every other family after,
since fontconfig's `spacing` is a label plenty of coding fonts are missing and a
list that hides them reads as a list of what is installed. The row says under
its title when the family picked is proportional, and when it is not installed
at all. Whatever is configured stays in the list either way, rather than the
dialog quietly selecting something else. It also says when no Nerd Font is
installed, which is the whole of why a prompt's icons come out as boxes.

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
| `Ctrl+Page Down` / `Ctrl+Page Up` | Next tab, previous tab |
| Hold `Ctrl`, press `Tab` | The tab switcher: cards for every tab, most recently used first. `Shift+Tab` walks back, `Escape` cancels, letting `Ctrl` go switches |
| `Alt+1` … `Alt+9` | Jump to a tab; `Alt+9` is the last one |
| `Ctrl+Shift+N` | New project |
| `Ctrl+Alt+Page Down` / `Ctrl+Alt+Page Up` | Next project, previous project |
| `Ctrl+Shift+1` … `Ctrl+Shift+9` | Jump to a project |
| `F9` | Show or hide the sidebar |
| `Ctrl+Shift+B` | Show or hide the panel |
| `Ctrl+Shift+G` | Open the panel on the repository, or close it when it is already there |
| `Ctrl+Shift+I` | Open the panel on the session: processes and ports |
| `Ctrl+,` | Preferences |
| `Ctrl+S` | Save the file in the focused pane |
| `Ctrl+Shift+P` | The command palette |
| `Ctrl+Shift+F` | Find in the pane that has the keyboard; Enter and `Shift+Enter` walk the matches, `Escape` closes |
| `F3` / `Shift+F3` | Next match, previous match |
| `Ctrl+Shift+H` | Find and replace in the file pane that has the keyboard |
| `Ctrl+Shift+K` | Clear the terminal: the screen, the scrollback behind it, and a fresh prompt |
| `Ctrl+F` / `Ctrl+H` | Find, find and replace — in a file pane only |
| `Ctrl+G` / `Ctrl+Shift+G` | Next match, previous match — in a file pane only |
| `Ctrl+Shift+C` / `Ctrl+Shift+V` | Copy selection, paste |
| Middle click | Paste the primary selection |
| `Ctrl+Shift+A` | Select everything, scrollback included |
| Drag, double click, triple click | Select by character, word, line |
| `Alt`+drag | Block selection |
| `Shift`+click | Select even while an application is following the pointer |
| `Ctrl+Shift+M` | Take the mouse back from applications entirely, and hand it over again |
| `Ctrl`+click | Open the hyperlink under the pointer |
| `Shift+Page Up` / `Shift+Page Down` | Scroll the viewport by a page |
| `Shift+Home` / `Shift+End` | Jump to the top of the scrollback, or the bottom |
| `Ctrl+plus` / `Ctrl+minus` / `Ctrl+0` | Font a point larger, smaller, back to the configured size |

New Window, Show Files and Use Selection for Find have no key of their own —
they are in the menu and in the palette, which is where anything without a
shortcut can be reached by name.

A pane nobody is looking at can still say something. The bell marks its tab as
wanting attention, and when the window is not the focused one it arrives as a
desktop notification named after the terminal that rang it. So does anything a
program asks for outright — OSC 9, OSC 777, and kitty's OSC 99, which is what
build tools and `notify-send`-alikes emit — and a progress report, OSC 9;4,
draws a hairline along the bottom of the pane that printed it: blue while it
runs, red when it failed, amber when it is paused, and the full width when the
program only knows that it is busy. It clears when the program says so, or
after fifteen seconds of nothing further. Each pane's notification replaces its
own rather than stacking one banner per line of output, and focusing the pane
withdraws it.

An image printed to a pane is drawn as an image. Tuni speaks the kitty graphics
protocol, so `timg`, `chafa -f kitty`, matplotlib's kitty backend and anything
else that transmits one lands in the pane at the size, position and stacking
order it asked for — under the text, over it, or under the cell backgrounds —
and scrolls with the text it was printed beside.

That is kero's behavior, less the two pieces of it that are macOS by nature:
the Sparkle auto-updater, which a package manager stands in for, and the window
blur, which Wayland has no protocol for.

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

The file tree is a flat list rather than a tree of nodes. What a list view
wants is rows, and what an expandable tree costs is a second structure that has
to be kept in step with them; a list of `(name, path, depth)` rebuilt from the
set of open directories is the same information with nothing to keep in step.
Only open directories are read, each read is sorted directories-first and then
naturally, so `file10` follows `file9`, and depth is capped at 32 so a symlink
that points at its own parent runs out rather than forever. `.git` is hidden
and every other dotfile is shown, dimmed — kero hides only `.git` too, and a
file tree that disagrees with `ls` is worse than one that shows build output.
The disk is re-read every two seconds instead of watched with inotify, which is
what kero does: a watch per open directory costs a descriptor and a debounce,
and a cached directory read costs nothing measurable. The panel redraws only
when the rows actually differ, so most of those reads change nothing on screen.
Deleting is `GFile.trash`, which is recoverable, and revealing a file goes
through `GtkFileLauncher` so it reaches the portal in a sandbox and the session
bus outside one. Whether the panel was showing, which page it was on, and how
wide it was dragged are part of `session.json`.

The repository is read by running `git`, not by linking a library, because the
question the panel answers is what the command line would say — the same
config, the same hooks, the same includes, the same worktree rules. Every one of
those processes runs off the main loop and comes back as a future, with a
generation number attached: a status that arrives after the shell has `cd`'d
into another repository is dropped rather than drawn. Reads carry
`GIT_OPTIONAL_LOCKS=0` so watching a repository cannot fight a build for the
index lock, `GIT_TERMINAL_PROMPT=0` so a fetch that wants a password fails
instead of hanging on a terminal that is not there, and `LC_ALL=C` because the
output is parsed. Status is `--porcelain=v2 -z`, which is the format that
survives a filename with a newline in it. Only one action runs at a time; the
working tree is re-read from scratch afterwards, since a commit moves the
history and a checkout moves everything. What that model decides — whether a
commit is possible, which paths a discard has to restore and which it has to
delete — lives in `tuni-core` with tests against a real repository, and the
widget only draws the answer.

What Info knows, it reads out of `/proc` rather than out of `ps` and `lsof`.
Both of those read `/proc` themselves, `lsof` is frequently not installed, and a
pair of processes every two seconds is a cost worth not paying. The shell's own
children are found by walking every `/proc/<pid>/stat` once and breadth-first
descending from the shell's process id, which also catches a grandchild — the
`node` a package script started. A listening port is `/proc/net/tcp` and
`/proc/net/tcp6` filtered to state `0A`, which yields socket inodes; those are
matched against the `socket:[…]` links in each process's `fd` directory, which
is what turns a port into the process holding it. CPU is the lifetime average
`ps` reports rather than an instantaneous sample: ticks over `_SC_CLK_TCK` over
how long the process has been alive. All of that parsing is pure functions over
the exact text a kernel writes, tested in `tuni-core` without a process tree to
point them at, and the reads happen on a worker thread with the same generation
stamp the git panel uses.

The agent numbers are read the same way, off files the agent wrote for its own
purposes. Claude Code and Codex append a line of JSON per turn; OpenCode keeps a
row per session in SQLite, opened read-only so that a database being written to
is safe to read. Which of those sessions belongs to the pane is settled by the
working directory: Claude Code puts it in the transcript directory's name, Codex
records it in the first line of a rollout, OpenCode stores it in a column. A
transcript grows to tens of megabytes over an afternoon and the page polls every
couple of seconds, so the reader remembers where in each file it stopped and what
the file came to by then; a poll costs the bytes appended since the last one,
which is milliseconds against a first read's hundred. A line that is half written
when the read reaches it is left for next time, because the agent is appending
while this reads. Claude Code writes several lines per reply and repeats the
usage on a retry, so a request id already counted is counted once.

The editor's keys belong to the pane rather than to the window, and that is the
whole reason it can share a window with terminals: `Ctrl+S` is flow control to a
shell and `Ctrl+F` is a page forward in `less`, so binding either one globally
would break the pane next door. They are a shortcut controller on the editor
widget, scoped locally, which means they exist exactly while a file has the
keyboard. Saving writes a neighbouring file and renames it onto the target,
which is atomic on every filesystem Linux ships: an interrupted save costs the
new text, never the old. Two things follow from renaming rather than truncating
and both are handled — a symlink is resolved first, so it is still a symlink
afterwards, and the old file's permissions are copied onto the new one, so a
script stays executable. What the session remembers is the cursor offset and
nothing else. Unsaved text is not written to a cache to be recovered later; what
is on disk is the file, and the question before closing is asked while there is
still someone there to answer it. A file is text if it decodes as UTF-8 and
binary if it does not, which is a decision the bytes make rather than the
extension.

A diff is `git diff` and its output parsed, for the same reason the panel is:
what the pane shows has to be what the command line would print, renames and
`diff.algorithm` and all. An untracked file has nothing in the index to compare
against, so it is `--no-index` against `/dev/null`, which is also the one form
of diff that answers with an exit status of one when it worked. Staging is the
hunk as it was parsed, handed back to `git apply --cached` on standard input,
and unstaging is that same patch with git's own `--reverse` — a patch written
backwards by hand is how a staging tool loses a line. Within a changed line the
words that moved come from `similar`, run only on pairs of lines that a hunk
already puts opposite each other; a diff of a generated file draws 4000 lines
and then says how much it left. The pane re-reads on the same two-second timer
as the panel, so a change made by the shell beside it appears without asking,
and a read that comes back byte-for-byte identical is dropped rather than
redrawn, which is what keeps the scroll position under someone reading.

Ghostty's theme catalog is vendored under `data/themes` and baked into the
binary at build time, so a fresh checkout runs with all 574 and a packaged
build needs no data directory beside the executable. The same theme drives the
window: libadwaita builds its stylesheet out of named colors, so overriding
those recolors the header bar and dialogs along with the terminal.

Both header bars drop the `icon` element out of whatever decoration layout the
desktop asks for, and follow the setting so a later change keeps dropping it.
KDE's default layout puts one there, and the window icon it draws is not a
button. It is a picture of the application that answers nothing when clicked,
and that shows the theme's missing-icon glyph on any machine running the binary
without having installed its icon.

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

Box drawing and block characters, U+2500 to U+259F, are the exception: they are
drawn from the cell's own measurements rather than taken from the font. A
designer's `█` is as tall as it was drawn and the cell is as tall as the line
height says, so a font's answer leaves a column of blocks striped and a frame
with holes at its corners. Ghostty calls its version of this the sprite font,
and the geometry here is that one ported: light lines at the face's underline
thickness, heavy at twice it, arms that stop level with whatever crosses them,
and the rounded corners stroked as curves.

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

An application that asks for the mouse gets it, and Shift is how the person at
the keyboard takes it back. That is the convention, and it is a poor deal when
the application asked for less than it is being given. Button-event and
any-event tracking follow the pointer, so a drag inside one is the thing it
asked for and selecting there needs Shift. The older click-only modes hear
about buttons and nothing else, and a drag handed to a program that cannot be
told the pointer moved is a drag nobody receives while a selection nobody made
is lost. So there the press waits: leaving the cell makes it a selection, and
lifting inside the cell makes it a click, which the application then gets
whole.

That leaves the programs that do follow the pointer, and there is no reading
of a drag that serves both sides: it is either theirs or it is a selection.
Shift is one answer and `mouse-reporting` is the other, the same key Ghostty
uses for the same setting. Turned off, no application is given the mouse
however loudly it asks, and every drag selects. `Ctrl+Shift+M` turns it off
and on again, because whether a program should have the mouse is a thing that
changes several times an hour.

The keyboard arriving and leaving is news as well, to the programs that asked
for it with mode 1004: an editor rereads a file that changed while it was away,
a multiplexer stops drawing a cursor nobody is typing at. A pane is a window of
its own here, the way Ghostty reports per surface, so handing the keyboard to
the split beside it is a departure for the pane that had it.

A hyperlink is whatever the program holding the PTY says it is, which over ssh
is not the person at the keyboard. So `Ctrl` has to be held before one lights up
at all, the press only opens on release and only if the same link is still
underneath, and the URI is handed to the desktop only when it carries no control
character and its scheme is one of `http`, `https`, `mailto`, `ftp`, `ftps`, or
a `file://` that names this machine.

Notifications are read out of the PTY stream a second time rather than out of
the terminal state, because at the pinned commit `libghostty-vt` recognizes
OSC 9, OSC 99 and OSC 777 without handing their payloads back — the callbacks
it offers are the title, the working directory, the bell, and the clipboard.
So `tuni-vt` runs a small state machine over the same bytes it feeds the
parser, and everything it learns arrives as part of the same `Effects` the rest
of a write produces. It is deliberately incurious: a DCS, APC, PM or SOS body
is skipped whole, so a tmux passing an inner program's OSC 9 through to the
outer terminal cannot ring the window it is running in, and a payload is
abandoned past 8 KiB so a program printing an escape it never terminates costs
nothing to ignore.

Images are the half of the kitty graphics protocol the library leaves to the
embedder: it parses the transmissions, stores the images and works out where
each placement belongs, and putting pixels on a surface is ours. So every frame
asks it for the placements — geometry alone, recomputed each time because
scrolling moves an image without touching the storage — and uploads a
`GdkTexture` only when the sixteen most recent miss. The cache key is the image
id together with the storage's generation stamp, so a plot retransmitted under
the same id is a new texture rather than the previous frame's. Drawing happens
in three passes because the protocol defines three layers, and a placement's
`z` says which: under the cell backgrounds, under the text, or over everything.
Two decisions are ours rather than the library's, since it makes neither by
default: images are capped at 64 MiB a terminal, oldest evicted first, and PNG —
which is what nearly every program transmits — is decoded by a callback that
this crate installs, with a ceiling on the decoded size, because the bytes come
from whatever is on the other end of the PTY. Virtual placements, kitty's
unicode-placeholder form, are stored and skipped: the C API reports no position
for one, and guessing is worse than not drawing.

## Installing

`make install` puts down the binary, the desktop entry, the AppStream data and
the icons; nothing else is needed at runtime, since the themes are baked into
the executable.

```sh
sudo make install PREFIX=/usr
```

`make install-data` is the same thing without the binary, for a Tuni run out of
the build directory: the window names its icon rather than carrying one, so the
desktop draws a fallback until the entry and the icon are somewhere the
compositor looks — `make install-data PREFIX=$HOME/.local`.

`packaging/` holds a Flatpak manifest and an RPM spec that both call into that
same target, and [packaging/README.md](../packaging/README.md) says what each one
needs — mainly Zig 0.15.2 and, unless it is given the offline paths, network
during the build. `make zig` fetches that Zig into `~/.local`, because every
distribution's package has moved past the version the pinned Ghostty commit
builds with.

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
| `cursor-style` | `"block"` | `block`, `bar`, `underline`, or `block_hollow`, the shape until the program running asks for another |
| `background-opacity` | `1` | How much of the desktop shows through the window, down to `0.2` |
| `background-blur` | `false` | Whether the compositor blurs what shows through, over `ext-background-effect-v1`. A number is read as Ghostty's blur intensity, which KWin decides for itself: any of them above zero means the same thing here |
| `window-padding-x` | `0` | Pixels of nothing between the sides of the window and the grid, up to `40` |
| `window-padding-y` | `0` | The same above and below |
| `copy-on-select` | `false` | Whether releasing a selection puts it on the clipboard |
| `mouse-reporting` | `true` | Whether a program that asks for the mouse gets it. Off keeps every drag for selecting |
| `bell` | `true` | Whether `\a` rings the desktop's own bell |
| `command` | `""` | The shell to run; empty means the login shell |
| `terminal.scrollback-lines` | `10000` | Lines kept above the screen |
| `terminal.restore-history` | `false` | Whether a restored pane replays what it had printed |
| `editor.wrap-lines` | `false` | Whether a file pane folds a long line rather than scrolling sideways |
| `window.auto-hide-tab-bar` | `false` | Whether the tab bar goes away while a window has one tab |

A Ghostty configuration can be pasted in whole. Values do not need the quotes
TOML would want, `theme` may name a theme instead of an appearance, in either
the `theme = Catppuccin Mocha` or the `theme = light:a,dark:b` form, and a key
Ghostty repeats for its fallbacks, `font-family`, is read from the first one.
`cursor-style-blink` is read as `cursor-blink`, and `copy-on-select =
clipboard` as `true`. Keys for what tuni does not do are ignored rather than
refused, so what is left of the file keeps working.

The session itself lives under `~/.local/share/tuni`. A file pane comes back
holding the file it held, with the cursor where it was left; unsaved edits are
not part of that, since a file is what is on disk.

## Building

Requires a Rust toolchain, GTK4, libadwaita and SQLite development headers, and
Zig, since `libghostty-vt` is Zig source that is compiled during the build.

Fedora ships Zig 0.16, which is too new; fetch 0.15.2 from ziglang.org and put
it on `PATH`.

```sh
sudo dnf install rustup gtk4-devel libadwaita-devel gtksourceview5-devel \
    sqlite-devel
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

GPL-3.0-or-later. See [LICENSE](../LICENSE).
