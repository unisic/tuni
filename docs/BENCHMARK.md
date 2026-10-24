# Throughput benchmark

How fast does a terminal consume PTY output? `scripts/throughput.sh` times each
terminal twice: once running only the marker `touch`, once running `cat` over
the payload first. The difference is time spent consuming output, so window and
shell startup cancel out. Each phase runs five times and the best time is kept.

Payload: 200 MiB (209 715 216 bytes) of mixed plain and SGR-colored lines, so
the parser does real work rather than `memcpy`. Every terminal runs `/bin/sh`,
not an interactive shell.

```sh
scripts/throughput.sh 200 alacritty ghostty kitty foot konsole ptyxis \
  gnome-terminal xfce4-terminal tilix terminator qterminal sakura \
  lxterminal st urxvt xterm wezterm kero
```

## Environment

| | |
| --- | --- |
| CPU | Intel Core i7-13700F (24 threads) |
| RAM | 32 GiB |
| GPU | AMD Radeon RX 9070 XT (Navi 48) |
| Kernel | 7.1.5-200.fc44.x86_64, Fedora 44; 7.2.11-200.fc44.x86_64 for the October run |
| Session | KDE Plasma on Wayland (KWin); the X11 terminals run through Xwayland |
| Date | 2026-07-28; tuni, kero and konsole again on 2026-10-24 |

## Results

200 MiB payload, best of five runs (`RUNS=5`), sorted by throughput. Rows
marked \* are from 2026-10-24, when only tuni, kero and konsole were still
installed; the rest are from 2026-07-28.

| Terminal | Version | Engine | Startup | Consume | Throughput |
| --- | --- | --- | ---: | ---: | ---: |
| **tuni** \* | 1.6.0 (7872d5e) | GTK4 + libghostty-vt | 1.095 s | 1.182 s | **169.1 MiB/s** |
| kero \* | 2.1.17 | Go + libghostty-vt | 0.024 s | 1.435 s | **139.4 MiB/s** |
| foot | 1.27.0 | Wayland, CPU | 0.016 s | 1.502 s | **133.1 MiB/s** |
| urxvt | 9.31 | X11, CPU | 0.022 s | 1.617 s | **123.6 MiB/s** |
| alacritty | 0.17.0 | GPU (OpenGL) | 0.044 s | 1.790 s | **111.7 MiB/s** |
| kitty | 0.47.1 | GPU (OpenGL) | 0.108 s | 1.908 s | **104.8 MiB/s** |
| st | 0.9.2 | X11, CPU | 0.019 s | 2.210 s | **90.4 MiB/s** |
| ghostty | 1.3.1 | GTK4 + GPU | 0.202 s | 2.524 s | **79.2 MiB/s** |
| sakura | 3.8.9 | VTE 0.84 | 0.068 s | 2.955 s | **67.6 MiB/s** |
| ptyxis | 50.1 | VTE 0.84 | 0.224 s | 2.982 s | **67.0 MiB/s** |
| gnome-terminal | 3.60.0 | VTE 0.84 | 0.170 s | 2.995 s | **66.7 MiB/s** |
| terminator | 2.1.5 | VTE 0.84 | 0.189 s | 2.998 s | **66.7 MiB/s** |
| tilix | 1.9.6 | VTE 0.84 | 0.073 s | 3.082 s | **64.9 MiB/s** |
| xterm | 406 | X11, CPU | 0.017 s | 3.798 s | **52.6 MiB/s** |
| qterminal | 2.4.0 | QTermWidget | 0.073 s | 4.242 s | **47.1 MiB/s** |
| konsole \* | 26.08.1 | Qt | 0.137 s | 4.965 s | **40.2 MiB/s** |
| wezterm | 20260716-nightly | GPU (wgpu) | 0.034 s | 6.134 s | **32.6 MiB/s** |
| lxterminal | 0.4.1 | VTE 0.84 | 0.046 s | 10.186 s | **19.6 MiB/s** |
| xfce4-terminal | 1.2.0 | VTE 0.84 | 0.084 s | 10.242 s | **19.5 MiB/s** |

Reading it:

- tuni sits first. In July, at 1.2.0, it was third at 117.2 MiB/s, inside 12%
  of foot. What moved it is the PTY reader: it used to do one blocking read
  per wakeup, a couple of hundred bytes under a flood, and now drains the
  master until the kernel has nothing left, waiting in an epoll set that holds
  the master for the life of the pane. Waiting in epoll rather than `poll`
  alone took the PTY, with nothing drawing, from 1.5-1.8 s to 1.1-1.4 s for
  the payload.
