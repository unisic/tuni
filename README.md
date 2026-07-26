<div align="center">

<img src="data/icons/hicolor/scalable/apps/dev.unisic.Tuni.svg" width="160" height="160" alt="Tuni" />

# Tuni

</div>

A native terminal workspace for Linux.

<img width="1100" height="700" alt="Unisic_2026-07-26_13-52-51" src="https://github.com/user-attachments/assets/06b27e07-bb6e-43b6-9cdf-5e6fadaa5434" />

## Features

- Rust + GTK4/libadwaita, with Ghostty's `libghostty-vt` doing the emulation
- Split panes in a niri-style layout, grouped into projects and tabs
- File tree, git panel, and a session inspector: processes and listening ports
- Source editor and a hunk-staging diff viewer, in panes beside the shells
- Command palette, tab switcher, find in whatever pane has the keyboard
- A host list over the hosts `~/.ssh/config` already declares, one shared
  connection per machine, and no password or passphrase held anywhere in tuni
- Kitty graphics, OSC 8 hyperlinks, desktop notifications, progress bars
- Ghostty's 574 themes, which paint the window chrome as well as the terminal
- The window comes back as it was left

Connecting to another machine is OpenSSH's job here, not tuni's. A host opens by
running `ssh` in a pane, so `~/.ssh/config` still means what it always meant,
the question about an unknown host's key is asked in a terminal, and a
password, a passphrase, a hardware key or a 2FA push is answered where it has
always been answered. Tuni stores no secret and has nowhere to put one: keys,
`ssh-agent` and the desktop's keyring already do that job. It is also why hosts
will never sync to a cloud account: syncing a credential means storing one.
Panes on one machine share a single authenticated connection, so the second tab
does not ask again. The session inspector lists what a connection forwards, and
switches tuni's own ports on and off against it without reconnecting. Snippets
are typed into the pane rather than run behind it, so the shell sees what you
would have typed and so do you. Keys are listed with their fingerprints and
whether the agent is holding them, and making one or copying one to a host is a
command put on a prompt for you to read before it runs. While a pane is on a
host the panel grows a page for that machine's files, read over the connection
already open and never on the timer the local tree uses. Files move both ways
over that connection, and a download takes the name you gave it only once it is
whole.

Tuni is a ground-up Linux implementation of the workspace
[egoist/kero](https://github.com/egoist/kero) built for macOS. Kero's Swift
source is read as a specification of behavior, not translated.

## Install

```sh
sudo make install PREFIX=/usr
```

`packaging/` holds a Flatpak manifest and an RPM spec that call into that same
target.

## Building

Needs a Rust toolchain, GTK4, libadwaita, GtkSourceView and SQLite headers, and
Zig 0.15.2, since `libghostty-vt` is Zig source compiled during the build.

```sh
sudo dnf install rustup gtk4-devel libadwaita-devel gtksourceview5-devel \
    sqlite-devel
make zig
cargo run --release
```

`make zig` fetches the official 0.15.2 tarball into `~/.local` — the
distribution's own package is 0.16 by now, and the pinned Ghostty commit does
not build against it.

## Configuration

`~/.config/tuni/config.toml`, written by the settings window under `Ctrl+,` and
readable without it. The session lives under `~/.local/share/tuni`.

## Documentation

[docs/DESIGN.md](docs/DESIGN.md) — every feature, every keyboard shortcut, and
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

<img src="docs/uni.png" width="230" alt="Uni, the Unisic mascot — a purple cat-girl sitting on a window" />

*Uni approves this shell.*

</div>
