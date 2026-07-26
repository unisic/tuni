//! PTY spawning and I/O for a terminal session.
//!
//! The reader runs on its own thread and hands buffers to the GTK main thread
//! over an async channel, because the VT state in `tuni-vt` is `!Send` and must
//! only ever be touched from the main thread.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// How much PTY output to hand over in one buffer. Big enough that a firehose
/// like `yes` does not wake the main loop per line, small enough that
/// interactive output still lands in the next frame.
const READ_BUF: usize = 64 * 1024;

#[derive(Debug)]
pub enum Error {
    Spawn(String),
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(msg) => write!(f, "failed to start shell: {msg}"),
            Self::Io(err) => write!(f, "pty I/O error: {err}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// What the reader thread sends to the main thread.
#[derive(Debug)]
pub enum PtyEvent {
    Output(Vec<u8>),
    /// The shell exited or the master side closed.
    Exited,
}

/// The executable a configured shell name refers to, or `None` for one this
/// machine has no such program for.
///
/// A path, meaning anything with a separator in it, is taken as written and has to
/// be a file that can be run. A bare name is looked up on `PATH`, the way a
/// shell would look it up, so `fish` in the settings means the `fish` a command
/// line would have started.
#[must_use]
pub fn resolve_shell(name: &str) -> Option<PathBuf> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    if name.contains('/') {
        let path = PathBuf::from(name);
        return executable(&path).then_some(path);
    }
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .map(|directory| directory.join(name))
        .find(|path| executable(path))
}

/// Whether a path names something this user can actually run: a regular file
/// with an execute bit they hold. A directory named `fish` on `PATH` is not a
/// shell, and neither is one that is only executable by root.
fn executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

pub struct PtyConfig {
    /// Shell to run. Defaults to `$SHELL`, then the passwd entry, then `/bin/sh`.
    /// Resolve a configured name with [`resolve_shell`] before putting it here:
    /// this is the path that gets run.
    pub shell: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    /// Extra environment on top of the inherited one.
    pub env: HashMap<String, String>,
    pub cols: u16,
    pub rows: u16,
    pub cell_width_px: u16,
    pub cell_height_px: u16,
}

impl Default for PtyConfig {
    fn default() -> Self {
        let mut env = HashMap::new();
        // libghostty-vt implements Ghostty's terminal, so its terminfo is the
        // honest description. Ghostty installs `xterm-ghostty` system-wide;
        // shipping our own copy is Etap 10 work.
        env.insert("TERM".to_owned(), "xterm-ghostty".to_owned());
        env.insert("COLORTERM".to_owned(), "truecolor".to_owned());
        env.insert("TERM_PROGRAM".to_owned(), "tuni".to_owned());

        Self {
            shell: None,
            cwd: None,
            env,
            cols: 80,
            rows: 24,
            cell_width_px: 8,
            cell_height_px: 16,
        }
    }
}

pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    /// Bytes on their way to the shell. A queue rather than the writer itself
    /// because writing is what blocks — see the thread in `spawn`.
    writes: async_channel::Sender<Vec<u8>>,
    /// The shell. Held in an `Option` so the hangup can hand it to whoever
    /// reaps it, and held on this side until then so its pid cannot be recycled
    /// under the signal the hangup sends.
    child: Option<Box<dyn Child + Send + Sync>>,
    events: async_channel::Receiver<PtyEvent>,
}

impl Drop for Pty {
    /// Hangs up the way closing a terminal window does.
    ///
    /// Dropping the master alone does not: the reader thread holds a duplicate
    /// of it and is parked in `read()`, so the kernel still sees an open master
    /// and sends nobody a hangup. A shell sitting at a prompt does leave —
    /// `portable-pty`'s writer sends it an EOF as it goes — but a shell with a
    /// foreground job never reads that EOF, so the shell, the job, the reader
    /// thread and its fd all outlive the pane. SIGHUP to the shell's process
    /// group is what actually ends it; the kernel then hangs up the foreground
    /// job as well, because the shell it kills is the session leader holding
    /// the controlling terminal.
    ///
    /// Reaping happens on a thread of its own, because waiting here would block
    /// the frame that is closing the pane for as long as the shell takes to
    /// die.
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        if let Some(pid) = child.process_id() {
            // Safe: `pid` names a process that has not been waited for — this
            // is the only place the child is handed away — so the kernel still
            // holds its slot and the number cannot mean a different process.
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGHUP);
            }
        }
        let reaper = std::thread::Builder::new()
            .name("tuni-pty-reaper".to_owned())
            .spawn(move || {
                let _ = child.wait();
                if std::env::var_os("TUNI_DEBUG_LIFETIME").is_some() {
                    eprintln!("tuni lifetime -pty-child");
                }
            });
        // A machine that cannot start the thread would leak a zombie rather
        // than a shell; nothing here can do better than let it.
        drop(reaper);
    }
}

