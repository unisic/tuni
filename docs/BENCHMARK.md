# Benchmark

How fast does a terminal start, consume PTY output, and what does it cost
while it waits? `scripts/throughput.sh` times each terminal twice: once
running only the marker `touch`, once running `cat` over the payload first.
The difference is time spent consuming output, so window and shell startup
cancel out. Each phase runs five times and the best time is kept. A third run
leaves the shell sleeping and reads what the terminal costs while it waits:
memory, and CPU over five idle seconds.

Payload: 200 MiB (209 715 216 bytes) of mixed plain and SGR-colored lines, so
the parser does real work rather than `memcpy`. Every terminal runs `/bin/sh`,
not an interactive shell.

```sh
RUNS=5 nice -n 19 scripts/throughput.sh 200 tuni foot alacritty kitty \
  konsole ptyxis gnome-terminal xfce4-terminal tilix terminator qterminal \
  sakura lxterminal st urxvt xterm wezterm ghostty kero
```

`TUNI=/usr/bin/tuni` in front of it times an installed tuni in place of this
tree's release build, which is how a release and the build after it go
through the same script.

## Environment

| | |
| --- | --- |
| CPU | Intel Core i7-13700F (24 threads) |
| RAM | 32 GiB |
| GPU | AMD Radeon RX 9070 XT (Navi 48) |
| Kernel | 7.2.11-200.fc44.x86_64, Fedora 44 |
| Session | A headless KWin (`kwin_wayland --virtual --xwayland`, 1280x800) on a session bus of its own; the X11 terminals through its Xwayland |
| Load | `nice -n 19` beside a running game, load average 1.4 to 4.8 |
| Date | 2026-10-25, all nineteen in one invocation |

Every window opened in the headless compositor, so nothing on the desktop lost
focus to the run. The terminals that were not installed ran out of their
Fedora 44 packages unpacked into a directory, with `LD_LIBRARY_PATH` at the
libraries that came with them.

## Results

200 MiB payload, best of five runs (`RUNS=5`), sorted by throughput. tuni is
900cde5, the tree released as 1.7.0.

| Terminal | Version | Engine | Startup | First frame | Consume | Throughput | RSS | PSS | Idle |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| **tuni** | 1.7.0 | GTK4 + libghostty-vt | **0.005 s** | 233 ms | 1.320 s | **151.4 MiB/s** | 112.3 MiB | 47.5 MiB | 0 ms |
| foot | 1.27.0 | Wayland, CPU | 0.016 s | 21 ms | 1.512 s | 132.2 MiB/s | 16.9 MiB | 7.3 MiB | 0 ms |
| alacritty | 0.17.0 | GPU (OpenGL) | 0.045 s | 57 ms | 1.695 s | 118.0 MiB/s | 85.9 MiB | 30.3 MiB | 0 ms |
| kitty | 0.47.1 | GPU (OpenGL) | 0.113 s | 94 ms | 1.712 s | 116.8 MiB/s | 113.3 MiB | 47.8 MiB | 0 ms |
| urxvt | 9.31 | X11, CPU | 0.011 s | n/a | 1.874 s | 106.7 MiB/s | 14.2 MiB | 4.7 MiB | 0 ms |
| st | 0.9.2 | X11, CPU | 0.018 s | n/a | 2.437 s | 82.0 MiB/s | 11.5 MiB | 2.8 MiB | 0 ms |
| ptyxis | 50.1 | VTE 0.84 | 0.520 s | 525 ms | 3.022 s | 66.1 MiB/s | 115.0 MiB | 48.2 MiB | 10 ms |
| gnome-terminal \* | 3.60.0 | VTE 0.84 | 0.171 s | n/a | 3.058 s | 65.3 MiB/s | 55.5 MiB | 20.4 MiB | 0 ms |
| terminator | 2.1.5 | VTE 0.84 | 0.206 s | 211 ms | 3.110 s | 64.3 MiB/s | 86.3 MiB | 43.0 MiB | 10 ms |
| sakura | 3.8.9 | VTE 0.84 | 0.051 s | 67 ms | 3.177 s | 62.9 MiB/s | 39.1 MiB | 11.6 MiB | 0 ms |
| tilix | 1.9.6 | VTE 0.84 | 0.069 s | 165 ms | 3.300 s | 60.6 MiB/s | 76.1 MiB | 40.8 MiB | 0 ms |
| ghostty | 1.3.1 | GTK4 + GPU | 0.531 s | 471 ms | 3.521 s | 56.7 MiB/s | 164.6 MiB | 83.5 MiB | 0 ms |
| xterm | 411 | X11, CPU | 0.017 s | n/a | 4.347 s | 46.0 MiB/s | 9.2 MiB | 3.4 MiB | 0 ms |
| konsole | 26.08.1 | Qt | 0.125 s | 127 ms | 4.696 s | 42.5 MiB/s | 94.8 MiB | 31.1 MiB | 0 ms |
| qterminal | 2.4.0 | QTermWidget | 0.079 s | 86 ms | 4.701 s | 42.5 MiB/s | 72.5 MiB | 18.6 MiB | 0 ms |
| kero \* | 2.1.17 | Go + libghostty-vt | 0.025 s | n/a | 4.739 s | 42.2 MiB/s | 167.0 MiB | 74.8 MiB | 90 ms |
| wezterm | 20261019-nightly | GPU (wgpu) | 0.034 s | 69 ms | 7.829 s | 25.5 MiB/s | 115.8 MiB | 61.5 MiB | 0 ms |
| lxterminal | 0.4.1 | VTE 0.84 | 0.045 s | 90 ms | 9.681 s | 20.6 MiB/s | 48.7 MiB | 15.2 MiB | 0 ms |
| xfce4-terminal | 1.2.0 | VTE 0.84 | 0.086 s | 106 ms | 10.091 s | 19.8 MiB/s | 52.5 MiB | 17.6 MiB | 0 ms |

