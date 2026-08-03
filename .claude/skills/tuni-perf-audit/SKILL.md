---
name: tuni-perf-audit
description: Full performance/concurrency audit of Tuni specifically (Rust/GTK4/libadwaita terminal workspace), with this repository's file:line landmarks, harnesses and known suspects already written in. Use when the user asks for a performance audit of Tuni, "make app fast", idle CPU/memory investigation, Rc cycles, RefCell borrow panics, main-thread stalls, terminal throughput, or invokes /tuni-perf-audit. Measurement-first - never guess; produces a structured report and minimal fixes. For any other codebase use /perf-audit, which detects the stack instead of assuming it.
---

You are a senior Linux desktop performance engineer and a Rust/GTK4 ownership and main-loop expert.

Audit this entire Rust/GTK4/libadwaita application (Tuni — a native terminal workspace for Linux: terminal panes, projects, tabs, a file tree, a git panel, a process/port inspector, a source editor and a diff viewer, all in one window, with terminal emulation provided by libghostty-vt).

Main goal:

MAKE APP FAST.
MAKE APP QUIET WHEN IDLE.
STOP MEMORY GROWTH.
STOP CPU WASTE.
KEEP THE MAIN LOOP FREE.
DO NOT GUESS.
MEASURE EVERYTHING.

Check the application for:

