//! A language server, spoken to over stdio.
//!
//! The same deal git and ssh get: an external program driven as a process, so
//! completion in a Rust file is whatever `rust-analyzer` says, with the exact
//! configuration the command line would have used, and nothing links into this
//! address space. Which server a file gets is a table here plus a lookup on
//! `PATH`; a machine without the server simply has an editor without one, the
//! way it already has a git panel without `git`.
//!
//! The split follows the PTY: the [`Connection`] lives on the caller's main
//! thread and owns the process and its stdin, and a reader thread turns stdout
//! frames into [`Event`]s for whatever channel the caller hands it. Everything
//! that can be computed without a process (positions, URIs, message parsing)
//! is a free function beside its tests.

use std::collections::HashMap;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::json;
// Re-exported because half this module's API speaks in it, and the caller
// should not need its own serde_json for that.
pub use serde_json::Value;

use crate::rpc;

// --- which server a file gets -----------------------------------------------

/// One language the editor can ask about: what the protocol calls it, which
/// files are in it, which servers speak it, and what marks a project root.
pub struct Language {
    /// The `languageId` the protocol wants in `didOpen`.
    pub id: &'static str,
    pub extensions: &'static [&'static str],
    /// Commands tried in order; the first whose program is on `PATH` runs.
    /// More than one because Python alone ships three servers a distribution
    /// might have installed, and any of them beats none.
    pub commands: &'static [&'static [&'static str]],
    /// Files that mark the root of a project in this language. `.git` is the
    /// fallback for all of them, so only the language's own markers are here.
    pub roots: &'static [&'static str],
}

/// The table is the policy: a language is in it when its server needs no
/// configuration to be useful over stdio, which is also what keeps a config
/// vocabulary from existing before anything needs one.
pub const LANGUAGES: &[Language] = &[
    Language {
        id: "rust",
        extensions: &["rs"],
        commands: &[&["rust-analyzer"]],
        roots: &["Cargo.toml"],
    },
    Language {
        id: "c",
        extensions: &["c", "h"],
        commands: &[&["clangd"]],
        roots: &[
            "compile_commands.json",
            ".clangd",
            "Makefile",
            "meson.build",
        ],
    },
    Language {
        id: "cpp",
        extensions: &["cc", "cpp", "cxx", "hh", "hpp", "hxx"],
        commands: &[&["clangd"]],
        roots: &[
            "compile_commands.json",
            ".clangd",
            "CMakeLists.txt",
            "meson.build",
        ],
    },
    Language {
        id: "python",
        extensions: &["py"],
        commands: &[
            &["pylsp"],
            &["pyright-langserver", "--stdio"],
            &["jedi-language-server"],
        ],
        roots: &["pyproject.toml", "setup.py", "requirements.txt"],
    },
    Language {
        id: "go",
        extensions: &["go"],
        commands: &[&["gopls"]],
        roots: &["go.mod"],
    },
    Language {
        id: "javascript",
        extensions: &["js", "mjs", "cjs"],
        commands: &[&["typescript-language-server", "--stdio"]],
        roots: &["package.json", "tsconfig.json", "jsconfig.json"],
    },
    Language {
        id: "typescript",
        extensions: &["ts", "mts", "cts"],
        commands: &[&["typescript-language-server", "--stdio"]],
        roots: &["tsconfig.json", "package.json"],
    },
    Language {
        id: "typescriptreact",
        extensions: &["tsx"],
        commands: &[&["typescript-language-server", "--stdio"]],
        roots: &["tsconfig.json", "package.json"],
    },
    Language {
        id: "javascriptreact",
        extensions: &["jsx"],
        commands: &[&["typescript-language-server", "--stdio"]],
        roots: &["package.json", "jsconfig.json"],
    },
    Language {
        id: "zig",
        extensions: &["zig", "zon"],
        commands: &[&["zls"]],
        roots: &["build.zig"],
    },
    Language {
        id: "lua",
        extensions: &["lua"],
        commands: &[&["lua-language-server"]],
        roots: &[".luarc.json", ".luarc.jsonc"],
    },
    Language {
        id: "shellscript",
        extensions: &["sh", "bash"],
        commands: &[&["bash-language-server", "start"]],
        roots: &[],
    },
];

/// The language a file is in, by its extension. `None` is a file the table
/// does not cover, which is most files, and means no server rather than an
/// error.
#[must_use]
pub fn language_for_path(path: &Path) -> Option<&'static Language> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    LANGUAGES
        .iter()
        .find(|language| language.extensions.contains(&extension.as_str()))
}

