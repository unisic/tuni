//! Hosts, as `ssh` itself defines them.
//!
//! Same bargain as [`crate::git`]: the window has to agree with the command
//! line the user types beside it, and the only thing certain to agree with
//! `ssh` is `ssh`. So nothing here reimplements SSH. It reads `~/.ssh/config`
//! well enough to *enumerate* the aliases a person might want to connect to,
//! and for what any one of them actually means it asks the binary.
//!
//! That split matters. `ssh -G alias` prints the fully resolved configuration
//! for an alias with `Match`, `Include`, wildcard `Host` blocks, canonicalisation
//! and the system defaults already applied. The reader below only has to find
//! the names; it never has to be right about their meaning. It comes with a
//! cost, which is why [`resolve`] is never called on a timer or in a loop:
//! `ssh -G` runs the user's `Match exec` commands.
//!
//! Nothing here is asynchronous, and two of these functions start a subprocess,
//! so the caller belongs off the main thread.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::git::Output;

/// How deep `Include` may nest. `files.rs` caps its own walk at 32; this is
/// lower because a configuration nested past a handful of files is a mistake
/// rather than a layout.
const INCLUDE_DEPTH: usize = 16;

/// How many files one load reads at all, however they were reached.
const INCLUDE_FILES: usize = 256;

/// The user's own configuration, which tuni reads and does not rewrite.
#[must_use]
pub fn config_path() -> PathBuf {
    crate::settings::home().join(".ssh/config")
}

/// The one file tuni writes hosts into. Under tuni's own configuration
/// directory, so removing `~/.config/tuni` removes everything tuni added.
#[must_use]
pub fn store_path() -> PathBuf {
    crate::settings::config_dir().join("ssh/hosts.conf")
}

/// Runs `ssh` and waits for it.
///
/// `BatchMode=yes` is [`crate::git`]'s `GIT_TERMINAL_PROMPT=0` applied to a
/// second tool: anything that could prompt runs in a real terminal pane where
/// the user can answer it, so a call from here has to fail rather than sit
/// forever on a password prompt nobody can see. `SSH_ASKPASS_REQUIRE=never`
/// closes the other door, where an askpass inherited from the environment pops
/// a dialog out of a background process. `LC_ALL=C` is what makes reading the
/// error text safe.
pub fn run<S: AsRef<OsStr>>(args: &[S]) -> Output {
    let output = Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .args(args)
        .env("SSH_ASKPASS_REQUIRE", "never")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output();
    match output {
        Ok(output) => Output {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        },
        Err(error) => Output {
            code: -1,
            stdout: String::new(),
            stderr: error.to_string(),
        },
    }
}

/// What an ssh pane calls itself at the far end.
///
/// Tuni describes itself with terminfo that is on *this* machine, and `ssh`
/// forwards `TERM` unchanged, so a remote without that entry renders garbage.
/// This is the name every machine has had for twenty years.
pub const REMOTE_TERM: &str = "xterm-256color";

/// How long a shared connection outlives the last pane using it, in seconds.
/// Long enough that closing a tab and opening another does not re-authenticate,
/// short enough that a forgotten window is not still holding a tunnel open an
/// hour later.
pub const CONTROL_PERSIST: u32 = 600;

/// How long to wait for a connection that is not answering, in seconds.
const CONNECT_TIMEOUT: u32 = 10;

/// How often a master asks the far end whether it is still there, in seconds,
/// and how many unanswered asks it takes to give up.
const ALIVE_INTERVAL: u32 = 15;
const ALIVE_COUNT: u32 = 3;

/// Where tuni's shared connections keep their sockets.
#[derive(Clone, Debug)]
pub struct Control {
    directory: PathBuf,
    persist: u32,
    enabled: bool,
}

