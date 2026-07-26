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
//! There is no timer in here. Every call blocks on a round trip, so the caller
//! belongs off the main thread, and a connection that has died is noticed the
//! way it always is: the master gives up on its own `ServerAlive` count, the
//! pipe closes, and the next read ends short.

use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};

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
const REALPATH: u8 = 16;
const STAT: u8 = 17;
const LSTAT: u8 = 7;
const CLOSE: u8 = 4;

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
/// asks for. Nothing here pipelines, so this is also the round trip: a file
/// crosses a 50 ms link at about 640 KB/s and a distant host feels it.
const CHUNK: usize = 32 * 1024;

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
/// Every call blocks on a round trip. The child is killed when this is dropped,
/// which is what closes the channel; the shared master it went over stays up.
pub struct Session {
    child: Option<Child>,
    out: Box<dyn Write + Send>,
    input: BufReader<Box<dyn Read + Send>>,
    next: u32,
    failure: Option<Failure>,
}

impl Session {
    /// Starts `argv` and agrees a version with it.
    ///
    /// The argv is built by the caller because it is the same `ssh` invocation
    /// as a pane's, options and all: the point is to land on the machine the
    /// pane is on, over the connection it already opened.
    pub fn open(argv: &[String]) -> Option<Self> {
        let (program, args) = argv.split_first()?;
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let out = child.stdin.take()?;
        let input = child.stdout.take()?;
        let mut session = Self {
            child: Some(child),
            out: Box::new(out),
            input: BufReader::new(Box::new(input)),
            next: 1,
            failure: None,
        };
        session.handshake().then_some(session)
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
    /// carrying `EOF` rather than an empty answer.
    pub fn list(&mut self, path: &str) -> Option<Vec<Entry>> {
        let handle = self.opendir(path)?;
        let mut entries = Vec::new();
        while let Some(id) = self.request(READDIR, &encode_string_bytes(&handle)) {
            let Some((kind, body)) = self.reply(id) else {
                break;
            };
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
                if entry.name != "." && entry.name != ".." {
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
    /// arrived and how much there is.
    ///
    /// The caller decides what `local` is named. Nothing here renames anything,
    /// so a caller that wants a half-written file to be unopenable writes to a
    /// name nobody looks for and moves it afterwards.
    pub fn get(
        &mut self,
        remote: &str,
        local: &Path,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Option<()> {
        let total = self.stat(remote).map_or(0, |entry| entry.size);
        let handle = self.open_file(remote, READABLE, None)?;
        let file = match File::create(local) {
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
        let done = self.download(&handle, file, remote, total, progress);
        self.close(&handle);
        done
    }

    /// Copies a local file to `remote`, calling `progress` with how much has
    /// gone and how much there is.
    ///
    /// The file arrives with the permission bits it has here, which is what the
    /// `sftp` program does too: a script that is executable on this machine is
    /// executable on the other one.
    pub fn put(
        &mut self,
        local: &Path,
        remote: &str,
        progress: &mut dyn FnMut(u64, u64),
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
        let mode = data.map_or(0o644, |data| {
            std::os::unix::fs::PermissionsExt::mode(&data.permissions()) & 0o777
        });
        let handle = self.open_file(remote, WRITABLE | CREATE | TRUNCATE, Some(mode))?;
        let done = self.upload(&handle, file, local, remote, total, progress);
        self.close(&handle);
        done
    }

    /// Why the last call failed, in the two parts a dialog wants.
    #[must_use]
    pub fn failure(&self) -> Option<&Failure> {
        self.failure.as_ref()
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
        if kind != REPLY_VERSION || Reader::new(&body).u32() != Some(VERSION) {
            self.failure = Some(Failure::new(
                "Couldn't start a file transfer.",
                "The far end does not speak SFTP version 3.".to_owned(),
            ));
            return false;
        }
        true
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
    fn download(
        &mut self,
        handle: &[u8],
        mut file: File,
        remote: &str,
        total: u64,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Option<()> {
        let mut at = 0_u64;
        progress(0, total);
        loop {
            let mut request = encode_string_bytes(handle);
            request.extend_from_slice(&at.to_be_bytes());
            request.extend_from_slice(&(CHUNK as u32).to_be_bytes());
            let id = self.request(READ, &request)?;
            let (kind, body) = self.reply(id)?;
            if kind == REPLY_STATUS {
                // The end of a file is a status packet, the way the end of a
                // listing is.
                if Reader::new(&body).u32() != Some(EOF) {
                    self.blame(kind, &body, "read", remote);
                    return None;
                }
                break;
            }
            if kind != REPLY_DATA {
                self.blame(kind, &body, "read", remote);
                return None;
            }
            let chunk = Reader::new(&body).string()?;
            // A server may answer with less than was asked for and that means
            // nothing; only the status packet above says the file has ended.
            if chunk.is_empty() {
                break;
            }
            if let Err(error) = file.write_all(&chunk) {
                self.wrote(&error.to_string());
                return None;
            }
            at += chunk.len() as u64;
            progress(at, total.max(at));
        }
        if let Err(error) = file.flush() {
            self.wrote(&error.to_string());
            return None;
        }
        Some(())
    }

    /// The body of an upload, closed over the same way.
    fn upload(
        &mut self,
        handle: &[u8],
        mut file: File,
        local: &Path,
        remote: &str,
        total: u64,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Option<()> {
        let mut buffer = vec![0_u8; CHUNK];
        let mut at = 0_u64;
        progress(0, total);
        loop {
            let read = match file.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) => {
                    self.failure = Some(Failure::new(
                        format!("Couldn't read {}.", local.display()),
                        error.to_string(),
                    ));
                    return None;
                }
            };
            let mut request = encode_string_bytes(handle);
            request.extend_from_slice(&at.to_be_bytes());
            request.extend_from_slice(&encode_string_bytes(&buffer[..read]));
            let id = self.request(WRITE, &request)?;
            let (kind, body) = self.reply(id)?;
            if kind != REPLY_STATUS || Reader::new(&body).u32() != Some(OK) {
                self.blame(kind, &body, "write", remote);
                return None;
            }
            at += read as u64;
            progress(at, total.max(at));
        }
        Some(())
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
    /// Requests go out one at a time, so anything carrying another id is a
    /// server that has lost the thread and the session with it.
    fn reply(&mut self, id: u32) -> Option<(u8, Vec<u8>)> {
        let (kind, body) = self.packet()?;
        let mut reader = Reader::new(&body);
        if reader.u32()? != id {
            self.lost("The far end answered a request nobody made.");
            return None;
        }
        Some((kind, body[4..].to_vec()))
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
        let message = reader
            .string()
            .map(|text| String::from_utf8_lossy(&text).into_owned())
            .filter(|text| !text.is_empty());
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
        let session = Session {
            child: None,
            out: Box::new(ours.try_clone().expect("a writer")),
            input: BufReader::new(Box::new(ours)),
            next: 1,
            failure: None,
        };
        (session, far)
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
    fn a_download_lands_as_the_bytes_the_far_end_sent() {
        let local = std::env::temp_dir().join(format!("tuni-sftp-get-{}", std::process::id()));
        let _ = std::fs::remove_file(&local);

        let mut version = Vec::new();
        version.extend_from_slice(&VERSION.to_be_bytes());
        let mut attributes = (SIZE | PERMISSIONS).to_be_bytes().to_vec();
        attributes.extend_from_slice(&5_u64.to_be_bytes());
        attributes.extend_from_slice(&0o100_644_u32.to_be_bytes());
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
        });
        assert_eq!(done, Some(()));
        assert_eq!(std::fs::read(&local).expect("the file"), b"hello");
        assert_eq!(seen, vec![(0, 5), (5, 5)]);

        drop(session);
        let asked = far.join().expect("the far end");
        assert_eq!(asked[2].0, OPEN);
        assert_eq!(asked[3].0, READ);
        let _ = std::fs::remove_file(&local);
    }

    #[test]
    fn an_upload_sends_the_file_from_its_start() {
        let local = std::env::temp_dir().join(format!("tuni-sftp-put-{}", std::process::id()));
        std::fs::write(&local, b"hello").expect("a file to send");

        let mut version = Vec::new();
        version.extend_from_slice(&VERSION.to_be_bytes());
        let (mut session, far) = serve(vec![
            (REPLY_VERSION, version),
            (REPLY_HANDLE, body(1, &encode_string("h"))),
            (REPLY_STATUS, body(2, &OK.to_be_bytes())),
            (REPLY_STATUS, body(3, &OK.to_be_bytes())),
        ]);
        assert!(session.handshake());
        assert_eq!(
            session.put(&local, "/tmp/notes.txt", &mut |_, _| {}),
            Some(())
        );

        drop(session);
        let asked = far.join().expect("the far end");
        assert_eq!(asked[1].0, OPEN);
        assert_eq!(asked[2].0, WRITE);

        let mut reader = Reader::new(&asked[2].1);
        reader.u32().expect("the request id");
        assert_eq!(reader.string().as_deref(), Some(&b"h"[..]));
        assert_eq!(reader.u64(), Some(0));
        assert_eq!(reader.string().as_deref(), Some(&b"hello"[..]));
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
        let (mut session, _far) = serve(vec![
            (REPLY_VERSION, version),
            (REPLY_STATUS, body(1, &status)),
        ]);
        assert!(session.handshake());
        assert!(session.put(&local, "/etc/passwd", &mut |_, _| {}).is_none());

        let failure = session.failure().expect("a reason");
        assert_eq!(failure.message, "Couldn't write /etc/passwd.");
        assert_eq!(
            failure.detail,
            "The account this connection logged in as may not write it."
        );
        let _ = std::fs::remove_file(&local);
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