/// The first of the language's servers this machine can run, or `None` for a
/// machine that has none of them installed.
#[must_use]
pub fn available_command(language: &Language) -> Option<&'static [&'static str]> {
    language
        .commands
        .iter()
        .copied()
        .find(|command| command.first().is_some_and(|program| runnable(program)))
}

/// Whether a program named in a server command is there to be run: the lookup
/// a shell would do, minus the shell. A name with a slash is taken as written.
/// The debug adapters answer the same question, hence the crate visibility.
pub(crate) fn runnable(program: &str) -> bool {
    let executable = |path: &Path| {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    };
    if program.contains('/') {
        return executable(Path::new(program));
    }
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| executable(&directory.join(program)))
    })
}

/// Where the project around a file starts: the nearest ancestor holding one of
/// the language's own markers, then the nearest holding `.git`, then the
/// file's directory. The server is told this once and builds its whole world
/// from it, which is why a wrong answer here is completion for the wrong
/// crate.
#[must_use]
pub fn find_root(path: &Path, language: &Language) -> PathBuf {
    let start = path.parent().unwrap_or(path);
    let mut git = None;
    let mut current = Some(start);
    while let Some(directory) = current {
        if language
            .roots
            .iter()
            .any(|marker| directory.join(marker).exists())
        {
            return directory.to_path_buf();
        }
        if git.is_none() && directory.join(".git").exists() {
            git = Some(directory.to_path_buf());
        }
        current = directory.parent();
    }
    git.unwrap_or_else(|| start.to_path_buf())
}

// --- positions ---------------------------------------------------------------

/// The unit a server counts columns in. UTF-16 is the protocol's unhappy
/// default, kept for JavaScript's sake; 3.17 lets a client offer UTF-8 and a
/// server accept it, so both are handled and the choice is whatever
/// `initialize` negotiated.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Encoding {
    Utf8,
    #[default]
    Utf16,
}

/// A server column on one line, turned into a character index. Clamped to the
/// line's end: a diagnostic pointing past what the buffer holds should mark
/// the end of the line, not vanish.
#[must_use]
pub fn column_to_char(line: &str, column: usize, encoding: Encoding) -> usize {
    let mut units = 0;
    for (characters, c) in line.chars().enumerate() {
        if units >= column {
            return characters;
        }
        units += match encoding {
            Encoding::Utf8 => c.len_utf8(),
            Encoding::Utf16 => c.len_utf16(),
        };
    }
    line.chars().count()
}

/// A character index on one line, turned into the server's column.
#[must_use]
pub fn char_to_column(line: &str, character: usize, encoding: Encoding) -> usize {
    line.chars()
        .take(character)
        .map(|c| match encoding {
            Encoding::Utf8 => c.len_utf8(),
            Encoding::Utf16 => c.len_utf16(),
        })
        .sum()
}

/// One line of a document, counting the way the protocol counts: a trailing
/// newline opens a final empty line rather than being swallowed.
fn line_of(text: &str, line: usize) -> &str {
    text.split('\n')
        .nth(line)
        .map_or("", |l| l.strip_suffix('\r').unwrap_or(l))
}

// --- URIs --------------------------------------------------------------------

/// `file://` plus the path, percent-encoded byte by byte. Only the bytes a URI
/// cannot carry raw are escaped, so the common path round-trips readably.
#[must_use]
pub fn uri_from_path(path: &Path) -> String {
    use std::fmt::Write as _;
    use std::os::unix::ffi::OsStrExt as _;
    let mut uri = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        let keep = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/');
        if keep {
            uri.push(byte as char);
        } else {
            let _ = write!(uri, "%{byte:02X}");
        }
    }
    uri
}

/// The path a `file://` URI names, or `None` for any other scheme: a server
/// answering with `untitled:` or `jdt://` is pointing at something this editor
/// cannot open anyway.
#[must_use]
pub fn path_from_uri(uri: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt as _;
    let rest = uri.strip_prefix("file://")?;
    // A host between the slashes is possible in the grammar and nonsense on a
    // local machine; tolerate the empty one some servers emit as `file:///`.
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    let mut bytes = Vec::with_capacity(path.len());
    let mut chars = path.bytes();
    while let Some(byte) = chars.next() {
        if byte == b'%' {
            let high = chars.next().and_then(|c| (c as char).to_digit(16));
            let low = chars.next().and_then(|c| (c as char).to_digit(16));
            match (high, low) {
                (Some(high), Some(low)) => bytes.push((high * 16 + low) as u8),
                _ => return None,
            }
        } else {
            bytes.push(byte);
        }
    }
    let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
    path.is_absolute().then_some(path)
}

// --- what the server sends ---------------------------------------------------

