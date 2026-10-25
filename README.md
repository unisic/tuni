<div align="center">

<img src="data/icons/hicolor/scalable/apps/dev.unisic.Tuni.svg" width="160" height="160" alt="Tuni" />

# Tuni

A native terminal workspace for Linux.

</div>

<img width="1163" height="700" alt="tuni" src="https://github.com/user-attachments/assets/f8c7e78c-5007-4ff6-8680-9b078b6279d3" />

## Features

- Split panes in a niri-style layout, grouped into projects and tabs
- File tree, git panel with hunk staging, and a source editor, in panes beside
  the shells
- Session inspector: what is running, what is listening, what a connection
  forwards
- Command palette, tab switcher, find in whichever pane has the keyboard
- The hosts `~/.ssh/config` already declares, one shared connection per machine,
  and an SFTP page for the machine a pane is on
- Kitty graphics, OSC 8 hyperlinks, desktop notifications, progress bars
- A long command finishing in a tab you are not watching says so, with what it
  was and how long it took
- Scroll a command at a time in a shell that marks its prompts, reopen the last
  five closed tabs, drag a tab or a whole project out into a window of its own
  without restarting the shells in it
- Type one command into every pane of a tab at once, and wipe the one being
  typed with `Ctrl+Shift+Backspace` whatever the shell
- A project's row takes an icon from the desktop's theme, or an emoji
- A coding agent thinking in a pane spins on its tab and on its project, and
  leaves an exclamation there when it finishes into a tab you are not watching
- Ghostty's 574 themes, which paint the window chrome as well as the terminal
- The window comes back as it was left, once "Restore the Last Session" is on;
  a fresh shell every time is the default

Rust and GTK4/libadwaita, with Ghostty's `libghostty-vt` doing the emulation.

Connecting to another machine is OpenSSH's job here, not tuni's: a host opens by
running `ssh` in a pane, so a password, a passphrase or a 2FA push is answered
where it has always been answered. Tuni stores no secret and has nowhere to put
one, which is also why hosts will never sync to a cloud account - syncing a
credential means storing one.

