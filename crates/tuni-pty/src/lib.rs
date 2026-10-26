//! PTY spawning and I/O for a terminal session.
//!
//! The reader runs on its own thread and hands buffers to the GTK main thread
//! over an async channel, because the VT state in `tuni-vt` is `!Send` and must
//! only ever be touched from the main thread.

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{
    AsFd as _, AsRawFd as _, BorrowedFd, FromRawFd as _, IntoRawFd as _, OwnedFd, RawFd,
};
use std::path::PathBuf;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// Where `Pty::hand_over` leaves a shell for the program it `exec`s:
/// `<fd>:<pid>`, then the two parts of its configuration that can differ.
const HANDED_OVER: &str = "TUNI_EARLY_SHELL";
const HANDED_OVER_SHELL: &str = "TUNI_EARLY_SHELL_PROGRAM";
const HANDED_OVER_CWD: &str = "TUNI_EARLY_SHELL_CWD";

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
    /// this is the path that gets run. Ignored when `argv` says what to run.
    pub shell: Option<PathBuf>,
    /// An exact argv to run instead of a login shell. Empty is the login shell,
    /// which is what every pane was before there was anything else to run.
    /// `argv[0]` is resolved on `PATH` the way a shell would resolve it.
    pub argv: Vec<String>,
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
        // honest description — when this machine has it. The packages ask for
        // it (Recommends/optdepends on ghostty's terminfo), but a machine
        // without it would otherwise greet every ncurses program with
        // "unknown terminal type", which is strictly worse than the slightly
        // less honest xterm-256color.
        env.insert(
            "TERM".to_owned(),
            if terminfo_installed("xterm-ghostty") {
                "xterm-ghostty"
            } else {
                "xterm-256color"
            }
            .to_owned(),
        );
        env.insert("COLORTERM".to_owned(), "truecolor".to_owned());
        env.insert("TERM_PROGRAM".to_owned(), "tuni".to_owned());

        Self {
            shell: None,
            argv: Vec::new(),
            cwd: None,
            env,
            cols: 80,
            rows: 24,
            cell_width_px: 8,
            cell_height_px: 16,
        }
    }
}

/// Whether ncurses would find a terminfo entry by this name, by looking where
/// ncurses looks: `$TERMINFO`, `~/.terminfo`, `$TERMINFO_DIRS`, then the
/// compiled-in defaults every distribution uses.
///
/// A hashed `terminfo.db` is not read; a system odd enough to have one
/// without the directory tree falls back to xterm-256color, which is the safe
/// direction to be wrong in.
fn terminfo_installed(name: &str) -> bool {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("TERMINFO") {
        dirs.push(dir.into());
    }
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".terminfo"));
    }
    if let Some(list) = std::env::var_os("TERMINFO_DIRS") {
        // An empty entry in the list means "the default place", per terminfo(5).
        for dir in std::env::split_paths(&list) {
            if dir.as_os_str().is_empty() {
                dirs.push(PathBuf::from("/usr/share/terminfo"));
            } else {
                dirs.push(dir);
            }
        }
    }
    for fallback in ["/etc/terminfo", "/lib/terminfo", "/usr/share/terminfo"] {
        dirs.push(PathBuf::from(fallback));
    }
    dirs.iter().any(|dir| terminfo_under(dir, name))
}