* memory leaks
* `Rc` reference cycles (`Rc`↔`Rc`, and `Rc<dyn Fn>` closures stored in the object they capture)
* GObject reference leaks (strong `glib::clone!` captures, floating and toggle refs)
* children never `unparent()`ed in `dispose`, leaking a whole subtree
* unbounded memory growth (scrollback, find hits, session history maps)
* excessive allocations (per-frame `Vec`s, per-run `FontDescription` clones, `to_vec()` per PTY read)
* abandoned objects (widgets alive after `dispose`, terminals alive after their pane closes)
* duplicated caches (the widget texture cache versus libghostty's own image storage)
* oversized or wrongly-evicting caches
* CPU leaks and runaway background work
* unnecessary wakeups
* busy loops
* excessive glib timeout, idle and tick sources
* excessive `queue_draw()` (redraw storms)
* excessive re-layout (`measure`/`size_allocate` churn)
* excessive GSK render-node construction per frame
* repeated property notifications (`notify("title")` / `notify("cwd")` storms)
* `RefCell` borrow panics
* re-entrancy — a `borrow_mut()` held across code that can call back into the same cell
* long-lived borrows spanning signal emission, widget mutation or FFI
* unsafe assumptions about the single-thread invariant (`!Send` state reachable from a worker)
* incorrect main-thread usage (GTK objects touched from `gio::spawn_blocking`)
* blocking the GLib main context (synchronous `fs`, `fsync`, `Command`, `write_all` to a PTY, `recv_blocking`)
* unnecessary work on the main thread
* excessive background tasks (git, `/proc` walks, diff reads)
* tasks that are never cancelled or superseded (stale generation stamps)
* signal handlers that are never disconnected, especially on process-global objects
* `glib::SourceId` and `gtk::TickCallbackId` not removed on `dispose`
* resources that are never released
* PTYs, reader threads, file descriptors or child shells that stay open
* performance regressions caused by widget or window lifecycle mistakes
* performance regressions caused by feed-to-frame coupling

The application is written in Rust (edition 2024, rust-version 1.90, workspace resolver 3, release profile `lto = "thin"` with `codegen-units = 1`), as four crates totalling roughly 24.7k lines, and integrates with:

* `tuni-vt` — a facade over `libghostty-vt`, pinned to git rev `cd59174f` and compiled by Zig during the build; owns terminal state, the grid, scrollback, reflow, key and mouse encoding
* `tuni-pty` — `portable-pty` 0.9 plus `async-channel` 2; one shell, one PTY, one reader thread per pane
* `tuni-core` — portable models with no GTK: settings, session, workspace, panes, git (shells out to the `git` binary), files, editor, info (`/proc` walk), theme, diff, fuzzy; `build.rs` bakes 574 Ghostty themes in via `include_str!`
* `tuni-gtk` — gtk4 0.10 (`v4_12`), libadwaita 0.8 (`v1_6`), sourceview5 0.10; the `tuni` binary
* raw gtk4-rs, imperative construction — no relm4, no `.ui` files, no GResource
* custom GObject subclasses via `glib::wrapper!` + `#[glib::object_subclass]`: `TuniWindow`, `TuniTerminal`, `TuniTiles`, `TuniGrid`, `TuniEditor`, `TuniDiff`, `TuniFiles`, `TuniGit`, `TuniInfo`, `TuniPanel`, `TuniFind`, `TuniSwitcher`
* `TuniTerminal` overrides `measure`, `size_allocate` and `snapshot`, and draws the whole viewport itself with Pango into GSK nodes
* `TuniTiles` replaces `GtkPaned` with its own `measure`/`size_allocate` over a weight model
* the Kitty graphics protocol, decoded with the `png` crate behind a size ceiling
* `git` as a subprocess, always through `gio::spawn_blocking`
* `/proc` read directly for the session inspector

**The concurrency model is single-threaded by construction.** There is one glib `MainContext`. The VT state is `!Send` and main-thread only. Interior mutability is `Rc`, `RefCell` and `Cell` throughout — 42 such fields in `imp::TuniTerminal` alone (`crates/tuni-gtk/src/terminal.rs:202`), 8 `RefCell<HashMap<Id, …>>` registries in `imp::TuniWindow` (`crates/tuni-gtk/src/window.rs:218`). There is no `Arc`, no `Mutex`, no `RwLock`, no tokio, no rayon and no crossbeam anywhere in the workspace. The only cross-thread surfaces are: one `std::thread` per PTY (`crates/tuni-pty/src/lib.rs:139`, named `tuni-pty-reader`, 64 KiB reads, `async_channel::bounded(64)`), five `gio::spawn_blocking` calls (`git.rs:492`, `git.rs:891`, `diff.rs:353`, `diff.rs:700`, `info.rs:352`), one `AtomicU64` id counter (`crates/tuni-core/src/workspace.rs:16`), and two `OnceLock` tables for ParamSpecs and Signals. Everything else is one thread. **So do not go looking for data races. Go looking for main-thread stalls, borrow panics, re-entrancy, and unbounded work scheduled onto the one loop that draws.**

TARGETS

When the application is open and completely idle — a window on screen, at least one shell sitting at a prompt, no keystrokes, no scrolling, no pending git, diff or info refresh — the only legitimate periodic work is the cursor blink:

* CPU usage should stay as close to 0% as practically possible
* there should be no constant CPU activity
* there should be no periodic CPU spikes without a valid reason
* there should be no unnecessary thread wakeups
* memory usage should reach a plateau and stay there
* memory must not continuously increase — not with idle time, and not with scrollback that is never written to
* the PTY reader threads should be blocked in `read()`, not spinning
* no `gio::spawn_blocking` task should be running
* timers should not run unless truly required
* the widget should not continuously redraw, re-measure, re-allocate, poll or re-read the disk
* no unnecessary disk, `/proc`, subprocess or `git` work should happen

Two sources of periodic work are named here because they are the only candidates, and each is a question the audit must answer, not a finding to repeat:

* the 2-second poll timer at `crates/tuni-gtk/src/window.rs:581` (`PANEL_POLL_SECONDS`, `window.rs:62`) — one per window, armed in `constructed`, never removed — must be either justified by measurement or replaced with a `gio::FileMonitor`. There is no file watcher anywhere in this repository: no `notify` crate, no `gio::FileMonitor`, no inotify. Decide which it should be.
* the cursor blink (`crates/tuni-gtk/src/terminal.rs:1178`) is legitimate while the window is focused. Decide, with a measurement, whether it should stop when the window is unfocused or the terminal loses the keyboard.

Do not claim that performance is good only because the code looks correct.

Prove every important conclusion with:

* code references (file and symbol)
* profiler results (`perf`, `sysprof`, Heaptrack, the GTK Inspector's frame recorder)
* sanitizer results, with their limits stated
* allocation behavior
* object lifetime observations (`GOBJECT_DEBUG=objects`, temporary `Drop` and `dispose` logging)
* source, thread and task behavior
* throughput and frame-time numbers from the harnesses this repository already has
* before-and-after measurements

AUDIT PROCESS

1. Understand the architecture

Map:

* application lifecycle (`crates/tuni-gtk/src/main.rs`, `adw::Application` setup, `startup`, `activate`, and the 14 `TUNI_CAPTURE_*` code paths at `main.rs:234-673`)
* window construction and ownership (`TuniWindow::constructed`, `crates/tuni-gtk/src/window.rs:292`)
* the widget tree and who parents whom — which custom widgets hold children they must `unparent` in `dispose` (`tiles.rs:122`, `files.rs:127`, `info.rs:79`, `window.rs:305`)
* the pane model in `crates/tuni-core/src/panes.rs` versus the widget tree in `crates/tuni-gtk/src/tiles.rs`, and which one is the record
* the tab model — `AdwTabView` is the record of order and selection, the core model is downstream
* the eight `RefCell<HashMap<Id, …>>` registries in `imp::TuniWindow` (`window.rs:218-239`): who inserts, who removes, and whether removal is guaranteed on every close path
* every `Rc<dyn Fn>` handler slot and what it captures: `grid.rs:49`, `files.rs:85`, `editor.rs:38`, `diff.rs:47`, `git.rs:36`, `git.rs:38`, `tiles.rs:51`, and the `thread_local! PREVIEW: RefCell<Option<Rc<dyn Fn>>>` at `preferences.rs:33`
* signal flow: 120 `connect_*` calls across `tuni-gtk`, against exactly one stored `glib::SignalHandlerId` and one `disconnect` (`find.rs:31`, `find.rs:256`)
* connections to process-global objects: four `connect_notify_local` on `gtk::Settings::default()` per `TuniTerminal` (`terminal.rs:562`), one dark-notify on `adw::StyleManager::default()` per window (`window.rs:869`)
* the PTY lifecycle: spawn, reader thread, `async_channel::bounded(64)`, the event pump at `terminal.rs:888`, hangup on close (`window.rs:326`), and the fact that `Pty` has no `Drop` and relies on field drop order to close the master
* the VT lifecycle: `tuni_vt::Terminal` holds `Rc<RefCell<Effects>>` shared with five libghostty C callbacks, is `!Send`, and has `impl Drop` at `crates/tuni-vt/src/lib.rs:1066`
* every `gio::spawn_blocking` site and the generation stamp that guards its result
* every `glib::spawn_future_local` site and what it captures
* every timer, idle and tick source (see step 8 for the full inventory)
* caches: the FIFO texture cache (`terminal.rs:116-160`, `TEXTURE_CACHE = 16`), libghostty's image storage (64 MiB per terminal, `crates/tuni-vt/src/image.rs:26`), `MAX_DECODED` 96 MiB (`image.rs:31`), the reused `search_buf` (`crates/tuni-vt/src/lib.rs:873`)
* persistence: `session.json` and `history.json`, written from `close_request` (`window.rs:322`, `save_session` at `window.rs:1044`)
* the 574-theme table baked in by `include_str!` (`crates/tuni-core/build.rs`)

Identify which object owns which other object, and where an `Rc` keeps something alive that a `Weak` should.

Call out unclear or dangerous ownership: an `Rc<dyn Fn>` stored in the widget it captures, a strong `glib::clone!` capture in a signal handler, a registry entry with no removal path.

2. Find memory leaks and retained objects

Inspect for:

* `Rc` cycles — an `Rc<dyn Fn>` handler stored in the `imp` of a widget that the closure itself captures strongly; check each of the eight slots listed in step 1
* `glib::clone!` captures: exactly one `#[strong]` exists in the whole crate (`terminal.rs:1985`, a `gdk::Clipboard`). Confirm it, and confirm every other capture is `#[weak]`/`#[weak_ref]` with a sound `#[upgrade_or]`
* GObject reference leaks: a widget alive after `dispose`, a floating reference never sunk, a toggle-ref cycle between a widget and a closure GObject holds
* children not `unparent()`ed in `dispose` — a custom `GtkWidget` that dies with a child attached leaks the whole subtree
* signal handlers connected to process-global objects and never disconnected: every `TuniTerminal` adds four handlers to `gtk::Settings::default()`, every `TuniWindow` one to `adw::StyleManager::default()`. Open and close ten terminals — how many handlers remain?
* `glib::SourceId` and `gtk::TickCallbackId` not removed: `imp::TuniTerminal::dispose` (`terminal.rs:411`) removes four. Verify that is all of them, and verify whether `TuniWindow`'s 2-second timer is removed anywhere at all
* entries left behind in the eight `imp::TuniWindow` registries after a pane, tab or project closes
* `pending_history` and `pending_cursors` (`window.rs:236`, `window.rs:239`) — entries for panes that were never restored
* retained scrollback: a `TuniTerminal` kept alive by a stale registry entry keeps its whole VT, its scrollback and its image storage
* the texture cache: FIFO eviction at 16 entries means scrolling past 17 images evicts the one about to be drawn again. Decide whether that is a leak, a thrash, or neither
* reader threads never joined; the master fd closed only by drop order
* child shell processes not reaped
* caches without a byte-size limit
* large `String`/`Vec` copies: `buf[..n].to_vec()` per PTY read (`crates/tuni-pty/src/lib.rs:148`), `dump_history`'s fresh 64 KiB `Vec` per call (`crates/tuni-vt/src/lib.rs:920`)
* global and `thread_local!` retention: `palette.rs:78`, `preferences.rs:33`, `diff.rs:811`, `editor.rs:980`, `window.rs:3258`, `window.rs:3328`, `crates/tuni-vt/src/image.rs:217`

Verify that a `TuniTerminal`, `TuniEditor`, `TuniDiff` and their models are actually dropped after their pane closes — not merely hidden, and not merely unparented.

Add temporary `Drop` impls and `dispose` logging where useful. There is no `Drop` on `imp::TuniTerminal` today, which means nothing currently proves a terminal is destroyed.

Use tools:

* Heaptrack — works on Rust; the primary allocation and leak tool here
* `valgrind --tool=massif` for a heap profile over time, `valgrind --tool=memcheck` where the runtime cost is bearable
* `bytehound` for a long-running allocation profile with call-stack attribution
* `dhat-rs` for in-process allocation counts in a `tuni-core` or `tuni-vt` harness
* `GOBJECT_DEBUG=objects` with `G_DEBUG=fatal-warnings` — the GObject lifetime tool; live-object counts at exit
* `GOBJECT_DEBUG=instance-count` for a per-type census
* `GTK_DEBUG=interactive` — the GTK Inspector: live widget tree, per-widget properties, CSS, and a frame recorder
* ASan+LSan: `RUSTFLAGS="-Zsanitizer=address" cargo +nightly build -Zbuild-std --target x86_64-unknown-linux-gnu`. State the caveat plainly: `libghostty-vt` is Zig-compiled and GTK, GLib and Pango are C, so an instrumented build covers Rust only, and LSan will report large volumes of GTK and GObject noise that needs a suppression file before its output means anything
* `cargo +nightly miri test -p tuni-core` — miri can check `tuni-core` and cannot run GTK or FFI. Say so, so nobody wastes an afternoon

Test repeated workflows:

* open and close 50 panes
* open and close 50 tabs
* open and close 20 projects
* open and close 50 file panes and 50 diff panes
* open and close the panel on each of its three pages, 50 times
* open and close the find bar 50 times — the one path with a real `disconnect`
* open a second window and close it, 20 times
* switch themes 50 times, and toggle light/dark 50 times
* scroll past 20 inline images repeatedly, to exercise the FIFO texture cache
* run a firehose, let it settle, three times

Memory may temporarily increase, but it must return to a stable plateau.

3. Find CPU waste

Use `perf top` and `perf record -g`, `sysprof`, and the GTK Inspector's frame recorder. Cross-check with `TUNI_DEBUG_FRAME_TIME`, which prints p50/p95/max draw time every 120 frames (`terminal.rs:341`, `terminal.rs:447`).

Inspect CPU usage in these states:

* immediately after launch
* idle with one pane at a prompt
* idle with 16 panes at a prompt
* idle with the panel open on Files
* idle with the panel open on Git
* idle with the panel open on Info
* idle with the find bar open and a needle typed
* idle with an editor pane focused
* during a firehose (`cat` of a large file)
* immediately after a firehose stops
* while scrolling the scrollback
* while dragging a pane divider
* after closing every pane but one
* with the window unfocused
* with the window minimized or on another workspace

Look for:

* busy loops
* polling where an event source exists — the 2-second `poll_files` + `poll_diffs` timer (`window.rs:581`) against `gio::FileMonitor`, which this repository does not use anywhere
* recursive synchronous `fs::read_dir` on the main thread: `poll_files` → `TuniFiles::poll` (`files.rs:416`) → `FileTree::rebuild` (`crates/tuni-core/src/files.rs:139`), depth-first to `MAX_DEPTH = 32` (`crates/tuni-core/src/files.rs:19`)
* `closest_git_repository` (`crates/tuni-core/src/workspace.rs:299`) walking up to `/` with `.exists()` stats, reached from the same 2-second timer
* `std::env::current_dir()` on the 2-second timer (`window.rs:978`)
* `std::env::var_os` in hot paths: per feed (`terminal.rs:996`) and per clipboard write (`terminal.rs:1039`)
* short repeating timers — the cursor blink at half the desktop blink cycle, one per terminal (`terminal.rs:1178`)
* `add_tick_callback` work: the scrollbar fade calls `queue_draw()` every frame while it runs (`terminal.rs:1275`, `terminal.rs:1316`). It does `Break` once the thumb is gone, so measure how often it is re-armed instead, given that `feed()` calls `reveal_scrollbar()` on every change in scroll fraction (`terminal.rs:1017`) — a firehose may hold the tick callback up indefinitely
* `queue_draw()` storms: `feed()` (`terminal.rs:985`) unconditionally calls `invalidate_links()`, `schedule_refind()` and `queue_draw()` per 64 KiB chunk, and nothing throttles feeds against the frame clock. Measure the feed-to-frame ratio under `scripts/throughput.sh`
* `schedule_refind` (`terminal.rs:1546`) re-running a whole-scrollback search once per main-loop turn while the find bar is open
* work that scales with scrollback rather than with the viewport
* per-frame heap allocation in `draw()` (`terminal.rs:2220`): `vec![false; cols*rows]`, `vec![0u8; cols*rows]`, a fresh `pango::Layout::new`, and a `FontDescription` clone per style run
* Pango attribute and layout churn; glyph cache misses on fallback faces
* GSK render-node churn: node count per frame, `push_clip`/`pop` nesting depth, and `append_scaled_texture` with `Trilinear` filtering rebuilt each frame instead of cached
* re-measure and re-allocate storms in `TuniTiles` during a divider drag and during a window resize
* `notify("title")` and `notify("cwd")` emitted more often than the value changes
* duplicate work from several handlers on one signal
* `info::snapshot` (`info.rs:352`) walking every `/proc/<pid>/stat` on the machine, plus `/proc/net/tcp` and `/proc/net/tcp6`, plus each candidate process's `fd/` directory — every 2 seconds while the Info page is visible
* work that continues after its pane, tab or window is gone
* `eprintln!` in hot paths — all currently env-gated; confirm the gate is read once at construction and not on every call

Find the exact stacks responsible for CPU use.

Do not optimize cold code.

Optimize verified hot paths first.

4. Check idle performance

Create a repeatable idle benchmark.

Idle means:

* no user input
* at least one shell sitting at a prompt, producing nothing
* no scrolling and no animation expected beyond the cursor blink
* no pending git, diff or info refresh
* no capture harness env vars set
* stable window contents

Measure for at least five minutes, in three configurations: 1 pane, 4 panes, 16 panes.

Report:

* average CPU usage
* CPU spikes and their frequency
* wakeups per second (`powertop`, `perf sched record` then `perf sched latency`)
* `voluntary_ctxt_switches` and `nonvoluntary_ctxt_switches` from `/proc/<pid>/status`, sampled at both ends
* a syscall census: `strace -c -f -p <pid>` over 60 s of idle
* thread count and what each thread is doing (`ls /proc/<pid>/task`, `perf top -t`)
* every live glib source, its interval, its owner and its stop condition
* frames rendered while idle (`GDK_DEBUG=frames`)
* RSS at the start, RSS at the end, and the growth rate
* live GObject counts by type (`GOBJECT_DEBUG=instance-count`, or the Inspector)

Find every source of recurring work.

For each source, explain:

* why it runs
* how often it runs
* whether it is necessary
* whether it can be event-driven — a `gio::FileMonitor` for the file tree and the diff, a GTK notify signal instead of a poll, a PTY event instead of a tick
* whether it can be cancelled
* whether it can use a longer or adaptive interval
* whether it should stop when the window is unfocused
* whether it should stop when its page, pane or panel is not visible
* whether it should stop when its window closes — the 2-second timer is armed in `constructed` and does not appear in `dispose`

Exact 0.00% CPU is not realistic: the compositor, the GLib main loop and the scheduler all cost something.

The real requirement is:

* no application-generated continuous CPU work
* no unnecessary periodic wakeups
* no unexplained CPU spikes
* sustained CPU usage as close to zero as the platform allows

5. Check the thread boundary and the single-thread invariant

This application has no shared mutable state across threads. There is no `Arc`, no `Mutex`, no `RwLock`. Do not report a data race you cannot name a second thread for. What can actually be wrong here is the boundary itself.

Inspect:

* the PTY reader thread (`crates/tuni-pty/src/lib.rs:139`): what it owns, what it sends, and what happens when the channel closes
* `async_channel::bounded(64)` at up to 64 KiB per message — roughly 4 MiB in flight per pane. Measure whether `send_blocking` actually blocks under a firehose, and what that does to the shell
* the event pump at `terminal.rs:888` — does one main-loop turn drain one message or all of them, and which is right?
* the five `gio::spawn_blocking` closures (`git.rs:492`, `git.rs:891`, `diff.rs:353`, `diff.rs:700`, `info.rs:352`): prove that none of them touches a GTK object, a `TuniTerminal`, or anything `!Send`. The compiler enforces `Send` on the closure — say what that does and does not buy you
* the generation-stamp pattern that discards stale results (`git.rs`, `diff.rs`, `info.rs`): confirm every async result is stamp-checked before it is drawn, and that the stamp is bumped on every input change, not only on some
* `git::run_with_input` (`crates/tuni-core/src/git.rs:64`) writes to child stdin and then `wait_with_output()`s. Every caller today reaches it through `spawn_blocking`; confirm that, and confirm no path adds a synchronous one
* the `AtomicU64` at `crates/tuni-core/src/workspace.rs:16` — its ordering, and whether id reuse is possible
* the two `OnceLock` tables (`terminal.rs:356`, `terminal.rs:375`) — initialized on the main thread, read everywhere. Fine, but say why
* the `Rc<RefCell<Effects>>` shared between `tuni_vt::Terminal` and five libghostty C callbacks: what can the C side call, when, and can it re-enter a borrow the Rust side already holds?
* FFI safety at the `libghostty-vt` boundary: buffer lengths, ownership of returned pointers, and behavior on `OutOfSpace`
* thread lifetime at shutdown: reader threads are never joined. `close_request` (`window.rs:313`) calls `shutdown()` on every terminal before chaining to the parent. Trace what actually closes the master fd and unblocks a reader parked in `read()`, and what happens instead if one is parked in `send_blocking` on a full channel

Tools:

* TSan is available (`RUSTFLAGS="-Zsanitizer=thread" cargo +nightly build -Zbuild-std --target x86_64-unknown-linux-gnu`) but will mostly report GTK and GLib internals and cannot see into the Zig-compiled VT. Run it, expect noise, suppress it, and state what coverage you actually got
* `G_DEBUG=fatal-warnings` — turns GLib's cross-thread and assertion warnings into aborts with a stack
* `GDK_SYNCHRONIZE=1` when a rendering problem needs to be pinned to a call site
* `cargo clippy --all-targets -- -W clippy::pedantic` for the ownership lints `make check` does not run

For every value that crosses the boundary, document who writes it, who reads it, on which thread, and what makes that safe — the type system, a generation stamp, or nothing.

6. Find borrow panics, re-entrancy and main-loop lockups

There are no locks to deadlock on. The analogue in an `Rc`/`RefCell` design is a cell borrowed twice, or borrowed across something that calls back in. A double `borrow_mut()` is a panic, not a hang — which is worse, because it is a crash in front of the user.

Search for dangerous patterns:

* a `borrow_mut()` held across an `emit_by_name`, a `notify`, a `queue_draw`, a widget setter, or any call into GTK that can synchronously run a handler
* `feed()` (`terminal.rs:985`) holds `imp.session.borrow_mut()` while the VT parses a whole chunk. Enumerate everything reachable from inside `session.term.feed()` — including the five C callbacks — and prove none of it re-enters `imp.session`
* `draw()` (`terminal.rs:2220`) holds `imp.session.borrow_mut()` for roughly 230 lines, and takes `imp.images` and `imp.textures` inside it. Map the borrow order and prove it is consistent everywhere else
* handler slots invoked while the widget's own `imp` is borrowed — an `Rc<dyn Fn>` cloned out of a `RefCell` and called while that same `RefCell` is still borrowed
* a signal handler that mutates the state its emitter is iterating
* a `RefCell<HashMap<…>>` iterated while a handler inserts into it or removes from it
* a `borrow()` in a `Drop` or `dispose` while a caller further up the stack holds `borrow_mut()`
* blocking the main context: any synchronous `fs::read`/`fs::write`, any `sync_all()`, any `Command::output()`, any `write_all` to a PTY that can fill (`crates/tuni-pty/src/lib.rs:178`), any `recv_blocking` — on the main thread
* `editor::load` on the main thread (`editor.rs:496` → `crates/tuni-core/src/editor.rs`, `fs::metadata` plus `fs::read`), and `editor::save`, which does an `fsync` via `sync_all()`
* `save_session()` inside `close_request` (`window.rs:322` → `window.rs:1044`): per terminal a `dump_history` (whole-scrollback select-all, VT format, grow-and-retry loop with a fresh 64 KiB `Vec` each call, `crates/tuni-vt/src/lib.rs:920`), then serde_json, then two write-plus-rename pairs — all on the main thread, at the moment the user clicked the close button
* a `spawn_blocking` future awaited in a way that starves the loop it was meant to free
* circular dependencies between widgets — the panel asks the window which terminal is focused, which asks the pane model, which the panel is currently mutating

Build a borrow map: for every `RefCell` field with a borrow spanning more than a few lines, list every function reachable during that borrow and whether any of them can touch the same cell.

For every possible panic, show the exact call sequence that reaches it.

Reproduce with `RUST_BACKTRACE=full`, `G_DEBUG=fatal-warnings` and a debug build — a `RefCell` panic inside a GTK signal handler unwinds into C, and the backtrace is the only evidence you will get.

7. Check GTK4, GSK and libadwaita performance problems

Inspect:

* `snapshot()` frequency per widget, against frames actually presented (`GDK_DEBUG=frames`)
* what triggers `queue_draw()`, and how many of those calls coalesce into one frame
* `measure()` and `size_allocate()` frequency in `TuniTiles` and `TuniTerminal`, especially during a divider drag and a window resize
* the resize debounce (`RESIZE_SETTLE`, `terminal.rs:1088`) — does the shell hear about the new size once, or once per intermediate frame?
* GSK node count per frame, clip depth, and whether the renderer falls back (`GSK_DEBUG=renderer,fallback`)
* texture upload per frame versus per cache miss, and the `Trilinear` path for scaled images
* `pango::Layout` and `AttrList` construction per frame and per run, and the per-run `FontDescription` clone
* glyph cache behavior when a fallback face is hit — a box-drawing or emoji run in a scrolling buffer
* `gtk::Settings` and `adw::StyleManager` notify handlers: four per terminal and one per window, on process-global objects, none disconnected. Confirm the count grows with instances, then decide whether it matters
* `AdwTabView` page churn: is a `TuniTerminal` destroyed when its page closes, or held by a registry?
* popovers and menus — parented, not packed; each needs an `unparent` in `dispose`
* `sourceview5` buffer cost for a large file, and whether wrapping changes the layout cost class
* the `TuniSwitcher` thumbnail path (`grid.rs:343`): how a card's picture is produced, and whether it is regenerated on every invocation
* window unfocus and occlusion — does anything stop when the window is not on screen?
* second-window behavior: a non-owning window never writes the session; confirm it also does not arm the session machinery

Make sure a closed pane's terminal, editor or diff is actually dropped, not merely unparented and still held by a `HashMap`.

Make sure a hidden panel page is not still polling, and a hidden terminal is not still drawing.

8. Check timers and recurring work

List every:

* `glib::timeout_add_seconds_local` — the panel poll (`window.rs:581`, 2 s, one per window)
* `glib::timeout_add_local` — the resize settle (`terminal.rs:1088`) and the cursor blink (`terminal.rs:1178`), both per terminal
* `glib::timeout_add_local_once` — the progress-stale retire (`terminal.rs:2551`), the Info page's post-terminate reload (`info.rs:671`), and the twenty in `main.rs:246-634` that drive the capture harness
* `glib::idle_add_local_once` — the find re-run (`terminal.rs:1546`), plus the startup and restore idles (`main.rs:179`, `window.rs:375`, `window.rs:1665`, `editor.rs:586`, `palette.rs:256`)
* `add_tick_callback` — the scrollbar fade (`terminal.rs:1275`)
* the PTY event pump future (`terminal.rs:888`) and the clipboard-read future (`terminal.rs:1982`)
* the five `gio::spawn_blocking` tasks and what re-arms them
* the reader thread's blocking `read()` loop

For each one report:

* interval, or "per frame", or "per event"
* owner, and whether it is per-process, per-window or per-terminal
* what removes it, and on which code path
* whether it survives `dispose`
* whether it runs while the window is unfocused, minimized or occluded
* whether it runs when its page or panel is not visible
* whether it can be replaced by an event source — `gio::FileMonitor`, a GTK notify signal, a PTY event

The 2-second panel poll is the single most consequential entry in this table. It is armed once per window in `constructed`, never removed, and it drives a recursive synchronous directory read, an upward `.exists()` walk to `/`, and a `current_dir()` call. Decide by measurement whether it costs anything, and if it does, whether a `GFileMonitor` is the answer.

9. Check memory efficiency

Look for:

* per-frame allocations in `draw()` — `vec![false; cols*rows]`, `vec![0u8; cols*rows]`, a fresh `pango::Layout`, a `FontDescription` clone per run; all reusable across frames
* `buf[..n].to_vec()` per PTY read (`crates/tuni-pty/src/lib.rs:148`) — one allocation of up to 64 KiB per read, at up to hundreds of reads a second
* `dump_history`'s fresh `vec![0u8; 64 * 1024]` per call plus grow-and-retry (`crates/tuni-vt/src/lib.rs:920`), against `search_buf`, which is reused but never shrunk (`crates/tuni-vt/src/lib.rs:873-897`)
* `Find.hits: Vec<Hit>` — unbounded, one entry per match over the whole scrollback
* `LinkHover.cells` — unbounded, one entry per matching cell
* the `History` map in `crates/tuni-core/src/session.rs` — a full scrollback dump per pane, held in memory before it is written
* `pending_history` and `pending_cursors` (`window.rs:236`, `window.rs:239`) — entries for panes that may never be created
* scrollback: `terminal.scrollback-lines` defaults to 10 000. Measure bytes per line as libghostty actually stores it, and the total per pane at the default and at 50 000
* the texture cache — FIFO at 16 entries, with no byte-size bound; an entry is a full decoded RGBA image
* libghostty image storage — 64 MiB per terminal (`crates/tuni-vt/src/image.rs:26`), `MAX_DECODED` 96 MiB per decode (`image.rs:31`). Sixteen panes is a 1 GiB ceiling; determine whether it is reachable and whether the bound belongs per-window instead
* the 574-theme table baked in by `include_str!` (`crates/tuni-core/build.rs`) — measure its binary-size contribution and whether any of it is parsed at startup or only on lookup
* repeated JSON decoding of settings or session where a cached value would do
* data loaded eagerly that could be lazy
* `String` allocation in the OSC state machine and in title/cwd propagation

Recommend limits and eviction policies based on measured behavior.

Do not remove useful caching if doing so increases CPU or I/O.

Balance RAM, CPU, disk and responsiveness.

10. Check responsiveness and main-context blocking

Measure:

* cold start to first paint
* time to first shell prompt
* pane split latency (keypress to new pane drawn)
* tab switch latency
* window close latency — `save_session` runs inside `close_request` and dumps every pane's scrollback before the window goes
* panel open latency on each of the three pages
* file pane open latency for a 1 MiB file and for one at the editor's size ceiling
* diff pane open latency on a large diff
* find-bar latency: keystroke to updated tally, at 10 000 and 50 000 scrollback lines
* scroll smoothness through a full scrollback
* input latency under a firehose — type while a `cat` is running, and measure the echo
* frame time at p50/p95/max via `TUNI_DEBUG_FRAME_TIME`, at 80x24 and at 200x60

Find every synchronous operation on the main thread: `fs::read_dir` (the file tree poll), `fs::metadata` and `fs::read` (`editor.rs:496`), `sync_all()` (editor save), the `.exists()` walk (`crates/tuni-core/src/workspace.rs:299`), `current_dir()` (`window.rs:978`), `write_all` to a PTY (`crates/tuni-pty/src/lib.rs:178`), and `dump_history` at close.

No heavy work should block the main context.

Use `gio::spawn_blocking`, batching or laziness where appropriate.

Do not move GTK widget or GSK operations off the main thread. The VT state is `!Send` and the widgets are main-thread only; moving the terminal off-thread is not on the table.

11. Check terminal throughput, scrollback and multi-pane scaling

Throughput and the feed path:

* run `scripts/throughput.sh` — it generates an N-MiB mixed plain/SGR payload, `cat`s it inside Tuni through the `TUNI_CAPTURE_*` harness, subtracts a bare-startup run and reports MiB/s; `RUNS=2`, takes the minimum. Pass other terminals as argv to compare: `scripts/throughput.sh 200 foot kitty alacritty ghostty`
* `docs/DESIGN.md:181` records 117 MiB/s for Tuni against Ghostty 98 and Konsole 45, and a 120x37 draw at 516 µs p50 and 1.0 ms p95 against a 16.7 ms budget. Reproduce those on your machine before comparing anything to them
* split the cost: libghostty parse, `take_effects` drain, `snapshot()` grid build, Pango layout, GSK node construction, GPU submit. Use `perf record -g` during a firehose, and `sysprof` for the GLib-marked view
* measure the feed-to-frame ratio. `feed()` calls `queue_draw()` per chunk and nothing throttles it against the frame clock. How many feeds land per presented frame, and is any of that draw work discarded?
* measure the channel: how full does the 64-slot queue get, and does `send_blocking` block? Instrument if necessary
* under a firehose, determine whether the frame clock is starved, whether input latency degrades, and whether the scrollbar tick callback is permanently re-armed

Scrollback memory:

* bytes per line, measured, at `scrollback_lines = 10 000`
* RSS after consuming 200 MiB — does it plateau at the scrollback bound, or keep climbing?
* the same at 50 000 lines
* `history.json` size after a session with a full scrollback, and how long `save_session` takes to produce it
* whether the per-pane history line cap is what actually gets written

Multi-pane scaling:

* per pane the app costs one OS thread, one `async_channel`, one blink timer, one tick-callback slot, and one VT with its own scrollback and its own 64 MiB image ceiling
* measure idle CPU, idle wakeups and RSS at 1, 4 and 16 panes
* measure throughput into one pane while 15 others sit idle
* measure throughput into four panes at once
* state whether the cost is linear, and if it is superlinear, say in which term

Repro harness: `crates/tuni-gtk/examples/resize_repro.rs` is the pattern for a VT-level repro with no GTK — a real `Pty` plus a real `Terminal`, drained on a timer and dumped as text. Copy it for any finding reproducible below the widget layer; a VT-only repro is faster, deterministic, and runs in CI.

12. Make fixes

For every confirmed problem:

* explain the root cause
* provide the exact file and symbol (`module::function`, or the `imp` struct field)
* show the problematic code
* implement the smallest safe fix
* explain why the fix is correct
* explain possible side effects
* add a regression test or reproducible verification method
* measure before and after

Do not perform unrelated architecture rewrites.

Do not introduce abstractions without measurable benefit.

Do not trade correctness for lower resource usage.

Do not hide problems by increasing timer intervals without understanding the root cause.

Do not remove synchronization without proving safety.

Do not replace `Rc` with `Arc`, or introduce a thread, to solve a main-thread stall. The fix for main-thread work is to move it to `gio::spawn_blocking` or to stop doing it — not to make the type system harder.

Do not weaken a borrow into a clone to silence a panic without understanding what the second borrow was for.

13. Add performance diagnostics

Where useful, add debug-only diagnostics for:

* widget destruction (a `Drop` on the `imp` struct, or a log in `dispose`)
* glib source creation and removal
* task start, completion and supersession (generation stamps)
* cache size and eviction (textures, images)
* feed size and rate, and drawn-frame count
* borrow depth on the hottest `RefCell`s
* long main-thread operations, timed and warned above a threshold

Follow the pattern this codebase already uses: an environment variable checked once, printing to stderr, costing nothing when unset — `TUNI_DEBUG_FRAME_TIME` (`terminal.rs:341`), `TUNI_DEBUG_PTY_WRITE` (`terminal.rs:996`), `TUNI_DEBUG_CLIPBOARD` (`terminal.rs:1039`). Note that the latter two read the environment on every call; a new one should read it once into a `Cell` at construction, the way frame timing does.

Do not leave logging enabled by default. Do not add a logging framework.

14. Add tests and benchmarks

There are 208 `#[test]` functions: roughly 140 in `tuni-core`, 60 in `tuni-vt` (13 in `osc.rs`, 47 in `tests/feed.rs`), and 8 in `tuni-gtk` — all of them in `keymap.rs`. There are zero tests in `tuni-pty` and zero across the 10.8k lines of GTK widget code. There are no benches and no criterion dependency. CI (`.github/workflows/ci.yml`, fedora:43) runs fmt, build, clippy with `-D warnings`, test and desktop validation — there is no bench step, no sanitizer step, no miri step and no performance gate.

Create tests for:

* object release — a closed pane leaves no entry in any `imp::TuniWindow` registry
* repeated open and close of panes, tabs, projects and windows, with no growth
* glib source removal on `dispose`
* signal handler disconnection on process-global objects
* texture cache eviction at its bound
* borrow discipline on the `feed` and `draw` paths
* PTY lifecycle in `tuni-pty`, which has no tests at all: spawn, read, write, resize, hangup, reader-thread exit, and channel-full backpressure
* session save and restore round-trip, including a full scrollback
* `dump_history` at the buffer boundary, forcing the grow-and-retry path
* find over a large scrollback
* rapid state transitions — split and close a pane repeatedly, start and stop a firehose repeatedly

Add criterion benches for: `tuni-vt` feed throughput on a fixed payload, `Terminal::snapshot()` grid build, `screen_text` plus search over 10 000 lines, `dump_history` at the cap, and `tuni-core` diff parsing and `/proc` parsing. These are the parts that can be benchmarked without a display, which makes them the parts that could gate CI.

Use the headless patterns this repository already has: `crates/tuni-gtk/examples/resize_repro.rs` for VT-level work, and the `TUNI_CAPTURE_*` harness (`main.rs:234-673`) for widget-level work under a nested compositor or Xvfb.

Note the build constraint: `libghostty-vt` compiles only with Zig 0.15.2, which `make zig` fetches. Any CI job you propose must account for that.

OUTPUT FORMAT

Return the audit in this exact structure:

# Executive summary

Include:

* overall severity
* biggest CPU problem
* biggest memory problem
* biggest main-thread stall
* biggest borrow-panic or re-entrancy risk
* expected improvement after fixes

# Baseline measurements

Provide a table:

| Scenario | CPU average | CPU peaks | Frame p50 | Frame p95 | Memory start | Memory end | Wakeups | Notes |
| -------- | ----------: | --------: | --------: | --------: | -----------: | ---------: | ------: | ----- |

Scenarios must include: 1 pane idle, 16 panes idle, panel on each page, find bar open, firehose, post-firehose.

Then a throughput table, covering Tuni plus at least two of foot, kitty, alacritty, ghostty:

| Terminal | Payload | Consume time | MiB/s |
| -------- | ------: | -----------: | ----: |

# Critical findings

For every finding include:

* severity: critical, high, medium or low
* category
* file
* symbol
* evidence
* root cause
* user-visible impact
* proposed fix
* verification method

# Memory findings

# CPU findings

# Idle-performance findings

# Thread-boundary findings

# Borrow-panic and re-entrancy findings

# Main-thread and responsiveness findings

# GTK and GObject lifecycle findings

# Terminal throughput findings

Cover the feed-path cost split, scrollback growth and plateau, and multi-pane scaling, each with its numbers.

# Timer and recurring-work inventory

Provide a table:

| Source | File:line | Interval | Owner | Removed on dispose | Stops when hidden | Required | Recommendation |
| ------ | --------- | -------: | ----- | ------------------ | ----------------- | -------- | -------------- |

# Changes implemented

For each change include a focused diff.

# Before-and-after results

Use the same benchmark scenarios as the baseline, including `scripts/throughput.sh` and `TUNI_DEBUG_FRAME_TIME`.

# Remaining risks

Clearly state anything that could not be proven, and specifically what the sanitizers could not see: the Zig-compiled VT and the C toolkit below it.

# Final verdict

Answer these questions directly:

* Does memory stabilize?
* Are terminals, editors and diffs dropped when their panes close?
* Is idle CPU close to zero, at 1 pane and at 16?
* Are there unnecessary wakeups? Is the 2-second poll one of them?
* Is the single-thread invariant actually held at every boundary?
* Are there reachable borrow panics?
* Is the main loop free under a firehose?
* What is the next highest-value optimization?

IMPORTANT RULES

DO NOT GUESS.

DO NOT SAY "PROBABLY FINE".

DO NOT CALL SOMETHING A LEAK WITHOUT EVIDENCE.

DO NOT CALL SOMETHING SAFE WITHOUT NAMING WHAT MAKES IT SAFE.

DO NOT REPORT A DATA RACE YOU CANNOT NAME A SECOND THREAD FOR.

DO NOT OPTIMIZE CODE THAT DOES NOT APPEAR IN MEASUREMENTS.

DO NOT MAKE MASSIVE REWRITES BEFORE ESTABLISHING A BASELINE.

FIRST MEASURE.

THEN FIND ROOT CAUSE.

THEN FIX.

THEN MEASURE AGAIN.

If you cannot run profilers or sanitizers in the current environment:

* still perform the static audit
* prepare exact build configurations: the nightly sanitizer invocations with `-Zbuild-std`, a criterion bench skeleton, and the `make zig` prerequisite
* provide exact manual test scenarios
* provide exact profiling tools and commands to run:
  * `scripts/throughput.sh 200 foot kitty alacritty ghostty`
  * `TUNI_DEBUG_FRAME_TIME=1 ./target/release/tuni`
  * `heaptrack ./target/release/tuni`
  * `perf record -g --call-graph dwarf ./target/release/tuni`, then `hotspot perf.data`
  * `perf top -p $(pidof tuni)`
  * `perf sched record -p $(pidof tuni)` then `perf sched latency`
  * `perf stat -e sched:sched_switch -p $(pidof tuni) -- sleep 60`
  * `sysprof-cli --use-trace-fd -- ./target/release/tuni`
  * `powertop --time=60`
  * `strace -c -f -p $(pidof tuni)`
  * `valgrind --tool=massif ./target/release/tuni`
  * `GTK_DEBUG=interactive ./target/release/tuni`
  * `GDK_DEBUG=frames`, `GSK_DEBUG=renderer,fallback`, `GTK_DEBUG=actions,layout`
  * `GOBJECT_DEBUG=objects,instance-count`, `G_DEBUG=fatal-warnings`, `GDK_SYNCHRONIZE=1`
  * `cargo +nightly miri test -p tuni-core`
  * `cargo clippy --all-targets -- -W clippy::pedantic`
  * `grep ctxt /proc/$(pidof tuni)/status`
* explain what evidence must be collected
* clearly separate confirmed problems from suspected problems
* never fabricate benchmark results
