# Throughput benchmark

How fast does a terminal consume PTY output? `scripts/throughput.sh` times each
terminal twice: once running only the marker `touch`, once running `cat` over
the payload first. The difference is time spent consuming output, so window and
shell startup cancel out. Each phase runs twice and the best time is kept.

Payload: 200 MiB (209 715 216 bytes) of mixed plain and SGR-colored lines, so
the parser does real work rather than `memcpy`. Every terminal runs `/bin/sh`,
not an interactive shell.

```sh
scripts/throughput.sh 200 alacritty ghostty kitty foot konsole ptyxis \
  gnome-terminal xfce4-terminal tilix terminator qterminal sakura \
  lxterminal st urxvt xterm
```

## Environment

| | |
| --- | --- |
| CPU | Intel Core i7-13700F (24 threads) |
| RAM | 31 GiB |
| GPU | AMD Radeon RX 9070 (Navi 48) |
| Kernel | 7.1.5-200.fc44.x86_64, Fedora 44 |
| Session | Wayland; the X11 terminals run through Xwayland |
| Date | 2026-07-27 |

## Results

200 MiB payload, best of two runs, sorted by throughput.

| Terminal | Version | Engine | Startup | Consume | Throughput |
| --- | --- | --- | ---: | ---: | ---: |
| urxvt | 9.31 | X11, CPU | 0.023 s | 1.579 s | **126.6 MiB/s** |
| foot | 1.27.0 | Wayland, CPU | 0.017 s | 1.691 s | **118.3 MiB/s** |
| **tuni** | 1.1.2 | GTK4 + libghostty-vt | 0.609 s | 1.723 s | **116.0 MiB/s** |
| alacritty | 0.17.0 | GPU (OpenGL) | 0.056 s | 1.785 s | **112.0 MiB/s** |
| kitty | 0.47.1 | GPU (OpenGL) | 0.109 s | 1.807 s | **110.6 MiB/s** |
| st | 0.9.2 | X11, CPU | 0.018 s | 2.215 s | **90.2 MiB/s** |
| ghostty | 1.3.1 | GTK4 + GPU | 0.189 s | 2.785 s | **71.8 MiB/s** |
| terminator | 2.1.5 | VTE 0.84 | 0.181 s | 2.829 s | **70.6 MiB/s** |
| ptyxis | 50.1 | VTE 0.84 | 0.217 s | 2.835 s | **70.5 MiB/s** |
| gnome-terminal | 3.60.0 | VTE 0.84 | 0.167 s | 2.852 s | **70.1 MiB/s** |
| tilix | 1.9.6 | VTE 0.84 | 0.081 s | 2.884 s | **69.3 MiB/s** |
| sakura | 3.8.9 | VTE 0.84 | 0.063 s | 2.892 s | **69.1 MiB/s** |
| xterm | 406 | X11, CPU | 0.013 s | 3.726 s | **53.6 MiB/s** |
| qterminal | 2.4.0 | QTermWidget | 0.079 s | 4.276 s | **46.7 MiB/s** |
| konsole | 26.04.3 | Qt | 0.114 s | 4.634 s | **43.1 MiB/s** |
| lxterminal | 0.4.1 | VTE 0.84 | 0.052 s | 9.825 s | **20.3 MiB/s** |
| xfce4-terminal | 1.2.0 | VTE 0.84 | 0.085 s | 10.136 s | **19.7 MiB/s** |

Reading it:

- tuni sits third, inside 10% of the fastest thing measured and ahead of both
  GPU terminals. Nothing here is a landslide: the top five span 13%.
- The five stock VTE frontends land within 4% of each other at ~70 MiB/s, which
  is the reassuring part — they share an engine, and the benchmark says so.
  lxterminal and xfce4-terminal use the same VTE and are 3.5× slower than their
  siblings, so that gap is configuration (scrollback, rewrap), not the engine.
- ghostty measures at 72 MiB/s while tuni, which uses ghostty's VT parser
  through `libghostty-vt`, measures at 116. The parser is not what separates
  them; the difference is on the drawing side of the same library.
- tuni's startup number is not comparable: it is a GTK4 window plus the capture
  harness, and the payload run subtracts it anyway.

## tuni against alacritty

The two are close enough in the table above that one run does not settle it, so
the pair was run on its own with `RUNS=5 scripts/throughput.sh 200 alacritty`:

| Pass | tuni | alacritty | Winner |
| --- | ---: | ---: | --- |
| table above, best of 2 | 116.0 MiB/s | 112.0 MiB/s | tuni, +3.6% |
| head-to-head, best of 5 | 127.2 MiB/s | 122.2 MiB/s | tuni, +4.1% |
| head-to-head, best of 5 | 121.4 MiB/s | 124.4 MiB/s | alacritty, +2.5% |

It is a tie. The order flips between passes and the run-to-run spread on either
terminal (~5%) is wider than the gap between them, so any single number picked
out of this is a coin toss dressed up as a result. What can be said is that a
GTK4 window with a full workspace around it keeps up with a terminal that draws
nothing else, which is the interesting part.

Caveats worth keeping in mind:

- The marker is touched when `cat` returns, that is, when the last byte has been
  accepted by the PTY, not when the last cell has been drawn. A terminal that
  reads greedily into a big buffer and renders lazily measures well here. It
  answers "how long until the shell gets its prompt back", which is the thing
  a user waits on.
- Wall-clock to a marker file includes whatever the compositor does with the
  frames. For the parser in isolation, use
  `cargo run --release -p tuni-vt --example feed_cost`.
- The poll interval in the script is the resolution of the whole measurement.
  It is 5 ms; at the 50 ms it used to be, everything above 100 MiB/s collapsed
  into a single bucket and read as a four-way tie.

## Not measured

- **WezTerm** — Flathub only (`flatpak install flathub org.wezfurlong.wezterm`).
  The sandbox does not see the host `/tmp`, so the payload has to move somewhere
  the app can read before the numbers mean anything.
- **Rio** — not packaged for Fedora; `cargo install rioterm`.
- **blackbox-terminal**, **guake**, **tilda**, **deepin-terminal** — VTE
  frontends again, and the five already in the table say what VTE does.

The script drives kitty, foot and wezterm with trailing arguments, `-x` for
xfce4-terminal and terminator, and `--wait` / `--standalone` for GNOME Terminal
and Ptyxis so the timer does not stop on a client that handed the window to a
server. Everything else speaks `-e sh -c`.