impl Control {
    #[must_use]
    pub fn new(persist: u32, enabled: bool) -> Self {
        // `$XDG_RUNTIME_DIR` is the right home for a socket: tmpfs, 0700,
        // per-user, and emptied at logout, so a stale one cannot survive a
        // reboot. `~/.cache` is the fallback and the reason a sweep has to
        // exist at all.
        let directory = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| crate::settings::home().join(".cache"))
            .join("tuni/ssh");
        Self::at(directory, persist, enabled)
    }

    /// A socket directory of somebody else's choosing, which is what the tests
    /// use: the real one is where a real master would be.
    #[must_use]
    pub fn at(directory: PathBuf, persist: u32, enabled: bool) -> Self {
        Self {
            directory,
            persist,
            enabled,
        }
    }

    /// The `-o` settings every `ssh` tuni runs for `destination` carries,
    /// without the `-o`s themselves.
    ///
    /// Nothing in here is a preference. Every option tuni adds overrides one
    /// the user set on purpose, so the list is the shortest that makes sharing
    /// work and keeps a dropped link from reading as a hung window: no
    /// ciphers, no compression, no `StrictHostKeyChecking`.
    ///
    /// Runs `ssh -G`, so the caller belongs off the main thread.
    #[must_use]
    pub fn options(&self, destination: &str) -> Vec<String> {
        let reported = describe(destination);
        let value = |keyword: &str| {
            reported
                .iter()
                .find(|(key, _)| key == keyword)
                .map(|(_, value)| value.as_str())
        };
        let mut options = Vec::new();

        // Adopt rather than override. A command-line `-o` is obtained first
        // and wins, so tuni *can* always override, which is exactly why it has
        // to decide not to: overriding a ControlMaster that works means two
        // logins to a host that may cap them, and two 2FA prompts.
        //
        // The one thing `ssh -G` will not say is the difference between an
        // unset `ControlPath` and an explicit `ControlPath none`, since both
        // leave the keyword out of the report. Somebody who turned sharing off
        // that way gets it back here, and turning it off in tuni is how they
        // say so again.
        let theirs = value("controlpath").is_some() && value("controlmaster") != Some("false");
        if self.enabled && !theirs && self.prepare() {
            // `%C` is ssh's own hash of `%l%h%p%r%j`: 40 hex characters, jump
            // host included, so a host reached through a different `ProxyJump`
            // gets its own socket without tuni ever implementing the hash.
            // Read this before "fixing" the length: `/run/user/1000/tuni/ssh/`
            // and 40 characters is 64 bytes against the 108 Linux allows in
            // `sun_path`.
            options.push("ControlMaster=auto".to_owned());
            options.push(format!(
                "ControlPath={}",
                self.directory.join("%C").display()
            ));
            options.push(format!("ControlPersist={}", self.persist));
        }

        // The one option that earns its place on merit. Without it a suspended
        // laptop's connection hangs for the kernel's retransmit timeout, about
        // fifteen minutes on Linux, and every pane on that host looks frozen.
        // With it the master gives up in about forty-five seconds, exits, and
        // unlinks its own socket, which reads as a disconnection instead.
        // Skipped when the user has an interval of their own.
        if value("serveraliveinterval").is_none_or(|interval| interval == "0") {
            options.push(format!("ServerAliveInterval={ALIVE_INTERVAL}"));
            options.push(format!("ServerAliveCountMax={ALIVE_COUNT}"));
        }
        if value("connecttimeout").is_none_or(|timeout| timeout == "none") {
            options.push(format!("ConnectTimeout={CONNECT_TIMEOUT}"));
        }
        options
    }

    /// The socket a shared connection to `destination` lives on, and whether it
    /// is tuni's own rather than one the user's configuration asked for.
    ///
    /// Asked of `ssh` rather than worked out here. The name is `%C`, ssh's own
    /// hash of the login, host, port, user and jump host, and a second
    /// implementation of it would disagree in exactly the cases that matter.
    /// `ssh -G` reports it expanded, `~` and all.
    ///
    /// Nothing when sharing is off, which is what `ControlPath none` says and
    /// what a configuration that never mentions it means.
    ///
    /// Two subprocesses, so the caller belongs off the main thread.
    #[must_use]
    pub fn socket(&self, destination: &str) -> Option<(PathBuf, bool)> {
        let options = self.options(destination);
        let ours = options
            .iter()
            .any(|option| option.starts_with("ControlPath="));
        let path = described(&options, destination)
            .into_iter()
            .find(|(keyword, _)| keyword == "controlpath")
            .map(|(_, value)| value)?;
        (path != "none").then(|| (PathBuf::from(path), ours))
    }

    /// Whether a shared connection to `destination` is already open and
    /// answering.
    ///
    /// This is the question that decides whether tuni may connect to something
    /// nobody asked it to connect to. A master that answers has been through
    /// whatever the far end wanted, be that a password, a code or a key touch,
    /// so attaching to it asks nobody anything. Anything else is a login, and a
    /// window putting eight panes back must not start eight logins.
    ///
    /// A socket left behind by a killed master answers nothing and reads as
    /// down here, which is the safe way round: the worst it costs is a
    /// connection the user has to ask for.
    ///
    /// Two subprocesses, so the caller belongs off the main thread.
    #[must_use]
    pub fn is_live(&self, destination: &str) -> bool {
        run(&self.request(destination, "check", None)).is_ok()
    }

    /// The master answering for `destination`, by process id.
    ///
    /// `ssh -O check` prints `Master running (pid=12345)` on the error stream.
    /// That number is the only way to learn when a connection was made: the
    /// master is not tuni's child, and nothing in the protocol reports its own
    /// age.
    ///
    /// Two subprocesses, so the caller belongs off the main thread.
    #[must_use]
    pub fn check(&self, destination: &str) -> Option<u32> {
        let output = run(&self.request(destination, "check", None));
        if !output.is_ok() {
            return None;
        }
        master_pid(&output.stderr)
    }

    /// Asks the master carrying `destination` to open `forward`.
    ///
    /// The port that ended up listening, which is `forward.listen_port` except
    /// for a remote forward asking the far end to pick one: there the number
    /// comes back in `Allocated port 34567 for remote forward`, and that line is
    /// the only confirmation the protocol offers for a remote forward at all.
    ///
    /// Fails when no master is answering. Opening a connection first is the
    /// caller's, because a connection may want a password and this cannot ask
    /// for one.
    ///
    /// Four subprocesses, so the caller belongs off the main thread.
    pub fn add(&self, destination: &str, forward: &Forward) -> Result<u16, String> {
        let output = run(&self.request(destination, "forward", Some(forward)));
        if !output.is_ok() {
            return Err(output.message(&format!("{} would not open", forward.title())));
        }
        Ok(allocated_port(&output.stdout)
            .or_else(|| allocated_port(&output.stderr))
            .unwrap_or(forward.listen_port))
    }

    /// Closes a forward the master is carrying.
    ///
    /// The spec has to arrive spelled the way it was sent, since the master
    /// matches it as text: a `localhost` helpfully turned into `127.0.0.1` on
    /// the way through matches nothing and the forward stays up. Both go
    /// through [`Forward::spec`], so both say the same thing.
    ///
    /// Four subprocesses, so the caller belongs off the main thread.
    pub fn cancel(&self, destination: &str, forward: &Forward) -> Result<(), String> {
        let output = run(&self.request(destination, "cancel", Some(forward)));
        if output.is_ok() {
            return Ok(());
        }
        Err(output.message(&format!("{} would not close", forward.title())))
    }

    /// `ssh -O <command> -- <destination>`, with the settings that decide which
    /// socket it goes to in front of it. The shape of every request this file
    /// makes of a master.
    ///
    /// Runs `ssh -G` by way of [`Self::options`], so the caller belongs off the
    /// main thread.
    fn request(&self, destination: &str, command: &str, forward: Option<&Forward>) -> Vec<String> {
        let mut args = Vec::new();
        for option in self.options(destination) {
            args.push("-o".to_owned());
            args.push(option);
        }
        args.push("-O".to_owned());
        args.push(command.to_owned());
        if let Some(forward) = forward {
            args.push(forward.direction.flag().to_owned());
            args.push(forward.spec());
        }
        // `--` because a destination may begin with a dash.
        args.push("--".to_owned());
        args.push(destination.to_owned());
        args
    }

    /// Hangs up the shared connection to `destination`, and with it every pane,
    /// tunnel and listing on it.
    ///
    /// Refuses one the user's own configuration asked for. `-O exit` ends a
    /// master and every session it is carrying, and a master tuni merely
    /// attached to may be carrying a shell somebody started by typing `ssh` in
    /// a terminal that has nothing to do with tuni.
    ///
    /// Ending something that is not running is not a failure: there is no
    /// connection either way round.
    ///
    /// Four subprocesses, so the caller belongs off the main thread.
    pub fn stop(&self, destination: &str) -> Result<(), String> {
        let Some((path, ours)) = self.socket(destination) else {
            return Ok(());
        };
        if !path.exists() {
            return Ok(());
        }
        if !ours {
            return Err(format!(
                "The connection to {destination} is shared by your own configuration, \
                 so tuni leaves it running"
            ));
        }
        let output = run(&self.request(destination, "exit", None));
        if output.is_ok() {
            return Ok(());
        }
        Err(output.message(&format!("{destination} would not hang up")))
    }

    /// Unlinks the sockets nothing answers on.
    ///
    /// A master killed rather than asked to leave, by an out-of-memory kill or
    /// a power cut, leaves its socket behind. Every `ssh` after that prints
    /// `ControlSocket ... already exists, disabling multiplexing` and then
    /// carries on with a connection of its own, so it looks like it worked
    /// while every tunnel and listing quietly attaches to nothing.
    ///
    /// This is the one place tuni deletes something, and it is written to be
    /// the most conservative code in the file: its own directory only, names
    /// only of the shape `%C` expands to, sockets only, and only where the
    /// kernel refuses the connection, which is its answer for a socket with
    /// nothing listening behind it. A master still starting up is bound to a
    /// name of another shape, and a master that is alive answers.
    ///
    /// No subprocess: a connect and a close.
    pub fn sweep(&self) {
        use std::os::unix::fs::FileTypeExt;

        let Ok(entries) = fs::read_dir(&self.directory) else {
            return;
        };
        for entry in entries.flatten() {
            if !hashed(&entry.file_name().to_string_lossy()) {
                continue;
            }
            if !entry.file_type().is_ok_and(|kind| kind.is_socket()) {
                continue;
            }
            let path = entry.path();
            let refused = std::os::unix::net::UnixStream::connect(&path)
                .err()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::ConnectionRefused);
            if refused {
                let _ = fs::remove_file(&path);
            }
        }
    }

    /// Whether the socket directory is there, making it if it is not. No
    /// directory means no sharing, which costs an authentication per pane and
    /// breaks nothing.
    fn prepare(&self) -> bool {
        use std::os::unix::fs::DirBuilderExt;
        self.directory.is_dir()
            || fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&self.directory)
                .is_ok()
    }
}

/// The host an alias names: a saved one when the configuration knows it, an
/// address when somebody typed one, and otherwise the bare name, which is what
/// `ssh` would make of it too.
#[must_use]
pub fn host(alias: &str) -> Host {
    if let Some(host) = Hosts::load().get(alias) {
        return host.clone();
    }
    Host::adhoc(alias).unwrap_or_else(|| Host {
        alias: alias.to_owned(),
        ..Host::default()
    })
}

/// The command line that opens `host`.
///
/// Runs `ssh -G` by way of [`Control::options`], so the caller belongs off the
/// main thread.
#[must_use]
pub fn command(host: &Host, control: &Control) -> Vec<String> {
    let destination = host.target();
    let mut argv = vec!["ssh".to_owned()];
    for option in control.options(&destination) {
        argv.push("-o".to_owned());
        argv.push(option);
    }
    // A saved host's port is already in the configuration `ssh` is about to
    // read for itself. One typed by hand has nowhere else to carry it.
    if host.source == Source::Adhoc && host.port != 0 {
        argv.push("-p".to_owned());
        argv.push(host.port.to_string());
    }
    // `--`, because a destination may begin with a dash.
    argv.push("--".to_owned());
    argv.push(destination);
    argv
}

/// Where a host came from, which is what decides whether it may be changed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Source {
    /// A file the user wrote. Read-only: it has `Include`, `Match`,
    /// first-value-wins semantics and comments people rely on, and it is often
    /// under version control.
    #[default]
    SshConfig,
    /// The file tuni owns and rewrites whole.
    Tuni,
    /// An address typed into the search box and never written down.
    Adhoc,
}

/// The file and line a block was declared on, so "edit this" can open the real
/// thing rather than a copy of it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Origin {
    pub path: PathBuf,
    /// One-based, which is what an editor counts in.
    pub line: usize,
}

/// Which way a forwarded port points.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// `-L`: a port here, reached through the far end.
    #[default]
    Local,
    /// `-R`: a port there, reached through this end.
    Remote,
    /// `-D`: a SOCKS proxy here. There is no single far end.
    Dynamic,
}

impl Direction {
    #[must_use]
    pub fn flag(self) -> &'static str {
        match self {
            Self::Local => "-L",
            Self::Remote => "-R",
            Self::Dynamic => "-D",
        }
    }

    /// The same thing spelled as a configuration file spells it.
    #[must_use]
    pub fn keyword(self) -> &'static str {
        match self {
            Self::Local => "LocalForward",
            Self::Remote => "RemoteForward",
            Self::Dynamic => "DynamicForward",
        }
    }
}

/// One forwarded port.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Forward {
    #[serde(default)]
    pub direction: Direction,
    /// What is listened on. Empty means localhost for `Local` and `Dynamic`,
    /// and whatever `GatewayPorts` says for `Remote`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bind: String,
    /// Zero asks the far end to allocate one, which only `Remote` can do.
    #[serde(default)]
    pub listen_port: u16,
    /// Empty for `Dynamic`, which has no far end until something connects.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

impl Forward {
    /// What to call it in a list: the name somebody gave it, and otherwise the
    /// flag and the port, which is the half of a spec that identifies it. The
    /// other half is what the row says underneath in words.
    #[must_use]
    pub fn title(&self) -> String {
        if self.label.is_empty() {
            format!("{} {}", self.direction.flag(), self.listen())
        } else {
            self.label.clone()
        }
    }

