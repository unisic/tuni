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

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

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
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
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
}

/// One forwarded port.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Forward {
    pub direction: Direction,
    /// What is listened on. Empty means localhost for `Local` and `Dynamic`,
    /// and whatever `GatewayPorts` says for `Remote`.
    pub bind: String,
    /// Zero asks the far end to allocate one, which only `Remote` can do.
    pub listen_port: u16,
    /// Empty for `Dynamic`, which has no far end until something connects.
    pub host: String,
    pub port: u16,
    pub label: String,
}

impl Forward {
    /// The argument that follows the flag: `8080:localhost:80`, `1080`.
    #[must_use]
    pub fn spec(&self) -> String {
        let listen = if self.bind.is_empty() {
            self.listen_port.to_string()
        } else {
            format!("{}:{}", literal(&self.bind), self.listen_port)
        };
        match self.direction {
            Direction::Dynamic => listen,
            _ => format!("{listen}:{}:{}", literal(&self.host), self.port),
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

/// Everything `ssh` decides an alias means, one lowercase keyword per line,
/// repeated for the keywords that may appear more than once and simply absent
/// for the ones nothing set.
///
/// Empty when `ssh` could not be run at all. It exits 0 even for an alias
/// nothing defines, because a name nothing defines is a hostname.
#[must_use]
pub fn describe(alias: &str) -> Vec<(String, String)> {
    // `--` because an alias may begin with a dash.
    let output = run(&["-G", "--", alias]);
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
    fn ssh_says_what_an_alias_resolves_to() {
        // Whatever this machine's configuration says, an undefined alias is its
        // own hostname on port 22. `ssh -G` answers for one without connecting.
        let host = resolve("tuni-no-such-alias").expect("ssh -G");
        assert_eq!(host.hostname, "tuni-no-such-alias");
        assert_eq!(host.port, 22);
        assert!(!host.user.is_empty());
    }
}
