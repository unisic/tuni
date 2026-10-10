//! Discord Rich Presence over the Discord client's local IPC socket.
//!
//! Discord on Linux listens on a Unix socket at `$XDG_RUNTIME_DIR/discord-ipc-0`
//! (up to `discord-ipc-9`, and under its own directory when it is a Flatpak or a
//! Snap). The protocol is JSON behind an 8-byte header: a little-endian opcode,
//! then a little-endian length. That is small enough to speak with std's
//! `UnixStream` and serde, so no Discord library and no network crate is linked.
//!
//! What goes out is decided by [`Discord`], and everything in it ends up on a
//! profile other people read: a file name, a program, a project, a remote.

use std::env;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::settings::Discord;

/// Tuni's application on Discord, which is what the profile names.
pub const CLIENT_ID: &str = "1549023340792913971";

/// The application's own icon, as Discord's CDN serves it.
pub const TUNI_ICON_URL: &str =
    "https://cdn.discordapp.com/app-icons/1549023340792913971/f6148aefc2c9ba0cafc0a57181c64f47.png";

/// The language icons the VS Code presence extension draws, so a file looks
/// the same on a profile whichever of the two it was edited in.
pub const ICON_BASE_URL: &str =
    "https://raw.githubusercontent.com/LeonardSSH/vscord/main/assets/icons";

pub const OP_HANDSHAKE: u32 = 0;
pub const OP_FRAME: u32 = 1;
pub const OP_CLOSE: u32 = 2;
pub const OP_PING: u32 = 3;
pub const OP_PONG: u32 = 4;

/// Discord refuses a whole activity over one string longer than this, so every
/// string is cut to it rather than losing the update.
const MAX_TEXT: usize = 128;

/// Rich Presence button shown on the user's profile card.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActivityButton {
    pub label: String,
    pub url: String,
}

/// Rich Presence activity payload as expected by Discord's `SET_ACTIVITY` command.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamps: Option<ActivityTimestamps>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assets: Option<ActivityAssets>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buttons: Option<Vec<ActivityButton>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<bool>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActivityTimestamps {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActivityAssets {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub large_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub large_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub small_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub small_text: Option<String>,
}

/// What the focused pane is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActivityKind {
    Editing {
        file_name: String,
    },
    /// A shell, by its pid. What it is running is read when the activity is
    /// built rather than when the pane changed, because the title a shell sets
    /// for a command arrives before the command is in the foreground.
    Terminal {
        shell_pid: Option<u32>,
    },
    Git,
    Idle,
}

/// What the window knows without reading anything: cheap enough to build on
/// the main thread every time a pane changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PresenceStatus {
    /// The sidebar project, which names the workspace only outside a
    /// repository: a shell that went to another checkout is working there.
    pub project: Option<String>,
    /// Where the branch and the remote are read from.
    pub directory: Option<PathBuf>,
    pub kind: ActivityKind,
}

/// What [`look_up`] reads for a [`PresenceStatus`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Lookup {
    /// The repository's directory name, which names the workspace.
    pub repository: Option<String>,
    pub branch: Option<String>,
    pub repository_url: Option<String>,
    pub program: Option<String>,
}

/// Finds the branch, the remote and the foreground program for `status`, and
/// only what `show` will put on the profile.
///
/// Up to three `git` runs and two small `/proc` reads, so it belongs on a worker
/// thread. `git` rather than reading `.git` by hand, so a linked worktree, an
/// included config and an `insteadOf` rewrite come out the way the command
/// line would say them.
#[must_use]
pub fn look_up(status: &PresenceStatus, show: Discord) -> Lookup {
    let git = |args: &[&str]| {
        let output = crate::git::run(args, status.directory.as_deref()?);
        let line = output.stdout.trim();
        (output.is_ok() && !line.is_empty()).then(|| line.to_owned())
    };
    Lookup {
        repository: show
            .project
            .then(|| git(&["rev-parse", "--show-toplevel"]))
            .flatten()
            .and_then(|top| Some(Path::new(&top).file_name()?.to_string_lossy().into_owned())),
        branch: show
            .project
            .then(|| git(&["branch", "--show-current"]))
            .flatten(),
        repository_url: show
            .repository
            .then(|| git(&["remote", "get-url", "origin"]))
            .flatten()
            .and_then(|url| normalize_repo_url(&url)),
        program: match status.kind {
            // The `comm` name and never the command line, which can hold a
            // password typed as an argument.
            ActivityKind::Terminal {
                shell_pid: Some(pid),
            } if show.file => crate::info::foreground(pid),
            _ => None,
        },
    }
}