Tuni is a ground-up Linux implementation of the workspace
[egoist/kero](https://github.com/egoist/kero) built for macOS. Kero's Swift
source is read as a specification of behavior, not translated.

## Performance

Nineteen terminals in one run on one machine, 2026-10-25: 200 MiB of mixed
plain and SGR-colored output, best of five runs. Startup is launch until the
shell ran, throughput is the time to consume the output with startup taken
out, memory three seconds after the shell started, idle the CPU over the five
seconds after that.

| Terminal | Engine | Startup | First frame | PSS | Idle | Throughput |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| **tuni** | GTK4 + libghostty-vt | **0.005 s** | 233 ms | 47.5 MiB | 0 ms | **151.4 MiB/s** |
| foot | Wayland, CPU | 0.016 s | 21 ms | 7.3 MiB | 0 ms | 132.2 MiB/s |
| alacritty | GPU (OpenGL) | 0.045 s | 57 ms | 30.3 MiB | 0 ms | 118.0 MiB/s |
| kitty | GPU (OpenGL) | 0.113 s | 94 ms | 47.8 MiB | 0 ms | 116.8 MiB/s |
| urxvt | X11, CPU | 0.011 s | n/a | 4.7 MiB | 0 ms | 106.7 MiB/s |
| st | X11, CPU | 0.018 s | n/a | 2.8 MiB | 0 ms | 82.0 MiB/s |
| ptyxis | VTE 0.84 | 0.520 s | 525 ms | 48.2 MiB | 10 ms | 66.1 MiB/s |
| gnome-terminal | VTE 0.84 | 0.171 s | n/a | 20.4 MiB \* | 0 ms \* | 65.3 MiB/s |
| terminator | VTE 0.84 | 0.206 s | 211 ms | 43.0 MiB | 10 ms | 64.3 MiB/s |
| sakura | VTE 0.84 | 0.051 s | 67 ms | 11.6 MiB | 0 ms | 62.9 MiB/s |
| tilix | VTE 0.84 | 0.069 s | 165 ms | 40.8 MiB | 0 ms | 60.6 MiB/s |
| ghostty | GTK4 + GPU | 0.531 s | 471 ms | 83.5 MiB | 0 ms | 56.7 MiB/s |
| xterm | X11, CPU | 0.017 s | n/a | 3.4 MiB | 0 ms | 46.0 MiB/s |
| konsole | Qt | 0.125 s | 127 ms | 31.1 MiB | 0 ms | 42.5 MiB/s |
| qterminal | QTermWidget | 0.079 s | 86 ms | 18.6 MiB | 0 ms | 42.5 MiB/s |
| kero | Go + libghostty-vt | 0.025 s | n/a | 74.8 MiB \* | 90 ms \* | 42.2 MiB/s |
| wezterm | GPU (wgpu) | 0.034 s | 69 ms | 61.5 MiB | 0 ms | 25.5 MiB/s |
| lxterminal | VTE 0.84 | 0.045 s | 90 ms | 15.2 MiB | 0 ms | 20.6 MiB/s |
| xfce4-terminal | VTE 0.84 | 0.086 s | 106 ms | 17.6 MiB | 0 ms | 19.8 MiB/s |

\* read from the server that draws the window, gnome-terminal-server, and
kero's app with kerod, because what the launch started is a client that waits.

First in throughput, 14.5% ahead of foot, and first to start a shell, because
`tuni` starts it before GTK is loaded. The run was headless, in a KWin of its
own; kero, which reaches 139.4 MiB/s on the desktop and loses to tuni by a
fifth there in each of three passes, does not reach that headless.

Against 1.6.0 in the same session: the shell 5 ms after launch instead of
660, the first frame at 234 ms instead of 671, PSS 47.1 MiB instead of 59.8,
and 146.4 MiB/s instead of 112.0.

First frame and memory are mostly GTK: an empty libadwaita window on the same
machine takes 259 ms to its first frame and 41 MiB PSS with the icon theme
there, Papirus-Dark, and 103 ms and 26 MiB with the stock one.

[docs/BENCHMARK.md](docs/BENCHMARK.md) - method, environment, caveats, where
the startup and memory go, and the head-to-head with kero.

## Install

One line, which opens a menu and installs the package your distribution wants:

```sh
curl -fsSL https://raw.githubusercontent.com/unisic/tuni/main/scripts/install.sh | bash
```

The menu says which version is installed, installs or updates to the newest
one, offers an older release from the list, and removes Tuni again with or
without your settings. `-y` does the install with no menu and `--check` only
reports.

Run it again to update, or let Tuni do it: it checks the release page once per
run and offers an Update button that opens the installer in a tab, where sudo
has somewhere to ask for your password. Preferences - Session - Updates turns
the check off.

Every [release](https://github.com/unisic/tuni/releases) carries the same three
packages for installing by hand:

```sh
sudo dnf install ./tuni-*.rpm            # Fedora 44+
sudo apt install ./tuni_*_amd64.deb      # Ubuntu 26.04+
sudo pacman -U ./tuni-*-x86_64.pkg.tar.zst
```

Fedora and Arch can take it from a package manager instead, which then updates
it along with everything else:

```sh
sudo dnf copr enable deandark/Tuni && sudo dnf install tuni   # Fedora
paru -S tuni-bin    # Arch, the release's own package
paru -S tuni        # Arch, built from source
```

An AUR build leaves updates to the helper, and its update dialog says so
instead of offering the installer.

Or from source, which is what all three do:

```sh
sudo make install PREFIX=/usr
```

`packaging/` holds those three recipes and a Flatpak manifest, each calling
into that same target.

## Building

Needs a Rust toolchain, GTK4, libadwaita, GtkSourceView and SQLite headers, and
Zig 0.15.2, since `libghostty-vt` is Zig source compiled during the build.

```sh
sudo dnf install cargo rust gcc make git-core curl tar xz \
    gtk4-devel libadwaita-devel gtksourceview5-devel sqlite-devel
make zig
cargo run --release
```

That is the list CI installs, so a machine with it builds what the release
builds. Fedora's own `rust` is new enough; on a distribution whose is not,
`rustup` puts a current toolchain beside it.

`make zig` fetches the official 0.15.2 tarball into `~/.local`, because the
distribution's own package is 0.16 by now and the pinned Ghostty commit does
not build against it. To build the 0.16 way instead - that toolchain, and the
newer Ghostty commit which requires it - `make build-next`.

`make check` is what CI runs: format, clippy, tests, and the desktop and
AppStream files. It needs five more packages, and none of them are needed to
build. `openssh-server` is there for its `sftp-server`, which the SFTP tests
copy real folders through; nothing starts a daemon:

```sh
sudo dnf install rustfmt clippy desktop-file-utils appstream openssh-server
make check
```

## Configuration

`~/.config/tuni/config.toml`, written by the settings window under `Ctrl+,` and
readable without it. The session lives under `~/.local/share/tuni`.

## Documentation

[docs/DESIGN.md](docs/DESIGN.md) - every feature, every keyboard shortcut, and
why each piece is built the way it is.

[AGENTS.md](AGENTS.md) - the map a coding agent is handed: the commands, the
four crates, and the rules that span files. `CLAUDE.md` is a symlink to it.

## License

GPLv3

## Credits

Built by [@DeBondor](https://github.com/DeBondor) and
[@D3anDark](https://github.com/D3anDark). Behavior after
[kero](https://github.com/egoist/kero); terminal emulation by
[Ghostty](https://ghostty.org/)'s `libghostty-vt`.

<div align="center">
<br />

<img src="docs/uni.png" width="230" alt="Uni, the Unisic mascot - a purple cat-girl sitting on a window" />

*Uni approves this shell.*

</div>
