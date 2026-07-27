//! A debug adapter, spoken to over stdio.
//!
//! The Debug Adapter Protocol is LSP's sibling: the same Content-Length
//! envelope from [`crate::rpc`], a different grammar inside it, spoken by the
//! debugger's own front end. lldb-dap for the compiled languages and debugpy
//! for Python, found on `PATH` and driven as processes, so a breakpoint stops
//! the program exactly where the same adapter under any other editor would
//! stop it. Delve is absent on purpose: `dlv dap` only listens on a socket,
//! and this client only speaks stdio.
//!
//! The shape is the one [`crate::lsp`] set: a [`Client`] on the caller's main
//! thread owning the process and its stdin, a reader thread turning stdout
//! into [`Event`]s, and the parsing in free functions beside their tests.

use std::collections::HashMap;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::json;
// Re-exported for the same reason lsp re-exports it: the API speaks in it.
pub use serde_json::Value;

use crate::rpc;

// --- which adapter a language gets -------------------------------------------

/// One debugger the panel can start: which languages it takes, and the
/// commands tried in order for a machine that has it.
pub struct Debugger {
    /// What the adapter is asked to call itself in `initialize`.
    pub id: &'static str,
    /// Language ids as [`crate::lsp::LANGUAGES`] spells them.
    pub languages: &'static [&'static str],
    pub commands: &'static [&'static [&'static str]],
}

pub const DEBUGGERS: &[Debugger] = &[
    Debugger {
        id: "lldb",
        languages: &["rust", "c", "cpp", "zig"],
        // The old name shipped for years before the rename; a machine still
        // carrying it can still debug.
        commands: &[&["lldb-dap"], &["lldb-vscode"]],
    },
    Debugger {
        id: "debugpy",
        languages: &["python"],
        commands: &[&["python3", "-m", "debugpy.adapter"]],
    },
];

/// The debugger for a language, or `None` for one nothing here can debug.
#[must_use]
pub fn debugger_for_language(language: &str) -> Option<&'static Debugger> {
    DEBUGGERS
        .iter()
        .find(|debugger| debugger.languages.contains(&language))
}

/// The first of the debugger's commands this machine can run.
#[must_use]
pub fn available_command(debugger: &Debugger) -> Option<&'static [&'static str]> {
    debugger.commands.iter().copied().find(|command| {
        command
            .first()
            .is_some_and(|program| crate::lsp::runnable(program))
    })
}

/// The `launch` request body for one adapter. The keys are the adapter's own
/// vocabulary rather than the protocol's, which is why this cannot be one
/// shape for everybody.
#[must_use]
pub fn launch_arguments(
    debugger: &Debugger,
    program: &Path,
    arguments: &[String],
    cwd: &Path,
) -> Value {
    let mut body = json!({
        "program": program,
        "args": arguments,
        "cwd": cwd,
    });
    if debugger.id == "debugpy" {
        // Without this debugpy asks for a terminal to run the program in, and
        // the answer to that request here is no.
        body["console"] = json!("internalConsole");
    }
    body
}

// --- what the adapter sends --------------------------------------------------

/// What the reader thread hands to the main thread: messages until the
/// process is gone, then one `Exited`.
#[derive(Debug)]
pub enum Event {
    Message(Value),
    Exited,
}

/// One frame of a stopped program's stack.
#[derive(Clone, Debug)]
pub struct Frame {
    pub id: i64,
    pub name: String,
    /// The file, when the frame has one a person can open. A frame down in a
    /// runtime without source stays listed, just not clickable.
    pub path: Option<PathBuf>,
    /// One-based, as the protocol is asked to count.
    pub line: usize,
}

/// One row of a variables listing.
#[derive(Clone, Debug)]
pub struct Variable {
    pub name: String,
    pub value: String,
    /// Non-zero when the value has children the adapter will expand.
    pub reference: i64,
}

/// Everything a message can turn out to be that the caller tells apart.
#[derive(Debug)]
pub enum Incoming {
    Response {
        request_seq: i64,
        command: String,
        success: bool,
        message: Option<String>,
        body: Value,
    },
    /// The adapter is ready for breakpoints and `configurationDone`.
    Initialized,
    Stopped {
        thread: Option<i64>,
        reason: String,
    },
    Continued,
    Output {
        text: String,
        category: String,
    },
    /// The debuggee finished; the exit code is worth a line in the log.
    ProgramExited {
        code: i64,
    },
    /// The session is over, whether or not the program is.
    Terminated,
    Other,
}