    /// The argument that follows the flag: `8080:localhost:80`, `1080`.
    #[must_use]
    pub fn spec(&self) -> String {
        match self.direction {
            Direction::Dynamic => self.listen(),
            _ => format!("{}:{}:{}", self.listen(), literal(&self.host), self.port),
        }
    }

    /// What is listened on, as the side of a spec before the target, which is
    /// also the word a configuration file puts after the keyword.
    #[must_use]
    pub fn listen(&self) -> String {
        if self.bind.is_empty() {
            self.listen_port.to_string()
        } else {
            format!("{}:{}", literal(&self.bind), self.listen_port)
        }
    }

    /// What answers at the other end, as a word of its own. Empty for a dynamic
    /// forward, which has no one far end: whatever connects says where it wants
    /// to go.
    #[must_use]
    pub fn target(&self) -> String {
        if self.direction == Direction::Dynamic {
            String::new()
        } else {
            format!("{}:{}", literal(&self.host), self.port)
        }
    }

    /// Reads one back. Accepts both spellings of the same thing: the colons
    /// `-L` takes, and the whitespace a configuration file and `ssh -G` put
    /// between the listening side and the target.
    #[must_use]
    pub fn parse(direction: Direction, spec: &str) -> Option<Self> {
        let mut forward = Self {
            direction,
            ..Self::default()
        };
        match (direction, fields(spec).as_slice()) {
            (Direction::Dynamic, [port]) => forward.listen_port = port.parse().ok()?,
            (Direction::Dynamic, [bind, port]) => {
                forward.bind = bind.clone();
                forward.listen_port = port.parse().ok()?;
            }
            (_, [listen, host, port]) => {
                forward.listen_port = listen.parse().ok()?;
                forward.host = host.clone();
                forward.port = port.parse().ok()?;
            }
            (_, [bind, listen, host, port]) => {
                forward.bind = bind.clone();
                forward.listen_port = listen.parse().ok()?;
                forward.host = host.clone();
                forward.port = port.parse().ok()?;
            }
            _ => return None,
        }
        Some(forward)
    }
}

/// One connectable host.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Host {
    /// The name `ssh` is given, and the key for everything else.
    pub alias: String,
    /// Empty when the block leaves the address as the alias. [`resolve`] always
    /// fills it in, because `ssh` always answers that question.
    pub hostname: String,
    /// Zero means the configuration does not say, which means 22.
    pub port: u16,
    pub user: String,
    /// As written, `~` and all: expanding one here would only disagree with the
    /// `ssh` that is going to read it.
    pub identities: Vec<String>,
    /// As written: a comma-separated chain of its own aliases.
    pub proxy_jump: String,
    /// The ones the configuration declares. `ssh` brings these up itself with
    /// the connection, so there is nothing here to start.
    pub forwards: Vec<Forward>,
    /// Lines inside the block this crate does not generate, kept verbatim so a
    /// rewrite of the file it owns loses nothing it did not understand.
    pub extra: Vec<String>,
    pub source: Source,
    pub origin: Option<Origin>,
    /// Whether another block declares this alias too. First value obtained
    /// wins, so one of the two is dead, and a row can say which.
    pub shadowed: bool,
}

impl Host {
    /// An address typed rather than saved: `user@host`, `host:port`, `[::1]`.
    /// `None` when there is no host in it.
    #[must_use]
    pub fn adhoc(spec: &str) -> Option<Self> {
        let spec = spec.trim();
        let (user, rest) = match spec.rsplit_once('@') {
            Some((user, rest)) => (user.to_owned(), rest),
            None => (String::new(), spec),
        };
        let (hostname, port) = match fields(rest).as_slice() {
            [host] => (host.clone(), 0),
            [host, port] => (host.clone(), port.parse().ok()?),
            _ => return None,
        };
        if hostname.is_empty() {
            return None;
        }
        Some(Self {
            alias: spec.to_owned(),
            hostname,
            port,
            user,
            source: Source::Adhoc,
            ..Self::default()
        })
    }

    /// The destination for an `ssh` command line. The alias, because
    /// everything the configuration says about the host is filed under that
    /// name; an address typed by hand has no name to be filed under and so
    /// carries itself. The port is not in here: it is `-p`.
    #[must_use]
    pub fn target(&self) -> String {
        match self.source {
            Source::Adhoc if !self.user.is_empty() => {
                format!("{}@{}", self.user, self.hostname)
            }
            Source::Adhoc => self.hostname.clone(),
            _ => self.alias.clone(),
        }
    }

    /// The address to show beside the name: `deploy@10.0.0.1:2222`, with the
    /// parts the configuration does not say left out rather than guessed at.
    #[must_use]
    pub fn address(&self) -> String {
        let host = if self.hostname.is_empty() {
            &self.alias
        } else {
            &self.hostname
        };
        let mut address = if self.user.is_empty() {
            host.clone()
        } else {
            format!("{}@{host}", self.user)
        };
        if self.port != 0 && self.port != 22 {
            address.push_str(&format!(":{}", self.port));
        }
        address
    }
}

/// A `Host` line naming a shape rather than a host: `Host *`, `Host !prod
/// *.corp`. Not connectable, but it decides what the hosts around it resolve
/// to, and no new alias may collide with one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pattern {
    pub patterns: Vec<String>,
    pub origin: Origin,
}

/// Every host the configuration names, read once.
#[derive(Clone, Debug, Default)]
pub struct Hosts {
    hosts: Vec<Host>,
    patterns: Vec<Pattern>,
    /// Every file read, with the modification time it had at the time. What
    /// [`Hosts::stale`] compares against.
    read: Vec<(PathBuf, Option<SystemTime>)>,
}

impl Hosts {
    /// The user's configuration, and then tuni's own, which the first of the
    /// two usually includes.
    #[must_use]
    pub fn load() -> Self {
        Self::read_all(&[config_path(), store_path()])
    }

    /// One file and whatever it includes.
    #[must_use]
    pub fn read_from(root: &Path) -> Self {
        Self::read_all(&[root.to_path_buf()])
    }

    /// In the order the files declare them, which is the order `ssh` resolves
    /// them in. Sorting is the caller's, because a launcher sorts by one thing
    /// and precedence is another.
    #[must_use]
    pub fn all(&self) -> &[Host] {
        &self.hosts
    }

    #[must_use]
    pub fn get(&self, alias: &str) -> Option<&Host> {
        self.hosts.iter().find(|host| host.alias == alias)
    }

    #[must_use]
    pub fn patterns(&self) -> &[Pattern] {
        &self.patterns
    }

    /// Whether any file this was read from has changed since.
    ///
    /// A file a new `Include` would newly pull in is not watched, because it
    /// was not read; but the file that grew the `Include` line was, so the
    /// change is still noticed.
    #[must_use]
    pub fn stale(&self) -> bool {
        self.read
            .iter()
            .any(|(path, when)| modified(path).as_ref() != when.as_ref())
    }

    fn read_all(roots: &[PathBuf]) -> Self {
        let mut seen = HashSet::new();
        let mut read = Vec::new();
        let mut lines = Vec::new();
        for root in roots {
            // Relative includes resolve against the directory of the file that
            // was opened. OpenSSH resolves them against `~/.ssh` whatever the
            // including file is; for the one file that matters this is the same
            // answer, and it is the version a test can point at a temporary
            // directory.
            let base = root.parent().unwrap_or(Path::new(".")).to_path_buf();
            lines.extend(flatten(root, &base, 0, &mut seen, &mut read));
        }

        let store = real(&store_path());
        let (hosts, patterns) = parse(&lines, &store);

        // First block for an alias wins, which is ssh's own rule, and the loser
        // marks the winner so a row can say the alias is declared twice. Linear
        // in the number of hosts squared, over a list that is tens long.
        let mut unique: Vec<Host> = Vec::new();
        for host in hosts {
            match unique.iter_mut().find(|kept| kept.alias == host.alias) {
                Some(kept) => kept.shadowed = true,
                None => unique.push(host),
            }
        }

        Self {
            hosts: unique,
            patterns,
            read,
        }
    }
}

/// What ssh syntax has no way to say about a host.
///
/// A file of its own, keyed by alias, so it attaches equally to a host the user
/// wrote by hand: somebody can tag a host in `~/.ssh/config`, and have the
/// launcher put it where they last left it, without tuni writing a byte into
/// their file.
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct Meta {
    /// What to call the host instead of its alias, for an alias that is a
    /// hostname nobody wants to read.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Seconds since the epoch, which is what the Recent section sorts on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<u64>,
    #[serde(default, skip_serializing_if = "never_used")]
    pub uses: u32,
    /// The forwards tuni opens and closes on a connection that is already up.
    /// The ones a `Host` carries are a different thing: those are lines in a
    /// configuration file, `ssh` brings them up with the connection, and there
    /// is nothing here to start or stop.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forwards: Vec<Forward>,
}

fn never_used(count: &u32) -> bool {
    *count == 0
}

/// The metadata for every host that has any.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Notes(HashMap<String, Meta>);

