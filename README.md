<div align="center">

<img src="data/icons/hicolor/scalable/apps/dev.unisic.Tuni.svg" width="160" height="160" alt="Tuni" />

# Tuni

A native terminal workspace for Linux.

</div>

<img width="1163" height="700" alt="tuni" src="https://github.com/user-attachments/assets/f8c7e78c-5007-4ff6-8680-9b078b6279d3" />

## Features

- Split panes in a niri-style layout, grouped into projects and tabs
- File tree, git panel with hunk staging, and a source editor beside the shells
- The hosts `~/.ssh/config` declares, one shared connection per machine, and
  SFTP for the machine a pane is on
- Command palette, tab switcher, find, and an inspector for what is running
  and listening
- Kitty graphics, OSC 8 links, notifications, Ghostty's 574 themes
- A finished command or coding agent in a tab you are not watching says so

Rust and GTK4/libadwaita, with Ghostty's `libghostty-vt` doing the emulation.
A Linux implementation of [egoist/kero](https://github.com/egoist/kero), whose
source is read as a specification, not translated. SSH is OpenSSH running in a
pane, so Tuni stores no password, passphrase or key.

## Performance

200 MiB of mixed output, best of five, nineteen terminals in one run on one
machine. Startup is launch until the shell ran.

| Terminal | Startup | First frame | PSS | Throughput |
| --- | ---: | ---: | ---: | ---: |
| **tuni** | **0.005 s** | 233 ms | 47.5 MiB | **151.4 MiB/s** |
| foot | 0.016 s | 21 ms | 7.3 MiB | 132.2 MiB/s |
| alacritty | 0.045 s | 57 ms | 30.3 MiB | 118.0 MiB/s |
| kitty | 0.113 s | 94 ms | 47.8 MiB | 116.8 MiB/s |
| ptyxis | 0.520 s | 525 ms | 48.2 MiB | 66.1 MiB/s |
| ghostty | 0.531 s | 471 ms | 83.5 MiB | 56.7 MiB/s |
| konsole | 0.125 s | 127 ms | 31.1 MiB | 42.5 MiB/s |
| wezterm | 0.034 s | 69 ms | 61.5 MiB | 25.5 MiB/s |

All nineteen, the method and the caveats: [docs/BENCHMARK.md](docs/BENCHMARK.md).

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/unisic/tuni/main/scripts/install.sh | bash
```

On Fedora, COPR instead:

```sh
sudo dnf copr enable debondor/Tuni && sudo dnf install tuni
```

Every [release](https://github.com/unisic/tuni/releases) also carries an RPM,
a DEB and an Arch package. Tuni offers updates itself; Preferences - Session -
Updates turns that off.

## Building

```sh
sudo dnf install cargo rust gcc make git-core curl tar xz \
    gtk4-devel libadwaita-devel gtksourceview5-devel sqlite-devel
make zig                        # Zig 0.15.2 into ~/.local, which the build needs
cargo run --release
sudo make install PREFIX=/usr
```

`make check` runs what CI runs and needs `rustfmt clippy desktop-file-utils
appstream openssh-server` on top. [AGENTS.md](AGENTS.md) has the rest.

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
