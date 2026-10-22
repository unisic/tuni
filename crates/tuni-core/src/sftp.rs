//! A small SFTP client, speaking the binary protocol to `ssh`'s own subsystem.
//!
//! `ssh -s <host> sftp` over a connection that is already authenticated opens a
//! channel and asks nothing, which is the whole reason this exists: the file
//! list has to reach the same machine the pane beside it is on, through the same
//! shared master, without a second login and without an SSH library holding a
//! private key inside this process.
//!
//! Version 3, which is what every server speaks and what OpenSSH's own client
//! negotiates. The alternative was driving `sftp -b -` and reading what it
//! prints, and it loses on facts rather than taste: a filename containing a
//! newline cannot be represented, `ls` expands globs with no way to turn it off
//! so a directory named `report[2024]` is unreachable, the batch mode gives up
//! on the first failure, and there is no progress at all when stdout is not a
//! terminal. Here names are bytes, attributes are integers, and nothing is
//! parsed out of prose.
//!
//! There is no timer in here. Every call blocks until it is answered, so the
//! caller belongs off the main thread, and a connection that has died is noticed the
//! way it always is: the master gives up on its own `ServerAlive` count, the
//! pipe closes, and the next read ends short.

use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::process::{Child, ChildStderr, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::editor::temporary_name;
use crate::files::Failure;

/// The protocol version this speaks and the only one it accepts.
const VERSION: u32 = 3;

/// What one packet may claim to be, so a length read off a broken pipe cannot
/// ask for a gigabyte. A directory listing is the biggest thing that arrives
/// and servers already cut those into several packets.
const MAX_PACKET: u32 = 4 * 1024 * 1024;

// Requests.
const INIT: u8 = 1;
const OPEN: u8 = 3;
const READ: u8 = 5;
const WRITE: u8 = 6;
const OPENDIR: u8 = 11;
const READDIR: u8 = 12;
const REMOVE: u8 = 13;
const MKDIR: u8 = 14;
const RMDIR: u8 = 15;
const REALPATH: u8 = 16;
const STAT: u8 = 17;
const RENAME: u8 = 18;
const LSTAT: u8 = 7;
const CLOSE: u8 = 4;
const FSETSTAT: u8 = 10;
/// A request named by a string rather than a number, for what a server offers
/// past version 3.
const EXTENSION: u8 = 200;

/// OpenSSH's rename that replaces a taken name in one step, the way
/// `rename(2)` does, where version 3's own leaves it to the server and
/// OpenSSH's refuses.
const POSIX_RENAME: &str = "posix-rename@openssh.com";

// Replies.
const REPLY_VERSION: u8 = 2;
const REPLY_STATUS: u8 = 101;
const REPLY_HANDLE: u8 = 102;
const REPLY_DATA: u8 = 103;
const REPLY_NAME: u8 = 104;
const REPLY_ATTRS: u8 = 105;

// What a file is opened for. Reading and writing are the only two, and the
// three that come with writing are what "upload this file" means: make it if it
// is not there, and start from nothing if it is.
const READABLE: u32 = 0x0000_0001;
const WRITABLE: u32 = 0x0000_0002;
const CREATE: u32 = 0x0000_0008;
const TRUNCATE: u32 = 0x0000_0010;

/// How much of a file moves in one request, which is what OpenSSH's own client
/// asks for.
const CHUNK: usize = 32 * 1024;

/// How many of those a transfer keeps in flight, OpenSSH's default as well. One
/// at a time made the round trip the speed limit, 640 KB/s across a 50 ms link
/// however fast the link was; 64 is 2 MiB on the wire, which a 50 ms link fills
/// at 40 MB/s. None of it is held here: the requests are a few bytes each on
/// the way down, and the answers are read one at a time as they arrive.
const WINDOW: usize = 64;

// The status codes worth telling apart. The rest are one failure.
const OK: u32 = 0;
const EOF: u32 = 1;
const NO_SUCH_FILE: u32 = 2;
const PERMISSION_DENIED: u32 = 3;

// Which attributes a server bothered to send.
const SIZE: u32 = 0x0000_0001;
const UIDGID: u32 = 0x0000_0002;
const PERMISSIONS: u32 = 0x0000_0004;
const ACMODTIME: u32 = 0x0000_0008;
const EXTENDED: u32 = 0x8000_0000;

/// The file type bits of a mode, and the two types a listing has to tell apart.
const TYPE: u32 = 0o170_000;
const DIRECTORY: u32 = 0o040_000;
const LINK: u32 = 0o120_000;

/// One name in a directory, with what the server said about it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Entry {
    /// As the server spelled it. Bytes with no defined encoding, so anything
    /// that is not UTF-8 comes through lossily and is shown rather than opened.
    pub name: String,
    pub size: u64,
    /// The permission bits and the file type, or zero from a server that did
    /// not send them.
    pub mode: u32,
    /// Seconds since the epoch, which is all version 3 has.
    pub mtime: u64,
    pub is_directory: bool,
    pub is_link: bool,
}

/// A connection to one host's sftp subsystem.
///
/// Every call blocks until it is answered, and a transfer keeps up to
/// [`WINDOW`] requests in flight on the way. The child is killed when this is dropped,
/// which is what closes the channel; the shared master it went over stays up.
pub struct Session {
    child: Option<Child>,
    out: Box<dyn Write + Send>,
    input: BufReader<Box<dyn Read + Send>>,
    next: u32,
    failure: Option<Failure>,
    /// The status code of the last refusal, for the one caller that acts on
    /// which code it was rather than only on the sentence.
    refusal: Option<u32>,
    /// Raised when the pipe broke or the far end lost the thread, after which
    /// nothing sent on this session will be answered.
    lost: bool,
    /// The extensions the server named when it said hello.
    extensions: Vec<String>,
}