fn set_nonblocking(fd: &OwnedFd) -> std::io::Result<()> {
    let fd = fd.as_raw_fd();
    // SAFETY: flag reads and writes on a descriptor the caller owns.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Parks the calling thread until `fd` is ready for `events`, or has hung up,
/// which is how a thread drives a non-blocking descriptor without spinning.
fn wait_for(fd: BorrowedFd<'_>, events: libc::c_short) {
    let mut ready = libc::pollfd {
        fd: fd.as_raw_fd(),
        events,
        revents: 0,
    };
    // SAFETY: one valid `pollfd`, and a count of one. A failure, which is an
    // interrupt in practice, only sends the caller back to the descriptor,
    // and that says whatever is really wrong.
    unsafe { libc::poll(&mut ready, 1, -1) };
}

/// The reader's wait for output: an epoll set holding the master for the life
/// of the pane, rather than a `poll` that puts it on the master's wait queue
/// only while the thread sleeps. Why the registration matters is the line
/// discipline's business; that it does was measured: 200 MiB of `cat` into a
/// reader that does nothing else took 1.5 to 1.8 s waiting in `poll` and 1.1
/// to 1.4 s with the master registered.
struct Readable(OwnedFd);

impl Readable {
    fn new(fd: BorrowedFd<'_>) -> std::io::Result<Self> {
        // SAFETY: no pointers; the result is checked before it is owned.
        let epoll = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if epoll < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: a fresh descriptor nothing else owns.
        let epoll = unsafe { OwnedFd::from_raw_fd(epoll) };
        let mut event = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: 0,
        };
        // SAFETY: two open descriptors and one valid event. Hangup and error
        // are reported whether asked for or not, so the end wakes the reader.
        if unsafe {
            libc::epoll_ctl(
                epoll.as_raw_fd(),
                libc::EPOLL_CTL_ADD,
                fd.as_raw_fd(),
                &mut event,
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self(epoll))
    }

    /// Parks the thread until the master has output or has hung up. A failure,
    /// which is an interrupt in practice, only sends the reader back to the
    /// descriptor, and that says whatever is really wrong.
    fn wait(&self) {
        let mut event = libc::epoll_event { events: 0, u64: 0 };
        // SAFETY: room for one event, and a count of one.
        unsafe { libc::epoll_wait(self.0.as_raw_fd(), &mut event, 1, -1) };
    }
}

/// `write_all` for the non-blocking master: a full input queue means the shell
/// has not read yet, which is a reason to wait rather than to give up.
fn write_through(master: &std::fs::File, mut bytes: &[u8]) -> std::io::Result<()> {
    while !bytes.is_empty() {
        match (&*master).write(bytes) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => wait_for(master.as_fd(), libc::POLLOUT),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// One directory's answer: entries live at `<dir>/<first letter>/<name>`.
fn terminfo_under(dir: &std::path::Path, name: &str) -> bool {
    let Some(first) = name.chars().next() else {
        return false;
    };
    dir.join(first.to_string()).join(name).is_file()
}

pub struct Pty {
    /// The master, for the size and the line discipline's mode. The reader
    /// and the writer each hold a duplicate of their own.
    master: OwnedFd,
    /// Bytes on their way to the shell. A queue rather than the writer itself
    /// because writing is what blocks — see the thread in `adopt`.
    writes: async_channel::Sender<Vec<u8>>,
    /// The shell's pid. Held in an `Option` so the hangup can hand it to
    /// whoever reaps it, and unreaped on this side until then so the number
    /// cannot be recycled under the signal the hangup sends: nothing else in
    /// the process waits for it.
    child: Option<u32>,
    events: async_channel::Receiver<PtyEvent>,
}

impl Drop for Pty {
    /// Hangs up the way closing a terminal window does.
    ///
    /// Dropping the master alone does not: the reader thread holds a duplicate
    /// of it and is parked waiting on it, so the kernel still sees an open master
    /// and sends nobody a hangup. A shell sitting at a prompt does leave —
    /// the writer sends it an EOF as it goes — but a shell with a foreground
    /// job never reads that EOF, so the shell, the job, the reader thread and
    /// its fd all outlive the pane. SIGHUP to the shell's process group is
    /// what actually ends it; the kernel then hangs up the foreground job as
    /// well, because the shell it kills is the session leader holding the
    /// controlling terminal.
    ///
    /// Reaping happens on a thread of its own, because waiting here would block
    /// the frame that is closing the pane for as long as the shell takes to
    /// die.
    fn drop(&mut self) {
        let Some(pid) = self.hang_up() else {
            return;
        };
        let reaper = std::thread::Builder::new()
            .name("tuni-pty-reaper".to_owned())
            .spawn(move || {
                let _ = wait(pid, 0);
                if std::env::var_os("TUNI_DEBUG_LIFETIME").is_some() {
                    eprintln!("tuni lifetime -pty-child");
                }
            });
        // A machine that cannot start the thread would leak a zombie rather
        // than a shell; nothing here can do better than let it.
        drop(reaper);
    }
}

/// `waitpid` for one child, answering with its status once it has one.
/// Under `WNOHANG` that is a question, and `None` means still running;
/// otherwise `None` is a pid this process cannot wait for.
fn wait(pid: u32, flags: libc::c_int) -> Option<libc::c_int> {
    let mut status = 0;
    loop {
        // SAFETY: one `int` for the kernel to write the status into.
        match unsafe { libc::waitpid(pid as libc::pid_t, &mut status, flags) } {
            0 => return None,
            -1 if std::io::Error::last_os_error().kind() == ErrorKind::Interrupted => {}
            -1 => return None,
            _ => return Some(status),
        }
    }
}

/// Whether `pid` is a child of this process that nobody has reaped yet,
/// asked without reaping it.
fn unreaped_child(pid: u32) -> bool {
    // SAFETY: `siginfo_t` is plain data the kernel fills in, and `WNOWAIT`
    // leaves the child exactly as waitable as it was.
    let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
    unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        ) == 0
    }
}

/// What `portable-pty`'s writer did as it was dropped: a newline, then the
/// terminal's EOF character, which a shell at its prompt reads as the end of
/// its input. After a newline because that is the only place the line
/// discipline takes it as one. One attempt, never a wait: a shell that is not
/// reading has the hangup coming anyway.
fn send_eof(master: &std::fs::File) {
    // SAFETY: `termios` is plain data that `tcgetattr` fills in whole.
    let mut termios = unsafe { std::mem::zeroed::<libc::termios>() };
    if unsafe { libc::tcgetattr(master.as_raw_fd(), &mut termios) } == 0 {
        let eof = termios.c_cc[libc::VEOF];
        if eof != 0 {
            let _ = (&*master).write_all(&[b'\n', eof]);
        }
    }
}

impl Pty {
    /// Waits for a shell that has hung up its end and says how it went: its
    /// exit code, or `None` for one a signal ended or that could not be
    /// waited for.
    ///
    /// For a pty whose reader has already seen the end. The shell is gone or
    /// leaving, which is why it gets a moment to go on its own before the
    /// hangup: closing the terminal is the last thing `ssh` does before it
    /// exits with the code that says whether the link dropped, and a SIGHUP
    /// landing in between would replace that code with a signal. Blocks until
    /// the shell is reaped, so the caller belongs off the main thread.
    #[must_use]
    pub fn finish(mut self) -> Option<u32> {
        let pid = self.child?;
        let mut status = None;
        for _ in 0..20 {
            status = wait(pid, libc::WNOHANG);
            if status.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let status = match status {
            Some(status) => {
                self.child = None;
                status
            }
            None => wait(self.hang_up()?, 0)?,
        };
        libc::WIFEXITED(status).then(|| libc::WEXITSTATUS(status) as u32)
    }

    /// Sends the hangup and hands over the shell for whoever reaps it.
    fn hang_up(&mut self) -> Option<u32> {
        let pid = self.child.take()?;
        // Safe: `pid` names a process that has not been waited for — this is
        // the only place the child is handed away — so the kernel still holds
        // its slot and the number cannot mean a different process.
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGHUP);
        }
        Some(pid)
    }

    pub fn spawn(config: &PtyConfig) -> Result<Self> {
        let (master, pid) = Self::start(config)?;
        Self::adopt(master, pid)
    }

    /// Starts what `config` asks for on a pty of its own, and hands back the
    /// master and the shell's pid with nothing reading either yet.
    ///
    /// Half of `spawn`. `adopt` is the other half, in this process or in the
    /// program this process `exec`s into, which is how the `tuni` launcher
    /// starts the first window's shell before GTK is loaded. Nothing has to
    /// read before then: what the shell prints waits in the kernel's queue.
    pub fn start(config: &PtyConfig) -> Result<(OwnedFd, u32)> {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: config.rows.max(1),
                cols: config.cols.max(1),
                pixel_width: config.cell_width_px.saturating_mul(config.cols),
                pixel_height: config.cell_height_px.saturating_mul(config.rows),
            })
            .map_err(|e| Error::Spawn(e.to_string()))?;
        // The child goes out of scope as soon as its pid is read, which neither
        // kills it nor waits for it: from here it is a pid, the form it takes
        // across an `exec` as well.
        let pid = pair
            .slave
            .spawn_command(Self::command(config))
            .map_err(|e| Error::Spawn(e.to_string()))?
            .process_id()
            .ok_or_else(|| Error::Spawn("the shell has no pid".to_owned()))?;
        // Drop the slave immediately: while we hold it, the master never sees
        // EOF after the child exits.
        drop(pair.slave);
        let master = pair
            .master
            .as_raw_fd()
            .ok_or_else(|| Error::Spawn("the pty has no descriptor".to_owned()))?;
        // SAFETY: `master` belongs to `pair.master`, which is open past this
        // line; the duplicate is the caller's from here on.
        let master = unsafe { BorrowedFd::borrow_raw(master) }
            .try_clone_to_owned()
            .map_err(Error::Io)?;
        Ok((master, pid))
    }

    /// What `config` runs, with this process's environment read into it.
    fn command(config: &PtyConfig) -> CommandBuilder {
        // The two branches cannot be merged: `arg` panics on a default-prog
        // builder, so a command has to be built from `new` instead.
        let mut cmd = match config.argv.as_slice() {
            [program, args @ ..] => {
                let mut cmd = CommandBuilder::new(program);
                cmd.args(args);
                cmd
            }
            // The default-prog builder is the one that starts a *login* shell:
            // it resolves $SHELL then the passwd entry, and prefixes argv[0]
            // with '-'. An explicit shell rides in through $SHELL rather than
            // through argv, because the builder resolves the executable from
            // argv[0] and a login-shell argv[0] is not a path. Which is also
            // why it stays in this branch: $SHELL is how a login shell is
            // *chosen*, and it means nothing to a command that names itself.
            [] => {
                let mut cmd = CommandBuilder::new_default_prog();
                if let Some(shell) = &config.shell {
                    cmd.env("SHELL", shell);
                }
                cmd
            }
        };
        if let Some(cwd) = &config.cwd {
            cmd.cwd(cwd);
        }
        // The activation token belongs to the window the launch opens, and GTK
        // takes both spellings of it out of the environment as it starts, so
        // nothing started later presents itself with a token already spent. A
        // shell started before GTK was would still find it there.
        cmd.env_remove("XDG_ACTIVATION_TOKEN");
        cmd.env_remove("DESKTOP_STARTUP_ID");
        for (key, value) in &config.env {
            cmd.env(key, value);
        }
        cmd
    }

    /// Gives a shell `start` made to the program `next` will `exec`, which
    /// takes it back with `inherited`. The master stays open across the
    /// `exec` and the rest goes in `next`'s environment; the shell is a child
    /// of the process either way, since an `exec` keeps the pid.
    pub fn hand_over(
        config: &PtyConfig,
        master: OwnedFd,
        pid: u32,
        next: &mut std::process::Command,
    ) -> Result<()> {
        let fd = master.as_raw_fd();
        // SAFETY: flag reads and writes on a descriptor this function owns.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }
        next.env(HANDED_OVER, format!("{}:{pid}", master.into_raw_fd()));
        for (key, value) in [
            (HANDED_OVER_SHELL, &config.shell),
            (HANDED_OVER_CWD, &config.cwd),
        ] {
            match value {
                Some(value) => next.env(key, value),
                None => next.env_remove(key),
            };
        }
        Ok(())
    }

    /// The shell `hand_over` gave this process, with the configuration it was
    /// started from, and the variables that carried it gone from the
    /// environment so that no shell started later is told about it.
    ///
    /// Anything in those variables that does not describe a shell this
    /// process may reap and a terminal it has open is left alone: they are
    /// only ever written by a `tuni` that `exec`ed into this one, but they are
    /// an environment, which anybody can write.
    ///
    /// # Safety
    ///
    /// Writes the environment, so no other thread may be running.
    #[must_use]
    pub unsafe fn inherited() -> Option<(PtyConfig, Self)> {
        let handed = std::env::var_os(HANDED_OVER)?;
        let config = PtyConfig {
            shell: std::env::var_os(HANDED_OVER_SHELL).map(PathBuf::from),
            cwd: std::env::var_os(HANDED_OVER_CWD).map(PathBuf::from),
            ..PtyConfig::default()
        };
        for key in [HANDED_OVER, HANDED_OVER_SHELL, HANDED_OVER_CWD] {
            // SAFETY: the caller's promise that nothing else is running.
            unsafe { std::env::remove_var(key) };
        }
        let (fd, pid) = handed.to_str()?.split_once(':')?;
        let (fd, pid) = (fd.parse::<RawFd>().ok()?, pid.parse::<u32>().ok()?);
        // SAFETY: `isatty` only asks; the descriptor is taken over below, and
        // only once it has been seen to be a terminal nothing else here holds.
        if !unreaped_child(pid) || unsafe { libc::isatty(fd) } != 1 {
            return None;
        }
        let master = unsafe { OwnedFd::from_raw_fd(fd) };
        // Closed again on every later `exec`, so the next shell, `git` or
        // `ssh` does not hold this pane's terminal open.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags >= 0 {
            unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
        }
        let pty = Self::adopt(master, pid).ok()?;
        Some((config, pty))
    }

    /// Reads and writes a shell already running on `master`, the way `spawn`
    /// does one it started itself. `pid` has to be a child of this process
    /// that nobody has reaped, because the hangup and the reaping are this
    /// struct's from here on.
    pub fn adopt(master: OwnedFd, pid: u32) -> Result<Self> {
        // A pid somebody else reaps is a number a later hangup could land on
        // after it means another process.
        if !unreaped_child(pid) {
            return Err(Error::Spawn(format!(
                "{pid} is not a shell of this process"
            )));
        }
        // The reader's own duplicate, because it has to wait on the descriptor
        // as well as read it.
        let reader = master.try_clone().map_err(Error::Io)?;
        let writer = std::fs::File::from(master.try_clone().map_err(Error::Io)?);
        // Non-blocking, so the reader can tell "nothing more for now" from
        // "wait", drain the master until the kernel has nothing left and hand
        // the whole burst over as one buffer. A blocking read returns whatever
        // the line discipline pushed since the last wakeup, which under a
        // firehose is a couple of hundred bytes: 200 MiB of `cat` woke the
        // main loop and fed the VT about a million times, and drained it is
        // about five thousand, 40 KiB each.
        //
        // The flag belongs to the open file, which every duplicate shares, so
        // the writer below has to wait for room instead of blocking in `write`.
        set_nonblocking(&reader).map_err(Error::Io)?;
        let readable = Readable::new(reader.as_fd()).map_err(Error::Io)?;
        // Bounded so a firehose applies backpressure to the reader instead of
        // growing an unbounded queue the UI can never drain.
        let (tx, rx) = async_channel::bounded(64);
        std::thread::Builder::new()
            .name("tuni-pty-reader".to_owned())
            .spawn(move || {
                let mut reader = std::fs::File::from(reader);
                let mut buf = vec![0u8; READ_BUF];
                let mut filled = 0;
                loop {
                    // EIO is how the master says the last slave closed.
                    let (ended, drained) = match reader.read(&mut buf[filled..]) {
                        Ok(0) => (true, true),
                        Ok(n) => {
                            filled += n;
                            if filled < buf.len() {
                                continue;
                            }
                            (false, false)
                        }
                        Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                        Err(e) if e.kind() == ErrorKind::WouldBlock => (false, true),
                        Err(_) => (true, true),
                    };
                    if filled > 0
                        && tx
                            .send_blocking(PtyEvent::Output(buf[..filled].to_vec()))
                            .is_err()
                    {
                        break;
                    }
                    filled = 0;
                    if ended {
                        break;
                    }
                    if drained {
                        readable.wait();
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
                    if write_through(&writer, &chunk).is_err() {
                        break;
                    }
                }
                // Either the pane closed and the sender went, or the write
                // above failed because the shell is gone.
                send_eof(&writer);
            })
            .map_err(Error::Io)?;

        Ok(Self {
            master,
            writes,
            child: Some(pid),
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
        let size = libc::winsize {
            ws_row: rows.max(1),
            ws_col: cols.max(1),
            ws_xpixel: cell_width_px.saturating_mul(cols.max(1)),
            ws_ypixel: cell_height_px.saturating_mul(rows.max(1)),
        };
        // SAFETY: `size` is the whole `winsize` the request reads, and the
        // master is open for as long as `self` is.
        if unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) } == 0 {
            Ok(())
        } else {
            Err(Error::Io(std::io::Error::last_os_error()))
        }
    }

    /// PID of the process group leader, used later for "what is this pane
    /// running" and for confirming a close.
    #[must_use]
    pub fn shell_pid(&self) -> Option<u32> {
        self.child
    }

    /// Whether the program in here has taken the terminal out of line mode,
    /// which is how `ssh` says a session is up: it reads a password or a
    /// host-key answer a line at a time, and goes raw only once it has
    /// logged in. False when the terminal cannot be asked.
    #[must_use]
    pub fn is_raw(&self) -> bool {
        // SAFETY: `termios` is plain data that `tcgetattr` fills in whole, and
        // the master is open for as long as `self` is.
        let mut termios = unsafe { std::mem::zeroed::<libc::termios>() };
        let asked = unsafe { libc::tcgetattr(self.master.as_raw_fd(), &mut termios) };
        asked == 0 && termios.c_lflag & libc::ICANON == 0
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

    #[test]
    fn a_terminfo_entry_is_found_under_its_first_letter() {
        let dir = std::env::temp_dir().join(format!("tuni-terminfo-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("x")).unwrap();
        std::fs::write(dir.join("x").join("xterm-ghostty"), b"").unwrap();

        assert!(terminfo_under(&dir, "xterm-ghostty"));
        assert!(!terminfo_under(&dir, "xterm-256color"));
        assert!(!terminfo_under(&dir, ""));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Drains a PTY until it says the program is gone, and answers with
    /// everything it printed. The reader hands over whole buffers rather than
    /// lines, so a program that prints once can still take two of them.
    fn drain(pty: &Pty) -> String {
        let events = pty.events();
        let mut out = Vec::new();
        while let Ok(event) = events.recv_blocking() {
            match event {
                PtyEvent::Output(bytes) => out.extend_from_slice(&bytes),
                PtyEvent::Exited => break,
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn a_command_runs_instead_of_the_login_shell() {
        let pty = Pty::spawn(&PtyConfig {
            argv: vec!["echo".to_owned(), "tuni".to_owned()],
            ..PtyConfig::default()
        })
        .expect("echo starts");
        assert!(drain(&pty).contains("tuni"));
    }

    #[test]
    fn input_larger_than_the_queue_waits_for_the_shell_instead_of_failing() {
        // Far past what the line discipline holds, so the writer meets a full
        // queue and has to wait on the non-blocking master. Whole lines, because
        // until `stty` runs the queue is canonical and drops an overlong one.
        let line = format!("{}\n", "x".repeat(99));
        let mut pty = Pty::spawn(&PtyConfig {
            argv: ["sh", "-c", "stty -echo; head -c 1000000 | wc -c"]
                .map(str::to_owned)
                .to_vec(),
            ..PtyConfig::default()
        })
        .expect("sh starts");
        pty.write(line.repeat(10_000).as_bytes()).expect("queued");
        assert!(drain(&pty).contains("1000000"));
    }

    #[test]
    fn an_empty_argv_is_still_the_login_shell() {
        // `/bin/sh` rather than the passwd entry, so the test does not depend
        // on whichever shell the machine running it happens to use. It sits at
        // a prompt and never exits, which is the point: `Drop` hangs it up.
        let pty = Pty::spawn(&PtyConfig {
            shell: Some(PathBuf::from("/bin/sh")),
            ..PtyConfig::default()
        })
        .expect("a login shell starts");
        assert!(pty.shell_pid().is_some());
    }

    #[test]
    fn a_started_shell_is_read_by_whoever_adopts_it() {
        let (master, pid) = Pty::start(&PtyConfig {
            argv: vec!["echo".to_owned(), "adopted".to_owned()],
            ..PtyConfig::default()
        })
        .expect("echo starts");
        let pty = Pty::adopt(master, pid).expect("its own child is adopted");
        assert_eq!(pty.shell_pid(), Some(pid));
        assert!(drain(&pty).contains("adopted"));
    }

    #[test]
    fn a_pid_this_process_did_not_start_is_not_adopted() {
        let (master, pid) = Pty::start(&PtyConfig {
            argv: vec!["true".to_owned()],
            ..PtyConfig::default()
        })
        .expect("true starts");
        assert!(Pty::adopt(master, 1).is_err());
        assert!(wait(pid, 0).is_some());
    }
}
