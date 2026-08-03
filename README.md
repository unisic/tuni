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
- Ghostty's 574 themes, which paint the window chrome as well as the terminal
- The window comes back as it was left

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

200 MiB of mixed plain and SGR-colored output, best of two runs, on one
machine. Time to consume it, so window and shell startup cancel out.

| Terminal | Engine | Consume | Throughput |
| --- | --- | ---: | ---: |
| urxvt | X11, CPU | 1.579 s | 126.6 MiB/s |
| foot | Wayland, CPU | 1.691 s | 118.3 MiB/s |
| **tuni** | GTK4 + libghostty-vt | 1.723 s | **116.0 MiB/s** |
| alacritty | GPU (OpenGL) | 1.785 s | 112.0 MiB/s |
| kitty | GPU (OpenGL) | 1.807 s | 110.6 MiB/s |
| st | X11, CPU | 2.215 s | 90.2 MiB/s |
| ghostty | GTK4 + GPU | 2.785 s | 71.8 MiB/s |
| terminator | VTE 0.84 | 2.829 s | 70.6 MiB/s |
| ptyxis | VTE 0.84 | 2.835 s | 70.5 MiB/s |
| gnome-terminal | VTE 0.84 | 2.852 s | 70.1 MiB/s |
| tilix | VTE 0.84 | 2.884 s | 69.3 MiB/s |
| sakura | VTE 0.84 | 2.892 s | 69.1 MiB/s |
| xterm | X11, CPU | 3.726 s | 53.6 MiB/s |
| qterminal | QTermWidget | 4.276 s | 46.7 MiB/s |
| konsole | Qt | 4.634 s | 43.1 MiB/s |
| lxterminal | VTE 0.84 | 9.825 s | 20.3 MiB/s |
| xfce4-terminal | VTE 0.84 | 10.136 s | 19.7 MiB/s |

Third of seventeen, within 10% of the fastest thing measured and ahead of both
GPU terminals - with a whole workspace drawn around the terminal.

[docs/BENCHMARK.md](docs/BENCHMARK.md) - method, environment, caveats, and the
head-to-head with alacritty that the table above is too close to settle.

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
has somewhere to ask for your password. Preferences - Terminal - Updates turns
the check off.

Every [release](https://github.com/unisic/tuni/releases) carries the same three
packages for installing by hand:

```sh
sudo dnf install ./tuni-*.rpm            # Fedora 44+
sudo apt install ./tuni_*_amd64.deb      # Ubuntu 26.04+
sudo pacman -U ./tuni-*-x86_64.pkg.tar.zst
```

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
AppStream files. It needs four more packages, and none of them are needed to
build:

```sh
sudo dnf install rustfmt clippy desktop-file-utils appstream
make check
```

## Configuration

`~/.config/tuni/config.toml`, written by the settings window under `Ctrl+,` and
readable without it. The session lives under `~/.local/share/tuni`.

## Documentation

[docs/DESIGN.md](docs/DESIGN.md) - every feature, every keyboard shortcut, and
why each piece is built the way it is.

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