impl Session {
    /// Starts `argv` and agrees a version with it.
    ///
    /// The argv is built by the caller because it is the same `ssh` invocation
    /// as a pane's, options and all: the point is to land on the machine the
    /// pane is on, over the connection it already opened.
    ///
    /// A session that does not start says why, from what `ssh` wrote on its
    /// way out: a refused key, a host key nobody has accepted and a server
    /// with no SFTP in it are three different things to do next.
    pub fn open(argv: &[String]) -> Result<Self, Failure> {
        let unstarted = |detail: String| Failure::new("Couldn't start a file transfer.", detail);
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| unstarted("There is no command to run.".to_owned()))?;
        let mut child = Command::new(program)
            .args(args)
            // The environment every background ssh here runs in, for the
            // reasons `ssh::run` gives: no askpass dialog out of nowhere, and
            // error text that can be read.
            .env("SSH_ASKPASS_REQUIRE", "never")
            .env("LC_ALL", "C")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| unstarted(error.to_string()))?;
        let (Some(out), Some(input), Some(mut errors)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(unstarted("Its pipes did not open.".to_owned()));
        };
        let mut session = Self {
            child: Some(child),
            out: Box::new(out),
            input: BufReader::new(Box::new(input)),
            next: 1,
            failure: None,
            refusal: None,
            lost: false,
            extensions: Vec::new(),
        };
        if session.handshake() {
            // Read for as long as the pipe is open and thrown away: ssh stops
            // dead once a pipe nobody reads is full, and a `LogLevel` turned
            // up in somebody's config fills one. One small thread per
            // session, gone when the pipe closes.
            std::thread::spawn(move || std::io::copy(&mut errors, &mut std::io::sink()));
            return Ok(session);
        }
        // Gone before it answered, or answering something else. Reaped, it has
        // said everything it was going to.
        if let Some(mut child) = session.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let said = waiting(&mut errors);
        if said.trim().is_empty() {
            return Err(session
                .failure
                .take()
                .unwrap_or_else(|| unstarted("The sftp subsystem did not answer.".to_owned())));
        }
        Err(explain(&said))
    }

    /// Where a path really is, with `~`, `.` and symlinks resolved by the far
    /// end. What a browser opens on, since an sftp session starts in a home
    /// directory only the server can name.
    pub fn realpath(&mut self, path: &str) -> Option<String> {
        let id = self.request(REALPATH, &encode_string(path))?;
        let (kind, body) = self.reply(id)?;
        if kind != REPLY_NAME {
            self.blame(kind, &body, "read", path);
            return None;
        }
        let mut reader = Reader::new(&body);
        reader.u32()?;
        let name = reader.string()?;
        Some(String::from_utf8_lossy(&name).into_owned())
    }

    /// Everything in a directory, `.` and `..` left out.
    ///
    /// One `OPENDIR` and then `READDIR` until the server says there is no more,
    /// which is how the protocol spells the end of a listing: a status packet
    /// carrying `EOF` rather than an empty answer. A connection that ends
    /// before that is no listing at all, since half a directory drawn as the
    /// whole of it is a folder that looks emptier than it is.
    pub fn list(&mut self, path: &str) -> Option<Vec<Entry>> {
        let handle = self.opendir(path)?;
        let mut entries = Vec::new();
        loop {
            let id = self.request(READDIR, &encode_string_bytes(&handle))?;
            let (kind, body) = self.reply(id)?;
            if kind == REPLY_STATUS {
                let mut reader = Reader::new(&body);
                if reader.u32() != Some(EOF) {
                    self.blame(kind, &body, "read", path);
                    self.close(&handle);
                    return None;
                }
                break;
            }
            if kind != REPLY_NAME {
                self.blame(kind, &body, "read", path);
                self.close(&handle);
                return None;
            }
            let mut reader = Reader::new(&body);
            let count = reader.u32()?;
            for _ in 0..count {
                let name = reader.string()?;
                // The long name is the server's own `ls -l` line. Nothing here
                // reads it: everything it says is in the attributes as numbers.
                reader.string()?;
                let mut entry = reader.attributes()?;
                entry.name = String::from_utf8_lossy(&name).into_owned();
                // A name is one component, and a server that sends `../x` or
                // `a/b` is either broken or hoping something here joins it
                // onto a path and walks out of the directory. Neither is
                // listed.
                if !matches!(entry.name.as_str(), "" | "." | "..")
                    && !entry.name.contains(['/', '\0'])
                {
                    entries.push(entry);
                }
            }
        }
        self.close(&handle);
        Some(entries)
    }

    /// One file, with symlinks followed, which is what decides whether a link
    /// opens as a directory.
    pub fn stat(&mut self, path: &str) -> Option<Entry> {
        self.attributes(STAT, path)
    }

    /// The same without following, so a link is a link.
    pub fn lstat(&mut self, path: &str) -> Option<Entry> {
        self.attributes(LSTAT, path)
    }

    /// Copies a remote file to `local`, calling `progress` with how much has
    /// arrived and how much there is. `progress` answering `false` stops the
    /// transfer where it stands, and the failure says cancelled.
    ///
    /// The bytes land beside `local` under a dotted name and are moved onto it
    /// once they are whole, which is how the editor saves: a transfer that dies
    /// leaves nothing under a name something else would open, and a file that
    /// was already there stays what it was.
    pub fn get(
        &mut self,
        remote: &str,
        local: &Path,
        progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Option<()> {
        let found = self.stat(remote);
        let total = found.as_ref().map_or(0, |entry| entry.size);
        // Zero from a server that did not say.
        let changed = found
            .filter(|entry| entry.mtime != 0)
            .map(|entry| UNIX_EPOCH + std::time::Duration::from_secs(entry.mtime));
        let handle = self.open_file(remote, READABLE, None)?;
        let part = local.with_file_name(temporary_name(local));
        let file = match File::create(&part) {
            Ok(file) => file,
            Err(error) => {
                self.failure = Some(Failure::new(
                    format!("Couldn't write {}.", local.display()),
                    error.to_string(),
                ));
                self.close(&handle);
                return None;
            }
        };
        let done = self.download(&handle, &file, remote, total, progress);
        self.close(&handle);
        if done.is_some()
            && let Some(changed) = changed
        {
            // When it was last changed over there, which is what a file
            // manager's copy keeps too. Not worth failing for: the bytes are
            // what was asked for, and they are right.
            let _ = file.set_modified(changed);
        }
        let landed = match done.map(|()| std::fs::rename(&part, local)) {
            Some(Ok(())) => Some(()),
            Some(Err(error)) => {
                self.failure = Some(Failure::new(
                    format!("Couldn't finish {}.", local.display()),
                    error.to_string(),
                ));
                None
            }
            None => None,
        };
        if landed.is_none() {
            // Dead, cancelled or refused, and nothing is coming back for the
            // half that arrived. The failure already says why; a temporary
            // that will not go is not a second thing worth saying.
            let _ = std::fs::remove_file(&part);
        }
        landed
    }

    /// Copies a local file to `remote`, calling `progress` with how much has
    /// gone and how much there is. `progress` answering `false` stops the
    /// transfer where it stands.
    ///
    /// The bytes land beside `remote` under a dotted name and replace it only
    /// once they are whole, so a cancelled or broken upload leaves whatever was
    /// there exactly as it was rather than cut short under its own name. A new
    /// file arrives with the permission bits it has here, which is what the
    /// `sftp` program does too: a script that is executable on this machine is
    /// executable on the other one. A file it replaces keeps the bits it had
    /// over there, which is what writing into it would have done; its owner
    /// becomes the account this logged in as, the one thing a rename cannot
    /// carry across. A symlink is followed to what it points at rather than
    /// replaced by a file.
    pub fn put(
        &mut self,
        local: &Path,
        remote: &str,
        progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Option<()> {
        let file = match File::open(local) {
            Ok(file) => file,
            Err(error) => {
                self.failure = Some(Failure::new(
                    format!("Couldn't read {}.", local.display()),
                    error.to_string(),
                ));
                return None;
            }
        };
        let data = file.metadata().ok();
        let total = data.as_ref().map_or(0, std::fs::Metadata::len);
        let times = data.as_ref().and_then(times);
        let here = data.map_or(0o644, |data| {
            std::os::unix::fs::PermissionsExt::mode(&data.permissions()) & 0o777
        });

        // Asked with the failure kept, since a name that is not there yet is
        // the usual answer rather than something that went wrong.
        let found = self.probe(|session| session.lstat(remote));
        if found.as_ref().is_some_and(|entry| entry.is_directory) {
            self.failure = Some(Failure::new(
                format!("Couldn't write {remote}."),
                "There is a directory there with that name.",
            ));
            return None;
        }
        let (target, found) = match found {
            Some(entry) if entry.is_link => match self.probe(|session| session.realpath(remote)) {
                Some(real) => {
                    let found = self.probe(|session| session.stat(&real));
                    (real, found)
                }
                // Somewhere the far end cannot name, so the link is written
                // through, which is what opening it does.
                None => return self.overwrite(file, local, remote, total, here, progress),
            },
            found => (remote.to_owned(), found),
        };
        let kept = found
            .as_ref()
            .map(|entry| entry.mode & 0o777)
            .filter(|mode| *mode != 0);

        let part = beside(&target);
        self.refusal = None;
        let Some(handle) = self.open_file(
            &part,
            WRITABLE | CREATE | TRUNCATE,
            Some(kept.unwrap_or(here)),
        ) else {
            // A directory this account may not write into can still hold a
            // file it may write to, so that file is written where it stands.
            // It costs what the temporary exists to prevent, a cancel there
            // leaves the file cut short, and it is the only way such a file
            // can be sent at all.
            if found.is_some() && self.refusal == Some(PERMISSION_DENIED) {
                return self.overwrite(file, local, remote, total, here, progress);
            }
            // Named as the file that was asked for. The temporary is this
            // client's business and nobody asked to write it.
            if let Some(failure) = self.failure.as_mut() {
                failure.message = format!("Couldn't write {remote}.");
            }
            return None;
        };
        let done = self.upload(&handle, file, local, remote, total, progress);
        if done.is_some() {
            // The mode went through the far end's umask on the way in, so a
            // file that was group-writable would come back without it.
            self.stamp(&handle, &part, kept, times);
        }
        self.close(&handle);
        if done.is_none() {
            // The one thing to sweep is the temporary. On a connection that
            // died this cannot reach it either, and a dotted name is what is
            // left over there rather than half of the real one.
            self.probe(|session| session.remove(&part));
            return None;
        }
        self.replace(&part, &target, found.is_some())
    }

    /// Straight into `remote`, cut to nothing first. What [`Self::put`] falls
    /// back to when there is nowhere beside the file to write.
    fn overwrite(
        &mut self,
        file: File,
        local: &Path,
        remote: &str,
        total: u64,
        mode: u32,
        progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Option<()> {
        let times = file.metadata().ok().as_ref().and_then(times);
        let handle = self.open_file(remote, WRITABLE | CREATE | TRUNCATE, Some(mode))?;
        let done = self.upload(&handle, file, local, remote, total, progress);
        if done.is_some() {
            self.stamp(&handle, remote, None, times);
        }
        self.close(&handle);
        done
    }

    /// Sets on an open handle what the bytes do not carry: a mode, past the
    /// far end's umask, and the access and change times the file has here,
    /// which is what a file manager's copy keeps. Not fatal when refused: the
    /// bytes are the thing that was asked for, and they are right.
    fn stamp(&mut self, handle: &[u8], path: &str, mode: Option<u32>, times: Option<(u32, u32)>) {
        let flags = mode.map_or(0, |_| PERMISSIONS) | times.map_or(0, |_| ACMODTIME);
        if flags == 0 {
            return;
        }
        let mut request = encode_string_bytes(handle);
        request.extend_from_slice(&flags.to_be_bytes());
        if let Some(mode) = mode {
            request.extend_from_slice(&mode.to_be_bytes());
        }
        if let Some((accessed, changed)) = times {
            request.extend_from_slice(&accessed.to_be_bytes());
            request.extend_from_slice(&changed.to_be_bytes());
        }
        self.probe(|session| session.settle(FSETSTAT, &request, "write", path));
    }

    /// Moves a finished upload onto the name it was for.
    ///
    /// A file that is there is replaced in one step where the server offers
    /// [`POSIX_RENAME`]. Elsewhere version 3 leaves a rename onto a taken name
    /// to the server and OpenSSH's refuses, so the file goes first, and between
    /// the two there is a moment with nothing under the name. A name that was
    /// free gets the plain rename either way, which refuses rather than
    /// replaces whatever appeared there in the meantime. The new bytes are
    /// whole by then, so when the rename is what fails they stay where they
    /// landed and the failure says where.
    fn replace(&mut self, part: &str, target: &str, taken: bool) -> Option<()> {
        let moved = if taken && self.offers(POSIX_RENAME) {
            let mut request = encode_string(POSIX_RENAME);
            request.extend_from_slice(&encode_string(part));
            request.extend_from_slice(&encode_string(target));
            self.settle(EXTENSION, &request, "replace", target)
        } else {
            if taken && self.remove(target).is_none() {
                if let Some(failure) = self.failure.as_mut() {
                    failure.message = format!("Couldn't replace {target}.");
                }
                self.probe(|session| session.remove(part));
                return None;
            }
            self.rename(part, target)
        };
        if moved.is_some() {
            return Some(());
        }
        if let Some(failure) = self.failure.as_mut() {
            failure.message = format!("Couldn't replace {target}.");
            failure.detail = format!("The new copy arrived whole and is at {part}.");
        }
        None
    }

    /// Makes one directory, which the far end refuses if it is already there.
    pub fn mkdir(&mut self, path: &str) -> Option<()> {
        let mut request = encode_string(path);
        // No attributes, so the directory gets the mode the server's umask
        // gives it, the same as one made by a shell over there.
        request.extend_from_slice(&0_u32.to_be_bytes());
        if self.settle(MKDIR, &request, "make", path).is_some() {
            return Some(());
        }
        if self.probe(|session| session.lstat(path)).is_some() {
            self.because("There is already something there with that name.");
        }
        None
    }

    /// Deletes one file. There is no trash at the far end and nothing here
    /// invents one.
    pub fn remove(&mut self, path: &str) -> Option<()> {
        if self
            .settle(REMOVE, &encode_string(path), "delete", path)
            .is_some()
        {
            return Some(());
        }
        if self
            .probe(|session| session.lstat(path))
            .is_some_and(|entry| entry.is_directory)
        {
            self.because("That is a directory, not a file.");
        }
        None
    }

    /// Deletes one directory, which the far end refuses unless it is empty.
    pub fn rmdir(&mut self, path: &str) -> Option<()> {
        if self
            .settle(RMDIR, &encode_string(path), "delete", path)
            .is_some()
        {
            return Some(());
        }
        if self
            .probe(|session| session.list(path))
            .is_some_and(|entries| !entries.is_empty())
        {
            self.because("It still has something in it, and only an empty directory can go.");
        }
        None
    }

    /// Renames one path to another.
    ///
    /// Version 3 leaves it to the server whether an existing name is replaced,
    /// and OpenSSH's refuses, which is the behaviour worth having: a rename
    /// cannot quietly delete something.
    pub fn rename(&mut self, from: &str, to: &str) -> Option<()> {
        let mut request = encode_string(from);
        request.extend_from_slice(&encode_string(to));
        self.settle(RENAME, &request, "rename", from)
    }

    /// Why the last call failed, in the two parts a dialog wants.
    #[must_use]
    pub fn failure(&self) -> Option<&Failure> {
        self.failure.as_ref()
    }

    /// Whether the connection under this session has gone, so the next thing
    /// to do is open another rather than ask this one again.
    #[must_use]
    pub fn is_lost(&self) -> bool {
        self.lost
    }

    /// Asks the far end one more question with the failure that prompted it
    /// kept, since every call here writes over that.
    fn probe<T>(&mut self, ask: impl FnOnce(&mut Self) -> Option<T>) -> Option<T> {
        let failure = self.failure.take();
        let answer = ask(self);
        self.failure = failure;
        answer
    }

    /// Says why a refusal happened, once something has been asked that knows.
    ///
    /// Version 3 has no status code for "it is already there" or "it is not
    /// empty": both arrive as the same bare failure. One extra round trip on
    /// the failure path buys the sentence a person can act on.
    fn because(&mut self, detail: &str) {
        if let Some(failure) = self.failure.as_mut() {
            failure.detail = detail.to_owned();
        }
    }

    /// One request whose whole answer is whether it worked.
    fn settle(&mut self, kind: u8, request: &[u8], verb: &str, path: &str) -> Option<()> {
        let id = self.request(kind, request)?;
        let (reply, body) = self.reply(id)?;
        if reply != REPLY_STATUS || Reader::new(&body).u32() != Some(OK) {
            self.blame(reply, &body, verb, path);
            return None;
        }
        Some(())
    }

    fn attributes(&mut self, kind: u8, path: &str) -> Option<Entry> {
        let id = self.request(kind, &encode_string(path))?;
        let (reply, body) = self.reply(id)?;
        if reply != REPLY_ATTRS {
            self.blame(reply, &body, "read", path);
            return None;
        }
        let mut entry = Reader::new(&body).attributes()?;
        entry.name = Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        Some(entry)
    }

    fn handshake(&mut self) -> bool {
        let mut body = Vec::new();
        body.extend_from_slice(&VERSION.to_be_bytes());
        if self.send(INIT, &body).is_none() {
            return false;
        }
        let Some((kind, body)) = self.packet() else {
            self.failure = Some(Failure::new(
                "Couldn't start a file transfer.",
                "The sftp subsystem did not answer.",
            ));
            return false;
        };
        // A server may answer with a version below ours and every one of them
        // answers with three, since three is where the protocol stopped being
        // the same protocol.
        let mut reader = Reader::new(&body);
        if kind != REPLY_VERSION || reader.u32() != Some(VERSION) {
            self.failure = Some(Failure::new(
                "Couldn't start a file transfer.",
                "The far end does not speak SFTP version 3.".to_owned(),
            ));
            return false;
        }
        // The rest is name and version pairs. Only the names are kept, since
        // every extension used here has had one version.
        while let (Some(name), Some(_)) = (reader.string(), reader.string()) {
            self.extensions
                .push(String::from_utf8_lossy(&name).into_owned());
        }
        true
    }

    fn offers(&self, extension: &str) -> bool {
        self.extensions.iter().any(|name| name == extension)
    }

    fn opendir(&mut self, path: &str) -> Option<Vec<u8>> {
        let id = self.request(OPENDIR, &encode_string(path))?;
        let (kind, body) = self.reply(id)?;
        if kind != REPLY_HANDLE {
            self.blame(kind, &body, "read", path);
            return None;
        }
        Reader::new(&body).string()
    }

    /// Opens one file and answers with the handle the far end gave it.
    fn open_file(&mut self, path: &str, flags: u32, mode: Option<u32>) -> Option<Vec<u8>> {
        let mut request = encode_string(path);
        request.extend_from_slice(&flags.to_be_bytes());
        match mode {
            Some(mode) => {
                request.extend_from_slice(&PERMISSIONS.to_be_bytes());
                request.extend_from_slice(&mode.to_be_bytes());
            }
            // No attributes, which is what opening something that already
            // exists asks for: leave it as whoever made it left it.
            None => request.extend_from_slice(&0_u32.to_be_bytes()),
        }
        let id = self.request(OPEN, &request)?;
        let (kind, body) = self.reply(id)?;
        if kind != REPLY_HANDLE {
            let verb = if flags & WRITABLE == 0 {
                "read"
            } else {
                "write"
            };
            self.blame(kind, &body, verb, path);
            return None;
        }
        Reader::new(&body).string()
    }

    /// The body of a download, split out so the handle is closed on every way
    /// out of it.
    ///
    /// The window opens one read at a time and grows by one with each read
    /// answered in full, which is how OpenSSH's client starts too: a small file
    /// costs the round trips it always did, and a short answer, usually the
    /// tail of the file, asks for nothing past it. Answers may arrive in any order,
    /// so each lands at its own offset. A refusal or a cancel stops new reads
    /// and still waits for the ones in flight, which is what lets the session
    /// carry on afterwards rather than meet their answers as a lost thread.
    fn download(
        &mut self,
        handle: &[u8],
        file: &File,
        remote: &str,
        total: u64,
        progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Option<()> {
        let mut flight: Vec<(u32, u64, u32)> = Vec::with_capacity(WINDOW);
        let mut window = 1;
        let mut next = 0_u64;
        let mut at = 0_u64;
        let mut ended = false;
        let mut stopped = false;
        if !progress(0, total) {
            return self.cancelled();
        }
        loop {
            while !ended && flight.len() < window {
                let id = self.read_request(handle, next, CHUNK as u32)?;
                flight.push((id, next, CHUNK as u32));
                next += CHUNK as u64;
            }
            if flight.is_empty() {
                break;
            }
            let ((offset, length), kind, body) = self.answer_in(&mut flight)?;
            if stopped {
                continue;
            }
            // The end of a file is a status packet, the way the end of a
            // listing is. Reads already past it get the same answer.
            if kind == REPLY_STATUS && Reader::new(&body).u32() == Some(EOF) {
                ended = true;
                continue;
            }
            let Some(mut chunk) = (kind == REPLY_DATA)
                .then(|| Reader::new(&body).string())
                .flatten()
            else {
                self.blame(kind, &body, "read", remote);
                (ended, stopped) = (true, true);
                continue;
            };
            if chunk.is_empty() {
                ended = true;
                continue;
            }
            // More than was asked for would land on the next read's bytes.
            chunk.truncate(length as usize);
            if let Err(error) = file.write_all_at(&chunk, offset) {
                self.wrote(&error.to_string());
                (ended, stopped) = (true, true);
                continue;
            }
            let got = chunk.len() as u32;
            at += u64::from(got);
            // Less than was asked for means nothing about the end of the file,
            // only about the server, so the rest of the span is asked again.
            if got < length {
                let id = self.read_request(handle, offset + u64::from(got), length - got)?;
                flight.push((id, offset + u64::from(got), length - got));
            } else {
                window = (window + 1).min(WINDOW);
            }
            if !progress(at, total.max(at)) {
                self.cancelled();
                (ended, stopped) = (true, true);
            }
        }
        (!stopped).then_some(())
    }

    /// The body of an upload, closed over the same way, with the window full
    /// from the start: the size is known here, so there is nothing to find out
    /// by asking slowly. The same waiting on whatever is in flight when it
    /// stops, for the same reason.
    fn upload(
        &mut self,
        handle: &[u8],
        mut file: File,
        local: &Path,
        remote: &str,
        total: u64,
        progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Option<()> {
        let mut buffer = vec![0_u8; CHUNK];
        let mut flight: Vec<(u32, u64, u32)> = Vec::with_capacity(WINDOW);
        let mut next = 0_u64;
        let mut at = 0_u64;
        let mut ended = false;
        let mut stopped = false;
        if !progress(0, total) {
            return self.cancelled();
        }
        loop {
            while !ended && flight.len() < WINDOW {
                let read = match file.read(&mut buffer) {
                    Ok(0) => {
                        ended = true;
                        break;
                    }
                    Ok(read) => read,
                    Err(error) => {
                        self.failure = Some(Failure::new(
                            format!("Couldn't read {}.", local.display()),
                            error.to_string(),
                        ));
                        (ended, stopped) = (true, true);
                        break;
                    }
                };
                let mut request = encode_string_bytes(handle);
                request.extend_from_slice(&next.to_be_bytes());
                request.extend_from_slice(&encode_string_bytes(&buffer[..read]));
                let id = self.request(WRITE, &request)?;
                flight.push((id, next, read as u32));
                next += read as u64;
            }
            if flight.is_empty() {
                break;
            }
            let ((_, length), kind, body) = self.answer_in(&mut flight)?;
            if stopped {
                continue;
            }
            if kind != REPLY_STATUS || Reader::new(&body).u32() != Some(OK) {
                self.blame(kind, &body, "write", remote);
                (ended, stopped) = (true, true);
                continue;
            }
            at += u64::from(length);
            if !progress(at, total.max(at)) {
                self.cancelled();
                (ended, stopped) = (true, true);
            }
        }
        (!stopped).then_some(())
    }

    /// The failure a refused `progress` callback turns into. A message all the
    /// same, because a transfer that returns `None` is expected to have one;
    /// the caller that cancelled knows it did and can choose not to show it.
    fn cancelled(&mut self) -> Option<()> {
        self.failure = Some(Failure::new(
            String::from("The transfer was cancelled."),
            String::new(),
        ));
        None
    }

    /// Asks for `length` bytes of `handle` from `offset`.
    fn read_request(&mut self, handle: &[u8], offset: u64, length: u32) -> Option<u32> {
        let mut request = encode_string_bytes(handle);
        request.extend_from_slice(&offset.to_be_bytes());
        request.extend_from_slice(&length.to_be_bytes());
        self.request(READ, &request)
    }

    /// The next answer to one of the requests in `flight`, taken off it with
    /// the offset and length it was for. `None` when the connection went, or
    /// the answer was to a request nobody made, which is a server that has lost
    /// the thread.
    fn answer_in(
        &mut self,
        flight: &mut Vec<(u32, u64, u32)>,
    ) -> Option<((u64, u32), u8, Vec<u8>)> {
        let (id, kind, body) = self.any_reply()?;
        let Some(index) = flight.iter().position(|(asked, ..)| *asked == id) else {
            self.lost("The far end answered a request nobody made.");
            return None;
        };
        let (_, offset, length) = flight.swap_remove(index);
        Some(((offset, length), kind, body))
    }

    fn close(&mut self, handle: &[u8]) {
        if let Some(id) = self.request(CLOSE, &encode_string_bytes(handle)) {
            self.reply(id);
        }
    }

    /// Sends one request and answers with the id it went out under.
    fn request(&mut self, kind: u8, body: &[u8]) -> Option<u32> {
        let id = self.next;
        self.next = self.next.wrapping_add(1).max(1);
        let mut framed = Vec::with_capacity(body.len() + 4);
        framed.extend_from_slice(&id.to_be_bytes());
        framed.extend_from_slice(body);
        self.send(kind, &framed)?;
        Some(id)
    }

    fn send(&mut self, kind: u8, body: &[u8]) -> Option<()> {
        let length = u32::try_from(body.len() + 1).ok()?;
        let mut packet = Vec::with_capacity(body.len() + 5);
        packet.extend_from_slice(&length.to_be_bytes());
        packet.push(kind);
        packet.extend_from_slice(body);
        match self.out.write_all(&packet).and_then(|()| self.out.flush()) {
            Ok(()) => Some(()),
            Err(error) => {
                self.lost(&error.to_string());
                None
            }
        }
    }

    /// The answer to one request, with the id checked off the front.
    ///
    /// Outside a transfer requests go out one at a time, so anything carrying
    /// another id is a server that has lost the thread and the session with it.
    fn reply(&mut self, id: u32) -> Option<(u8, Vec<u8>)> {
        let (answered, kind, body) = self.any_reply()?;
        if answered != id {
            self.lost("The far end answered a request nobody made.");
            return None;
        }
        Some((kind, body))
    }

    /// The next answer to anything, with the id it carries split off.
    fn any_reply(&mut self) -> Option<(u32, u8, Vec<u8>)> {
        let (kind, mut body) = self.packet()?;
        let Some(id) = Reader::new(&body).u32() else {
            self.lost("The far end sent an answer with no request in it.");
            return None;
        };
        body.drain(..4);
        Some((id, kind, body))
    }

    /// One packet off the wire: its type, and everything after it.
    fn packet(&mut self) -> Option<(u8, Vec<u8>)> {
        let mut header = [0_u8; 5];
        if let Err(error) = self.input.read_exact(&mut header) {
            self.lost(&error.to_string());
            return None;
        }
        let length = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        if length == 0 || length > MAX_PACKET {
            self.lost("The far end sent a packet of an impossible size.");
            return None;
        }
        let mut body = vec![0_u8; length as usize - 1];
        if let Err(error) = self.input.read_exact(&mut body) {
            self.lost(&error.to_string());
            return None;
        }
        Some((header[4], body))
    }

    /// Turns a status packet into something worth showing, and anything else
    /// into the fact that it was not what was asked for. `verb` is what was
    /// being done, since a refused upload is not a failed read.
    fn blame(&mut self, kind: u8, body: &[u8], verb: &str, path: &str) {
        if kind != REPLY_STATUS {
            self.failure = Some(Failure::new(
                format!("Couldn't {verb} {path}."),
                "The far end answered with something else.",
            ));
            return;
        }
        let mut reader = Reader::new(body);
        let code = reader.u32().unwrap_or(OK);
        self.refusal = Some(code);
        let message = reader
            .string()
            .map(|text| String::from_utf8_lossy(&text).into_owned())
            .filter(|text| !text.is_empty())
            // OpenSSH's server sends the status code's own name as the message,
            // so the general one says nothing the code did not. Dropping it
            // leaves room for a sentence.
            .filter(|text| !text.eq_ignore_ascii_case("failure"));
        let detail = match code {
            NO_SUCH_FILE => "There is nothing there.".to_owned(),
            PERMISSION_DENIED => {
                format!("The account this connection logged in as may not {verb} it.")
            }
            _ => message.unwrap_or_else(|| "The far end refused.".to_owned()),
        };
        self.failure = Some(Failure::new(format!("Couldn't {verb} {path}."), detail));
    }

    fn lost(&mut self, detail: &str) {
        self.lost = true;
        self.failure = Some(Failure::new("The connection ended.", detail));
    }

    /// A transfer that the far end was fine with and this machine was not.
    fn wrote(&mut self, detail: &str) {
        self.failure = Some(Failure::new("Couldn't write the file here.", detail));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A local file's access and change times in the seconds version 3 carries,
/// or nothing for one outside what 32 bits can say.
fn times(data: &std::fs::Metadata) -> Option<(u32, u32)> {
    let seconds = |time: SystemTime| {
        let since = time.duration_since(UNIX_EPOCH).ok()?;
        u32::try_from(since.as_secs()).ok()
    };
    Some((
        seconds(data.accessed().ok()?)?,
        seconds(data.modified().ok()?)?,
    ))
}

/// Whatever `ssh` left in its error pipe, up to 4 KiB, without waiting for
/// more. Non-blocking because the pipe can outlive the ssh that wrote to it: a
/// `ProxyCommand` inherits it and may hold it open long after.
fn waiting(errors: &mut ChildStderr) -> String {
    let fd = errors.as_raw_fd();
    // Safe: the flags of a descriptor this owns, and the kernel checks both
    // calls. Failing leaves it blocking, which a reaped ssh still ends.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    let mut said = Vec::new();
    // Ends on `WouldBlock` with what was there kept, which is the point.
    let _ = errors.take(4096).read_to_end(&mut said);
    String::from_utf8_lossy(&said).into_owned()
}

/// What `ssh` said when it gave up, turned into what to do about it.
///
/// Matched on the text because ssh has no other way to say it: its exit code
/// is 255 for every one of these. `LC_ALL=C` keeps the text the text, and a
/// line nothing here recognizes is shown as it is, since ssh's own sentence
/// beats a guess. The last line rather than the first, because the warnings
/// ssh prints on the way in come before the reason it stopped.
#[must_use]
pub fn explain(said: &str) -> Failure {
    let detail = if said.contains("Permission denied") {
        "Nothing here can answer a password or a passphrase, so the connection \
         has to be open already. Connect the host in a pane first."
            .to_owned()
    } else if said.contains("Host key verification failed") {
        "This machine does not know the host's key yet, or the key has changed. \
         Connect in a pane, where ssh can show it and ask."
            .to_owned()
    } else if said.contains("subsystem request failed") {
        "The far end has no SFTP server switched on.".to_owned()
    } else {
        said.lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .unwrap_or("The connection ended before the far end answered.")
            .to_owned()
    };
    Failure::new("Couldn't start a file transfer.", detail)
}

/// Where an unfinished copy of `path` is written: beside it, so it is on the
/// same filesystem and moves onto it in one rename.
fn beside(path: &str) -> String {
    let path = Path::new(path);
    path.with_file_name(temporary_name(path))
        .to_string_lossy()
        .into_owned()
}

/// A string as the protocol writes one: its length and then its bytes, with no
/// terminator and no encoding.
fn encode_string(text: &str) -> Vec<u8> {
    encode_string_bytes(text.as_bytes())
}

fn encode_string_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 4);
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
    out
}

/// A cursor over a packet body. Everything returns `None` rather than panicking:
/// the bytes came off a network and being short is one of the things they do.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(count)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn u32(&mut self) -> Option<u32> {
        let bytes = self.take(4)?;
        Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Option<u64> {
        let high = u64::from(self.u32()?);
        Some((high << 32) | u64::from(self.u32()?))
    }

    fn string(&mut self) -> Option<Vec<u8>> {
        let length = self.u32()? as usize;
        Some(self.take(length)?.to_vec())
    }

    /// The attribute block, whose fields are each there only if the flags say
    /// so. Skipping the ones nothing here uses is the whole reason the flags
    /// have to be read in order rather than looked up.
    fn attributes(&mut self) -> Option<Entry> {
        let flags = self.u32()?;
        let mut entry = Entry::default();
        if flags & SIZE != 0 {
            entry.size = self.u64()?;
        }
        if flags & UIDGID != 0 {
            self.u32()?;
            self.u32()?;
        }
        if flags & PERMISSIONS != 0 {
            entry.mode = self.u32()?;
            entry.is_directory = entry.mode & TYPE == DIRECTORY;
            entry.is_link = entry.mode & TYPE == LINK;
        }
        if flags & ACMODTIME != 0 {
            self.u32()?;
            entry.mtime = u64::from(self.u32()?);
        }
        if flags & EXTENDED != 0 {
            let count = self.u32()?;
            for _ in 0..count {
                self.string()?;
                self.string()?;
            }
        }
        Some(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    /// A packet, as the far end of the pipe reads and writes one.
    type Packet = (u8, Vec<u8>);

    /// A server that answers from a script of packets, so the client can be
    /// tested without a machine to log in to.
    fn serve(answers: Vec<Packet>) -> (Session, std::thread::JoinHandle<Vec<Packet>>) {
        let (ours, theirs) = UnixStream::pair().expect("a pipe");
        let far = std::thread::spawn(move || {
            let mut stream = theirs;
            let mut asked = Vec::new();
            for (kind, body) in answers {
                let mut header = [0_u8; 5];
                if stream.read_exact(&mut header).is_err() {
                    break;
                }
                let length =
                    u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
                let mut rest = vec![0_u8; length - 1];
                if stream.read_exact(&mut rest).is_err() {
                    break;
                }
                asked.push((header[4], rest));
                let mut packet = Vec::new();
                packet.extend_from_slice(&((body.len() + 1) as u32).to_be_bytes());
                packet.push(kind);
                packet.extend_from_slice(&body);
                if stream.write_all(&packet).is_err() {
                    break;
                }
            }
            asked
        });
        (session_on(ours), far)
    }

    fn session_on(stream: UnixStream) -> Session {
        Session {
            child: None,
            out: Box::new(stream.try_clone().expect("a writer")),
            input: BufReader::new(Box::new(stream)),
            next: 1,
            failure: None,
            refusal: None,
            lost: false,
            extensions: Vec::new(),
        }
    }

    /// A far end holding one file, `content` to read and whatever arrives to
    /// write, that answers everything asked together in the reverse of the
    /// order it was asked. Out of order is within the protocol, and this is
    /// how to be sure the client copes. Every third chunk is read short.
    ///
    /// Answers with the most requests that were ever waiting at once, and the
    /// bytes that were written.
    fn serve_file(content: Vec<u8>) -> (Session, std::thread::JoinHandle<(usize, Vec<u8>)>) {
        use std::io::{BufRead, ErrorKind};
        let (ours, theirs) = UnixStream::pair().expect("a pipe");
        let far = std::thread::spawn(move || {
            let mut out = theirs.try_clone().expect("a writer");
            let mut input = BufReader::new(theirs);
            let mut waiting: Vec<Vec<u8>> = Vec::new();
            let (mut widest, mut written) = (0, Vec::new());
            loop {
                // Whatever has arrived by the time the pipe goes quiet is one
                // batch. Waiting on the buffer rather than in a read means a
                // packet is never cut in half by the timeout.
                let quiet = Some(std::time::Duration::from_millis(20));
                input.get_ref().set_read_timeout(quiet).expect("a timeout");
                match input.fill_buf() {
                    Ok([]) => break,
                    Ok(_) => {}
                    Err(error)
                        if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                    {
                        widest = widest.max(waiting.len());
                        for packet in waiting.drain(..).rev() {
                            out.write_all(&packet).expect("an answer");
                        }
                        continue;
                    }
                    Err(_) => break,
                }
                input.get_ref().set_read_timeout(None).expect("no timeout");
                let mut header = [0_u8; 5];
                input.read_exact(&mut header).expect("a header");
                let length = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
                let mut rest = vec![0_u8; length as usize - 1];
                input.read_exact(&mut rest).expect("a request");
                let mut reader = Reader::new(&rest);
                let id = reader.u32().expect("an id");
                let (kind, answer) = match header[4] {
                    INIT => (REPLY_VERSION, VERSION.to_be_bytes().to_vec()),
                    STAT => {
                        let mut attributes = SIZE.to_be_bytes().to_vec();
                        attributes.extend_from_slice(&(content.len() as u64).to_be_bytes());
                        (REPLY_ATTRS, body(id, &attributes))
                    }
                    LSTAT => (REPLY_STATUS, body(id, &NO_SUCH_FILE.to_be_bytes())),
                    OPEN => (REPLY_HANDLE, body(id, &encode_string("h"))),
                    READ => {
                        reader.string().expect("a handle");
                        let at = reader.u64().expect("an offset") as usize;
                        let mut asked = reader.u32().expect("a length") as usize;
                        if (at / CHUNK) % 3 == 1 {
                            asked = asked.min(CHUNK / 2);
                        }
                        if at >= content.len() {
                            (REPLY_STATUS, body(id, &EOF.to_be_bytes()))
                        } else {
                            let end = content.len().min(at + asked);
                            (
                                REPLY_DATA,
                                body(id, &encode_string_bytes(&content[at..end])),
                            )
                        }
                    }
                    WRITE => {
                        reader.string().expect("a handle");
                        let at = reader.u64().expect("an offset") as usize;
                        let data = reader.string().expect("the bytes");
                        if written.len() < at + data.len() {
                            written.resize(at + data.len(), 0);
                        }
                        written[at..at + data.len()].copy_from_slice(&data);
                        (REPLY_STATUS, body(id, &OK.to_be_bytes()))
                    }
                    _ => (REPLY_STATUS, body(id, &OK.to_be_bytes())),
                };
                let mut packet = ((answer.len() + 1) as u32).to_be_bytes().to_vec();
                packet.push(kind);
                packet.extend_from_slice(&answer);
                waiting.push(packet);
            }
            (widest, written)
        });
        (session_on(ours), far)
    }

    fn a_megabyte() -> Vec<u8> {
        (0..1_000_003_u32).map(|at| (at % 251) as u8).collect()
    }

    #[test]
    fn a_download_keeps_many_reads_in_flight_and_lands_each_where_it_belongs() {
        let local = std::env::temp_dir().join(format!("tuni-sftp-many-{}", std::process::id()));
        let content = a_megabyte();
        let (mut session, far) = serve_file(content.clone());
        assert!(session.handshake());
        assert_eq!(session.get("/srv/big", &local, &mut |_, _| true), Some(()));
        assert!(std::fs::read(&local).expect("the file") == content);

        drop(session);
        let (widest, _) = far.join().expect("the far end");
        assert!(widest > 1, "never more than {widest} at once");
        let _ = std::fs::remove_file(&local);
    }

    #[test]
    fn an_upload_keeps_many_writes_in_flight_and_lands_each_where_it_belongs() {
        let local = std::env::temp_dir().join(format!("tuni-sftp-send-{}", std::process::id()));
        let content = a_megabyte();
        std::fs::write(&local, &content).expect("a file to send");
        let (mut session, far) = serve_file(Vec::new());
        assert!(session.handshake());
        assert_eq!(session.put(&local, "/srv/big", &mut |_, _| true), Some(()));

        drop(session);
        let (widest, written) = far.join().expect("the far end");
        assert!(written == content);
        assert!(widest > 1, "never more than {widest} at once");
        let _ = std::fs::remove_file(&local);
    }

    #[test]
    fn a_cancelled_download_waits_for_its_reads_and_the_session_carries_on() {
        let local = std::env::temp_dir().join(format!("tuni-sftp-halt-{}", std::process::id()));
        let (mut session, _far) = serve_file(a_megabyte());
        assert!(session.handshake());
        let enough = 4 * CHUNK as u64;
        assert!(
            session
                .get("/srv/big", &local, &mut |at, _| at < enough)
                .is_none()
        );
        assert!(!local.exists());
        // The reads still in flight were answered into the cancelled transfer
        // rather than into whatever was asked next.
        assert!(
            session.stat("/srv/big").is_some(),
            "{:?}",
            session.failure()
        );
        assert!(!session.is_lost());
    }

    /// A reply body: the id it answers, then the rest.
    fn body(id: u32, rest: &[u8]) -> Vec<u8> {
        let mut out = id.to_be_bytes().to_vec();
        out.extend_from_slice(rest);
        out
    }

    fn name_entry(name: &str, mode: u32, size: u64) -> Vec<u8> {
        let mut out = encode_string(name);
        out.extend_from_slice(&encode_string("-rw-r--r--  1 dean dean 4 Jul 27 00:00 x"));
        out.extend_from_slice(&(SIZE | PERMISSIONS).to_be_bytes());
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(&mode.to_be_bytes());
        out
    }

    #[test]
    fn a_version_the_client_does_not_speak_ends_the_session() {
        let mut version = Vec::new();
        version.extend_from_slice(&4_u32.to_be_bytes());
        let (mut session, _far) = serve(vec![(REPLY_VERSION, version)]);
        assert!(!session.handshake());
        assert!(session.failure().is_some());
    }

    #[test]
    fn a_listing_comes_back_as_the_names_it_holds() {
        let mut three = 3_u32.to_be_bytes().to_vec();
        three.extend_from_slice(&name_entry(".", DIRECTORY | 0o755, 0));
        three.extend_from_slice(&name_entry("notes.txt", 0o100_644, 12));
        three.extend_from_slice(&name_entry("src", DIRECTORY | 0o755, 4096));

        let mut version = Vec::new();
        version.extend_from_slice(&VERSION.to_be_bytes());
        let (mut session, far) = serve(vec![
            (REPLY_VERSION, version),
            (REPLY_HANDLE, body(1, &encode_string("h"))),
            (REPLY_NAME, body(2, &three)),
            (REPLY_STATUS, body(3, &EOF.to_be_bytes())),
            (REPLY_STATUS, body(4, &OK.to_be_bytes())),
        ]);
        assert!(session.handshake());
        let entries = session.list("/home/dean").expect("a listing");

        // The two the protocol always sends and no browser ever shows.
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "notes.txt");
        assert_eq!(entries[0].size, 12);
        assert!(!entries[0].is_directory);
        assert!(entries[1].is_directory);

        drop(session);
        let asked = far.join().expect("the far end");
        assert_eq!(asked[1].0, OPENDIR);
        assert_eq!(asked[2].0, READDIR);
    }

    #[test]
    fn a_listing_cut_off_halfway_is_no_listing() {
        let mut one = 1_u32.to_be_bytes().to_vec();
        one.extend_from_slice(&name_entry("notes.txt", 0o100_644, 12));

        let mut version = Vec::new();
        version.extend_from_slice(&VERSION.to_be_bytes());
        // The script ends after the first page, so the pipe closes where the
        // second `READDIR` is waiting for its answer.
        let (mut session, _far) = serve(vec![
            (REPLY_VERSION, version),
            (REPLY_HANDLE, body(1, &encode_string("h"))),
            (REPLY_NAME, body(2, &one)),
        ]);
        assert!(session.handshake());
        assert!(session.list("/home/dean").is_none());
        assert!(session.is_lost());
    }

    #[test]
    fn a_name_the_far_end_sends_with_a_slash_in_it_is_not_listed() {
        let mut four = 4_u32.to_be_bytes().to_vec();
        four.extend_from_slice(&name_entry("../../etc/passwd", 0o100_644, 1));
        four.extend_from_slice(&name_entry("a/b", 0o100_644, 1));
        four.extend_from_slice(&name_entry("", 0o100_644, 1));
        four.extend_from_slice(&name_entry("notes.txt", 0o100_644, 1));

        let mut version = Vec::new();
        version.extend_from_slice(&VERSION.to_be_bytes());
        let (mut session, _far) = serve(vec![
            (REPLY_VERSION, version),
            (REPLY_HANDLE, body(1, &encode_string("h"))),
            (REPLY_NAME, body(2, &four)),
            (REPLY_STATUS, body(3, &EOF.to_be_bytes())),
            (REPLY_STATUS, body(4, &OK.to_be_bytes())),
        ]);
        assert!(session.handshake());
        let entries = session.list("/home/dean").expect("a listing");
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["notes.txt"]);
        assert!(!session.is_lost());
    }

    #[test]
    fn a_directory_that_is_not_there_says_so_rather_than_answering() {
        let mut version = Vec::new();
        version.extend_from_slice(&VERSION.to_be_bytes());
        let mut status = NO_SUCH_FILE.to_be_bytes().to_vec();
        status.extend_from_slice(&encode_string("No such file"));
        let (mut session, _far) = serve(vec![
            (REPLY_VERSION, version),
            (REPLY_STATUS, body(1, &status)),
        ]);
        assert!(session.handshake());
        assert!(session.list("/nowhere").is_none());
        let failure = session.failure().expect("a reason");
        assert!(failure.message.contains("/nowhere"), "{failure:?}");
        assert_eq!(failure.detail, "There is nothing there.");
    }

    #[test]
    fn a_download_lands_as_the_bytes_and_the_time_the_far_end_sent() {
        let local = std::env::temp_dir().join(format!("tuni-sftp-get-{}", std::process::id()));
        let _ = std::fs::remove_file(&local);

        let mut version = Vec::new();
        version.extend_from_slice(&VERSION.to_be_bytes());
        let mut attributes = (SIZE | PERMISSIONS | ACMODTIME).to_be_bytes().to_vec();
        attributes.extend_from_slice(&5_u64.to_be_bytes());
        attributes.extend_from_slice(&0o100_644_u32.to_be_bytes());
        attributes.extend_from_slice(&1_700_000_000_u32.to_be_bytes());
        attributes.extend_from_slice(&A_WHILE_AGO.to_be_bytes());
        let (mut session, far) = serve(vec![
            (REPLY_VERSION, version),
            (REPLY_ATTRS, body(1, &attributes)),
            (REPLY_HANDLE, body(2, &encode_string("h"))),
            (REPLY_DATA, body(3, &encode_string("hello"))),
            (REPLY_STATUS, body(4, &EOF.to_be_bytes())),
            (REPLY_STATUS, body(5, &OK.to_be_bytes())),
        ]);
        assert!(session.handshake());

        let mut seen = Vec::new();
        let done = session.get("/home/dean/notes.txt", &local, &mut |at, total| {
            seen.push((at, total));
            true
        });
        assert_eq!(done, Some(()));
        assert_eq!(std::fs::read(&local).expect("the file"), b"hello");
        assert_eq!(seen, vec![(0, 5), (5, 5)]);
        let changed = std::fs::metadata(&local).and_then(|data| data.modified());
        assert_eq!(
            changed.ok(),
            Some(UNIX_EPOCH + std::time::Duration::from_secs(A_WHILE_AGO.into()))
        );

        drop(session);
        let asked = far.join().expect("the far end");
        assert_eq!(asked[2].0, OPEN);
        assert_eq!(asked[3].0, READ);
        let _ = std::fs::remove_file(&local);
    }

    #[test]
    fn a_refused_progress_callback_cancels_the_transfer() {
        let local = std::env::temp_dir().join(format!("tuni-sftp-stop-{}", std::process::id()));

        let mut version = Vec::new();
        version.extend_from_slice(&VERSION.to_be_bytes());
        let mut attributes = (SIZE | PERMISSIONS).to_be_bytes().to_vec();
        attributes.extend_from_slice(&5_u64.to_be_bytes());
        attributes.extend_from_slice(&0o100_644_u32.to_be_bytes());
        // No data reply on the script: a cancel at the first progress call
        // means nothing is ever read, only opened and closed again.
        let (mut session, far) = serve(vec![
            (REPLY_VERSION, version),
            (REPLY_ATTRS, body(1, &attributes)),
            (REPLY_HANDLE, body(2, &encode_string("h"))),
            (REPLY_STATUS, body(3, &OK.to_be_bytes())),
        ]);
        assert!(session.handshake());

        let done = session.get("/home/dean/notes.txt", &local, &mut |_, _| false);
        assert_eq!(done, None);
        // Neither the name asked for nor the one the bytes were landing under.
        assert!(!local.exists());
        assert!(!local.with_file_name(temporary_name(&local)).exists());
        assert!(
            session
                .failure
                .as_ref()
                .is_some_and(|failure| failure.message.contains("cancelled"))
        );

        drop(session);
        let asked = far.join().expect("the far end");
        assert!(asked.iter().all(|(kind, _)| *kind != READ));
        let _ = std::fs::remove_file(&local);
    }

    #[test]
    fn an_upload_sends_the_file_from_its_start_and_the_time_it_was_changed() {
        let local = std::env::temp_dir().join(format!("tuni-sftp-put-{}", std::process::id()));
        a_file_changed_a_while_ago(&local);

        let mut version = Vec::new();
        version.extend_from_slice(&VERSION.to_be_bytes());
        let (mut session, far) = serve(vec![
            (REPLY_VERSION, version),
            (REPLY_STATUS, body(1, &NO_SUCH_FILE.to_be_bytes())),
            (REPLY_HANDLE, body(2, &encode_string("h"))),
            (REPLY_STATUS, body(3, &OK.to_be_bytes())),
            (REPLY_STATUS, body(4, &OK.to_be_bytes())),
            (REPLY_STATUS, body(5, &OK.to_be_bytes())),
            (REPLY_STATUS, body(6, &OK.to_be_bytes())),
        ]);
        assert!(session.handshake());
        assert_eq!(
            session.put(&local, "/tmp/notes.txt", &mut |_, _| true),
            Some(())
        );

        drop(session);
        let asked = far.join().expect("the far end");
        let kinds: Vec<u8> = asked.iter().map(|(kind, _)| *kind).collect();
        // Nothing was there, so nothing is removed before the rename.
        assert_eq!(
            kinds,
            vec![INIT, LSTAT, OPEN, WRITE, FSETSTAT, CLOSE, RENAME]
        );
        // A new file's mode went with the open, so only the times are set.
        let mut reader = Reader::new(&asked[4].1);
        reader.u32().expect("the request id");
        reader.string().expect("the handle");
        assert_eq!(reader.u32(), Some(ACMODTIME));
        reader.u32().expect("the access time");
        assert_eq!(reader.u32(), Some(A_WHILE_AGO));

        let mut reader = Reader::new(&asked[3].1);
        reader.u32().expect("the request id");
        assert_eq!(reader.string().as_deref(), Some(&b"h"[..]));
        assert_eq!(reader.u64(), Some(0));
        assert_eq!(reader.string().as_deref(), Some(&b"hello"[..]));
        let _ = std::fs::remove_file(&local);
    }

    /// The paths a request names, in order, read off what the far end was sent.
    fn paths(asked: &Packet) -> Vec<String> {
        let mut reader = Reader::new(&asked.1);
        reader.u32().expect("the request id");
        std::iter::from_fn(|| reader.string())
            .map(|path| String::from_utf8_lossy(&path).into_owned())
            .collect()
    }

    /// A time well before the test runs, so one that was kept is not the
    /// same number as one the clock happened to give.
    const A_WHILE_AGO: u32 = 1_000_000_000;

    fn a_file_changed_a_while_ago(path: &Path) {
        std::fs::write(path, b"hello").expect("a file to send");
        let file = File::options().write(true).open(path).expect("the file");
        let then = UNIX_EPOCH + std::time::Duration::from_secs(A_WHILE_AGO.into());
        file.set_modified(then).expect("a time");
    }

    fn a_regular_file(mode: u32) -> Vec<u8> {
        let mut attributes = (SIZE | PERMISSIONS).to_be_bytes().to_vec();
        attributes.extend_from_slice(&3_u64.to_be_bytes());
        attributes.extend_from_slice(&(0o100_000 | mode).to_be_bytes());
        attributes
    }

    #[test]
    fn an_upload_over_a_file_lands_under_a_temporary_name_and_is_moved_on_at_the_end() {
        let local = std::env::temp_dir().join(format!("tuni-sftp-over-{}", std::process::id()));
        a_file_changed_a_while_ago(&local);

        let mut version = Vec::new();
        version.extend_from_slice(&VERSION.to_be_bytes());
        let (mut session, far) = serve(vec![
            (REPLY_VERSION, version),
            (REPLY_ATTRS, body(1, &a_regular_file(0o664))),
            (REPLY_HANDLE, body(2, &encode_string("h"))),
            (REPLY_STATUS, body(3, &OK.to_be_bytes())),
            (REPLY_STATUS, body(4, &OK.to_be_bytes())),
            (REPLY_STATUS, body(5, &OK.to_be_bytes())),
            (REPLY_STATUS, body(6, &OK.to_be_bytes())),
            (REPLY_STATUS, body(7, &OK.to_be_bytes())),
        ]);
        assert!(session.handshake());
        assert_eq!(
            session.put(&local, "/srv/notes.txt", &mut |_, _| true),
            Some(())
        );

        drop(session);
        let asked = far.join().expect("the far end");
        let kinds: Vec<u8> = asked.iter().map(|(kind, _)| *kind).collect();
        assert_eq!(
            kinds,
            vec![INIT, LSTAT, OPEN, WRITE, FSETSTAT, CLOSE, REMOVE, RENAME]
        );
        let temporary = format!("/srv/.notes.txt.tuni-{}", std::process::id());
        assert_eq!(paths(&asked[2])[0], temporary);
        assert_eq!(paths(&asked[6]), vec!["/srv/notes.txt"]);
        assert_eq!(
            paths(&asked[7]),
            vec![temporary, "/srv/notes.txt".to_owned()]
        );

        // The bits the file had over there, set past the far end's umask, and
        // the time it was changed here.
        let mut reader = Reader::new(&asked[4].1);
        reader.u32().expect("the request id");
        reader.string().expect("the handle");
        assert_eq!(reader.u32(), Some(PERMISSIONS | ACMODTIME));
        assert_eq!(reader.u32(), Some(0o664));
        reader.u32().expect("the access time");
        assert_eq!(reader.u32(), Some(A_WHILE_AGO));
        let _ = std::fs::remove_file(&local);
    }

    #[test]
    fn a_server_that_can_replace_a_name_in_one_step_is_asked_to() {
        let local = std::env::temp_dir().join(format!("tuni-sftp-posix-{}", std::process::id()));
        std::fs::write(&local, b"hello").expect("a file to send");

        let mut version = VERSION.to_be_bytes().to_vec();
        for (name, revision) in [("statvfs@openssh.com", "2"), (POSIX_RENAME, "1")] {
            version.extend_from_slice(&encode_string(name));
            version.extend_from_slice(&encode_string(revision));
        }
        let (mut session, far) = serve(vec![
            (REPLY_VERSION, version),
            (REPLY_ATTRS, body(1, &a_regular_file(0o644))),
            (REPLY_HANDLE, body(2, &encode_string("h"))),
            (REPLY_STATUS, body(3, &OK.to_be_bytes())),
            (REPLY_STATUS, body(4, &OK.to_be_bytes())),
            (REPLY_STATUS, body(5, &OK.to_be_bytes())),
            (REPLY_STATUS, body(6, &OK.to_be_bytes())),
        ]);
        assert!(session.handshake());
        assert_eq!(
            session.put(&local, "/srv/notes.txt", &mut |_, _| true),
            Some(())
        );

        drop(session);
        let asked = far.join().expect("the far end");
        let kinds: Vec<u8> = asked.iter().map(|(kind, _)| *kind).collect();
        // No remove: there is never a moment with nothing under the name.
        assert_eq!(
            kinds,
            vec![INIT, LSTAT, OPEN, WRITE, FSETSTAT, CLOSE, EXTENSION]
        );
        let temporary = format!("/srv/.notes.txt.tuni-{}", std::process::id());
        assert_eq!(
            paths(&asked[6]),
            vec![
                POSIX_RENAME.to_owned(),
                temporary,
                "/srv/notes.txt".to_owned()
            ]
        );
        let _ = std::fs::remove_file(&local);
    }

    #[test]
    fn a_cancelled_upload_leaves_the_file_that_was_there() {
        let local = std::env::temp_dir().join(format!("tuni-sftp-keep-{}", std::process::id()));
        std::fs::write(&local, b"hello").expect("a file to send");

        let mut version = Vec::new();
        version.extend_from_slice(&VERSION.to_be_bytes());
        let (mut session, far) = serve(vec![
            (REPLY_VERSION, version),
            (REPLY_ATTRS, body(1, &a_regular_file(0o644))),
            (REPLY_HANDLE, body(2, &encode_string("h"))),
            (REPLY_STATUS, body(3, &OK.to_be_bytes())),
            (REPLY_STATUS, body(4, &OK.to_be_bytes())),
        ]);
        assert!(session.handshake());
        assert!(
            session
                .put(&local, "/srv/notes.txt", &mut |_, _| false)
                .is_none()
        );

        drop(session);
        let asked = far.join().expect("the far end");
        let kinds: Vec<u8> = asked.iter().map(|(kind, _)| *kind).collect();
        assert_eq!(kinds, vec![INIT, LSTAT, OPEN, CLOSE, REMOVE]);
        // The only thing deleted is the temporary; the real name was never
        // opened, so it was never cut short.
        let temporary = format!("/srv/.notes.txt.tuni-{}", std::process::id());
        assert_eq!(paths(&asked[2])[0], temporary);
        assert_eq!(paths(&asked[4]), vec![temporary]);
        let _ = std::fs::remove_file(&local);
    }

    #[test]
    fn a_file_the_account_may_not_write_says_which_way_round_it_failed() {
        let local = std::env::temp_dir().join(format!("tuni-sftp-denied-{}", std::process::id()));
        std::fs::write(&local, b"hello").expect("a file to send");

        let mut version = Vec::new();
        version.extend_from_slice(&VERSION.to_be_bytes());
        let mut status = PERMISSION_DENIED.to_be_bytes().to_vec();
        status.extend_from_slice(&encode_string("Permission denied"));
        // Refused beside the file, since nobody may write into /etc, and then
        // refused in it, since nobody may write that file either.
        let (mut session, far) = serve(vec![
            (REPLY_VERSION, version),
            (REPLY_ATTRS, body(1, &a_regular_file(0o644))),
            (REPLY_STATUS, body(2, &status)),
            (REPLY_STATUS, body(3, &status)),
        ]);
        assert!(session.handshake());
        assert!(
            session
                .put(&local, "/etc/passwd", &mut |_, _| true)
                .is_none()
        );

        let failure = session.failure().expect("a reason");
        assert_eq!(failure.message, "Couldn't write /etc/passwd.");
        assert_eq!(
            failure.detail,
            "The account this connection logged in as may not write it."
        );

        drop(session);
        let asked = far.join().expect("the far end");
        assert_eq!(paths(&asked[3])[0], "/etc/passwd");
        let _ = std::fs::remove_file(&local);
    }

    #[test]
    fn a_rename_names_both_paths_in_the_order_the_protocol_reads_them() {
        let mut version = Vec::new();
        version.extend_from_slice(&VERSION.to_be_bytes());
        let (mut session, far) = serve(vec![
            (REPLY_VERSION, version),
            (REPLY_STATUS, body(1, &OK.to_be_bytes())),
        ]);
        assert!(session.handshake());
        assert_eq!(session.rename("/srv/old.txt", "/srv/new.txt"), Some(()));

        drop(session);
        let asked = far.join().expect("the far end");
        assert_eq!(asked[1].0, RENAME);
        let mut reader = Reader::new(&asked[1].1);
        reader.u32().expect("the request id");
        assert_eq!(reader.string().as_deref(), Some(&b"/srv/old.txt"[..]));
        assert_eq!(reader.string().as_deref(), Some(&b"/srv/new.txt"[..]));
    }

    #[test]
    fn a_directory_the_far_end_will_not_delete_says_what_it_said() {
        let mut version = Vec::new();
        version.extend_from_slice(&VERSION.to_be_bytes());
        let mut status = 4_u32.to_be_bytes().to_vec();
        status.extend_from_slice(&encode_string("Directory not empty"));
        let (mut session, _far) = serve(vec![
            (REPLY_VERSION, version),
            (REPLY_STATUS, body(1, &status)),
        ]);
        assert!(session.handshake());
        assert!(session.rmdir("/srv/logs").is_none());

        let failure = session.failure().expect("a reason");
        assert_eq!(failure.message, "Couldn't delete /srv/logs.");
        assert_eq!(failure.detail, "Directory not empty");
    }

    #[test]
    fn a_name_that_is_taken_is_named_as_taken_rather_than_as_a_failure() {
        let mut version = Vec::new();
        version.extend_from_slice(&VERSION.to_be_bytes());
        // What OpenSSH's server answers with, which is the code's own name and
        // nothing a person can act on.
        let mut status = 4_u32.to_be_bytes().to_vec();
        status.extend_from_slice(&encode_string("Failure"));
        let mut attributes = PERMISSIONS.to_be_bytes().to_vec();
        attributes.extend_from_slice(&(DIRECTORY | 0o755).to_be_bytes());
        let (mut session, far) = serve(vec![
            (REPLY_VERSION, version),
            (REPLY_STATUS, body(1, &status)),
            (REPLY_ATTRS, body(2, &attributes)),
        ]);
        assert!(session.handshake());
        assert!(session.mkdir("/srv/logs").is_none());

        let failure = session.failure().expect("a reason");
        assert_eq!(failure.message, "Couldn't make /srv/logs.");
        assert_eq!(
            failure.detail,
            "There is already something there with that name."
        );

        drop(session);
        let asked = far.join().expect("the far end");
        assert_eq!(asked[1].0, MKDIR);
        assert_eq!(asked[2].0, LSTAT);
    }

    #[test]
    fn a_refused_key_says_to_connect_in_a_pane_first() {
        let said = "Warning: Permanently added 'box' (ED25519) to the list of known hosts.\r\n\
                    dean@box: Permission denied (publickey,password).\r\n";
        assert!(
            explain(said)
                .detail
                .contains("Connect the host in a pane first")
        );
    }

    #[test]
    fn a_server_without_sftp_says_so() {
        let said = "subsystem request failed on channel 0\n";
        assert_eq!(
            explain(said).detail,
            "The far end has no SFTP server switched on."
        );
    }

    #[test]
    fn a_reason_nothing_here_knows_is_shown_as_ssh_said_it() {
        let said = "** WARNING: connection is not using a post-quantum key exchange algorithm.\n\
                    ssh: Could not resolve hostname nowhere: Name or service not known\n\n";
        assert_eq!(
            explain(said).detail,
            "ssh: Could not resolve hostname nowhere: Name or service not known"
        );
    }

    #[test]
    fn an_ssh_that_exits_at_once_says_what_it_printed() {
        let argv: Vec<String> = [
            "sh",
            "-c",
            "echo 'Host key verification failed.' >&2; exit 255",
        ]
        .iter()
        .map(|&part| part.to_owned())
        .collect();
        let failure = Session::open(&argv).err().expect("no session");
        assert!(
            failure.detail.contains("does not know the host's key"),
            "{failure:?}"
        );
    }

    #[test]
    fn a_path_is_resolved_by_the_far_end() {
        let mut version = Vec::new();
        version.extend_from_slice(&VERSION.to_be_bytes());
        let mut named = 1_u32.to_be_bytes().to_vec();
        named.extend_from_slice(&name_entry("/home/dean", DIRECTORY | 0o755, 4096));
        let (mut session, _far) = serve(vec![
            (REPLY_VERSION, version),
            (REPLY_NAME, body(1, &named)),
        ]);
        assert!(session.handshake());
        assert_eq!(session.realpath(".").as_deref(), Some("/home/dean"));
    }
}