\* The process the script starts is a client that hands the terminal to a
server, so the script reads n/a for its memory and idle. These were measured
separately, three times each, from the server with one terminal open:
`gnome-terminal-server`, and for kero the app plus `kerod` (60.4 + 14.4 MiB
PSS, 141.0 + 26.0 MiB RSS), the app freshly started for each sample. A kero
app that has been open longer idles at 0 to 10 ms.

Startup is the empty run: launch until the shell has run `touch`. First frame
is launch until the first `wl_surface.attach` of a buffer that
`WAYLAND_DEBUG=client` prints, median of five, measured straight after the
run in the same session; tuni's is the median of nine, because its launches
spread wider than the others'. It is n/a for the X11 terminals, which draw
through Xwayland and not as Wayland clients, and for gnome-terminal and kero,
whose window belongs to the server. Memory is read from
`/proc/<pid>/smaps_rollup` three seconds after `touch`, once a GTK window has
stopped painting its first frames. PSS divides each shared page among the
processes mapping it, so it is the fairer column; RSS counts all of Mesa's
`libLLVM` (25 MiB) against every GPU terminal. Idle is the CPU the process
spends in the five seconds after the memory read, counted in 10 ms clock
ticks, so 10 ms is one tick and 0 is less than one.

Reading it:

- tuni consumes first, 14.5% ahead of foot, and its shell starts first:
  `tuni` is a launcher linked against GLib and GIO alone that starts the shell
  before GTK is loaded, so `touch` runs 4.9 ms after launch, ahead of urxvt's
  10.8 and foot's 16.4, while the window is still being built.
- First frame is where tuni is behind: 233 ms, twelfth of the fourteen with a
  number, ahead of ghostty and ptyxis. The section below is what that is made
  of. For the others startup and first frame are one moment, because they
  start their shell after their window.
- On memory it is fourteenth of nineteen by PSS, 47.5 MiB, level with kitty and
  ptyxis and below ghostty, kero and wezterm. Every GTK terminal pays for GTK
  before it holds a cell; see below.