impl Notes {
    #[must_use]
    pub fn path() -> PathBuf {
        crate::settings::config_dir().join("ssh/meta.json")
    }

    /// What is on disk, or nothing, which is the same thing as a host list
    /// nobody has labelled yet.
    #[must_use]
    pub fn load() -> Self {
        fs::read_to_string(Self::path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// A host with nothing written down about it reads as a host with the
    /// default written down about it, so no caller has to hold an `Option`.
    #[must_use]
    pub fn get(&self, alias: &str) -> Meta {
        self.0.get(alias).cloned().unwrap_or_default()
    }

    /// Files metadata under an alias, or forgets it when there is nothing left
    /// in it worth a line in the file.
    pub fn set(&mut self, alias: &str, meta: Meta) {
        if meta == Meta::default() {
            self.0.remove(alias);
        } else {
            self.0.insert(alias.to_owned(), meta);
        }
    }

    /// Records that a connection was opened, which is what puts a host at the
    /// top of the list next time.
    pub fn used(&mut self, alias: &str) {
        let mut meta = self.get(alias);
        meta.uses = meta.uses.saturating_add(1);
        meta.last_used = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()
            .map(|since| since.as_secs());
        self.set(alias, meta);
    }

    /// Written beside itself and renamed into place, the way every other file
    /// tuni owns is.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string(self).map_err(std::io::Error::other)?;
        let temporary = path.with_extension("json.new");
        fs::write(&temporary, text)?;
        fs::rename(&temporary, &path)
    }
}

/// What tuni writes at the top of the file it owns, so somebody who opens it
/// knows what happens to an edit made there.
const STORE_HEADER: &str = "\
# Written by tuni. This file is rewritten whole whenever a host in it changes.
#
# A keyword tuni does not generate is kept as it was written, so an option it
# has never heard of survives an edit made in the window. The layout does not:
# comments and blank lines between blocks are not read back.
";

/// What tuni adds to the user's own configuration, and the only thing it ever
/// adds there.
const INCLUDE_HEADER: &str =
    "# Added by tuni. Everything below this line is yours; tuni does not touch it.";

/// The copy taken of `~/.ssh/config` before it is first modified, kept forever
/// after and never overwritten.
#[must_use]
pub fn backup_path() -> PathBuf {
    config_path().with_extension("tuni-backup")
}

/// The hosts in the file tuni owns, which is the set the window may change.
#[must_use]
pub fn saved() -> Vec<Host> {
    Hosts::read_from(&store_path())
        .hosts
        .into_iter()
        .filter(|host| host.source == Source::Tuni)
        .collect()
}

/// Rewrites the file tuni owns from `hosts`, whole.
///
/// Validated before it is put in place, because this file is included into the
/// user's own configuration and a broken include breaks `ssh` for every script
/// on the machine, not only for tuni. `ssh -F` on the file that is about to
/// become the real one is the only check that agrees with the program that
/// will read it, and it touches no network.
pub fn save(hosts: &[Host]) -> Result<(), String> {
    write_hosts(&store_path(), hosts)
}

/// Adds tuni's `Include` to `~/.ssh/config` if it is not there yet, and says
/// whether it wrote anything.
///
/// At the top, because `ssh` takes the first value it obtains for a keyword and
/// a `Host *` block early in the file swallows everything after it, so an
/// include appended to the end is partly dead configuration.
pub fn ensure_include() -> Result<bool, String> {
    add_include(&config_path(), &store_path(), &backup_path())
}

fn write_hosts(path: &Path, hosts: &[Host]) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

    let text = render(hosts)?;
    let Some(directory) = path.parent() else {
        return Err(format!("{} has no directory to write into", path.display()));
    };
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
        .or_else(|error| {
            if directory.is_dir() {
                Ok(())
            } else {
                Err(describe_io(directory, &error))
            }
        })?;

    // 0600 from the moment it exists rather than after the write: a host list
    // names machines and accounts, and there is no instant where it is worth
    // being readable by everybody.
    let temporary = path.with_extension("conf.new");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|error| describe_io(&temporary, &error))?;
    let written = std::io::Write::write_all(&mut file, text.as_bytes())
        .and_then(|()| std::io::Write::flush(&mut file));
    drop(file);
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(describe_io(&temporary, &error));
    }

    let alias = hosts.first().map_or("tuni-check", |host| &host.alias);
    let checked = run(&[
        OsStr::new("-F"),
        temporary.as_os_str(),
        OsStr::new("-G"),
        OsStr::new("--"),
        OsStr::new(alias),
    ]);
    if checked.code != 0 {
        let _ = fs::remove_file(&temporary);
        let complaint = checked
            .stderr
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("ssh would not read the file");
        return Err(format!("ssh rejects the hosts file: {complaint}"));
    }

    fs::rename(&temporary, path).map_err(|error| describe_io(path, &error))
}

/// The whole file, or the first host that cannot be written and the reason.
fn render(hosts: &[Host]) -> Result<String, String> {
    let mut out = String::from(STORE_HEADER);
    for host in hosts {
        let alias = quote(&host.alias)
            .filter(|_| !host.alias.is_empty())
            .ok_or_else(|| format!("{} cannot be a host name", host.alias))?;
        // A name with a wildcard in it is a pattern, and a pattern here would
        // silently answer for hosts the user never meant it to.
        if host.alias.contains(['*', '?', '!']) {
            return Err(format!("{} is a pattern rather than a name", host.alias));
        }
        out.push_str(&format!("\nHost {alias}\n"));

        let mut line = |keyword: &str, value: &str| -> Result<(), String> {
            if value.is_empty() {
                return Ok(());
            }
            let value =
                quote(value).ok_or_else(|| format!("{value} cannot be the value of {keyword}"))?;
            out.push_str(&format!("    {keyword} {value}\n"));
            Ok(())
        };
        line("HostName", &host.hostname)?;
        line("User", &host.user)?;
        if host.port != 0 {
            line("Port", &host.port.to_string())?;
        }
        for identity in &host.identities {
            line("IdentityFile", identity)?;
        }
        line("ProxyJump", &host.proxy_jump)?;

        for forward in &host.forwards {
            // `-L` takes the whole forward as one colon-joined argument and a
            // configuration file does not: it reads the target as a word of its
            // own, and rejects the joined form with `Missing target argument`.
            // So the two sides are written, and quoted, apart.
            let keyword = forward.direction.keyword();
            let word = |value: &str| -> Result<String, String> {
                quote(value).ok_or_else(|| format!("{value} cannot be the value of {keyword}"))
            };
            let mut text = format!("    {keyword} {}", word(&forward.listen())?);
            let target = forward.target();
            if !target.is_empty() {
                text.push(' ');
                text.push_str(&word(&target)?);
            }
            out.push_str(&text);
            out.push('\n');
        }

        for extra in &host.extra {
            let extra = extra.trim();
            if extra.is_empty() {
                continue;
            }
            // Written as it was typed, so the check has to be the whole line.
            // A newline in one would add a keyword nobody asked for, and a
            // `Host` or `Match` in one would end this block and hand every
            // line after it to a host that is not this one.
            if extra.contains(['\n', '\r', '\0']) {
                return Err(format!("{extra} cannot be an option"));
            }
            let keyword = extra.split_whitespace().next().unwrap_or_default();
            if keyword.eq_ignore_ascii_case("host") || keyword.eq_ignore_ascii_case("match") {
                return Err(format!("{keyword} would start another host's block"));
            }
            out.push_str(&format!("    {extra}\n"));
        }
    }
    Ok(out)
}

/// One value as the file may hold it, or `None` for one it may not.
///
/// This is the security boundary of the whole store. The file is included into
/// the user's own configuration, so a value carrying a newline carries an
/// arbitrary keyword with it, `ProxyCommand` among them, and that keyword runs
/// a command. The answer is to refuse the value rather than to escape it: there
/// is no address, user or path anybody means that contains a line break.
fn quote(value: &str) -> Option<String> {
    if value.contains(['\n', '\r', '\0']) {
        return None;
    }
    if !value.contains([' ', '\t', '"', '\\']) {
        return Some(value.to_owned());
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        if character == '"' || character == '\\' {
            out.push('\\');
        }
        out.push(character);
    }
    out.push('"');
    Some(out)
}