/// What the reader thread hands to the main thread, in the shape the PTY's
/// events set: messages until the process is gone, then one `Exited`.
#[derive(Debug)]
pub enum Event {
    Message(Value),
    Exited,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Severity {
    Error,
    Warning,
    Information,
    Hint,
}

/// One diagnostic as the server spelled it: positions are lines and columns in
/// the *server's* units, converted only when there is a document to convert
/// against.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub start: (usize, usize),
    pub end: (usize, usize),
    pub severity: Severity,
    pub message: String,
    /// Who is complaining, "rustc" or "clippy", when the server says.
    pub source: Option<String>,
}

/// Everything a message from the server can turn out to be that the caller
/// cares to tell apart.
#[derive(Debug)]
pub enum Incoming {
    /// The answer to a request this client sent.
    Response {
        id: i64,
        result: Value,
        error: Option<String>,
    },
    Diagnostics {
        uri: String,
        diagnostics: Vec<Diagnostic>,
    },
    /// `window/showMessage`: the server wants the user to read something.
    Message { text: String },
    /// Progress, logs, and everything else that changes nothing here.
    Other,
}

/// Sorts one message. Server-initiated *requests* never reach this: the reader
/// thread answers them itself, because every one of them is either trivially
/// satisfiable or safely refusable without asking the user anything.
#[must_use]
pub fn parse_incoming(message: &Value) -> Incoming {
    if let Some(id) = message.get("id").and_then(Value::as_i64)
        && message.get("method").is_none()
    {
        let error = message
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        return Incoming::Response {
            id,
            result: message.get("result").cloned().unwrap_or(Value::Null),
            error,
        };
    }
    match message.get("method").and_then(Value::as_str) {
        Some("textDocument/publishDiagnostics") => {
            let params = message.get("params");
            let uri = params
                .and_then(|params| params.get("uri"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let diagnostics = params
                .and_then(|params| params.get("diagnostics"))
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(parse_diagnostic).collect())
                .unwrap_or_default();
            Incoming::Diagnostics { uri, diagnostics }
        }
        Some("window/showMessage") => {
            let text = message
                .pointer("/params/message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            Incoming::Message { text }
        }
        _ => Incoming::Other,
    }
}

fn parse_diagnostic(value: &Value) -> Option<Diagnostic> {
    let range = value.get("range")?;
    let position = |which: &str| {
        let point = range.get(which)?;
        Some((
            point.get("line")?.as_u64()? as usize,
            point.get("character")?.as_u64()? as usize,
        ))
    };
    Some(Diagnostic {
        start: position("start")?,
        end: position("end")?,
        // Unspecified severity is an error by the protocol's own reading.
        severity: match value.get("severity").and_then(Value::as_u64) {
            Some(2) => Severity::Warning,
            Some(3) => Severity::Information,
            Some(4) => Severity::Hint,
            _ => Severity::Error,
        },
        message: value.get("message")?.as_str()?.to_owned(),
        source: value
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// The prose out of a hover answer, whatever of the three shapes the server
/// chose to say it in. Markdown arrives as markdown; how much of that to
/// render is the widget's decision, not a parsing one.
#[must_use]
pub fn hover_text(result: &Value) -> Option<String> {
    fn flatten(contents: &Value) -> Option<String> {
        if let Some(text) = contents.as_str() {
            return Some(text.to_owned());
        }
        if let Some(items) = contents.as_array() {
            let parts: Vec<String> = items.iter().filter_map(flatten).collect();
            return (!parts.is_empty()).then(|| parts.join("\n\n"));
        }
        // MarkupContent and MarkedString-with-language both keep the text
        // under "value"; the language tag adds nothing a tooltip can use.
        contents
            .get("value")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
    let text = flatten(result.get("contents")?)?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// One completion, reduced to what inserting it needs. Positions in the edit
/// range are still in server units, like every position that has not met a
/// document yet.
#[derive(Clone, Debug)]
pub struct Completion {
    pub label: String,
    pub insert: String,
    pub detail: Option<String>,
    /// Where the insert replaces, when the server said. Without it the caller
    /// falls back to replacing the word behind the cursor.
    pub start: Option<(usize, usize)>,
    pub end: Option<(usize, usize)>,
    /// What to sort by; the server's order encodes its relevance ranking.
    pub sort: String,
    /// What typed prefixes are matched against.
    pub filter: String,
}

/// The items out of a completion answer, which the protocol allows as a bare
/// array or wrapped in a list object.
#[must_use]
pub fn parse_completions(result: &Value) -> Vec<Completion> {
    let items = result
        .as_array()
        .or_else(|| result.get("items").and_then(Value::as_array));
    let Some(items) = items else {
        return Vec::new();
    };
    items.iter().filter_map(parse_completion).collect()
}

fn parse_completion(item: &Value) -> Option<Completion> {
    let label = item.get("label")?.as_str()?.trim().to_owned();
    let edit = item.get("textEdit");
    let mut insert = edit
        .and_then(|edit| edit.get("newText"))
        .or_else(|| item.get("insertText"))
        .and_then(Value::as_str)
        .unwrap_or(&label)
        .to_owned();
    // Snippet support is declared off, but servers with only a snippet to
    // offer send one anyway. Better the text minus its placeholders than a
    // literal "$0" in the file.
    if item.get("insertTextFormat").and_then(Value::as_u64) == Some(2) {
        insert = scrub_snippet(&insert);
    }
    // An InsertReplaceEdit carries two ranges; "insert" is the conservative
    // one, replacing only what was typed.
    let range = edit.and_then(|edit| edit.get("range").or_else(|| edit.get("insert")));
    let position = |which: &str| {
        let point = range?.get(which)?;
        Some((
            point.get("line")?.as_u64()? as usize,
            point.get("character")?.as_u64()? as usize,
        ))
    };
    Some(Completion {
        start: position("start"),
        end: position("end"),
        insert,
        detail: item
            .get("detail")
            .and_then(Value::as_str)
            .map(|detail| detail.trim().to_owned()),
        sort: item
            .get("sortText")
            .and_then(Value::as_str)
            .unwrap_or(&label)
            .to_owned(),
        filter: item
            .get("filterText")
            .and_then(Value::as_str)
            .unwrap_or(&label)
            .to_owned(),
        label,
    })
}

/// Snippet syntax reduced to the text it would leave behind: tabstops vanish,
/// a placeholder keeps its default, a choice keeps its first option.
#[must_use]
pub fn scrub_snippet(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(&next) = chars.peek()
            && matches!(next, '$' | '}' | '\\')
        {
            out.push(next);
            chars.next();
            continue;
        }
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            // $1, $0: a bare tabstop is a place, not text.
            Some(d) if d.is_ascii_digit() => {
                while chars.peek().is_some_and(char::is_ascii_digit) {
                    chars.next();
                }
            }
            Some('{') => {
                chars.next();
                // ${1:default} or ${1|first,second|}: skip to the payload,
                // keep it up to the closing brace, nested braces included.
                let mut body = String::new();
                let mut depth = 1;
                for inner in chars.by_ref() {
                    match inner {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    body.push(inner);
                }
                let payload = body
                    .split_once(':')
                    .map(|(_, rest)| rest.to_owned())
                    .or_else(|| {
                        body.split_once('|')
                            .map(|(_, rest)| rest.split([',', '|']).next().unwrap_or("").to_owned())
                    })
                    .unwrap_or_default();
                out.push_str(&scrub_snippet(&payload));
            }
            _ => out.push('$'),
        }
    }
    out
}

/// One place a definition lookup points at, still in server units.
#[derive(Clone, Debug)]
pub struct Location {
    pub uri: String,
    pub line: usize,
    pub column: usize,
}

/// The targets out of a definition answer: a location, an array of them, or
/// an array of links: three shapes for one idea, so they collapse here.
#[must_use]
pub fn parse_definition(result: &Value) -> Vec<Location> {
    fn parse_one(value: &Value) -> Option<Location> {
        // A LocationLink names its file "targetUri" and points twice; the
        // selection range is the name itself, which is where a reader lands.
        let uri = value
            .get("uri")
            .or_else(|| value.get("targetUri"))?
            .as_str()?
            .to_owned();
        let start = value
            .get("range")
            .or_else(|| value.get("targetSelectionRange"))
            .and_then(|range| range.get("start"));
        Some(Location {
            uri,
            line: start
                .and_then(|s| s.get("line"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize,
            column: start
                .and_then(|s| s.get("character"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize,
        })
    }
    match result {
        Value::Array(items) => items.iter().filter_map(parse_one).collect(),
        Value::Object(_) => parse_one(result).into_iter().collect(),
        _ => Vec::new(),
    }
}

// --- the connection ----------------------------------------------------------

/// What a request that is still in flight was asking, so the answer can be
/// routed when it lands. The URI rides along because a completion for a file
/// that has since closed should be dropped, not drawn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Pending {
    Initialize,
    Hover { uri: String },
    Completion { uri: String },
    Definition,
}

struct Document {
    text: String,
    version: i64,
}

/// One running server. Owns the process and its stdin; stdout belongs to the
/// reader thread once [`Connection::start_reader`] hands it over. Dropping the
/// connection kills the process: a language server with no editor attached is
/// a background CPU eater with nobody to talk to.
pub struct Connection {
    child: Child,
    writer: Arc<Mutex<ChildStdin>>,
    stdout: Option<ChildStdout>,
    next_id: i64,
    encoding: Encoding,
    documents: HashMap<String, Document>,
    pending: HashMap<i64, Pending>,
}

impl Connection {
    /// Starts the server in the project root. Stderr goes to the void: these
    /// programs narrate on it, and the narration interleaved with tuni's own
    /// output helps nobody.
    pub fn spawn(command: &[&str], root: &Path) -> io::Result<Self> {
        let (program, arguments) = command
            .split_first()
            .ok_or_else(|| io::Error::other("empty server command"))?;
        let mut child = Command::new(program)
            .args(arguments)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("no stdin"))?;
        let stdout = child.stdout.take();
        Ok(Self {
            child,
            writer: Arc::new(Mutex::new(stdin)),
            stdout,
            next_id: 0,
            encoding: Encoding::default(),
            documents: HashMap::new(),
            pending: HashMap::new(),
        })
    }

    /// Takes stdout and starts the thread that reads it. Server-initiated
    /// requests are answered right here on the reader thread, because each is either
    /// trivially satisfiable or safely refusable, and holding one for the main
    /// thread would let a slow frame stall the server.
    pub fn start_reader(&mut self, on_event: impl Fn(Event) + Send + 'static) -> bool {
        let Some(stdout) = self.stdout.take() else {
            return false;
        };
        let writer = Arc::clone(&self.writer);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(message)) = rpc::read_frame(&mut reader) {
                if let Some(id) = answer_server_request(&message) {
                    let reply = server_request_reply(&message);
                    let frame = match reply {
                        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                        Err(code) => json!({
                            "jsonrpc": "2.0", "id": id,
                            "error": {"code": code, "message": "unsupported"},
                        }),
                    };
                    if let Ok(mut writer) = writer.lock() {
                        let _ = rpc::write_frame(&mut *writer, &frame);
                    }
                    continue;
                }
                on_event(Event::Message(message));
            }
            on_event(Event::Exited);
        });
        true
    }

    /// Sends `initialize`. The rest of the handshake happens in
    /// [`Connection::finish_initialize`] when the answer arrives.
    pub fn initialize(&mut self, root: &Path) -> i64 {
        let uri = uri_from_path(root);
        let name = root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": {"name": "tuni", "version": env!("CARGO_PKG_VERSION")},
            "rootUri": uri,
            "workspaceFolders": [{"uri": uri, "name": name}],
            "capabilities": {
                // UTF-8 first: it is what the buffer speaks natively, and a
                // server that grants it saves every conversion after this one.
                "general": {"positionEncodings": ["utf-8", "utf-16"]},
                "textDocument": {
                    "synchronization": {"didSave": true},
                    "publishDiagnostics": {},
                    "hover": {"contentFormat": ["plaintext", "markdown"]},
                    "completion": {
                        "completionItem": {
                            "snippetSupport": false,
                            "documentationFormat": ["plaintext", "markdown"],
                        },
                    },
                    "definition": {},
                },
            },
        });
        self.request("initialize", params, Pending::Initialize)
    }

    /// Reads what `initialize` granted and says `initialized`, which is what
    /// frees the server to start publishing.
    pub fn finish_initialize(&mut self, result: &Value) {
        if result
            .pointer("/capabilities/positionEncoding")
            .and_then(Value::as_str)
            == Some("utf-8")
        {
            self.encoding = Encoding::Utf8;
        }
        self.notify("initialized", json!({}));
    }

    /// Tells the server a file is open and hands it the whole text. The
    /// connection keeps its own copy: every position that later crosses this
    /// boundary is converted against the text the server was actually given,
    /// not against a buffer that may have moved on.
    pub fn open(&mut self, path: &Path, language: &str, text: &str) {
        let uri = uri_from_path(path);
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": uri, "languageId": language, "version": 0, "text": text,
            }}),
        );
        self.documents.insert(
            uri,
            Document {
                text: text.to_owned(),
                version: 0,
            },
        );
    }

    /// Replaces the server's copy of the file. Whole text every time: the
    /// protocol's incremental sync exists to save bandwidth to a process on
    /// the same machine, and a wrong delta poisons every answer after it.
    pub fn change(&mut self, path: &Path, text: &str) {
        let uri = uri_from_path(path);
        let Some(document) = self.documents.get_mut(&uri) else {
            return;
        };
        document.version += 1;
        document.text = text.to_owned();
        let version = document.version;
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": version},
                "contentChanges": [{"text": text}],
            }),
        );
    }

    /// Tells the server the file reached the disk, which is what lets servers
    /// that only check saved files, most linters behind pylsp, run at all.
    pub fn saved(&mut self, path: &Path) {
        let uri = uri_from_path(path);
        if self.documents.contains_key(&uri) {
            self.notify(
                "textDocument/didSave",
                json!({"textDocument": {"uri": uri}}),
            );
        }
    }

    pub fn close(&mut self, path: &Path) {
        let uri = uri_from_path(path);
        if self.documents.remove(&uri).is_some() {
            self.notify(
                "textDocument/didClose",
                json!({"textDocument": {"uri": uri}}),
            );
        }
    }

    /// Whether any file still needs this server, which is what decides when it
    /// is shut down rather than kept for a tab that may never reopen.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.documents.is_empty()
    }

    pub fn hover(&mut self, path: &Path, line: usize, character: usize) -> Option<i64> {
        let uri = uri_from_path(path);
        let position = self.position(&uri, line, character)?;
        let params = json!({"textDocument": {"uri": &uri}, "position": position});
        Some(self.request("textDocument/hover", params, Pending::Hover { uri }))
    }

    pub fn completion(&mut self, path: &Path, line: usize, character: usize) -> Option<i64> {
        let uri = uri_from_path(path);
        let position = self.position(&uri, line, character)?;
        let params = json!({"textDocument": {"uri": &uri}, "position": position});
        Some(self.request(
            "textDocument/completion",
            params,
            Pending::Completion { uri },
        ))
    }

    pub fn definition(&mut self, path: &Path, line: usize, character: usize) -> Option<i64> {
        let uri = uri_from_path(path);
        let position = self.position(&uri, line, character)?;
        let params = json!({"textDocument": {"uri": &uri}, "position": position});
        Some(self.request("textDocument/definition", params, Pending::Definition))
    }

    /// What a request that just answered was asking. Taking it means asking
    /// once; an answer nobody claimed was cancelled by a newer question.
    pub fn take_pending(&mut self, id: i64) -> Option<Pending> {
        self.pending.remove(&id)
    }

    /// A server position out of a character index, against the text the server
    /// has. `None` is a file the server was never told about.
    fn position(&self, uri: &str, line: usize, character: usize) -> Option<Value> {
        let document = self.documents.get(uri)?;
        let column = char_to_column(line_of(&document.text, line), character, self.encoding);
        Some(json!({"line": line, "character": column}))
    }

    /// A server position turned into a character index, for a diagnostic or an
    /// edit range about to meet the buffer.
    #[must_use]
    pub fn to_char(&self, uri: &str, position: (usize, usize)) -> Option<(usize, usize)> {
        let document = self.documents.get(uri)?;
        let character = column_to_char(
            line_of(&document.text, position.0),
            position.1,
            self.encoding,
        );
        Some((position.0, character))
    }

    /// Asks the server to pack up. The `exit` follows immediately rather than
    /// after the reply: the connection is being dropped either way, and the
    /// protocol says a server that got both leaves cleanly.
    pub fn shutdown(&mut self) {
        let id = self.next_id();
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": "shutdown", "params": null}));
        self.notify("exit", Value::Null);
    }

    fn request(&mut self, method: &str, params: Value, pending: Pending) -> i64 {
        let id = self.next_id();
        self.pending.insert(id, pending);
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.write(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    fn next_id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }

    /// A failed write is a dead server; the reader thread is already on its
    /// way to saying so, and there is nothing useful to add here.
    fn write(&mut self, frame: &Value) {
        if let Ok(mut writer) = self.writer.lock() {
            let _ = rpc::write_frame(&mut *writer, frame);
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The id of a server-initiated request, or `None` for everything else.
fn answer_server_request(message: &Value) -> Option<&Value> {
    let id = message.get("id")?;
    message.get("method").and_then(Value::as_str)?;
    Some(id)
}

/// What to answer a server that asked. `Ok` is a result the method is content
/// with; `Err` is a JSON-RPC error code, which for everything unexpected is
/// "method not found", the honest answer from a client this small.
fn server_request_reply(message: &Value) -> Result<Value, i64> {
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params");
    match method {
        // One null per configuration item asked about: "use your defaults".
        "workspace/configuration" => {
            let count = params
                .and_then(|params| params.get("items"))
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            Ok(Value::Array(vec![Value::Null; count]))
        }
        "window/workDoneProgress/create"
        | "client/registerCapability"
        | "client/unregisterCapability" => Ok(Value::Null),
        // Nothing here applies edits the user did not make.
        "workspace/applyEdit" => Ok(json!({"applied": false})),
        _ => Err(-32601),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rust_file_is_rust_and_a_mystery_file_is_nobodys() {
        assert_eq!(
            language_for_path(Path::new("/a/main.rs")).unwrap().id,
            "rust"
        );
        assert_eq!(
            language_for_path(Path::new("/a/view.tsx")).unwrap().id,
            "typescriptreact"
        );
        assert!(language_for_path(Path::new("/a/notes.txt")).is_none());
        assert!(language_for_path(Path::new("/a/Makefile")).is_none());
    }

    #[test]
    fn no_extension_is_claimed_twice() {
        let mut seen = std::collections::HashSet::new();
        for language in LANGUAGES {
            for extension in language.extensions {
                assert!(seen.insert(*extension), "{extension} appears twice");
            }
        }
    }

    #[test]
    fn ascii_columns_are_character_indexes_in_both_encodings() {
        for encoding in [Encoding::Utf8, Encoding::Utf16] {
            assert_eq!(column_to_char("let x = 1;", 4, encoding), 4);
            assert_eq!(char_to_column("let x = 1;", 4, encoding), 4);
        }
    }

    #[test]
    fn a_multibyte_character_counts_once_but_measures_more() {
        // "żółw": every letter but w is two UTF-8 bytes, one UTF-16 unit.
        assert_eq!(char_to_column("żółw", 3, Encoding::Utf8), 6);
        assert_eq!(char_to_column("żółw", 3, Encoding::Utf16), 3);
        assert_eq!(column_to_char("żółw", 6, Encoding::Utf8), 3);
        assert_eq!(column_to_char("żółw", 3, Encoding::Utf16), 3);
    }

    #[test]
    fn an_emoji_is_two_utf16_units() {
        // "𝄞x": the clef is one character, four UTF-8 bytes, two UTF-16
        // units, the case UTF-16 clients get subtly wrong.
        assert_eq!(char_to_column("𝄞x", 1, Encoding::Utf16), 2);
        assert_eq!(column_to_char("𝄞x", 2, Encoding::Utf16), 1);
        assert_eq!(column_to_char("𝄞x", 3, Encoding::Utf16), 2);
    }

    #[test]
    fn a_column_past_the_line_lands_on_its_end() {
        assert_eq!(column_to_char("ab", 99, Encoding::Utf16), 2);
        assert_eq!(column_to_char("", 5, Encoding::Utf8), 0);
    }

    #[test]
    fn a_path_with_a_space_survives_the_round_trip() {
        let path = Path::new("/home/dean/my projekt/główny plik.rs");
        let uri = uri_from_path(path);
        assert!(!uri.contains(' '), "{uri}");
        assert_eq!(path_from_uri(&uri).as_deref(), Some(path));
    }

    #[test]
    fn a_plain_uri_decodes_and_a_foreign_scheme_does_not() {
        assert_eq!(
            path_from_uri("file:///tmp/a.rs"),
            Some(PathBuf::from("/tmp/a.rs"))
        );
        assert_eq!(
            path_from_uri("file:///tmp/with%20space.rs"),
            Some(PathBuf::from("/tmp/with space.rs"))
        );
        assert!(path_from_uri("untitled:Untitled-1").is_none());
        assert!(path_from_uri("file:///bad%zz").is_none());
    }

    #[test]
    fn diagnostics_arrive_sorted_into_their_fields() {
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///tmp/a.rs",
                "diagnostics": [
                    {
                        "range": {"start": {"line": 2, "character": 4},
                                  "end": {"line": 2, "character": 9}},
                        "severity": 2,
                        "source": "clippy",
                        "message": "unused variable",
                    },
                    {
                        "range": {"start": {"line": 0, "character": 0},
                                  "end": {"line": 0, "character": 1}},
                        "message": "mismatched types",
                    },
                ],
            },
        });
        let Incoming::Diagnostics { uri, diagnostics } = parse_incoming(&message) else {
            panic!("not diagnostics");
        };
        assert_eq!(uri, "file:///tmp/a.rs");
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].severity, Severity::Warning);
        assert_eq!(diagnostics[0].start, (2, 4));
        assert_eq!(diagnostics[0].source.as_deref(), Some("clippy"));
        // No severity means an error, per the protocol.
        assert_eq!(diagnostics[1].severity, Severity::Error);
    }

    #[test]
    fn a_response_is_told_apart_from_a_server_request() {
        let response = serde_json::json!({"jsonrpc": "2.0", "id": 7, "result": {"ok": true}});
        assert!(matches!(
            parse_incoming(&response),
            Incoming::Response { id: 7, .. }
        ));
        // Same id field, but a method makes it a question, not an answer.
        let request = serde_json::json!({
            "jsonrpc": "2.0", "id": 8, "method": "workspace/configuration",
            "params": {"items": [{}, {}]},
        });
        assert!(matches!(parse_incoming(&request), Incoming::Other));
        assert_eq!(
            server_request_reply(&request),
            Ok(Value::Array(vec![Value::Null, Value::Null]))
        );
    }

    #[test]
    fn an_unknown_server_request_is_refused_not_ignored() {
        let request = serde_json::json!({
            "jsonrpc": "2.0", "id": 9, "method": "window/showMessageRequest", "params": {},
        });
        assert_eq!(server_request_reply(&request), Err(-32601));
    }

    #[test]
    fn hover_text_flattens_all_three_shapes() {
        let markup = serde_json::json!({
            "contents": {"kind": "markdown", "value": "```rust\nfn main()\n```"},
        });
        assert_eq!(hover_text(&markup).unwrap(), "```rust\nfn main()\n```");

        let plain = serde_json::json!({"contents": "a str"});
        assert_eq!(hover_text(&plain).unwrap(), "a str");

        let mixed = serde_json::json!({
            "contents": ["first", {"language": "rust", "value": "second"}],
        });
        assert_eq!(hover_text(&mixed).unwrap(), "first\n\nsecond");

        let empty = serde_json::json!({"contents": []});
        assert!(hover_text(&empty).is_none());
    }

    #[test]
    fn completions_come_from_either_wrapper() {
        let bare = serde_json::json!([{"label": "push"}]);
        assert_eq!(parse_completions(&bare).len(), 1);

        let wrapped = serde_json::json!({
            "isIncomplete": false,
            "items": [{
                "label": "push_str(…)",
                "filterText": "push_str",
                "sortText": "0001",
                "detail": "fn(&mut self, string: &str)",
                "textEdit": {
                    "range": {"start": {"line": 3, "character": 6},
                              "end": {"line": 3, "character": 8}},
                    "newText": "push_str",
                },
            }],
        });
        let items = parse_completions(&wrapped);
        assert_eq!(items[0].insert, "push_str");
        assert_eq!(items[0].filter, "push_str");
        assert_eq!(items[0].start, Some((3, 6)));
        assert_eq!(items[0].end, Some((3, 8)));
    }

    #[test]
    fn a_snippet_keeps_its_words_and_loses_its_holes() {
        assert_eq!(scrub_snippet("println!(\"$1\")$0"), "println!(\"\")");
        assert_eq!(
            scrub_snippet("for ${1:item} in ${2:iter} {}"),
            "for item in iter {}"
        );
        assert_eq!(scrub_snippet("${1|first,second|}"), "first");
        assert_eq!(scrub_snippet("cost: \\$5"), "cost: $5");
        assert_eq!(scrub_snippet("plain text"), "plain text");
        assert_eq!(scrub_snippet("${1:outer ${2:inner}}"), "outer inner");
    }

    #[test]
    fn a_definition_answer_collapses_from_any_of_its_shapes() {
        let single = serde_json::json!({
            "uri": "file:///tmp/lib.rs",
            "range": {"start": {"line": 10, "character": 3},
                      "end": {"line": 10, "character": 8}},
        });
        assert_eq!(parse_definition(&single)[0].line, 10);

        let links = serde_json::json!([{
            "targetUri": "file:///tmp/lib.rs",
            "targetRange": {"start": {"line": 5, "character": 0},
                            "end": {"line": 20, "character": 1}},
            "targetSelectionRange": {"start": {"line": 6, "character": 7},
                                     "end": {"line": 6, "character": 12}},
        }]);
        let parsed = parse_definition(&links);
        // The selection range is the name; the full range is the whole item.
        assert_eq!((parsed[0].line, parsed[0].column), (6, 7));

        assert!(parse_definition(&Value::Null).is_empty());
    }

    #[test]
    fn the_root_is_the_marker_then_git_then_the_directory() {
        let base = std::env::temp_dir().join(format!("tuni-lsp-root-{}", std::process::id()));
        let project = base.join("repo/crates/deep");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(base.join("repo/.git")).unwrap();
        let rust = language_for_path(Path::new("x.rs")).unwrap();
        let file = project.join("main.rs");

        // Only .git above: the repository is the root.
        assert_eq!(find_root(&file, rust), base.join("repo"));

        // A Cargo.toml closer in beats the repository.
        std::fs::write(base.join("repo/crates/deep/Cargo.toml"), "").unwrap();
        assert_eq!(find_root(&file, rust), project);

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_line_is_looked_up_the_way_the_protocol_counts() {
        let text = "one\ntwo\r\nthree";
        assert_eq!(line_of(text, 0), "one");
        assert_eq!(line_of(text, 1), "two");
        assert_eq!(line_of(text, 2), "three");
        assert_eq!(line_of(text, 9), "");
    }
}