- Idle: no terminal does real work while it waits. ptyxis and terminator spend
  one tick in five seconds, and kero, freshly started, nine.
- The five stock VTE frontends land within 9% of each other at 60.6 to
  66.1 MiB/s: they share an engine, and the benchmark says so. lxterminal and
  xfce4-terminal use the same VTE and are three times slower than their
  siblings, so that gap is configuration (scrollback, rewrap), not the engine.
- ghostty measures at 56.7 MiB/s while tuni, which uses ghostty's VT parser
  through `libghostty-vt`, measures at 151.4. The parser is not what separates
  them; the difference is what is built around the same library.

### Headless against the desktop

Sixteen of the nineteen land within 14% of what it measured on
the KDE desktop in July (October for tuni, kero and konsole), in both
directions: foot 133.1 to 132.2, alacritty 111.7 to 118.0, konsole 40.2 to
42.5, tuni 169.1 to 151.4. Three do not, and the cause is not known for any of
them: ghostty (79.2 to 56.7, -28%), wezterm (32.6 to 25.5, -22%, a different
nightly) and kero (139.4 to 42.2, -70%, and 49.8 in a second pass with a
freshly started app). kero draws on the GPU through Mesa in both sessions. Its
desktop head-to-head with tuni is below, and its row here says what kero does
in a headless compositor, not on a desktop.

### What GTK costs before tuni does anything

`floor` is a C program holding nothing but an `AdwApplicationWindow`, a header
bar and a label, linked against the same GTK 4.22.5 and libadwaita 1.9.4 and
measured the same way. The stock theme is an empty `XDG_DATA_HOME`, which
hides Papirus-Dark, the icon theme on this machine.

| | First frame, Papirus-Dark | First frame, stock | PSS, Papirus-Dark | PSS, stock |
| --- | ---: | ---: | ---: | ---: |
| tuni (ab2d619) | 233 ms | 117 ms | 48.1 MiB | 33.6 MiB |
| floor | 259 ms | 103 ms | 41.1 MiB | 25.9 MiB |

- With Papirus-Dark, tuni draws its first frame 26 ms before the empty window
  does: it creates the renderer while the theme loads, which floor does not.
  Every terminal in the table with a first frame is ahead of that floor
  except ghostty and ptyxis, which link libadwaita too, so no change inside
  tuni can bring its first frame below theirs while it is a libadwaita
  program. With the stock theme the floor is 103 ms, still behind seven of
  them, and tuni's 117 ms is ahead of konsole's 127, tilix's 165 and
  terminator's 211 measured with the desktop's theme.
- Most of the gap between the two columns is GTK loading the icon theme. GTK
  does that on a thread and the first icon the window draws waits for it; with
  Papirus-Dark that is about 180 ms of CPU building hash tables out of the
  theme's cache, the same in floor as in tuni, and 13.1 MiB of tuni's PSS is
  those tables, still allocated in the loader thread's arena.
- PSS has the same shape. The empty window, 41.1 MiB, is above twelve of the
  eighteen other terminals before any terminal exists in it, and kitty is
  6.7 MiB above it. tuni is 7.0 MiB above it, and the largest single part of that is tuni's own
  binary: 5.8 MiB of PSS, 4.6 MiB of it code.
- Linking alone has a price. From exec to `main`, median of fifteen: 0.4 ms
  for a program linked only against GLib, 13.4 ms against GTK 4 (114
  libraries), 17.7 ms against libadwaita (134). A shell started in `main`
  starts no sooner than that, which is why `tuni` is now a launcher linked
  against GLib and GIO alone: it starts the shell and then `exec`s
  `tuni-gtk`, which adopts it. The window appears no sooner for it; see
  below.

### What moved it

- Before this round tuni loaded the icon theme several times over: every
  search path added after GTK starts the load throws it away. Loading it once
  took the median startup from 728 to 310 ms and PSS from 65 to 56 MiB, and
  creating the renderer while the theme loads took startup to 278 ms (seven
  launches each, interleaved, load average 4 to 7).
- Starting the shell before GTK (dce2a1b) took the startup column from 259 to
  28 ms. It moves the shell, not the window.