fn add_include(config: &Path, store: &Path, backup: &Path) -> Result<bool, String> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

    let line = format!("Include {}", tilde(store));
    let existing = fs::read_to_string(config).ok();

    if let Some(text) = &existing
        && includes(text, store)
    {
        return Ok(false);
    }

    let Some(directory) = config.parent() else {
        return Err(format!("{} has no directory", config.display()));
    };
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
        .or_else(|error| {
            if directory.is_dir() {
                Ok(())
            } else {
                Err(describe_io(directory, &error))
            }
        })?;

    let Some(body) = existing else {
        // No configuration at all: the file tuni creates is the whole of it,
        // and 0600 is what `ssh` expects of anything in `~/.ssh`.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(config)
            .map_err(|error| describe_io(config, &error))?;
        std::io::Write::write_all(&mut file, format!("{INCLUDE_HEADER}\n{line}\n").as_bytes())
            .map_err(|error| describe_io(config, &error))?;
        return Ok(true);
    };

    // `~/.ssh/config` is very often a symlink into a dotfiles repository.
    // Renaming onto the link replaces the link with a regular file and quietly
    // detaches somebody from their own dotfiles, so the write goes to what the
    // link points at.
    let real = fs::canonicalize(config).map_err(|error| describe_io(config, &error))?;
    let Some(directory) = real.parent() else {
        return Err(format!("{} has no directory", real.display()));
    };

    // The user's most valuable dotfile, copied once and then left alone: a
    // second copy would overwrite the one from before tuni ever touched it.
    if !backup.exists() {
        fs::copy(&real, backup).map_err(|error| describe_io(backup, &error))?;
    }

    let mode = fs::metadata(&real)
        .map(|data| std::os::unix::fs::PermissionsExt::mode(&data.permissions()))
        .unwrap_or(0o600);
    let temporary = directory.join("config.tuni-new");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(&temporary)
        .map_err(|error| describe_io(&temporary, &error))?;
    let written = std::io::Write::write_all(
        &mut file,
        format!("{INCLUDE_HEADER}\n{line}\n\n{body}").as_bytes(),
    );
    drop(file);
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(describe_io(&temporary, &error));
    }
    fs::rename(&temporary, &real).map_err(|error| describe_io(&real, &error))?;
    Ok(true)
}

/// Whether a configuration already includes `store`, whatever it spells the
/// path as.
fn includes(text: &str, store: &Path) -> bool {
    let store = real(store);
    text.lines().any(|line| {
        let Some((keyword, value)) = split(line) else {
            return false;
        };
        keyword == "include"
            && words(&value)
                .iter()
                .any(|path| real(&untilde(path)) == store)
    })
}

/// A path under `$HOME` written the way a configuration file writes it, which
/// is the form that still means the same thing to somebody reading the file on
/// another machine.
fn tilde(path: &Path) -> String {
    let home = crate::settings::home();
    match path.strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

fn untilde(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => crate::settings::home().join(rest),
        None => PathBuf::from(path),
    }
}

/// An `io::Error` with the file it happened to in front of it, which is the
/// half of the message the caller cannot add afterwards.
fn describe_io(path: &Path, error: &std::io::Error) -> String {
    format!("{}: {error}", path.display())
}

/// Everything `ssh` decides an alias means, one lowercase keyword per line,
/// repeated for the keywords that may appear more than once and simply absent
/// for the ones nothing set.
///
/// Empty when `ssh` could not be run at all. It exits 0 even for an alias
/// nothing defines, because a name nothing defines is a hostname.
#[must_use]
pub fn describe(alias: &str) -> Vec<(String, String)> {
    described(&[], alias)
}

/// The same report with settings of tuni's own applied first, which is the only
/// way to learn where a connection tuni asked to share actually ends up: a
/// command-line `-o` is obtained before the file, and `%C` is a hash this crate
/// does not implement.
fn described(options: &[String], destination: &str) -> Vec<(String, String)> {
    let mut args: Vec<&str> = Vec::new();
    for option in options {
        args.push("-o");
        args.push(option);
    }
    // `--` because an alias may begin with a dash.
    args.extend(["-G", "--", destination]);
    let output = run(&args);
    if !output.is_ok() {
        return Vec::new();
    }
    output
        .stdout
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(keyword, value)| (keyword.to_owned(), value.to_owned()))
        .collect()
}

/// Whether the local side of `forward` is free, and what is on it when it is
/// not.
///
/// Only the forwards that listen on this machine. A remote forward listens at
/// the far end, where nothing here can look, and the master's own answer is all
/// there is.
///
/// This is a race and it is written as one: the socket is bound and dropped
/// again, and something else can take the port between that and the master's
/// own bind. The real error path stays where it is. What this buys is a
/// sentence a person can act on, because the mux client's own version of it is
/// `Error: remote port forwarding failed` with no port and no process in it.
pub fn check_port(forward: &Forward) -> Result<(), String> {
    use std::net::TcpListener;

    if forward.direction == Direction::Remote || forward.listen_port == 0 {
        return Ok(());
    }
    let bind = forward.bind.trim_matches(['[', ']'].as_slice());
    let bind = if bind.is_empty() { "127.0.0.1" } else { bind };
    let port = forward.listen_port;
    let error = match TcpListener::bind((bind, port)) {
        Ok(_) => return Ok(()),
        Err(error) => error,
    };
    match error.kind() {
        std::io::ErrorKind::AddrInUse => Err(match crate::info::listener(port) {
            Some((pid, process)) => {
                format!("Port {port} is already in use: {process} (pid {pid}) is listening on it")
            }
            // Another user's process, which `/proc` does not let this one see
            // through to a name.
            None => format!("Port {port} is already in use by another program"),
        }),
        std::io::ErrorKind::PermissionDenied => Err(format!(
            "Port {port} is one of the first 1024, which only the system may listen on"
        )),
        _ => Err(format!("Nothing can listen on {bind}:{port}: {error}")),
    }
}