/// Detects the programming language from a file name, returning `(icon_name, display_name)`.
#[must_use]
pub fn detect_language(file_name: &str) -> (&'static str, &'static str) {
    let lower = file_name.to_ascii_lowercase();
    if lower == "dockerfile" || lower.starts_with("dockerfile.") {
        return ("docker", "Dockerfile");
    }
    if lower == "makefile" || lower == "gnumakefile" {
        return ("shell", "Makefile");
    }
    if lower == "cargo.toml" || lower == "cargo.lock" {
        return ("cargo", "Cargo");
    }

    let ext = Path::new(&lower)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("");

    match ext {
        "rs" => ("rust", "Rust"),
        "py" | "pyw" | "pyi" => ("python", "Python"),
        "js" | "mjs" | "cjs" => ("js", "JavaScript"),
        "jsx" => ("jsx", "React JSX"),
        "ts" | "mts" | "cts" => ("ts", "TypeScript"),
        "tsx" => ("tsx", "React TSX"),
        "c" | "h" => ("c", "C"),
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => ("cpp", "C++"),
        "cs" => ("csharp", "C#"),
        "go" => ("go", "Go"),
        "zig" | "zon" => ("zig", "Zig"),
        "html" | "htm" => ("html", "HTML"),
        "css" => ("css", "CSS"),
        "scss" | "sass" => ("scss", "Sass"),
        "json" | "jsonc" => ("json", "JSON"),
        "toml" => ("toml", "TOML"),
        "yaml" | "yml" => ("yaml", "YAML"),
        "md" | "markdown" => ("markdown", "Markdown"),
        "sh" | "bash" | "zsh" => ("shell", "Shell"),
        "lua" => ("lua", "Lua"),
        "php" => ("php", "PHP"),
        "java" => ("java", "Java"),
        "kt" | "kts" => ("kotlin", "Kotlin"),
        "rb" => ("ruby", "Ruby"),
        "swift" => ("swift", "Swift"),
        "sql" => ("sql", "SQL"),
        "vue" => ("vue", "Vue"),
        "svelte" => ("svelte", "Svelte"),
        "xml" => ("xml", "XML"),
        _ => ("text", "Plain Text"),
    }
}

/// Builds the direct HTTPS image URL for a given icon key.
#[must_use]
pub fn icon_url(icon: &str) -> String {
    format!("{ICON_BASE_URL}/{icon}.png")
}

/// The web page of a git remote, for a button anyone can press.
///
/// Credentials in a remote (`https://user:token@host/...`) are the user's and
/// are dropped, as is a port, which belongs to the transport and not to the
/// page. A remote that is not on a host, a path or `file://`, has no page.
#[must_use]
pub fn normalize_repo_url(url: &str) -> Option<String> {
    let url = url.trim();
    // A URL ends its host at the first slash; scp-like syntax has no scheme
    // and ends it at the first colon.
    let (authority, path) = match url.split_once("://") {
        Some(("https" | "http" | "ssh" | "git" | "git+ssh", rest)) => rest.split_once('/')?,
        Some(_) => return None,
        None => url
            .split_once(':')
            .filter(|(host, _)| !host.contains('/'))?,
    };
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = host.split_once(':').map_or(host, |(host, _)| host);
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    (!host.is_empty() && !path.is_empty()).then(|| format!("https://{host}/{path}"))
}

/// Cuts `text` to what Discord accepts, on a character boundary.
fn clip(text: String) -> String {
    match text.char_indices().nth(MAX_TEXT) {
        Some((end, _)) => text[..end].to_owned(),
        None => text,
    }
}

