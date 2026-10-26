//! What a hangup has to do, checked against a real shell and a real PTY.
//!
//! The case that matters is the one a closed pane hits every day: a shell with
//! something running in front of it. Closing the master alone does not reach
//! that shell — the reader thread holds a duplicate of the master, so the
//! kernel never hangs the session up — and a shell that survives keeps its job,
//! its reader thread, its file descriptor and, through the channel, the widget
//! that was supposed to die with the pane.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tuni_pty::{Pty, PtyConfig, PtyEvent};

/// Is this process still there? Signal 0 asks without sending anything.
fn alive(pid: u32) -> bool {
    // Safe: signal 0 delivers nothing, and the pid is one this test spawned and
    // has not waited for.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn wait_until_gone(pid: u32, limit: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < limit {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    !alive(pid)
}

fn shell() -> PtyConfig {
    PtyConfig {
        shell: Some(PathBuf::from("/bin/sh")),
        ..PtyConfig::default()
    }
}

#[test]
fn hangup_ends_a_shell_that_is_running_something() {
    let mut pty = Pty::spawn(&shell()).expect("spawn a shell on a pty");
    let pid = pty.shell_pid().expect("the shell has a pid");
    let events = pty.events();

    // Give the shell a foreground job, and let it get as far as reading none of
    // the input that follows.
    pty.write(b"sleep 300\n").expect("write to the pty");
    std::thread::sleep(Duration::from_millis(400));
    assert!(alive(pid), "the shell should still be running its job");

    drop(pty);

    assert!(
        wait_until_gone(pid, Duration::from_secs(5)),
        "the shell outlived its pty: pid {pid} is still there"
    );

    // The reader thread must come back too, because its copy of the master is
    // the descriptor that kept the session open.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut exited = false;
    while Instant::now() < deadline {
        match events.recv_blocking() {
            Ok(PtyEvent::Exited) => {
                exited = true;
                break;
            }
            Ok(PtyEvent::Output(_)) => {}
            Err(_) => break,
        }
    }
    assert!(exited, "the reader thread never reported the hangup");
}

#[test]
fn writing_to_a_shell_that_is_not_reading_does_not_block_the_caller() {
    // The caller is always the GTK main loop, and a shell with a foreground job
    // that reads nothing fills the master's buffer in about 64 KiB. Before the
    // writer thread this call parked the main loop until the shell read — which
    // it could not do, because its own output had nowhere to go while the main
    // loop was in `write` instead of draining the reader's channel.
    let mut pty = Pty::spawn(&shell()).expect("spawn a shell on a pty");
    pty.write(b"sleep 5\n").expect("write to the pty");
    std::thread::sleep(Duration::from_millis(300));

    // Whole lines, not a single long one: a canonical-mode line discipline
    // *discards* input past its line limit, so a megabyte of one unterminated
    // line never blocks anybody and would not test anything. Completed lines
    // are kept until the shell reads them, and that is what fills up.
    let paste: Vec<u8> = std::iter::repeat_n(b"echo hello\n".as_slice(), 8192)
        .flatten()
        .copied()
        .collect();

    let started = Instant::now();
    pty.write(&paste).expect("write to the pty");
    let took = started.elapsed();

    assert!(
        took < Duration::from_secs(1),
        "write blocked for {took:?} on a shell that is not reading"
    );
}

#[test]
fn a_shell_that_leaves_on_its_own_is_reaped() {
    let mut pty = Pty::spawn(&shell()).expect("spawn a shell on a pty");
    let pid = pty.shell_pid().expect("the shell has a pid");

    pty.write(b"exit\n").expect("write to the pty");
    // The shell is gone within milliseconds, and stays a zombie until somebody
    // waits for it. Nothing in the process can see the difference, so the test
    // reads the state the kernel publishes.
    std::thread::sleep(Duration::from_millis(400));
    drop(pty);

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut state = String::new();
    while Instant::now() < deadline {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            // Field three, after the comm field's parentheses.
            Ok(stat) => {
                state = stat
                    .rsplit_once(") ")
                    .and_then(|(_, rest)| rest.split(' ').next())
                    .unwrap_or_default()
                    .to_owned();
                if state != "Z" {
                    break;
                }
            }
            // Reaped: the entry is gone.
            Err(_) => {
                state = "gone".to_owned();
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_ne!(state, "Z", "the shell was left as a zombie");
}

#[test]
fn a_shell_that_leaves_says_with_what_code() {
    // What tells a dropped ssh connection, 255, from a logout that ended with
    // the far shell's own code: the line ssh prints is the same for both.
    let mut pty = Pty::spawn(&shell()).expect("spawn a shell on a pty");
    let events = pty.events();
    pty.write(b"exit 7\n").expect("write to the pty");
    while let Ok(event) = events.recv_blocking() {
        if matches!(event, PtyEvent::Exited) {
            break;
        }
    }
    assert_eq!(pty.finish(), Some(7));
}

#[test]
fn a_program_that_goes_raw_is_seen_to() {
    // What a host's on-connect snippet waits on: `ssh` asks for a password in
    // line mode and goes raw once it is logged in. `stty raw` is the same
    // switch without a server to log in to. A program run directly, the way a
    // pane runs `ssh`, and not an interactive shell, whose line editor takes
    // the terminal out of line mode at every prompt.
    let mut pty = Pty::spawn(&PtyConfig {
        argv: ["sh", "-c", "read line; stty raw -echo; sleep 300"]
            .map(str::to_owned)
            .to_vec(),
        ..PtyConfig::default()
    })
    .expect("spawn a program on a pty");
    std::thread::sleep(Duration::from_millis(200));
    assert!(!pty.is_raw(), "a program reading a line keeps line mode");

    pty.write(b"answer\n").expect("write to the pty");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pty.is_raw() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(pty.is_raw(), "the terminal never left line mode");
}