/// The port a remote forward was actually given, out of `Allocated port 34567
/// for remote forward to localhost:5432`.
fn allocated_port(text: &str) -> Option<u16> {
    let digits: String = text
        .split_once("Allocated port ")?
        .1
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// The process id out of what a master answers an alive check with, which is
/// `Master running (pid=12345)` and a carriage return.
fn master_pid(text: &str) -> Option<u32> {
    let digits: String = text
        .split_once("pid=")?
        .1
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// Whether a name is the forty hex characters `%C` expands to, which is what
/// tells a socket tuni asked for from anything else in the directory. OpenSSH
/// binds a master to a name with a suffix on it and moves it into place once it
/// is listening, so the half-built ones are not this shape.
fn hashed(name: &str) -> bool {
    name.len() == 40 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// What an alias resolves to once `ssh` has applied everything it applies.
///
/// Expensive and not idempotent: this runs the user's `Match exec` commands.
/// Call it when a host is opened or its detail is shown, never on a timer.
#[must_use]
pub fn resolve(alias: &str) -> Option<Host> {
    let options = describe(alias);
    if options.is_empty() {
        return None;
    }
    let mut host = Host {
        alias: alias.to_owned(),
        ..Host::default()
    };
    for (keyword, value) in options {
        match keyword.as_str() {
            "hostname" => host.hostname = value,
            "user" => host.user = value,
            "port" => host.port = value.parse().unwrap_or_default(),
            "identityfile" => host.identities.push(value),
            "proxyjump" => host.proxy_jump = value,
            "localforward" => extend(&mut host.forwards, Direction::Local, &value),
            "remoteforward" => extend(&mut host.forwards, Direction::Remote, &value),
            "dynamicforward" => extend(&mut host.forwards, Direction::Dynamic, &value),
            _ => {}
        }
    }
    Some(host)
}

/// Whether `text` matches an ssh pattern: `*` for any run of characters, `?`
/// for exactly one, and everything else literal. What `Host` lines and
/// `Include` globs are both written in.
///
/// A leading `!` is the caller's to strip, because negation only means
/// anything against a whole list of patterns. Character classes are not
/// supported and a `[` is literal; the cost is a file that goes unread, which
/// is a missing row rather than a wrong one.
#[must_use]
pub fn matches(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0, 0);
    // Where to resume from when the run a `*` was allowed to swallow turns out
    // to have been one character too short. Iterative for the usual reason: a
    // pattern is user input and this must not be able to blow the stack.
    let mut star: Option<usize> = None;
    let mut resume = 0;
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some(p);
                resume = t;
                p += 1;
            }
            Some('?') => {
                p += 1;
                t += 1;
            }
            Some(c) if *c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some(index) => {
                    p = index + 1;
                    resume += 1;
                    t = resume;
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

/// One line of configuration and where it came from, so a host can point at
/// the file and line that declared it after every `Include` has been folded in.
struct Line {
    path: PathBuf,
    number: usize,
    text: String,
}

/// Reads one file and everything it includes, in place, so a block opened
/// before an `Include` and continued after it stays one block.
fn flatten(
    path: &Path,
    base: &Path,
    depth: usize,
    seen: &mut HashSet<PathBuf>,
    read: &mut Vec<(PathBuf, Option<SystemTime>)>,
) -> Vec<Line> {
    // Canonicalised, so a symlink pointing back at a file already read is
    // caught along with a file that literally includes itself.
    let path = real(path);
    if depth > INCLUDE_DEPTH || read.len() >= INCLUDE_FILES || !seen.insert(path.clone()) {
        return Vec::new();
    }
    read.push((path.clone(), modified(&path)));

    let Ok(text) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for (index, text) in text.lines().enumerate() {
        if text.trim().is_empty() {
            continue;
        }
        if let Some((keyword, value)) = split(text)
            && keyword == "include"
        {
            for glob in words(&value) {
                for file in expand(&glob, base) {
                    lines.extend(flatten(&file, base, depth + 1, seen, read));
                }
            }
            continue;
        }
        lines.push(Line {
            path: path.clone(),
            number: index + 1,
            text: text.to_owned(),
        });
    }
    lines
}

/// The block being read, held open until the next `Host`, `Match` or the end
/// of the configuration closes it.
struct Block {
    /// The plain names on the `Host` line. Several of them is one body under
    /// several names, and none of them is a `Host *` whose body is read only
    /// to keep it out of the block after it.
    aliases: Vec<String>,
    origin: Origin,
    source: Source,
    host: Host,
}

fn parse(lines: &[Line], store: &Path) -> (Vec<Host>, Vec<Pattern>) {
    let mut hosts = Vec::new();
    let mut patterns = Vec::new();
    let mut block: Option<Block> = None;

    for line in lines {
        let text = line.text.trim();
        if text.starts_with('#') {
            if let Some(block) = &mut block {
                block.host.extra.push(text.to_owned());
            }
            continue;
        }
        let Some((keyword, value)) = split(text) else {
            continue;
        };
        match keyword.as_str() {
            "host" => {
                flush(&mut block, &mut hosts);
                let origin = Origin {
                    path: line.path.clone(),
                    line: line.number,
                };
                let (aliases, shapes): (Vec<String>, Vec<String>) = words(&value)
                    .into_iter()
                    .partition(|name| !name.contains(['*', '?', '!']));
                if !shapes.is_empty() {
                    patterns.push(Pattern {
                        patterns: shapes,
                        origin: origin.clone(),
                    });
                }
                let source = if line.path == store {
                    Source::Tuni
                } else {
                    Source::SshConfig
                };
                block = Some(Block {
                    aliases,
                    origin,
                    source,
                    host: Host::default(),
                });
            }
            // A `Match` block is not read from, and closing the block before it
            // is what stops the keywords under it being attributed to the last
            // `Host`.
            "match" => flush(&mut block, &mut hosts),
            _ => {
                let Some(block) = &mut block else { continue };
                let host = &mut block.host;
                match keyword.as_str() {
                    "hostname" => host.hostname = first(&value),
                    "port" => host.port = first(&value).parse().unwrap_or_default(),
                    "user" => host.user = first(&value),
                    "identityfile" => host.identities.push(first(&value)),
                    "proxyjump" => host.proxy_jump = first(&value),
                    "localforward" => extend(&mut host.forwards, Direction::Local, &value),
                    "remoteforward" => extend(&mut host.forwards, Direction::Remote, &value),
                    "dynamicforward" => extend(&mut host.forwards, Direction::Dynamic, &value),
                    _ => host.extra.push(text.to_owned()),
                }
            }
        }
    }
    flush(&mut block, &mut hosts);
    (hosts, patterns)
}

fn flush(block: &mut Option<Block>, hosts: &mut Vec<Host>) {
    let Some(block) = block.take() else { return };
    for alias in block.aliases {
        hosts.push(Host {
            alias,
            source: block.source,
            origin: Some(block.origin.clone()),
            ..block.host.clone()
        });
    }
}

fn extend(forwards: &mut Vec<Forward>, direction: Direction, value: &str) {
    if let Some(forward) = Forward::parse(direction, value) {
        forwards.push(forward);
    }
}

/// One `Keyword value` line, split the way `ssh` splits it: the keyword
/// lowercased, `=` a separator as good as a space, and a `#` opening a comment
/// wherever a token could have started.
///
/// Deviation, deliberate: OpenSSH reads a `#` inside a `Host` pattern list as a
/// pattern rather than as a comment. Nobody writes that.
fn split(line: &str) -> Option<(String, String)> {
    let line = uncomment(line).trim();
    if line.is_empty() {
        return None;
    }
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let rest = line[end..].trim_start();
    let rest = rest.strip_prefix('=').map_or(rest, str::trim_start);
    Some((line[..end].to_lowercase(), rest.to_owned()))
}

fn uncomment(line: &str) -> &str {
    let mut quoted = false;
    let mut after_space = true;
    for (index, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '#' if !quoted && after_space => return &line[..index],
            _ => {}
        }
        after_space = c.is_whitespace();
    }
    line
}

/// The words of a value, with double quotes taken off and `\"` and `\\`
/// honoured inside them. A backslash outside quotes is literal, as it is to
/// `ssh`.
fn words(value: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let (mut quoted, mut started, mut escaped) = (false, false, false);
    for c in value.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' if quoted => escaped = true,
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(current);
    }
    words
}

fn first(value: &str) -> String {
    words(value).into_iter().next().unwrap_or_default()
}

/// A forward spec split on the separators between its fields, leaving the
/// colons inside the brackets an IPv6 literal is written with. Whitespace
/// separates too: `-L` joins the listening side to the target with a colon and
/// a configuration file puts a space there.
fn fields(spec: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut bracketed = false;
    for c in spec.trim().chars() {
        match c {
            '[' => bracketed = true,
            ']' => bracketed = false,
            ':' if !bracketed => fields.push(std::mem::take(&mut current)),
            c if c.is_whitespace() && !bracketed => {
                if !current.is_empty() {
                    fields.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    fields.push(current);
    fields
}

/// An address as a forward spec writes it. An IPv6 literal goes in brackets or
/// its own colons read as the separators between fields.
fn literal(address: &str) -> String {
    if address.contains(':') {
        format!("[{address}]")
    } else {
        address.to_owned()
    }
}

/// The files an `Include` glob names, sorted, which is the order `ssh` reads
/// them in.
fn expand(pattern: &str, base: &Path) -> Vec<PathBuf> {
    let pattern = match pattern.strip_prefix("~/") {
        Some(rest) => crate::settings::home().join(rest),
        None => PathBuf::from(pattern),
    };
    if !pattern.to_string_lossy().contains(['*', '?']) {
        // Returned whether or not it exists: a file that is named and missing
        // is one `stale` should notice appearing.
        return vec![if pattern.is_absolute() {
            pattern
        } else {
            base.join(pattern)
        }];
    }

    let mut found = vec![if pattern.is_absolute() {
        PathBuf::from("/")
    } else {
        base.to_path_buf()
    }];
    for component in pattern.components() {
        let name = component.as_os_str().to_string_lossy().into_owned();
        if name == "/" {
            continue;
        }
        if !name.contains(['*', '?']) {
            for path in &mut found {
                path.push(&name);
            }
            continue;
        }
        let mut next = Vec::new();
        for path in &found {
            let Ok(entries) = fs::read_dir(path) else {
                continue;
            };
            let mut here: Vec<PathBuf> = entries
                .flatten()
                .filter(|entry| matches(&name, &entry.file_name().to_string_lossy()))
                .map(|entry| entry.path())
                .collect();
            here.sort();
            next.extend(here);
        }
        found = next;
    }
    found.retain(|path| path.is_file());
    found
}

fn real(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn tempdir() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("tuni-ssh-{stamp}"));
        std::fs::create_dir_all(&path).expect("temporary directory");
        path
    }

    /// Writes a configuration to a temporary file and reads it back.
    fn read(text: &str) -> (Hosts, PathBuf) {
        let directory = tempdir();
        let path = directory.join("config");
        fs::write(&path, text).expect("write");
        (Hosts::read_from(&path), directory)
    }

    #[test]
    fn a_host_block_becomes_a_host() {
        let (hosts, _dir) = read(
            "Host web\n  HostName 10.0.0.1\n  User deploy\n  Port 2222\n  IdentityFile ~/.ssh/id_ed25519\n",
        );
        let host = hosts.get("web").expect("web");
        assert_eq!(host.hostname, "10.0.0.1");
        assert_eq!(host.user, "deploy");
        assert_eq!(host.port, 2222);
        assert_eq!(host.identities, ["~/.ssh/id_ed25519"]);
        assert_eq!(host.address(), "deploy@10.0.0.1:2222");
    }

    #[test]
    fn several_names_on_one_host_line_are_several_hosts() {
        let (hosts, _dir) = read("Host web web2\n  User deploy\n");
        assert_eq!(hosts.all().len(), 2);
        assert_eq!(hosts.get("web2").expect("web2").user, "deploy");
    }

    #[test]
    fn a_wildcard_host_line_is_a_pattern_and_not_a_host() {
        let (hosts, _dir) = read("Host *.corp !prod\n  User root\n\nHost web\n  User deploy\n");
        assert_eq!(hosts.all().len(), 1);
        assert_eq!(hosts.get("web").expect("web").user, "deploy");
        assert_eq!(hosts.patterns().len(), 1);
        assert_eq!(hosts.patterns()[0].patterns, ["*.corp", "!prod"]);
    }

    #[test]
    fn a_match_block_ends_the_host_before_it() {
        let (hosts, _dir) = read("Host web\n  User deploy\n\nMatch host bastion\n  User root\n");
        assert_eq!(hosts.get("web").expect("web").user, "deploy");
        assert!(hosts.get("web").expect("web").extra.is_empty());
    }

    #[test]
    fn a_keyword_this_does_not_understand_is_kept_verbatim() {
        let (hosts, _dir) = read("Host web\n  ForwardAgent yes\n  # mind the gap\n");
        assert_eq!(
            hosts.get("web").expect("web").extra,
            ["ForwardAgent yes", "# mind the gap"]
        );
    }

    #[test]
    fn an_equals_sign_separates_a_keyword_from_its_value() {
        let (hosts, _dir) = read("Host=web\n  Port=2222\n  User = deploy\n");
        let host = hosts.get("web").expect("web");
        assert_eq!(host.port, 2222);
        assert_eq!(host.user, "deploy");
    }

    #[test]
    fn a_quoted_value_keeps_its_spaces() {
        let (hosts, _dir) = read("Host web\n  IdentityFile \"~/keys/work laptop\"\n");
        assert_eq!(
            hosts.get("web").expect("web").identities,
            ["~/keys/work laptop"]
        );
    }

    #[test]
    fn a_comment_after_a_value_is_not_part_of_it() {
        let (hosts, _dir) = read("Host web\n  HostName 10.0.0.1 # the staging box\n");
        assert_eq!(hosts.get("web").expect("web").hostname, "10.0.0.1");
    }

    #[test]
    fn an_include_is_read_where_it_stands() {
        let directory = tempdir();
        fs::write(
            directory.join("extra"),
            "  User deploy\n\nHost db\n  Port 5433\n",
        )
        .expect("write");
        let path = directory.join("config");
        fs::write(&path, "Host web\n  HostName 10.0.0.1\n  Include extra\n").expect("write");

        let hosts = Hosts::read_from(&path);
        // The included keyword lands in the block that was open at the
        // `Include`, and the block the included file opens survives it.
        assert_eq!(hosts.get("web").expect("web").user, "deploy");
        assert_eq!(hosts.get("db").expect("db").port, 5433);
    }

    #[test]
    fn a_file_that_includes_itself_is_read_once() {
        let directory = tempdir();
        let path = directory.join("config");
        fs::write(&path, "Include config\nHost web\n  Port 22\n").expect("write");
        assert_eq!(Hosts::read_from(&path).all().len(), 1);
    }

    #[test]
    fn a_glob_include_reads_every_file_it_names() {
        let directory = tempdir();
        fs::create_dir_all(directory.join("conf.d")).expect("directory");
        fs::write(directory.join("conf.d/a.conf"), "Host alpha\n").expect("write");
        fs::write(directory.join("conf.d/b.conf"), "Host beta\n").expect("write");
        fs::write(directory.join("conf.d/notes.txt"), "Host gamma\n").expect("write");
        let path = directory.join("config");
        fs::write(&path, "Include conf.d/*.conf\n").expect("write");

        let hosts = Hosts::read_from(&path);
        let names: Vec<&str> = hosts.all().iter().map(|h| h.alias.as_str()).collect();
        assert_eq!(names, ["alpha", "beta"]);
    }

    #[test]
    fn the_first_block_for_an_alias_wins_and_says_it_was_shadowed() {
        let (hosts, _dir) = read("Host web\n  Port 22\n\nHost web\n  Port 2222\n");
        assert_eq!(hosts.all().len(), 1);
        let host = hosts.get("web").expect("web");
        assert_eq!(host.port, 22);
        assert!(host.shadowed);
    }

    #[test]
    fn a_host_points_at_the_line_that_declared_it() {
        let (hosts, _dir) = read("Host web\n  Port 22\n\nHost db\n  Port 5432\n");
        let origin = hosts.get("db").expect("db").origin.clone().expect("origin");
        assert_eq!(origin.line, 4);
    }

    #[test]
    fn a_pattern_matches_the_way_ssh_matches() {
        for (pattern, text, expected) in [
            ("*", "anything", true),
            ("*", "", true),
            ("*.corp", "web.corp", true),
            ("*.corp", "corp", false),
            ("web?", "web1", true),
            ("web?", "web", false),
            ("web?", "web12", false),
            ("a*b*c", "axxbyyc", true),
            ("a*b*c", "axxbyy", false),
            ("*.conf", "a.conf", true),
            ("*.conf", "notes.txt", false),
            ("prod", "prod", true),
            ("prod", "prod2", false),
            ("*x*", "x", true),
        ] {
            assert_eq!(matches(pattern, text), expected, "{pattern} vs {text}");
        }
    }

    #[test]
    fn a_forward_reads_back_out_of_its_own_spec() {
        for (direction, spec) in [
            (Direction::Local, "8080:localhost:80"),
            (Direction::Local, "127.0.0.1:5432:db.internal:5432"),
            (Direction::Local, "8080:[::1]:80"),
            (Direction::Local, "[::1]:8080:[fe80::1]:80"),
            (Direction::Remote, "9000:localhost:9000"),
            (Direction::Remote, "0:localhost:22"),
            (Direction::Dynamic, "1080"),
            (Direction::Dynamic, "[::1]:1080"),
        ] {
            let forward = Forward::parse(direction, spec).expect(spec);
            assert_eq!(forward.spec(), spec);
        }
    }

    #[test]
    fn a_forward_line_is_written_with_a_space_and_read_with_a_colon() {
        let (hosts, _dir) = read(
            "Host web\n  LocalForward 8080 localhost:80\n  LocalForward [127.0.0.1]:5432 [db]:5432\n  DynamicForward 1080\n",
        );
        let forwards = &hosts.get("web").expect("web").forwards;
        assert_eq!(forwards[0].spec(), "8080:localhost:80");
        assert_eq!(forwards[1].spec(), "127.0.0.1:5432:db:5432");
        assert_eq!(forwards[2].direction, Direction::Dynamic);
        assert_eq!(forwards[2].spec(), "1080");
    }

    #[test]
    fn an_address_typed_by_hand_is_a_host() {
        let host = Host::adhoc("deploy@10.0.0.1:2222").expect("host");
        assert_eq!(host.user, "deploy");
        assert_eq!(host.hostname, "10.0.0.1");
        assert_eq!(host.port, 2222);
        assert_eq!(host.target(), "deploy@10.0.0.1");
        assert_eq!(Host::adhoc("[::1]:22").expect("host").hostname, "::1");
        assert_eq!(Host::adhoc("box").expect("host").target(), "box");
        assert!(Host::adhoc("two words here").is_none());
        assert!(Host::adhoc("").is_none());
    }

    #[test]
    fn a_changed_file_makes_a_read_stale() {
        let directory = tempdir();
        let path = directory.join("config");
        fs::write(&path, "Host web\n").expect("write");
        let hosts = Hosts::read_from(&path);
        assert!(!hosts.stale());
        // Coarse clocks exist, so move the time rather than trust a rewrite to
        // land in a different tick.
        fs::write(&path, "Host web\nHost db\n").expect("write");
        let later = SystemTime::now() + std::time::Duration::from_secs(2);
        fs::File::open(&path)
            .expect("open")
            .set_modified(later)
            .expect("touch");
        assert!(hosts.stale());
    }

    #[test]
    fn a_connection_ends_in_the_host_it_opens() {
        let control = Control::new(CONTROL_PERSIST, false);
        let host = Host {
            alias: "tuni-no-such-alias".to_owned(),
            ..Host::default()
        };
        let argv = command(&host, &control);
        assert_eq!(argv[0], "ssh");
        assert_eq!(argv[argv.len() - 2], "--");
        assert_eq!(argv[argv.len() - 1], "tuni-no-such-alias");
        assert!(!argv.iter().any(|arg| arg.starts_with("ControlPath")));

        // A port the configuration has never heard of has to travel on the
        // command line.
        let typed = Host::adhoc("deploy@10.0.0.1:2222").expect("host");
        let argv = command(&typed, &control);
        assert!(argv.windows(2).any(|pair| pair == ["-p", "2222"]));
        assert_eq!(argv[argv.len() - 1], "deploy@10.0.0.1");
    }

    #[test]
    fn ssh_says_what_an_alias_resolves_to() {
        // Whatever this machine's configuration says, an undefined alias is its
        // own hostname on port 22. `ssh -G` answers for one without connecting.
        let host = resolve("tuni-no-such-alias").expect("ssh -G");
        assert_eq!(host.hostname, "tuni-no-such-alias");
        assert_eq!(host.port, 22);
        assert!(!host.user.is_empty());
    }

    #[test]
    fn a_host_written_out_reads_back_the_same() {
        let directory = tempdir();
        let path = directory.join("hosts.conf");
        let host = Host {
            alias: "web".to_owned(),
            hostname: "10.0.0.4".to_owned(),
            port: 2222,
            user: "deploy".to_owned(),
            identities: vec!["~/.ssh/id_ed25519".to_owned()],
            proxy_jump: "bastion".to_owned(),
            forwards: vec![Forward::parse(Direction::Local, "5432:localhost:5432").expect("spec")],
            extra: vec!["ForwardAgent yes".to_owned()],
            source: Source::Tuni,
            ..Host::default()
        };
        write_hosts(&path, std::slice::from_ref(&host)).expect("write");

        let read = Hosts::read_from(&path);
        let back = read.get("web").expect("web");
        assert_eq!(back.hostname, host.hostname);
        assert_eq!(back.port, host.port);
        assert_eq!(back.user, host.user);
        assert_eq!(back.identities, host.identities);
        assert_eq!(back.proxy_jump, host.proxy_jump);
        assert_eq!(back.forwards, host.forwards);
        // A keyword this crate does not generate survives being rewritten,
        // which is the whole point of keeping the extra lines.
        assert_eq!(back.extra, host.extra);
    }

    #[test]
    fn a_value_that_would_start_a_line_of_its_own_is_refused() {
        assert_eq!(quote("10.0.0.4"), Some("10.0.0.4".to_owned()));
        assert_eq!(quote("/home/a b/key"), Some("\"/home/a b/key\"".to_owned()));
        assert_eq!(quote("host\nProxyCommand touch /tmp/pwned"), None);
        assert_eq!(quote("host\rProxyCommand x"), None);

        let injected = Host {
            alias: "web".to_owned(),
            hostname: "10.0.0.4\nProxyCommand touch /tmp/pwned".to_owned(),
            ..Host::default()
        };
        assert!(render(&[injected]).is_err());

        // Not a value at all, but a line that would hand every keyword after
        // it to a host nobody named.
        let smuggled = Host {
            alias: "web".to_owned(),
            extra: vec!["Host *".to_owned()],
            ..Host::default()
        };
        assert!(render(&[smuggled]).is_err());
    }

    #[test]
    fn a_name_that_answers_for_other_hosts_is_not_a_name() {
        let pattern = Host {
            alias: "*.corp".to_owned(),
            hostname: "10.0.0.4".to_owned(),
            ..Host::default()
        };
        assert!(render(&[pattern]).is_err());
    }

    #[test]
    fn a_file_ssh_would_not_read_is_never_put_in_place() {
        let directory = tempdir();
        let path = directory.join("hosts.conf");
        write_hosts(&path, &[]).expect("write an empty one");
        let before = fs::read_to_string(&path).expect("read");

        let broken = Host {
            alias: "web".to_owned(),
            extra: vec!["NotAKeyword yes".to_owned()],
            ..Host::default()
        };
        assert!(write_hosts(&path, &[broken]).is_err());
        assert_eq!(fs::read_to_string(&path).expect("read"), before);
        assert!(!path.with_extension("conf.new").exists());
    }

    #[test]
    fn the_include_is_added_once_and_the_file_below_it_is_untouched() {
        let directory = tempdir();
        let config = directory.join("config");
        let store = directory.join("hosts.conf");
        let backup = directory.join("config.tuni-backup");
        let body = "Host web\n  HostName 10.0.0.4\n";
        fs::write(&config, body).expect("write");

        assert!(add_include(&config, &store, &backup).expect("first"));
        let text = fs::read_to_string(&config).expect("read");
        assert!(text.contains(&format!("Include {}", store.display())));
        assert!(text.ends_with(body), "the user's own file is kept whole");
        // Before the first line, because ssh takes the first value it obtains
        // and an early `Host *` would swallow an include put at the end.
        assert!(text.find("Include").unwrap() < text.find("Host web").unwrap());
        assert_eq!(fs::read_to_string(&backup).expect("backup"), body);

        assert!(!add_include(&config, &store, &backup).expect("second"));
        assert_eq!(fs::read_to_string(&config).expect("read"), text);
    }

    #[test]
    fn a_configuration_that_is_a_symlink_stays_a_symlink() {
        let directory = tempdir();
        let real = directory.join("dotfiles-config");
        let config = directory.join("config");
        let store = directory.join("hosts.conf");
        let backup = directory.join("config.tuni-backup");
        fs::write(&real, "Host web\n").expect("write");
        std::os::unix::fs::symlink(&real, &config).expect("symlink");

        assert!(add_include(&config, &store, &backup).expect("include"));
        assert!(
            fs::symlink_metadata(&config)
                .expect("stat")
                .file_type()
                .is_symlink(),
            "renaming onto the link would detach the user from their dotfiles"
        );
        assert!(fs::read_to_string(&real).expect("read").contains("Include"));
    }

    #[test]
    fn metadata_survives_being_written_down() {
        let mut notes = Notes::default();
        assert_eq!(notes.get("web"), Meta::default());

        notes.used("web");
        let text = serde_json::to_string(&notes).expect("write");
        let read: Notes = serde_json::from_str(&text).expect("read");
        assert_eq!(read.get("web").uses, 1);
        assert!(read.get("web").last_used.is_some());

        // Nothing left worth saying about it, so it stops taking a line.
        notes.set("web", Meta::default());
        assert!(
            !serde_json::to_string(&notes)
                .expect("write")
                .contains("web")
        );
    }

    #[test]
    fn a_connection_nobody_opened_is_not_live() {
        // The whole restore rule hangs on this answering `false` rather than
        // hanging or dialling: `ssh -O check` talks to a socket that is not
        // there and gives up at once, without touching the network.
        assert!(!Control::new(CONTROL_PERSIST, true).is_live("tuni-no-such-alias"));
        assert!(!Control::new(CONTROL_PERSIST, false).is_live("tuni-no-such-alias"));
    }

    #[test]
    fn a_forward_tuni_opens_survives_being_written_down() {
        let mut notes = Notes::default();
        notes.set(
            "web",
            Meta {
                forwards: vec![
                    Forward::parse(Direction::Local, "8080:localhost:80").expect("local"),
                    Forward::parse(Direction::Dynamic, "1080").expect("dynamic"),
                ],
                ..Meta::default()
            },
        );
        let text = serde_json::to_string(&notes).expect("write");
        let read: Notes = serde_json::from_str(&text).expect("read");
        let forwards = read.get("web").forwards;
        assert_eq!(forwards[0].spec(), "8080:localhost:80");
        assert_eq!(forwards[1].direction, Direction::Dynamic);
        assert_eq!(forwards[1].spec(), "1080");
    }

    #[test]
    fn a_port_somebody_else_is_listening_on_is_refused_by_name() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let port = held.local_addr().expect("address").port();
        let taken = Forward {
            direction: Direction::Local,
            listen_port: port,
            host: "localhost".to_owned(),
            port: 80,
            ..Forward::default()
        };
        // This process is the one holding it, so the message has to name this
        // process: that naming is the whole point of checking first.
        let refused = check_port(&taken).expect_err("in use");
        assert!(
            refused.contains(&format!("Port {port} is already in use")),
            "{refused}"
        );
        assert!(
            refused.contains(&format!("(pid {})", std::process::id())),
            "{refused}"
        );

        // The far end's ports are the far end's business, and a forward that
        // asks for whichever port is free cannot be checked at all.
        drop(held);
        assert!(check_port(&taken).is_ok());
        assert!(
            check_port(&Forward {
                direction: Direction::Remote,
                ..taken
            })
            .is_ok()
        );
    }

    #[test]
    fn a_remote_forward_reports_the_port_it_was_given() {
        assert_eq!(
            allocated_port("Allocated port 34567 for remote forward to localhost:5432\n"),
            Some(34567)
        );
        assert_eq!(allocated_port("Master running (pid=12345)"), None);
    }

    #[test]
    fn a_master_is_read_out_of_the_line_it_answers_a_check_with() {
        assert_eq!(master_pid("Master running (pid=12345)\r\n"), Some(12345));
        assert_eq!(master_pid("Control socket connect: No such file"), None);
    }

    #[test]
    fn only_a_name_ssh_itself_would_have_chosen_is_a_socket_of_ours() {
        assert!(hashed("7beec8e1563d1471b67ad65b8a4db8b98b9f780b"));
        assert!(!hashed("7beec8e1563d1471b67ad65b8a4db8b98b9f780b.4ab1c9"));
        assert!(!hashed("prod-db"));
    }

    #[test]
    fn a_sweep_takes_the_dead_socket_and_nothing_else() {
        use std::os::unix::net::{UnixListener, UnixStream};

        let directory = tempdir();
        let name = |character: &str| directory.join(character.repeat(40));

        let dead = name("a");
        drop(UnixListener::bind(&dead).expect("bind"));
        // Another test forking while that socket was open leaves a child
        // holding a copy of it until it execs, and a socket somebody holds is
        // one somebody answers on. Wait for the kernel to agree it is gone.
        for _ in 0..1000 {
            if UnixStream::connect(&dead).is_err() {
                break;
            }
            std::thread::yield_now();
        }
        let alive = name("b");
        let listener = UnixListener::bind(&alive).expect("bind");
        let regular = name("c");
        fs::write(&regular, "not a socket").expect("write");
        // What a master looks like in the moment between binding its socket and
        // moving it to the name it will be found under.
        let starting = directory.join(format!("{}.4ab1c9", "d".repeat(40)));
        drop(UnixListener::bind(&starting).expect("bind"));

        Control::at(directory.clone(), CONTROL_PERSIST, true).sweep();

        assert!(!dead.exists(), "a socket with nothing behind it survived");
        assert!(alive.exists(), "a live master lost its socket");
        assert!(regular.exists(), "a file that is not a socket was unlinked");
        assert!(starting.exists(), "a master starting up lost its socket");

        drop(listener);
        let _ = fs::remove_dir_all(&directory);
    }
}