- The rows come from two dates, so the order between them is not a race run
  side by side. Konsole is the one terminal measured on both, at 41.7 MiB/s
  (26.04.3) and then 40.2 (26.08.1): 4% slower, so nothing says the machine
  got faster in between.
- The five stock VTE frontends land within 4% of each other at ~66 MiB/s,
  which is the reassuring part — they share an engine, and the benchmark says
  so. lxterminal and xfce4-terminal use the same VTE and are 3.4× slower than
  their siblings, so that gap is configuration (scrollback, rewrap), not the
  engine.
- ghostty measures at 79 MiB/s while tuni and kero, which both use ghostty's
  VT parser through `libghostty-vt`, measure at 169 and 139. The parser is not
  what separates them; the difference is in what is built around the same
  library.
- wezterm is the surprise at the bottom: a GPU renderer consuming at 33 MiB/s,
  behind every VTE frontend that isn't misconfigured. A GPU pipeline says
  nothing about how fast the reader thread drains the PTY. (Nightly build,
  since that is what the wezterm repo ships for Fedora 44.)
- tuni's startup number is not comparable: it is a GTK4 window plus the capture
  harness, and the payload run subtracts it anyway.

## tuni against kero

kero runs its terminals in a daemon, `kerod`, and the app draws them, so the
script starts each run as a session in the app already open (`kero new -f -w`)
and closes it afterwards. The pair was run three times on 2026-10-24 with
`RUNS=5 scripts/throughput.sh 200 kero`:

| Pass | tuni | kero | Winner |
| --- | ---: | ---: | --- |
| head-to-head, best of 5 | 156.2 MiB/s | 130.0 MiB/s | tuni, +20.2% |
| head-to-head, best of 5 | 164.5 MiB/s | 133.6 MiB/s | tuni, +23.1% |
| table above, best of 5 | 169.1 MiB/s | 139.4 MiB/s | tuni, +21.3% |

Before the reader change the same pairing went to kero, 112.7 and 121.2 MiB/s
against 138.9 and 129.4.

In July, at 1.2.0, the pair to settle was tuni and alacritty, and three passes
called it a dead heat between 106 and 117 MiB/s. alacritty was not installed
for the October run, so that pairing has not been repeated.

Caveats worth keeping in mind:

- The marker is touched when `cat` returns, that is, when the last byte has been
  accepted by the PTY, not when the last cell has been drawn. A terminal that
  reads greedily into a big buffer and renders lazily measures well here. It
  answers "how long until the shell gets its prompt back", which is the thing
  a user waits on.
- Wall-clock to a marker file includes whatever the compositor does with the
  frames. For the parser in isolation, use
  `cargo run --release -p tuni-vt --example feed_cost`.
- One October pass was thrown away: tuni's payload run finished 0.013 s after
  its empty run, as if `cat` had returned at once. It did not happen again in
  the 26 runs after it, and the cause is not known. Best of five keeps a run
  like that, so a number that far off is a broken pass, not a fast terminal.
- The poll interval in the script is the resolution of the whole measurement.
  It is 5 ms; at the 50 ms it used to be, everything above 100 MiB/s collapsed
  into a single bucket and read as a four-way tie.

## Not measured

- **Rio** — installed as a Flatpak (`com.rioterm.Rio`), which cannot be driven:
  that build ignores `-e` and any configured `shell`, and always spawns the
  user's login shell on the host through `flatpak-spawn --host`, so no command
  reaches it. (`RIO_CONFIG_HOME` does make it honor a config `shell`, but then
  it spawns inside the sandbox and never produced output here.) Measuring Rio
  means `cargo install rioterm` and adding it to the list; the default
  `rio -e sh -c …` case in the script already fits its CLI.
- **blackbox-terminal**, **guake**, **tilda**, **deepin-terminal** — VTE
  frontends again, and the five already in the table say what VTE does.

The script drives kitty, foot and wezterm with trailing arguments, `-x` for
xfce4-terminal and terminator, `--wait` / `--standalone` for GNOME Terminal
and Ptyxis so the timer does not stop on a client that handed the window to a
server, and `kero new -f -w` for kero. Everything else speaks `-e sh -c`.
