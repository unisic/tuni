# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Tuni is a GTK4/libadwaita terminal workspace for Linux, written in Rust, with
Ghostty's `libghostty-vt` doing the terminal emulation. Behavior follows
[kero](https://github.com/egoist/kero), which is read as a specification rather
than translated.

## Commands

```sh
make zig                  # fetch Zig 0.15.2 into ~/.local; needed once before any build
cargo build --release     # or `make build`, which warns if the wrong zig is on PATH
cargo run --release
make check                # fmt --check, clippy -D warnings, test, desktop/AppStream validation
```

Tests:

```sh
cargo test                                    # whole workspace
cargo test -p tuni-core                       # one crate
cargo test -p tuni-vt --test feed             # one integration test file
cargo test -p tuni-core a_rename_carries      # one test by name substring
cargo test -p tuni-core -- --nocapture
```

Cost profiles and repros are examples, not tests:

```sh
cargo run --release -p tuni-vt --example feed_cost
cargo run --release -p tuni-vt --example search_cost
cargo run --release -p tuni-gtk --example resize_repro
scripts/throughput.sh 200        # needs target/release/tuni; compares against other terminals
```

Installing needs the Makefile, not cargo: `sudo make install PREFIX=/usr`. It
puts down the binary, the desktop entry, the AppStream metadata and two icons,
and every recipe under `packaging/` calls into it so there is one list of
installed files.

### The Zig pin

`libghostty-vt` is Zig source compiled during the build, so which Zig works is
decided by the Ghostty commit `libghostty-rs` pins. **0.15.2** is that
toolchain, and what CI, the RPM spec and the Flatpak manifest all use. Current
distributions ship 0.16, which the pinned commit rejects, so building that way
also means a newer Ghostty: `make build-next` and `make test-next` fetch both
and pass the checkout in as `GHOSTTY_SOURCE_DIR`. Releases come from the pinned
pair. If you move either version, four files have to agree: `Makefile`,
`.github/workflows/ci.yml`, `packaging/tuni.spec`, `packaging/dev.unisic.Tuni.yml`.

### Where the binary lands

`.cargo/config.toml` is gitignored and, on this checkout, redirects
`build.target-dir` to `~/.cache/tuni-target`, because the tree lives on exFAT
and the `libghostty-vt` build creates a symlink inside the target directory.
Ask `cargo metadata --format-version 1 --no-deps` for `target_directory` rather
than assuming `./target`.

## Architecture

Four crates, and the dependency arrow only points one way:

| Crate | Responsibility |
| --- | --- |
| `tuni-vt` | Facade over `libghostty-vt`. Nothing above this line imports it. |
| `tuni-pty` | Shell process, PTY, reader thread, window resize. |
| `tuni-core` | Portable models: settings, themes, projects, panes, session, git, ssh, sftp, lsp, syntax, `/proc`. No GTK. |
| `tuni-gtk` | Widgets, window, actions, keybindings. The binary is `tuni`. |

The upstream C API is pre-1.0 and expected to change, which is why the
dependency is pinned to a rev in the workspace `Cargo.toml` and why every call
to it goes through `tuni-vt`.

### Rules that span files

**Logic in `tuni-core`, drawing in `tuni-gtk`.** Whether a commit is possible,
which paths a discard restores, how weights become a row of tiles, what
`/proc/net/tcp` says: all of it lives in `tuni-core` beside its tests, and the
widget draws the answer. A calculation that ends up in a widget is unreachable
from a test.

**One record per thing.** Tab order and selection live in the `AdwTabView` and
report one way into the model, never the reverse, because a tab strip already
knows what a new tab neighbors and where a drag ended. Pane layout is the
opposite: no GTK widget implements a niri layout, so the `tuni-core` model is
the record and `tiles.rs` draws whatever it says. Two records that can disagree
need a guard against drift; one record needs nothing.

**`tuni-vt` is `!Send` and lives on the main thread.** PTY reads arrive from
`tuni-pty`'s reader thread as byte buffers over an async channel. Feeding the
terminal returns an `Effects` struct that the widget drains: PTY writes,
bell, title change, clipboard requests, notifications, progress.

**Anything that can block goes through `gio::spawn_blocking` with a generation
counter.** Git commands, `/proc` sweeps, `ssh -G`, SFTP requests, agent
transcript reads. The counter is bumped when the subject changes, and an answer
that arrives stamped with an older one is dropped rather than drawn, so a status
for the repository the shell just left never reaches the panel.

**External tools are driven as processes, not linked as libraries.** Git is
`git`, so the panel shows what the command line would say, with the same config,
hooks and includes. SSH is OpenSSH, so an alias resolves the way `ssh -G` says
and no credential enters this process. Reads carry `GIT_OPTIONAL_LOCKS=0`,
`GIT_TERMINAL_PROMPT=0` and `LC_ALL=C`; everything ssh-related that is not a
pane carries `BatchMode=yes` and `SSH_ASKPASS_REQUIRE=never`. The pane is the
exception on purpose: it is the one thing in the window that answers an
interactive prompt correctly.

**Tuni holds no secret.** Not a password, not a passphrase, not a key. Anything
that would collect one is either a command typed onto a shell prompt for the
user to read and run, or it does not exist. Treat a proposal that puts a secret
in this address space as a security bug rather than a feature.

**Writes are atomic.** Editor saves, `session.json`, `hosts.conf`, SFTP
downloads: write beside the target, then rename onto it. A save resolves a
symlink first and copies the old permissions across, so a script stays
executable.

**Session restore is forgiving by construction.** Every field the snapshot reads
is optional and an unreadable tab is skipped, which is what lets a file written
by an older build still open. `TUNI_SESSION=0` turns save and restore off.

**Themes are baked in at build time.** `crates/tuni-core/build.rs` turns
`data/themes/` into a sorted table of `include_str!`, so a fresh checkout runs
with all 574 and an installed build needs no data directory. Parsing stays at
runtime.

### GTK conventions

Widgets are GObject subclasses in the usual shape: `mod imp` with the state,
then `glib::wrapper!`. `window.rs` owns everything the window does and is the
largest file in the tree; `main.rs` is only the process and the `ACCELS` table,
which is the one place a keyboard shortcut is spelled.

Actions are grouped by who owns them: `win.` on the window, and `editor.`,
`diff.`, `hosts.` on the pane showing one. The editor's own keys are a shortcut
controller scoped to the editor widget rather than window accelerators, because
`Ctrl+S` is flow control to a shell and `Ctrl+F` is a page forward in `less`.

### Debug and capture

A debug capture renders the window to a PNG and quits, driven entirely by
environment variables so it costs nothing in a normal run. This is how a change
is checked in the real app:

```sh
TUNI_CAPTURE_PNG=/tmp/shot.png TUNI_CAPTURE_INPUT=$'ls\n' \
TUNI_CAPTURE_DELAY_MS=1500 cargo run --release
```

`TUNI_CAPTURE_WIDGET=window|active` decides whether the chrome is in the shot.
The rest of the family drives one part each: `TUNI_CAPTURE_ACTIONS` (comma
separated action names, one step apart), `_OPEN`, `_DIFF`, `_STAGE`, `_FIND`,
`_SEARCH`, `_PALETTE`, `_SWITCHER`, `_EDIT`, `_ZOOM`, `_SCROLL`, `_RESIZE`,
`_HOVER`, `_SELECT`. See `maybe_capture` in `crates/tuni-gtk/src/main.rs`.

Others: `TUNI_DEBUG_LIFETIME` prints a line per construction and destruction
with a live count per type, which is how "the closed pane should be gone" turns
into something readable. `TUNI_THEME`, `TUNI_FONT`, `TUNI_LIGATURES` override
settings for one run. `TUNI_DEBUG_FRAME_TIME`, `TUNI_DEBUG_PTY_WRITE`,
`TUNI_DEBUG_CLIPBOARD` are per-area traces.

## Conventions

Test names are sentences about behavior, not labels:
`a_rename_carries_the_path_it_came_from`, `an_absurd_size_is_ignored`,
`bel_raises_the_bell_once_per_drain`. Unit tests sit in a `mod tests` beside
the code; `tuni-vt/tests/feed.rs` and `tuni-pty/tests/hangup.rs` are the
integration tests, and the second one spawns a real shell on a real PTY.

Comments explain why a thing is the way it is, and the tree is dense with them
by design: the alternative that was rejected and the reason is usually more
useful than a description of what the code does. Match that when adding to a
file.

Commit subjects are a sentence about what changed for the user, in the
imperative, capitalized, no type prefix: "Open a menu on a right click in the
terminal", "Share one connection per host". The body is prose paragraphs
explaining why, not a bulleted inventory of edits.

## Further reading

`docs/DESIGN.md` is the long form: every feature, every keyboard shortcut, and
the reasoning behind each design decision, including the alternatives that were
ruled out. `packaging/README.md` covers the four package formats and the
offline build path a distribution would use.
