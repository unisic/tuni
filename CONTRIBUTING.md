# Contributing

Bug reports and patches are both welcome. This file is the short version of
what a change has to satisfy; [docs/DESIGN.md](docs/DESIGN.md) is the long form
and explains why each piece is built the way it is.

## Building

The dependencies and the `make zig` step are in the
[README](README.md#building). Zig **0.15.2** is not optional: `libghostty-vt`
is Zig source compiled during the build, and the Ghostty commit pinned in
`Cargo.toml` does not build against the 0.16 your distribution ships. To work
against the newer pair instead, `make build-next` and `make test-next` fetch
both. If you move either version, four files have to agree: `Makefile`,
`.github/workflows/ci.yml`, `packaging/tuni.spec`,
`packaging/dev.unisic.Tuni.yml`.

## Before opening a pull request

```sh
make check
```

That is `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, and the
desktop-entry and AppStream validation - the same things CI fails on. Run it
once and the review is about the change instead of about whitespace.

## Where code goes

Four crates, and the dependency arrow only points one way: `tuni-vt` (terminal
emulation) → `tuni-pty` (shell and PTY) → `tuni-core` (portable models, no GTK)
→ `tuni-gtk` (widgets and the binary).

The rule that catches most patches is **logic in `tuni-core`, drawing in
`tuni-gtk`**. Whether a commit is possible, how weights become a row of tiles,
what `/proc/net/tcp` says: all of it belongs beside its tests in `tuni-core`,
and the widget draws the answer. A calculation that ends up in a widget is
unreachable from a test.

Three more that a review will ask about:

- **Tuni holds no secret.** Not a password, not a passphrase, not a key.
  Anything that would collect one is a command typed onto a shell prompt for
  the user to run, or it does not exist.
- **Anything that can block goes through `gio::spawn_blocking`** with a
  generation counter, so an answer for the repository the shell just left is
  dropped rather than drawn.
- **Writes are atomic**: write beside the target, then rename onto it.

## Tests

Test names are sentences about behavior, not labels -
`a_rename_carries_the_path_it_came_from`, `an_absurd_size_is_ignored`. Unit
tests sit in a `mod tests` beside the code. Non-trivial logic arrives with one.

```sh
cargo test -p tuni-core                       # one crate
cargo test -p tuni-core a_rename_carries      # one test by name
```

For a change you want to see in the running window, a debug capture renders it
to a PNG and quits:

```sh
TUNI_CAPTURE_PNG=/tmp/shot.png TUNI_CAPTURE_INPUT=$'ls\n' \
TUNI_CAPTURE_DELAY_MS=1500 cargo run --release
```

## Commits

Subjects are a sentence about what changed for the user, in the imperative,
capitalized, with no type prefix: "Open a menu on a right click in the
terminal", "Share one connection per host". The body is prose explaining why,
not a bulleted inventory of edits. Comments follow the same idea: the
alternative that was rejected and the reason is more useful than a description
of what the code does.

## License

Tuni is GPLv3, and a contribution is offered under it.