- A `malloc_trim` on the tick after the first frame hands back what startup
  freed: median PSS 55.0 to 53.6 MiB over two interleaved series of three,
  first frame unchanged within noise. Every `madvise` of a whole run together
  took 0.42 ms.
- Linking with `-z pack-relative-relocs` writes the binary's 22 837 relative
  relocations as DT_RELR, 33 KiB instead of 574: PSS 53.4 to 52.95 MiB, the
  file 528 KiB smaller, first frame unchanged within noise.
- Building the side panel, the main menu, the find bar, the tab switcher and
  the empty page the first time each is wanted (ab2d619) took median PSS from
  52.9 to 47.9 MiB over five launches each, the panel about 4 MiB of it, and
  the first frame from 239.1 to 233.4 ms over nine interleaved launches. The
  panel's first opening pays 6 to 8 ms for it.
- Splitting `tuni` into a launcher that starts the shell and `exec`s
  `tuni-gtk` took startup from 26.3 to 5.6 ms, median of nine launches each
  interleaved with b32b8ab; foot is 17.8 ms measured the same way. The exec
  and the D-Bus question of whether a Tuni is already running are inside
  those 5.6 ms. First frame, 277.9 against 282.5 ms over fifteen launches
  each, and PSS, 47.6 against 47.4 MiB, did not move beyond noise: like
  dce2a1b, it moves the shell and not the window.
- Tried and kept out: setting `gtk-icon-theme-name` to Adwaita before
  `adw::init`. PSS fell from 55.0 to 47.2 MiB, but the window was presented at
  about 281 ms instead of 209, because the settings GDK reads from the desktop
  portal name Papirus-Dark while `gtk::init` opens the display, before tuni
  can set anything, so the load has already started and switching adds a
  second one. It would also replace the icons the desktop asked for.
- Arming each pane's two `/proc` polls as GLib seconds timers, which fire
  together across the process, instead of millisecond timers that each land
  on a moment of their own: context switches over 20 idle seconds at 16 panes
  from 92 and 97 to 22 and 21, at 4 panes from 44 to 24 and 22, a single pane
  unchanged at 12 to 13. A poll may now run up to a second late.

### 1.6.0 against 1.7.0

The release before this round, installed as `/usr/bin/tuni`, against this
tree's build, in the same headless session and interleaved: two throughput
passes of five runs each, nine first frames each, five PSS readings each.

| | 1.6.0 | 1.7.0 | |
| --- | --- | --- | --- |
| Startup | 663 to 734 ms | 5.2 to 5.6 ms | shell 130 times sooner |
| First frame, median of 9 | 671.4 ms | 234.1 ms | 65% less |
| Throughput, best pass | 112.0 MiB/s | 146.4 MiB/s | 31% more |
| RSS, median of 5 | 129.0 MiB | 112.2 MiB | 13% less |
| PSS, median of 5 | 59.8 MiB | 47.1 MiB | 21% less |
| Idle | 0 to 10 ms/5s | 0 to 10 ms/5s | unchanged |

Throughput moves between passes in both (1.6.0 100.9 to 112.0, 1.7.0 122.6
to 146.4) because the run shared the machine with a game; the 151.4 in the
table above is the same build's best in the nineteen-terminal run.

### Panes, a flood, and closing them

One window driven by `TUNI_CAPTURE_ACTIONS` in the same headless session,
every pane running `exec sleep 300`, read once the actions settle and again
32 seconds later; the two readings matched to 0.3 MiB. Two launches each,
which agreed to 0.6 MiB. Memory is from the build before the polls moved to
seconds timers, which touches no allocation; the context switches of the
pane rows are from after it.

