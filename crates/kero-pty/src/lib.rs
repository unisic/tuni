//! PTY spawning and I/O for a terminal session.
//!
//! The reader runs on its own thread and hands buffers to the GTK main thread
//! over an async channel, because the VT state in `kero-vt` is `!Send` and must
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

pub struct PtyConfig {
    /// Shell to run. Defaults to `$SHELL`, then the passwd entry, then `/bin/sh`.
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
        env.insert("TERM_PROGRAM".to_owned(), "kero".to_owned());

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
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    events: async_channel::Receiver<PtyEvent>,
}

impl Pty {
    pub fn spawn(config: &PtyConfig) -> Result<Self> {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: config.rows.max(1),
                cols: config.cols.max(1),
                pixel_width: config.cell_width_px * config.cols,
                pixel_height: config.cell_height_px * config.rows,
            })
            .map_err(|e| Error::Spawn(e.to_string()))?;

        let shell = config.shell.clone().unwrap_or_else(login_shell);
        let mut cmd = CommandBuilder::new(&shell);
        // A leading '-' in argv[0] is what makes a shell a *login* shell, which
        // is what a terminal emulator is expected to start.
        cmd.arg0(format!(
            "-{}",
            shell
                .file_name()
                .map_or_else(|| "sh".into(), |n| n.to_string_lossy())
        ));
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

        let writer = pair
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
            .name("kero-pty-reader".to_owned())
            .spawn(move || {
                let mut buf = vec![0u8; READ_BUF];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if tx.send_blocking(PtyEvent::Output(buf[..n].to_vec())).is_err() {
                                break;
                            }
                        }
                    }
                }
                let _ = tx.send_blocking(PtyEvent::Exited);
            })
            .map_err(Error::Io)?;

        Ok(Self {
            master: pair.master,
            writer,
            child,
            events: rx,
        })
    }

    /// Receiver for PTY output, driven from the GTK main context.
    #[must_use]
    pub fn events(&self) -> async_channel::Receiver<PtyEvent> {
        self.events.clone()
    }

    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        self.writer.write_all(data)?;
        self.writer.flush()?;
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16, cell_width_px: u16, cell_height_px: u16) -> Result<()> {
        self.master
            .resize(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: cell_width_px * cols.max(1),
                pixel_height: cell_height_px * rows.max(1),
            })
            .map_err(|e| Error::Spawn(e.to_string()))
    }

    /// PID of the process group leader, used later for "what is this pane
    /// running" and for confirming a close.
    #[must_use]
    pub fn shell_pid(&self) -> Option<u32> {
        self.child.process_id()
    }
}

fn login_shell() -> PathBuf {
    if let Ok(shell) = std::env::var("SHELL")
        && !shell.is_empty()
    {
        return PathBuf::from(shell);
    }
    if let Ok(Some(user)) = nix::unistd::User::from_uid(nix::unistd::getuid()) {
        return user.shell;
    }
    PathBuf::from("/bin/sh")
}