/// Sorts one message. Adapter-initiated requests never reach this: the reader
/// thread refuses them itself, because the only ones defined (run this in a
/// terminal, start another session) are things this client does not do.
#[must_use]
pub fn parse_incoming(message: &Value) -> Incoming {
    let body = message.get("body").cloned().unwrap_or(Value::Null);
    match message.get("type").and_then(Value::as_str) {
        Some("response") => Incoming::Response {
            request_seq: message
                .get("request_seq")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            command: message
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            success: message
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            message: message
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned),
            body,
        },
        Some("event") => match message.get("event").and_then(Value::as_str) {
            Some("initialized") => Incoming::Initialized,
            Some("stopped") => Incoming::Stopped {
                thread: body.get("threadId").and_then(Value::as_i64),
                reason: body
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("stopped")
                    .to_owned(),
            },
            Some("continued") => Incoming::Continued,
            Some("output") => Incoming::Output {
                text: body
                    .get("output")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                category: body
                    .get("category")
                    .and_then(Value::as_str)
                    .unwrap_or("console")
                    .to_owned(),
            },
            Some("exited") => Incoming::ProgramExited {
                code: body.get("exitCode").and_then(Value::as_i64).unwrap_or(-1),
            },
            Some("terminated") => Incoming::Terminated,
            _ => Incoming::Other,
        },
        _ => Incoming::Other,
    }
}