| | PSS | RSS | Threads | fds | Context switches, 20 s idle |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 pane | 48.7 MiB | 112.0 MiB | 14 | 32 | 12 to 13 |
| 4 panes | 50.1 MiB | 113.2 MiB | 20 | 44 | 22 to 24 |
| 16 panes | 55.0 MiB | 118.1 MiB | 44 | 92 | 21 to 22 |
| 16 tabs | 53.9 MiB | 117.3 MiB | 44 | 92 | 92 to 100, before |
| 16 panes, 15 closed | 53.6 MiB | 117.0 MiB | 14 | 32 | 12, before |
| 1 pane after 200 MiB | 66.3 MiB | 129.9 MiB | 14 | 32 | 13 to 14, before |

- A pane costs about 0.4 MiB of PSS, two threads (the PTY reader and writer)
  and four descriptors, and nothing in CPU while it waits: no row spent more
  than one tick in 20 seconds.
- Closing returns every thread and descriptor, and `TUNI_DEBUG_LIFETIME`
  reports all fifteen terminals released and none still alive. What PSS
  keeps, about 5 MiB over a single pane, is allocator and GTK state rather
  than a leak: a `malloc_trim(0)` called from gdb handed 1.9 MiB of it back,
  and the rest is caches grown while sixteen panes were drawn.
- Output is bounded by the scrollback budget of 50 000 lines: 200 MiB through
  one pane adds 16 to 18 MiB, and 400 MiB adds nothing more (64.9 against
  65.3 MiB in a separate pair of launches).
- Since then a closed pane, and every MiB a pane reads, asks for one
  `malloc_trim(0)` once two seconds pass without another ask. On 2026-10-26,
  two interleaved pairs, the closed-panes row went from 52.8 and 53.7 MiB to
  50.7 and 51.6, and the row after 200 MiB from 65.1 and 66.7 to 64.0 and
  63.9. A trim from gdb afterwards found nothing left to hand back on either
  row. On a C heap of 64 MiB with 62 freed in page-sized runs one trim took
  2.7 ms, and 0.24 ms once there was nothing to return; it runs on the main
  thread, once per burst, and never during one.

## tuni against kero

kero runs its terminals in a daemon, `kerod`, and the app draws them, so the
script starts each run as a session in the app already open (`kero new -f -w`)
and closes it afterwards. The pair was run three times on 2026-10-24, on the
KDE desktop rather than headless, with `RUNS=5 scripts/throughput.sh 200 kero`:

| Pass | tuni | kero | Winner |
| --- | ---: | ---: | --- |
| head-to-head, best of 5 | 156.2 MiB/s | 130.0 MiB/s | tuni, +20.2% |
| head-to-head, best of 5 | 164.5 MiB/s | 133.6 MiB/s | tuni, +23.1% |
| full run, best of 5 | 169.1 MiB/s | 139.4 MiB/s | tuni, +21.3% |

Before the reader change the same pairing went to kero, 112.7 and 121.2 MiB/s
against 138.9 and 129.4. Headless, kero does not reach those numbers (42.2
and 49.8 MiB/s), so the pairing is read from the desktop passes; see
"Headless against the desktop".

In July, at 1.2.0, the pair to settle was tuni and alacritty, and three passes
called it a dead heat between 106 and 117 MiB/s. In the headless run above it
is tuni by 28%, 151.4 against 118.0.

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

- **Rio**: installed as a Flatpak (`com.rioterm.Rio`), which cannot be driven:
  that build ignores `-e` and any configured `shell`, and always spawns the
  user's login shell on the host through `flatpak-spawn --host`, so no command
  reaches it. (`RIO_CONFIG_HOME` does make it honor a config `shell`, but then
  it spawns inside the sandbox and never produced output here.) Measuring Rio
  means `cargo install rioterm` and adding it to the list; the default
  `rio -e sh -c …` case in the script already fits its CLI.
- **blackbox-terminal**, **guake**, **tilda**, **deepin-terminal**: VTE
  frontends again, and the five already in the table say what VTE does.

The script drives kitty, foot and wezterm with trailing arguments, `-x` for
xfce4-terminal and terminator, `--wait` / `--standalone` for GNOME Terminal
and Ptyxis so the timer does not stop on a client that handed the window to a
server, and `kero new -f -w` for kero. Everything else speaks `-e sh -c`.