/// The activity for `status`, saying only what `show` allows. `start` is the
/// unix time the elapsed clock counts from.
#[must_use]
pub fn format_activity(
    status: &PresenceStatus,
    found: &Lookup,
    show: Discord,
    start: u64,
) -> Activity {
    let (details, small_image, small_text) = match &status.kind {
        ActivityKind::Editing { file_name } => {
            let (icon, language) = detect_language(file_name);
            let details = if show.file {
                format!("Editing {file_name}")
            } else {
                format!("Writing {language}")
            };
            (details, Some(icon_url(icon)), Some(language.to_owned()))
        }
        ActivityKind::Terminal { .. } => {
            let details = match &found.program {
                Some(program) => format!("Running {program}"),
                None => "In the terminal".to_owned(),
            };
            (
                details,
                Some(icon_url("shell")),
                Some("Terminal".to_owned()),
            )
        }
        ActivityKind::Git => (
            "Reviewing changes".to_owned(),
            Some(icon_url("git")),
            Some("Git".to_owned()),
        ),
        ActivityKind::Idle => ("Idling".to_owned(), None, None),
    };

    let project = found
        .repository
        .as_deref()
        .or(status.project.as_deref())
        .map(str::trim)
        .filter(|project| show.project && !project.is_empty());
    let state = project.map(|project| match &found.branch {
        Some(branch) => format!("Workspace: {project} ({branch})"),
        None => format!("Workspace: {project}"),
    });

    let buttons = found
        .repository_url
        .clone()
        .filter(|_| show.repository)
        .map(|url| {
            vec![ActivityButton {
                label: "View Repository".to_owned(),
                url,
            }]
        });

    Activity {
        details: Some(clip(details)),
        state: state.map(clip),
        timestamps: Some(ActivityTimestamps {
            start: Some(start),
            end: None,
        }),
        assets: Some(ActivityAssets {
            large_image: Some(TUNI_ICON_URL.to_owned()),
            large_text: Some("Tuni".to_owned()),
            small_image,
            small_text: small_text.map(clip),
        }),
        buttons,
        instance: Some(false),
    }
}

/// Encodes an opcode and payload into a Discord IPC wire frame.
#[must_use]
pub fn encode_frame(opcode: u32, payload: &str) -> Vec<u8> {
    let len = payload.len() as u32;
    let mut bytes = Vec::with_capacity(8 + payload.len());
    bytes.extend_from_slice(&opcode.to_le_bytes());
    bytes.extend_from_slice(&len.to_le_bytes());
    bytes.extend_from_slice(payload.as_bytes());
    bytes
}

/// Decodes the 8-byte header into (opcode, length).
#[must_use]
pub fn decode_header(header: &[u8; 8]) -> (u32, u32) {
    let opcode = u32::from_le_bytes(header[0..4].try_into().unwrap());
    let length = u32::from_le_bytes(header[4..8].try_into().unwrap());
    (opcode, length)
}

/// Writes a frame to a stream.
pub fn write_frame(writer: &mut impl Write, opcode: u32, payload: &Value) -> io::Result<()> {
    let body = payload.to_string();
    let frame = encode_frame(opcode, &body);
    writer.write_all(&frame)?;
    writer.flush()
}

/// Reads a frame from a stream.
pub fn read_frame(reader: &mut impl Read) -> io::Result<(u32, Value)> {
    let mut header = [0u8; 8];
    reader.read_exact(&mut header)?;
    let (opcode, length) = decode_header(&header);

    // Discord IPC messages are typically small (a few KB at most). Cap at 16MB.
    if length > 16 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Discord IPC frame exceeds maximum supported size",
        ));
    }

    let mut body = vec![0u8; length as usize];
    reader.read_exact(&mut body)?;

    let value: Value = serde_json::from_slice(&body).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Invalid JSON frame: {e}"),
        )
    })?;

    Ok((opcode, value))
}

/// Every Discord IPC socket that exists, in the order they are tried.
///
/// All of them rather than the first: a client that crashed leaves its socket
/// behind, and a stale one in the runtime directory would otherwise hide a
/// Flatpak Discord that is running.
#[must_use]
pub fn sockets() -> Vec<PathBuf> {
    let mut base_dirs = Vec::new();

    if let Some(xdg) = env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        base_dirs.push(xdg.clone());
        // Flatpak Discord
        base_dirs.push(xdg.join("app").join("com.discordapp.Discord"));
        // Vesktop (Vencord)
        base_dirs.push(xdg.join("app").join("dev.vencord.Vesktop"));
        // WebCord
        base_dirs.push(xdg.join("app").join("io.github.spacingbat3.webcord"));
        // Snap Discord
        base_dirs.push(xdg.join("snap.discord"));
    }

    // Standard Linux /run/user/<uid> fallback
    let uid = unsafe { libc::getuid() };
    base_dirs.push(PathBuf::from(format!("/run/user/{uid}")));

    if let Some(tmp) = env::var_os("TMPDIR").map(PathBuf::from) {
        base_dirs.push(tmp);
    }
    base_dirs.push(PathBuf::from("/tmp"));

    let mut found = Vec::new();
    for base in base_dirs {
        for i in 0..10 {
            let socket_path = base.join(format!("discord-ipc-{i}"));
            if socket_path.exists() && !found.contains(&socket_path) {
                found.push(socket_path);
            }
        }
    }
    found
}

