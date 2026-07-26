//! Headless repro for the prompt-duplication-on-resize report.
//!
//! Spawns the real shell on a real PTY, feeds its output into the same VT this
//! app uses, resizes exactly the way `TuniTerminal::commit_size` does, and
//! dumps the viewport before and after. Temporary: delete when the bug is
//! understood.

use std::time::{Duration, Instant};

use tuni_pty::{Pty, PtyConfig, PtyEvent};
use tuni_vt::Terminal;

fn drain(
    term: &mut Terminal,
    pty: &mut Pty,
    pty_events: &async_channel::Receiver<PtyEvent>,
    for_: Duration,
) {
    let deadline = Instant::now() + for_;
    let mut raw = Vec::new();
    while Instant::now() < deadline {
        match pty_events.try_recv() {
            Ok(PtyEvent::Output(bytes)) => {
                raw.extend_from_slice(&bytes);
                term.feed(&bytes);
            }
            Ok(PtyEvent::Exited) => break,
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
        // Answer the terminal's query responses the way the widget does.
        // fish sends DA1 and terminfo queries at startup and waits for the
        // replies before it draws a prompt at all.
        let effects = term.take_effects();
        if !effects.pty_write.is_empty() {
            let _ = pty.write(&effects.pty_write);
        }
    }
    if std::env::var_os("REPRO_RAW").is_some() && !raw.is_empty() {
        println!("raw: {:?}", String::from_utf8_lossy(&raw));
    }
}

fn dump(term: &mut Terminal, label: &str) {
    let grid = term.snapshot().expect("snapshot");
    println!("--- {label} ({}x{}) ---", grid.cols, grid.rows);
    for row in 0..grid.rows {
        let mut line = String::new();
        for cell in grid.row(row) {
            line.push_str(if cell.text.is_empty() {
                " "
            } else {
                &cell.text
            });
        }
        let line = line.trim_end();
        if !line.is_empty() {
            println!("{row:>2} |{line}|");
        }
    }
    if let Some(cursor) = grid.cursor {
        println!("cursor: col {} row {}", cursor.col, cursor.row);
    }
    println!();
}

fn main() {
    let wide: u16 = 120;
    let narrow: u16 = 80;
    let rows: u16 = 20;

    let mut term = Terminal::new(wide, rows, 1000).expect("terminal");
    // libghostty-vt's C API starts with `shell_redraws_prompt = .false`, so
    // the prompt is never cleared before a reflow. The sequence that turns it
    // on is one the shell is meant to send; sending it ourselves at startup
    // arms it for any shell that marks its prompts at all. The trailing `C`
    // puts the cursor's semantic content back to command output, so nothing is
    // treated as a prompt until a shell actually says so.
    if let Ok(mode) = std::env::var("REPRO_ARM") {
        term.feed(b"\x1b]133;A;redraw=1\x1b\\");
        if mode == "c" {
            term.feed(b"\x1b]133;C\x1b\\");
        }
    }
    let mut config = PtyConfig {
        cwd: Some(std::env::current_dir().expect("cwd")),
        cols: wide,
        rows,
        ..PtyConfig::default()
    };
    if let Ok(zdotdir) = std::env::var("REPRO_ZDOTDIR") {
        config.env.insert("ZDOTDIR".to_owned(), zdotdir);
    }
    if let Ok(dirs) = std::env::var("REPRO_XDG_DATA_DIRS") {
        config.env.insert("XDG_DATA_DIRS".to_owned(), dirs);
    }
    if let Ok(shell) = std::env::var("REPRO_SHELL") {
        config.shell = Some(shell.into());
    }
    let mut pty = Pty::spawn(&config).expect("pty");
    let events = pty.events();

    drain(&mut term, &mut pty, &events, Duration::from_millis(2500));

    // Real command output above the prompt: it has to survive the reflow, and
    // it is what a prompt-clearing resize could wrongly eat.
    // Anything but `1` is taken as the command itself, so a shell with other
    // syntax than zsh can be driven too.
    if let Ok(cmd) = std::env::var("REPRO_OUTPUT") {
        let cmd = if cmd == "1" {
            "printf 'y%.0s' {1..150}; echo".to_owned()
        } else {
            cmd
        };
        pty.write(format!("{cmd}\n").as_bytes()).expect("write");
        drain(&mut term, &mut pty, &events, Duration::from_millis(1500));
    }
    dump(&mut term, "start");

    // The order `commit_size` uses: VT first, then the ioctl that raises
    // SIGWINCH, so the grid is the new shape before the shell redraws into it.
    term.resize(narrow, rows, 8, 16).expect("vt resize");
    pty.resize(narrow, rows, 8, 16).expect("pty resize");
    drain(&mut term, &mut pty, &events, Duration::from_millis(1500));
    dump(&mut term, "after shrink");

    term.resize(wide, rows, 8, 16).expect("vt resize");
    pty.resize(wide, rows, 8, 16).expect("pty resize");
    drain(&mut term, &mut pty, &events, Duration::from_millis(1500));
    dump(&mut term, "after grow back");

    term.resize(narrow, rows, 8, 16).expect("vt resize");
    pty.resize(narrow, rows, 8, 16).expect("pty resize");
    drain(&mut term, &mut pty, &events, Duration::from_millis(1500));
    dump(&mut term, "after shrink again");
}