/// The frames out of a `stackTrace` response body.
#[must_use]
pub fn parse_stack(body: &Value) -> Vec<Frame> {
    body.get("stackFrames")
        .and_then(Value::as_array)
        .map(|frames| {
            frames
                .iter()
                .filter_map(|frame| {
                    Some(Frame {
                        id: frame.get("id")?.as_i64()?,
                        name: frame.get("name")?.as_str()?.to_owned(),
                        path: frame
                            .pointer("/source/path")
                            .and_then(Value::as_str)
                            .map(PathBuf::from),
                        line: frame.get("line").and_then(Value::as_u64).unwrap_or(0) as usize,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The first thread id out of a `threads` response body, for a `stopped`
/// event that did not say which thread.
#[must_use]
pub fn first_thread(body: &Value) -> Option<i64> {
    body.get("threads")?
        .as_array()?
        .first()?
        .get("id")?
        .as_i64()
}

/// The scopes out of a `scopes` response body: name and the reference to ask
/// `variables` about, cheapest first the way adapters order them.
#[must_use]
pub fn parse_scopes(body: &Value) -> Vec<(String, i64)> {
    body.get("scopes")
        .and_then(Value::as_array)
        .map(|scopes| {
            scopes
                .iter()
                .filter_map(|scope| {
                    Some((
                        scope.get("name")?.as_str()?.to_owned(),
                        scope.get("variablesReference")?.as_i64()?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The rows out of a `variables` response body.
#[must_use]
pub fn parse_variables(body: &Value) -> Vec<Variable> {
    body.get("variables")
        .and_then(Value::as_array)
        .map(|variables| {
            variables
                .iter()
                .filter_map(|variable| {
                    Some(Variable {
                        name: variable.get("name")?.as_str()?.to_owned(),
                        value: variable
                            .get("value")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        reference: variable
                            .get("variablesReference")
                            .and_then(Value::as_i64)
                            .unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

// --- the client --------------------------------------------------------------

/// One running adapter. The same ownership deal as the language server: main
/// thread holds the process and stdin, the reader thread gets stdout, and
/// dropping the client kills the process along with whatever it was
/// debugging: a debuggee with no debugger attached is a process nobody can
/// see or stop.
pub struct Client {
    child: Child,
    writer: Arc<Mutex<ChildStdin>>,
    stdout: Option<ChildStdout>,
    next_seq: i64,
    /// Which command each outstanding request was, for routing the response.
    pending: HashMap<i64, String>,
}

impl Client {
    pub fn spawn(command: &[&str], cwd: &Path) -> io::Result<Self> {
        let (program, arguments) = command
            .split_first()
            .ok_or_else(|| io::Error::other("empty adapter command"))?;
        let mut child = Command::new(program)
            .args(arguments)
            .current_dir(cwd)
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
            next_seq: 0,
            pending: HashMap::new(),
        })
    }

    /// Takes stdout and starts the thread that reads it. Adapter-initiated
    /// requests are refused right there: both defined ones ask this client to
    /// run something, and it does not.
    pub fn start_reader(&mut self, on_event: impl Fn(Event) + Send + 'static) -> bool {
        let Some(stdout) = self.stdout.take() else {
            return false;
        };
        let writer = Arc::clone(&self.writer);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(message)) = rpc::read_frame(&mut reader) {
                if message.get("type").and_then(Value::as_str) == Some("request") {
                    let refusal = refuse(&message);
                    if let Ok(mut writer) = writer.lock() {
                        let _ = rpc::write_frame(&mut *writer, &refusal);
                    }
                    continue;
                }
                on_event(Event::Message(message));
            }
            on_event(Event::Exited);
        });
        true
    }

    pub fn initialize(&mut self, adapter_id: &str) -> i64 {
        self.request(
            "initialize",
            json!({
                "adapterID": adapter_id,
                "clientID": "tuni",
                "clientName": "Tuni",
                "linesStartAt1": true,
                "columnsStartAt1": true,
                "pathFormat": "path",
                "locale": "en",
            }),
        )
    }

    pub fn launch(&mut self, arguments: Value) -> i64 {
        self.request("launch", arguments)
    }

    /// Replaces the breakpoints in one file. The whole set every time, which
    /// is how the protocol wants it and what makes a toggle simple: send what
    /// the gutter shows now.
    pub fn set_breakpoints(&mut self, path: &Path, lines: &[usize]) -> i64 {
        self.request("setBreakpoints", breakpoints_arguments(path, lines))
    }

    /// Ends the handshake the `initialized` event opened; the program starts
    /// or resumes after this.
    pub fn configuration_done(&mut self) -> i64 {
        self.request("configurationDone", json!({}))
    }

    pub fn threads(&mut self) -> i64 {
        self.request("threads", json!({}))
    }

    pub fn stack_trace(&mut self, thread: i64) -> i64 {
        self.request(
            "stackTrace",
            json!({"threadId": thread, "startFrame": 0, "levels": 32}),
        )
    }

    pub fn scopes(&mut self, frame: i64) -> i64 {
        self.request("scopes", json!({"frameId": frame}))
    }

    pub fn variables(&mut self, reference: i64) -> i64 {
        self.request("variables", json!({"variablesReference": reference}))
    }

    pub fn resume(&mut self, thread: i64) -> i64 {
        self.request("continue", json!({"threadId": thread}))
    }

    pub fn next(&mut self, thread: i64) -> i64 {
        self.request("next", json!({"threadId": thread}))
    }

    pub fn step_in(&mut self, thread: i64) -> i64 {
        self.request("stepIn", json!({"threadId": thread}))
    }

    pub fn step_out(&mut self, thread: i64) -> i64 {
        self.request("stepOut", json!({"threadId": thread}))
    }

    /// Asks the adapter to pack up and take the debuggee with it. The drop
    /// will kill the adapter either way; asking first lets it kill the
    /// debuggee cleanly.
    pub fn disconnect(&mut self) -> i64 {
        self.request("disconnect", json!({"terminateDebuggee": true}))
    }

    /// What a response that just arrived was answering.
    pub fn take_pending(&mut self, request_seq: i64) -> Option<String> {
        self.pending.remove(&request_seq)
    }

    fn request(&mut self, command: &str, arguments: Value) -> i64 {
        self.next_seq += 1;
        let seq = self.next_seq;
        self.pending.insert(seq, command.to_owned());
        let frame = json!({
            "seq": seq,
            "type": "request",
            "command": command,
            "arguments": arguments,
        });
        if let Ok(mut writer) = self.writer.lock() {
            let _ = rpc::write_frame(&mut *writer, &frame);
        }
        seq
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The `setBreakpoints` body: the file and every line that should stop it.
#[must_use]
pub fn breakpoints_arguments(path: &Path, lines: &[usize]) -> Value {
    json!({
        "source": {"path": path},
        "breakpoints": lines.iter().map(|line| json!({"line": line})).collect::<Vec<_>>(),
    })
}

/// The refusal an adapter's own request gets.
fn refuse(message: &Value) -> Value {
    json!({
        "seq": 0,
        "type": "response",
        "request_seq": message.get("seq").cloned().unwrap_or(Value::Null),
        "command": message.get("command").cloned().unwrap_or(Value::Null),
        "success": false,
        "message": "unsupported",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_compiled_language_debugs_with_lldb_and_python_with_debugpy() {
        assert_eq!(debugger_for_language("rust").unwrap().id, "lldb");
        assert_eq!(debugger_for_language("c").unwrap().id, "lldb");
        assert_eq!(debugger_for_language("python").unwrap().id, "debugpy");
        assert!(debugger_for_language("go").is_none());
        assert!(debugger_for_language("lua").is_none());
    }

    #[test]
    fn a_launch_body_speaks_the_adapters_dialect() {
        let lldb = launch_arguments(
            debugger_for_language("rust").unwrap(),
            Path::new("/tmp/demo"),
            &["--fast".to_owned()],
            Path::new("/tmp"),
        );
        assert_eq!(lldb["program"], "/tmp/demo");
        assert_eq!(lldb["args"][0], "--fast");
        assert!(lldb.get("console").is_none());

        let debugpy = launch_arguments(
            debugger_for_language("python").unwrap(),
            Path::new("/tmp/demo.py"),
            &[],
            Path::new("/tmp"),
        );
        assert_eq!(debugpy["console"], "internalConsole");
    }

    #[test]
    fn a_stopped_event_carries_its_thread_and_reason() {
        let message = json!({
            "seq": 9, "type": "event", "event": "stopped",
            "body": {"reason": "breakpoint", "threadId": 1, "allThreadsStopped": true},
        });
        let Incoming::Stopped { thread, reason } = parse_incoming(&message) else {
            panic!("not stopped");
        };
        assert_eq!(thread, Some(1));
        assert_eq!(reason, "breakpoint");
    }

    #[test]
    fn a_failed_response_keeps_its_explanation() {
        let message = json!({
            "seq": 4, "type": "response", "request_seq": 2, "command": "launch",
            "success": false, "message": "could not launch",
        });
        let Incoming::Response {
            request_seq,
            command,
            success,
            message,
            ..
        } = parse_incoming(&message)
        else {
            panic!("not a response");
        };
        assert_eq!(
            (request_seq, command.as_str(), success),
            (2, "launch", false)
        );
        assert_eq!(message.as_deref(), Some("could not launch"));
    }

    #[test]
    fn a_stack_keeps_its_frames_even_where_source_is_missing() {
        let body = json!({"stackFrames": [
            {"id": 1, "name": "main", "line": 4,
             "source": {"path": "/tmp/demo/src/main.rs"}},
            {"id": 2, "name": "__libc_start_main", "line": 0},
        ]});
        let stack = parse_stack(&body);
        assert_eq!(stack.len(), 2);
        assert_eq!(
            stack[0].path.as_deref(),
            Some(Path::new("/tmp/demo/src/main.rs"))
        );
        assert_eq!(stack[0].line, 4);
        assert!(stack[1].path.is_none());
    }

    #[test]
    fn variables_and_scopes_reduce_to_rows() {
        let scopes = json!({"scopes": [
            {"name": "Locals", "variablesReference": 100, "expensive": false},
            {"name": "Registers", "variablesReference": 200, "expensive": true},
        ]});
        assert_eq!(parse_scopes(&scopes)[0], ("Locals".to_owned(), 100));

        let variables = json!({"variables": [
            {"name": "greeting", "value": "\"hello\"", "variablesReference": 0},
            {"name": "items", "value": "Vec(3)", "variablesReference": 12},
        ]});
        let rows = parse_variables(&variables);
        assert_eq!(rows[0].reference, 0);
        assert_eq!(rows[1].name, "items");
        assert_eq!(rows[1].reference, 12);
    }

    #[test]
    fn an_output_event_says_where_the_text_belongs() {
        let message = json!({
            "seq": 5, "type": "event", "event": "output",
            "body": {"category": "stdout", "output": "hello\n"},
        });
        let Incoming::Output { text, category } = parse_incoming(&message) else {
            panic!("not output");
        };
        assert_eq!((text.as_str(), category.as_str()), ("hello\n", "stdout"));
    }

    #[test]
    fn the_breakpoints_body_lists_every_line_the_gutter_shows() {
        let body = breakpoints_arguments(Path::new("/tmp/a.rs"), &[3, 10]);
        assert_eq!(body["source"]["path"], "/tmp/a.rs");
        assert_eq!(body["breakpoints"][1]["line"], 10);
    }

    #[test]
    fn an_adapters_own_request_is_refused_with_its_seq() {
        let request = json!({
            "seq": 41, "type": "request", "command": "runInTerminal", "arguments": {},
        });
        let refusal = refuse(&request);
        assert_eq!(refusal["request_seq"], 41);
        assert_eq!(refusal["success"], false);
    }
}