/// IPC client to communicate with Discord over Unix domain socket.
pub struct DiscordIpc {
    stream: UnixStream,
    nonce: u64,
}

impl DiscordIpc {
    /// Connects and introduces `client_id` to the first socket that answers.
    pub fn connect(client_id: &str) -> io::Result<Self> {
        let mut last = io::Error::new(io::ErrorKind::NotFound, "no Discord IPC socket found");
        for path in sockets() {
            match Self::connect_to(&path).and_then(|mut ipc| ipc.handshake(client_id).map(|_| ipc))
            {
                Ok(ipc) => return Ok(ipc),
                Err(error) => {
                    last = io::Error::new(error.kind(), format!("{}: {error}", path.display()))
                }
            }
        }
        Err(last)
    }

    /// Connects directly to a specified socket path.
    pub fn connect_to(path: &Path) -> io::Result<Self> {
        let stream = UnixStream::connect(path)?;
        // Set 3 second read/write timeouts so IPC calls never block the caller indefinitely.
        let timeout = Some(Duration::from_secs(3));
        let _ = stream.set_read_timeout(timeout);
        let _ = stream.set_write_timeout(timeout);

        Ok(Self { stream, nonce: 0 })
    }

    /// Performs the handshake with the given client ID.
    pub fn handshake(&mut self, client_id: &str) -> io::Result<Value> {
        let payload = serde_json::json!({
            "v": 1,
            "client_id": client_id,
        });

        write_frame(&mut self.stream, OP_HANDSHAKE, &payload)?;
        let (opcode, resp) = self.reply()?;

        if opcode == OP_CLOSE {
            let msg = resp
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Handshake rejected");
            return Err(io::Error::new(io::ErrorKind::ConnectionRefused, msg));
        }

        Ok(resp)
    }