impl Pty {
    pub fn spawn(config: &PtyConfig) -> Result<Self> {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: config.rows.max(1),
                cols: config.cols.max(1),
                pixel_width: config.cell_width_px.saturating_mul(config.cols),
                pixel_height: config.cell_height_px.saturating_mul(config.rows),
            })
            .map_err(|e| Error::Spawn(e.to_string()))?;

        // The default-prog builder is the one that starts a *login* shell: it
        // resolves $SHELL then the passwd entry, and prefixes argv[0] with '-'.
        // An explicit shell rides in through $SHELL rather than through argv,
        // because the builder resolves the executable from argv[0] and a
        // login-shell argv[0] is not a path.
        let mut cmd = CommandBuilder::new_default_prog();
        if let Some(shell) = &config.shell {
            cmd.env("SHELL", shell);
        }
        if let Some(cwd) = &config.cwd {
            cmd.cwd(cwd);
        }
        for (key, value) in &config.env {
            cmd.env(key, value);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| Error::Spawn(e.to_string()))?;
        // Drop the slave immediately: while we hold it, the master never sees
        // EOF after the child exits.
        drop(pair.slave);

        let mut writer = pair
            .master
            .take_writer()
            .map_err(|e| Error::Spawn(e.to_string()))?;
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| Error::Spawn(e.to_string()))?;

        // Bounded so a firehose applies backpressure to the reader instead of
        // growing an unbounded queue the UI can never drain.
        let (tx, rx) = async_channel::bounded(64);
        std::thread::Builder::new()
            .name("tuni-pty-reader".to_owned())
            .spawn(move || {
                let mut buf = vec![0u8; READ_BUF];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if tx
                                .send_blocking(PtyEvent::Output(buf[..n].to_vec()))
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
                let _ = tx.send_blocking(PtyEvent::Exited);
                // A reader that never reaches this line is a thread, a master
                // fd and a shell that outlived their pane. `TUNI_DEBUG_LIFETIME`
                // is read here rather than at spawn because it runs once per
                // thread, at its end.
                if std::env::var_os("TUNI_DEBUG_LIFETIME").is_some() {
                    eprintln!("tuni lifetime -pty-reader");
                }
            })
            .map_err(Error::Io)?;

        // Writing to the master blocks whenever the shell is not reading, and
        // the caller is always the GTK main loop. That is not merely a stall:
        // a shell that has stopped reading is usually a shell that is waiting
        // to write, and its output has nowhere to go while the main thread is
        // parked in `write` instead of draining `rx` — so the reader thread
        // fills the bounded channel, blocks, the shell blocks, and nobody
        // moves again. Doing the write here, on a thread whose only job is to
        // wait, leaves the main loop free to drain the reader and breaks the
        // cycle at the only point where waiting is harmless.
        //
        // Unbounded because what queues here is what somebody typed, pasted or
        // asked for, and it is bounded in practice by the shell reading it.
        let (writes, pending) = async_channel::unbounded::<Vec<u8>>();
        std::thread::Builder::new()
            .name("tuni-pty-writer".to_owned())
            .spawn(move || {
                while let Ok(chunk) = pending.recv_blocking() {
                    if writer.write_all(&chunk).and_then(|()| writer.flush()).is_err() {
                        break;
                    }
                }
                // Dropping the writer here is what sends the shell its EOF, and
                // it happens either when the pane closes and the sender goes,
                // or when the write above fails because the shell is gone.
            })
            .map_err(Error::Io)?;

        Ok(Self {
            master: pair.master,
            writes,
            child: Some(child),
            events: rx,
        })
    }

    /// Receiver for PTY output, driven from the GTK main context.
    #[must_use]
    pub fn events(&self) -> async_channel::Receiver<PtyEvent> {
        self.events.clone()
    }

    /// Hands `data` to the writer thread. Never blocks, so it is safe to call
    /// from the main loop; the error case is a shell that is already gone.
    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        self.writes
            .try_send(data.to_vec())
            .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))
    }

    pub fn resize(
        &self,
        cols: u16,
        rows: u16,
        cell_width_px: u16,
        cell_height_px: u16,
    ) -> Result<()> {
        self.master
            .resize(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: cell_width_px.saturating_mul(cols.max(1)),
                pixel_height: cell_height_px.saturating_mul(rows.max(1)),
            })
            .map_err(|e| Error::Spawn(e.to_string()))
    }

    /// PID of the process group leader, used later for "what is this pane
    /// running" and for confirming a close.
    #[must_use]
    pub fn shell_pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(|child| child.process_id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_name_is_looked_up_on_path() {
        // /bin/sh is the one program a POSIX machine is required to have, and
        // whichever directory it lives in is on PATH by definition.
        let found = resolve_shell("sh").expect("sh is on PATH");
        assert!(found.ends_with("sh"));
        assert!(executable(&found));
    }

    #[test]
    fn a_path_is_taken_as_written() {
        assert_eq!(resolve_shell("/bin/sh"), Some(PathBuf::from("/bin/sh")));
    }

    #[test]
    fn nothing_runnable_resolves_to_nothing() {
        assert_eq!(resolve_shell("tuni-no-such-shell"), None);
        assert_eq!(resolve_shell("/etc/hostname"), None);
        assert_eq!(resolve_shell("   "), None);
    }
}