    /// Sets or clears the current activity. Passing `None` clears presence.
    ///
    /// An activity Discord refuses is `InvalidInput` with Discord's reason, and
    /// the connection is still good; any other error means it is not.
    pub fn set_activity(&mut self, activity: Option<&Activity>) -> io::Result<Value> {
        self.nonce += 1;
        let pid = std::process::id();
        let payload = serde_json::json!({
            "cmd": "SET_ACTIVITY",
            "args": {
                "pid": pid,
                "activity": activity,
            },
            "nonce": self.nonce.to_string(),
        });

        write_frame(&mut self.stream, OP_FRAME, &payload)?;
        let (opcode, resp) = self.reply()?;

        if opcode == OP_CLOSE {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "Discord connection closed during set_activity",
            ));
        }
        if resp["evt"] == "ERROR" {
            let message = resp["data"]["message"]
                .as_str()
                .unwrap_or("activity refused");
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                message.to_owned(),
            ));
        }

        Ok(resp)
    }

    /// The next frame that is not a ping, answering any ping on the way so
    /// Discord does not drop the connection as dead.
    fn reply(&mut self) -> io::Result<(u32, Value)> {
        loop {
            let (opcode, value) = read_frame(&mut self.stream)?;
            if opcode != OP_PING {
                return Ok((opcode, value));
            }
            write_frame(&mut self.stream, OP_PONG, &value)?;
        }
    }

    /// Closes the connection gracefully.
    pub fn close(&mut self) -> io::Result<()> {
        let payload = serde_json::json!({"v": 1});
        let _ = write_frame(&mut self.stream, OP_CLOSE, &payload);
        self.stream.shutdown(std::net::Shutdown::Both)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const ALL: Discord = Discord {
        presence: true,
        project: true,
        file: true,
        repository: true,
    };

    fn status(kind: ActivityKind) -> PresenceStatus {
        PresenceStatus {
            project: Some("tuni".to_owned()),
            directory: None,
            kind,
        }
    }

    fn found() -> Lookup {
        Lookup {
            repository: Some("tuni".to_owned()),
            branch: Some("main".to_owned()),
            repository_url: Some("https://github.com/unisic/tuni".to_owned()),
            program: Some("cargo".to_owned()),
        }
    }

    #[test]
    fn a_frame_encodes_and_decodes_header() {
        let frame = encode_frame(OP_FRAME, r#"{"cmd":"TEST"}"#);
        assert!(frame.len() >= 8);
        let header: [u8; 8] = frame[..8].try_into().unwrap();
        let (op, len) = decode_header(&header);
        assert_eq!(op, OP_FRAME);
        assert_eq!(len as usize, r#"{"cmd":"TEST"}"#.len());
    }

    #[test]
    fn writing_and_reading_frames_roundtrips() {
        let mut buffer = Vec::new();
        let payload = serde_json::json!({"key": "value", "count": 42});
        write_frame(&mut buffer, OP_FRAME, &payload).unwrap();

        let mut cursor = Cursor::new(buffer);
        let (op, val) = read_frame(&mut cursor).unwrap();
        assert_eq!(op, OP_FRAME);
        assert_eq!(val, payload);
    }

    #[test]
    fn an_edited_file_is_named_with_its_language_and_workspace() {
        let editing = status(ActivityKind::Editing {
            file_name: "window.rs".to_owned(),
        });
        let activity = format_activity(&editing, &found(), ALL, 1_700_000_000);
        assert_eq!(activity.details.as_deref(), Some("Editing window.rs"));
        assert_eq!(activity.state.as_deref(), Some("Workspace: tuni (main)"));
        assert_eq!(
            activity.timestamps.and_then(|t| t.start),
            Some(1_700_000_000)
        );
        let assets = activity.assets.unwrap();
        assert_eq!(assets.large_image.as_deref(), Some(TUNI_ICON_URL));
        assert!(assets.small_image.unwrap().ends_with("/rust.png"));
        assert_eq!(assets.small_text.as_deref(), Some("Rust"));
        let buttons = activity.buttons.unwrap();
        assert_eq!(buttons[0].label, "View Repository");
        assert_eq!(buttons[0].url, "https://github.com/unisic/tuni");
    }

    #[test]
    fn a_hidden_file_leaves_only_its_language() {
        let editing = status(ActivityKind::Editing {
            file_name: "secret-plan.ts".to_owned(),
        });
        let show = Discord { file: false, ..ALL };
        let activity = format_activity(&editing, &found(), show, 0);
        assert_eq!(activity.details.as_deref(), Some("Writing TypeScript"));
        assert!(
            activity
                .assets
                .unwrap()
                .small_image
                .unwrap()
                .ends_with("/ts.png")
        );
    }

    #[test]
    fn a_hidden_project_takes_the_branch_and_the_button_only_when_asked() {
        let editing = status(ActivityKind::Editing {
            file_name: "main.py".to_owned(),
        });
        let show = Discord {
            project: false,
            repository: false,
            ..ALL
        };
        let activity = format_activity(&editing, &found(), show, 0);
        assert_eq!(activity.state, None);
        assert!(activity.buttons.is_none());
    }

    #[test]
    fn a_repository_names_the_workspace_over_the_project_it_was_opened_in() {
        let terminal = status(ActivityKind::Terminal { shell_pid: None });
        let elsewhere = Lookup {
            repository: Some("unisic".to_owned()),
            ..found()
        };
        let activity = format_activity(&terminal, &elsewhere, ALL, 0);
        assert_eq!(activity.state.as_deref(), Some("Workspace: unisic (main)"));

        let outside = Lookup::default();
        let activity = format_activity(&terminal, &outside, ALL, 0);
        assert_eq!(activity.state.as_deref(), Some("Workspace: tuni"));
    }

    #[test]
    fn a_terminal_names_the_program_in_front_and_nothing_else() {
        let terminal = status(ActivityKind::Terminal { shell_pid: Some(1) });
        let activity = format_activity(&terminal, &found(), ALL, 0);
        assert_eq!(activity.details.as_deref(), Some("Running cargo"));
        assert!(
            activity
                .assets
                .unwrap()
                .small_image
                .unwrap()
                .ends_with("/shell.png")
        );

        let idle_shell = Lookup {
            program: None,
            ..found()
        };
        let activity = format_activity(&terminal, &idle_shell, ALL, 0);
        assert_eq!(activity.details.as_deref(), Some("In the terminal"));
    }

    #[test]
    fn a_git_panel_describes_repository_activity() {
        let activity = format_activity(&status(ActivityKind::Git), &found(), ALL, 0);
        assert_eq!(activity.details.as_deref(), Some("Reviewing changes"));
        assert!(
            activity
                .assets
                .unwrap()
                .small_image
                .unwrap()
                .ends_with("/git.png")
        );
    }

    #[test]
    fn nothing_open_is_idling_without_a_workspace() {
        let idle = PresenceStatus {
            project: None,
            directory: None,
            kind: ActivityKind::Idle,
        };
        let activity = format_activity(&idle, &Lookup::default(), ALL, 0);
        assert_eq!(activity.details.as_deref(), Some("Idling"));
        assert_eq!(activity.state, None);
        assert!(activity.assets.unwrap().small_image.is_none());
    }

    #[test]
    fn a_name_longer_than_discord_takes_is_cut_on_a_character() {
        let editing = status(ActivityKind::Editing {
            file_name: "ż".repeat(200),
        });
        let details = format_activity(&editing, &found(), ALL, 0).details.unwrap();
        assert_eq!(details.chars().count(), MAX_TEXT);
    }

    #[test]
    fn a_remote_becomes_its_web_page_without_credentials_or_port() {
        for (remote, page) in [
            (
                "git@github.com:unisic/tuni.git",
                Some("https://github.com/unisic/tuni"),
            ),
            (
                "gitea@code.example:team/app",
                Some("https://code.example/team/app"),
            ),
            (
                "ssh://git@github.com:22/owner/repo.git",
                Some("https://github.com/owner/repo"),
            ),
            (
                "https://gitlab.com/group/project.git",
                Some("https://gitlab.com/group/project"),
            ),
            (
                "https://github.com/unisic/tuni/",
                Some("https://github.com/unisic/tuni"),
            ),
            (
                "https://user:ghp_secret@github.com/unisic/tuni.git",
                Some("https://github.com/unisic/tuni"),
            ),
            ("file:///local/repo", None),
            ("/srv/git/repo.git", None),
            ("../sibling", None),
        ] {
            assert_eq!(normalize_repo_url(remote).as_deref(), page, "{remote}");
        }
    }

    #[test]
    fn a_lookup_reads_the_branch_and_a_clean_remote_from_git() {
        let path = env::temp_dir().join(format!("tuni-discord-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        for args in [
            &["init", "-b", "feature"][..],
            &[
                "remote",
                "add",
                "origin",
                "https://me:token@github.com/me/app.git",
            ],
        ] {
            assert!(crate::git::run(args, &path).is_ok());
        }
        let editing = PresenceStatus {
            project: Some("app".to_owned()),
            directory: Some(path.clone()),
            kind: ActivityKind::Editing {
                file_name: "a.rs".to_owned(),
            },
        };

        let found = look_up(&editing, ALL);
        assert_eq!(
            found.repository.as_deref(),
            path.file_name().and_then(|name| name.to_str())
        );
        assert_eq!(found.branch.as_deref(), Some("feature"));
        assert_eq!(
            found.repository_url.as_deref(),
            Some("https://github.com/me/app")
        );
        assert_eq!(
            look_up(
                &editing,
                Discord {
                    repository: false,
                    ..ALL
                }
            )
            .repository_url,
            None
        );
        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn a_ping_is_answered_and_the_reply_after_it_returned() {
        let mut incoming = Vec::new();
        write_frame(&mut incoming, OP_PING, &serde_json::json!({"n": 1})).unwrap();
        write_frame(&mut incoming, OP_FRAME, &serde_json::json!({"evt": null})).unwrap();
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        theirs.write_all(&incoming).unwrap();
        let mut ipc = DiscordIpc {
            stream: ours,
            nonce: 0,
        };

        let (opcode, _) = ipc.reply().unwrap();
        assert_eq!(opcode, OP_FRAME);
        let (opcode, value) = read_frame(&mut theirs).unwrap();
        assert_eq!((opcode, value), (OP_PONG, serde_json::json!({"n": 1})));
    }

    #[test]
    fn a_stream_ending_early_returns_an_unexpected_eof_error() {
        let incomplete = vec![0u8; 4];
        let mut cursor = Cursor::new(incomplete);
        let result = read_frame(&mut cursor);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }
}
