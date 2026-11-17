//! What another program running as the same user can read and do through
//! `dev.unisic.Tuni.Remote`, the D-Bus interface Tuni offers while
//! Preferences - Session - Remote Control is on: the projects, tabs and panes,
//! a pane's text, typing and keys, and what the coding agent in one has spent
//! and which slash commands it takes.
//!
//! Off by default. A program that can type into a shell can do whatever that
//! shell can, and an install script running as you is a program running as
//! you. The session bus is the door because it already takes callers from this
//! user alone, and because the window's actions are on it too: `select-tab`,
//! `new-project` and the rest are `org.gtk.Actions` on the window's own path,
//! so a client needs one connection for everything.
//!
//! This half is what a test can reach: the interface's description, the JSON
//! the state and the usage go out as, and the limits on what a call may ask.
//! The window answers the calls in `tuni-gtk`.

use std::path::Path;

use serde::Serialize;
use serde_json::json;

use crate::panes::{Content, Pane};
use crate::usage::{Agent, Snapshot};
use crate::workspace::{Id, Workspace};

pub const INTERFACE: &str = "dev.unisic.Tuni.Remote";

/// Under the application's own object path, beside the windows GTK puts there.
pub const PATH: &str = "Remote";

pub const XML: &str = r#"<node>
  <interface name="dev.unisic.Tuni.Remote">
    <method name="State">
      <arg type="s" name="json" direction="out"/>
    </method>
    <method name="Text">
      <arg type="t" name="pane" direction="in"/>
      <arg type="u" name="lines" direction="in"/>
      <arg type="s" name="text" direction="out"/>
    </method>
    <method name="Type">
      <arg type="t" name="pane" direction="in"/>
      <arg type="s" name="text" direction="in"/>
    </method>
    <method name="Key">
      <arg type="t" name="pane" direction="in"/>
      <arg type="s" name="key" direction="in"/>
    </method>
    <method name="Focus">
      <arg type="t" name="pane" direction="in"/>
    </method>
    <method name="NewTab">
      <arg type="t" name="project" direction="in"/>
    </method>
    <method name="CloseTab">
      <arg type="t" name="tab" direction="in"/>
    </method>
    <method name="Usage">
      <arg type="t" name="pane" direction="in"/>
      <arg type="s" name="json" direction="out"/>
    </method>
    <method name="Commands">
      <arg type="t" name="pane" direction="in"/>
      <arg type="s" name="json" direction="out"/>
    </method>
  </interface>
</node>"#;

/// The most lines one `Text` call hands back: a screenful many times over,
/// and a bound on a string built on the main thread.
pub const TEXT_LINES_MAX: u32 = 2_000;

/// The longest text one `Type` call may carry. A message, not a file: a file
/// belongs in an editor pane, and a paste this size already takes a shell a
/// moment to echo.
pub const TYPE_MAX: usize = 64 * 1024;

/// How many lines `Text` gives for an ask of `asked`: at least one, at most
/// [`TEXT_LINES_MAX`].
#[must_use]
pub fn text_lines(asked: u32) -> usize {
    asked.clamp(1, TEXT_LINES_MAX) as usize
}

/// A key `Key` names: one base key and the modifiers held with it, as in
/// `enter`, `shift-tab`, `ctrl-c` or `alt-enter`. The window turns it into
/// bytes with the pane's own encoder, so an arrow is `ESC [ A` or `ESC O A`
/// as the program asked and a program on the Kitty protocol gets its codes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Chord {
    pub base: Base,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Base {
    Enter,
    Escape,
    Tab,
    Backspace,
    Space,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    /// A lowercase letter or a digit: a menu's `1`, a prompt's `y`, the
    /// letter of a `ctrl-` shortcut.
    Char(char),
}

/// Reads a key name. Modifiers come first, each once, in any order.
#[must_use]
pub fn chord(name: &str) -> Option<Chord> {
    let (mut ctrl, mut alt, mut shift) = (false, false, false);
    let mut rest = name;
    loop {
        let (flag, tail) = if let Some(tail) = rest.strip_prefix("ctrl-") {
            (&mut ctrl, tail)
        } else if let Some(tail) = rest.strip_prefix("alt-") {
            (&mut alt, tail)
        } else if let Some(tail) = rest.strip_prefix("shift-") {
            (&mut shift, tail)
        } else {
            break;
        };
        if std::mem::replace(flag, true) {
            return None;
        }
        rest = tail;
    }
    let base = match rest {
        "enter" => Base::Enter,
        "esc" => Base::Escape,
        "tab" => Base::Tab,
        "backspace" => Base::Backspace,
        "space" => Base::Space,
        "up" => Base::Up,
        "down" => Base::Down,
        "left" => Base::Left,
        "right" => Base::Right,
        "home" => Base::Home,
        "end" => Base::End,
        "page-up" => Base::PageUp,
        "page-down" => Base::PageDown,
        _ => {
            let mut chars = rest.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_lowercase() || c.is_ascii_digit() => Base::Char(c),
                _ => return None,
            }
        }
    };
    Some(Chord {
        base,
        ctrl,
        alt,
        shift,
    })
}

/// A slash command the agent in a pane takes, for a remote to offer rather
/// than have somebody remember. Sent the way anything else is: `Type` then
/// `Key enter`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Command {
    /// With its slash: `/compact`.
    pub name: String,
    /// What it takes after the name, as the agent's own help writes it.
    pub hint: String,
    pub description: String,
    /// `builtin`, `skill`, `command` or `prompt`.
    pub source: &'static str,
}

/// The most commands one answer carries. A skills directory is somebody's
/// to fill, and this is built on a worker thread but sent in one message.
pub const COMMANDS_MAX: usize = 1_000;

// shortcut: the built-ins are copied from agent 0.1.82's `/help` and from the
// command table in Claude Code 2.1.322; a command a later version adds is
// missing until these lists are, and one it drops answers as unknown.
const AGENT_BUILTINS: &[(&str, &str, &str)] = &[
    ("clear", "", "Start a fresh conversation"),
    ("resume", "", "Resume a saved session"),
    ("rename-title", "", "Rename the current session"),
    (
        "rename-title-ai",
        "",
        "Name the session again from its first prompt",
    ),
    (
        "clone",
        "",
        "Duplicate the current session at the current position",
    ),
    (
        "import",
        "<file.jsonl>",
        "Import and resume a session from a JSONL file",
    ),
    (
        "export",
        "[path]",
        "Export session: .jsonl (default) or .md for plain text",
    ),
    (
        "workspace",
        "[add <path> | remove <path> | list]",
        "Extra directories this session also works in",
    ),
    ("model", "[search]", "Select model"),
    ("settings", "", "Open settings menu"),
    ("permissions", "<mode>", "ask, auto or full access"),
    ("thinking", "<level>", "Set thinking level"),
    ("effort", "<level>", "Alias for /thinking"),
    (
        "fast",
        "",
        "Step speed: standard, fast, ultrafast; quicker output, billed higher",
    ),
    (
        "ponytail",
        "<off | lite | full | ultra>",
        "Set the ponytail level",
    ),
    (
        "caveman",
        "<off | full | ultra | wenyan>",
        "Set the caveman level",
    ),
    (
        "voice",
        "",
        "Dictate in any language with whisper.cpp, Enter to stop",
    ),
    (
        "context",
        "",
        "Show what fills the context window and how well it caches",
    ),
    (
        "context-window",
        "<100k to 1M | auto>",
        "Set this model's context window",
    ),
    ("copy", "", "Copy last agent message to clipboard"),
    (
        "copy-session-path",
        "",
        "Copy this session's file path to clipboard",
    ),
    ("session", "", "Show the session id, file and context use"),
    ("hotkeys", "", "Show all keyboard shortcuts"),
    ("compact", "[note]", "Summarize older messages"),
    (
        "handoff",
        "[next task]",
        "Summarize this conversation and continue in a fresh session",
    ),
    (
        "plan",
        "[task | export [file] | implement | exit]",
        "Plan mode, read-only until you pick what to do with the plan",
    ),
    ("todos", "", "Show and edit the todo list"),
    ("plugins", "", "Install, enable and disable plugins"),
    ("skills", "", "Enable and disable skills"),
    (
        "trust",
        "",
        "Load this project's .agents files and run its edit hooks",
    ),
    (
        "untrust",
        "",
        "Stop loading this project's .agents files and running its edit hooks",
    ),
    (
        "mcp",
        "",
        "Show MCP servers: reconnect, log in, enable and disable",
    ),
    (
        "usage",
        "",
        "Show token usage, cost and subscription limits",
    ),
    ("history", "", "Search previous prompts"),
    (
        "changes",
        "",
        "List files changed in this session and show their diffs",
    ),
    (
        "skill-usage",
        "",
        "Show how often each skill was used, least used first",
    ),
    ("rtk", "", "Show rtk token savings"),
    ("tasks", "", "Show background shells and subagent runs"),
    (
        "workflows",
        "",
        "Show workflow runs: progress, pause, stop, save as a command",
    ),
    (
        "goal",
        "[objective | pause | resume | stop | done]",
        "Set a goal the agent keeps working toward, or manage it",
    ),
    (
        "btw",
        "<question>",
        "Ask a side question without touching the session",
    ),
    (
        "worktree",
        "[name]",
        "Move this session to a git worktree or remove one",
    ),
    ("login", "", "Add a Claude or ChatGPT account or an API key"),
    ("logout", "", "Remove a saved account"),
    (
        "account",
        "",
        "Show accounts and their usage; pick one to use",
    ),
    (
        "detach",
        "",
        "Close this terminal's view and leave the session running",
    ),
    (
        "attach",
        "",
        "Show the running sessions: switch this terminal to one, or stop it",
    ),
    (
        "env",
        "[name]",
        "Pick, add or remove the computers sessions can run on",
    ),
    ("quit", "", "Quit agent"),
    ("help", "", "Show available slash commands"),
];

const CLAUDE_BUILTINS: &[(&str, &str, &str)] = &[
    (
        "clear",
        "",
        "Start a new session with empty context; the previous one stays resumable",
    ),
    (
        "compact",
        "[instructions]",
        "Free up context by summarizing the conversation so far",
    ),
    ("resume", "", "Resume a previous conversation"),
    (
        "rewind",
        "",
        "Go back to an earlier point in the conversation",
    ),
    ("model", "", "Set the model"),
    ("effort", "", "Set effort level for model usage"),
    (
        "plan",
        "",
        "Enable plan mode or view the current session plan",
    ),
    ("goal", "", "Set a goal Claude checks before stopping"),
    (
        "btw",
        "<question>",
        "Ask a quick side question without interrupting the main conversation",
    ),
    (
        "context",
        "",
        "Visualize current context usage as a colored grid",
    ),
    (
        "usage",
        "",
        "Show session cost, plan usage, and activity stats",
    ),
    (
        "status",
        "",
        "Show version, model, account, API connectivity and tool statuses",
    ),
    (
        "tasks",
        "",
        "View and manage everything running in the background",
    ),
    (
        "permissions",
        "",
        "Manage allow and deny tool permission rules",
    ),
    (
        "export",
        "",
        "Export the current conversation to a file or clipboard",
    ),
    (
        "init",
        "",
        "Initialize a new CLAUDE.md file with codebase documentation",
    ),
    ("memory", "", "Edit CLAUDE.md files and memory settings"),
    ("mcp", "", "Manage MCP servers"),
    ("skills", "", "List available skills"),
    ("config", "", "Open settings"),
    ("add-dir", "<path>", "Add a new working directory"),
    ("recap", "", "Generate a one-line session recap now"),
    (
        "fork",
        "",
        "Copy this conversation into a new background session and keep working here",
    ),
    (
        "branch",
        "",
        "Create a branch of the current conversation at this point",
    ),
    ("help", "", "Show help and available commands"),
    ("exit", "", "Quit Claude Code"),
];

/// Every slash command the given agent takes: the built-ins, then what is on
/// disk.
/// `home` and `cwd` are parameters so a test can hand it a directory of its
/// own. Agents with no list here answer with none, and their prompt still
/// takes whatever is typed into it.
#[must_use]
pub fn commands(agent: Agent, home: &Path, cwd: &Path) -> Vec<Command> {
    let (builtins, found) = match agent {
        Agent::Rokartur => (AGENT_BUILTINS, agent_found(home)),
        Agent::Claude => (CLAUDE_BUILTINS, claude_found(home, cwd)),
        Agent::Codex | Agent::OpenCode | Agent::Pi => (&[][..], Vec::new()),
    };
    let mut commands: Vec<Command> = builtins
        .iter()
        .map(|(name, hint, description)| Command {
            name: format!("/{name}"),
            hint: (*hint).to_owned(),
            description: (*description).to_owned(),
            source: "builtin",
        })
        .collect();
    for command in found {
        if !commands.iter().any(|known| known.name == command.name) {
            commands.push(command);
        }
    }
    commands.truncate(COMMANDS_MAX);
    commands
}

/// The part of `~/.agents/settings.json` that says which skills are loaded.
#[derive(Default, serde::Deserialize)]
struct AgentSkills {
    #[serde(default)]
    disabled_skills: Vec<String>,
    #[serde(default)]
    skill_profiles: std::collections::HashMap<String, Vec<String>>,
    #[serde(default)]
    active_profiles: Vec<String>,
}

// shortcut: a trusted project's own `.agents/skills`, plugins' commands and
// `/profile:` are left out; read them once a remote needs them.
fn agent_found(home: &Path) -> Vec<Command> {
    let root = home.join(".agents");
    let settings: AgentSkills = std::fs::read(root.join("settings.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    // A skill named in a profile loads only while that profile is on, which
    // is how agent's `/help` comes to count fewer skills than the directory.
    let off = |name: &str| {
        settings.disabled_skills.iter().any(|skill| skill == name)
            || settings.skill_profiles.iter().any(|(profile, skills)| {
                !settings.active_profiles.contains(profile)
                    && skills.iter().any(|skill| skill == name)
            })
    };
    let mut found: Vec<Command> = skills(&root.join("skills"))
        .into_iter()
        .filter(|command| !off(&command.name[1..]))
        .collect();
    found.extend(markdown(&root.join("prompts"), "prompt:", "prompt", 1));
    found
}

fn claude_found(home: &Path, cwd: &Path) -> Vec<Command> {
    let mut found = Vec::new();
    // The project's own come first: where both have one, it is the project's
    // that runs.
    for root in [cwd.join(".claude"), home.join(".claude")] {
        found.extend(markdown(&root.join("commands"), "", "command", 3));
        found.extend(skills(&root.join("skills")));
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found.dedup_by(|a, b| a.name == b.name);
    found
}

/// A skills directory: one directory per skill with a `SKILL.md` in it, named
/// by its front matter or else by the directory.
fn skills(dir: &Path) -> Vec<Command> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<Command> = entries
        .flatten()
        .take(COMMANDS_MAX)
        .filter_map(|entry| {
            let (name, description) = front_matter(&entry.path().join("SKILL.md"))?;
            let name = name.unwrap_or_else(|| entry.file_name().to_string_lossy().into_owned());
            Some(Command {
                name: format!("/{name}"),
                hint: String::new(),
                description,
                source: "skill",
            })
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// Every `.md` file under `dir`, `depth` levels down, as a command named by
/// its file name: Claude Code's commands keep a subdirectory for sorting and
/// not in the name.
fn markdown(dir: &Path, prefix: &str, source: &'static str, depth: u32) -> Vec<Command> {
    let mut found = Vec::new();
    let mut pending = vec![(dir.to_path_buf(), depth)];
    while let Some((dir, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        if found.len() >= COMMANDS_MAX {
            break;
        }
        for path in entries
            .flatten()
            .take(COMMANDS_MAX)
            .map(|entry| entry.path())
        {
            if path.is_dir() {
                if depth > 1 {
                    pending.push((path, depth - 1));
                }
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "md") {
                continue;
            }
            let Some(stem) = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
            else {
                continue;
            };
            let description = front_matter(&path)
                .map(|(_, description)| description)
                .unwrap_or_default();
            found.push(Command {
                name: format!("/{prefix}{stem}"),
                hint: String::new(),
                description,
                source,
            });
            if found.len() >= COMMANDS_MAX {
                break;
            }
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// The `name` and `description` of a Markdown file's YAML front matter, read
/// from its first 8 KiB, or its first line of text when it has none. A
/// folded `description: >` runs on over the indented lines under it.
fn front_matter(path: &Path) -> Option<(Option<String>, String)> {
    use std::io::Read;
    let mut head = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(8 * 1024)
        .read_to_end(&mut head)
        .ok()?;
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.lines();
    let unquote = |value: &str| {
        value
            .trim()
            .trim_matches(|c| c == '"' || c == '\'')
            .to_owned()
    };
    if lines.clone().next().map(str::trim) != Some("---") {
        let first = lines
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default();
        return Some((None, first.trim_start_matches('#').trim().to_owned()));
    }
    lines.next();
    let (mut name, mut description) = (None, String::new());
    let mut folding = false;
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if folding && line.starts_with(char::is_whitespace) {
            if !description.is_empty() {
                description.push(' ');
            }
            description.push_str(line.trim());
            continue;
        }
        folding = false;
        if let Some(value) = line.strip_prefix("name:") {
            name = Some(unquote(value)).filter(|name| !name.is_empty());
        } else if let Some(value) = line.strip_prefix("description:") {
            let value = value.trim();
            folding = value.is_empty() || value.starts_with(['>', '|']);
            description = if folding {
                String::new()
            } else {
                unquote(value)
            };
        }
    }
    Some((name, description))
}

/// What `Commands` returns.
#[must_use]
pub fn commands_json(agent: Option<Agent>, commands: &[Command]) -> String {
    json!({ "agent": agent.map(Agent::label), "commands": commands }).to_string()
}

/// What the window knows about a pane that the model does not: whether the
/// agent in it is in the middle of a turn, and whether one ended out of sight.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Live {
    pub working: bool,
    pub finished: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct WindowState {
    pub window: u32,
    pub projects: Vec<ProjectState>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProjectState {
    pub id: u64,
    pub name: String,
    pub selected: bool,
    pub tabs: Vec<TabState>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TabState {
    pub id: u64,
    pub name: String,
    pub selected: bool,
    pub directory: Option<String>,
    pub panes: Vec<PaneState>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PaneState {
    pub id: u64,
    pub kind: &'static str,
    pub title: Option<String>,
    pub directory: Option<String>,
    pub focused: bool,
    pub working: bool,
    pub finished: bool,
}

/// One window's workspace as `State` describes it, in the order the sidebar
/// and the tab strip show.
#[must_use]
pub fn describe(window: u32, workspace: &Workspace, live: impl Fn(Id) -> Live) -> WindowState {
    let selected_project = workspace.selected_id();
    let projects = workspace
        .projects()
        .iter()
        .map(|project| ProjectState {
            id: project.id().raw(),
            name: project.name().to_owned(),
            selected: Some(project.id()) == selected_project,
            tabs: project
                .tabs()
                .iter()
                .map(|tab| TabState {
                    id: tab.id().raw(),
                    name: tab.name().to_owned(),
                    selected: Some(tab.id()) == project.selected_id(),
                    directory: tab.directory().map(str::to_owned),
                    panes: tab
                        .layout()
                        .panes()
                        .map(|pane| pane_state(pane, tab.layout().focused(), live(pane.id())))
                        .collect(),
                })
                .collect(),
        })
        .collect();
    WindowState { window, projects }
}

fn pane_state(pane: &Pane, focused: Id, live: Live) -> PaneState {
    PaneState {
        id: pane.id().raw(),
        kind: match pane.content {
            Content::Terminal => "terminal",
            Content::File(_) => "file",
            Content::Diff { .. } => "diff",
            Content::Ssh { .. } => "ssh",
            Content::Hosts => "hosts",
            Content::Sftp { .. } => "sftp",
        },
        title: pane.title.clone(),
        directory: pane.directory.clone(),
        focused: pane.id() == focused,
        working: live.working,
        finished: live.finished,
    }
}

/// Every window, as the one string `State` returns.
#[must_use]
pub fn state_json(windows: &[WindowState]) -> String {
    json!({ "windows": windows }).to_string()
}

/// What `Usage` returns: the same figures the Info page's Agent section draws.
/// Tokens and percentages, never prices, as there.
#[must_use]
pub fn usage_json(usage: &Snapshot) -> String {
    let limits = |limits: &[crate::usage::Limit]| -> Vec<_> {
        limits
            .iter()
            .map(|limit| {
                json!({
                    "window": limit.window,
                    "used_percent": limit.used_percent,
                    "resets_at": limit.resets_at,
                })
            })
            .collect()
    };
    json!({
        "agent": usage.agent.map(crate::usage::Agent::label),
        "model": usage.model,
        "source": usage.source,
        "tokens": {
            "input": usage.session.input,
            "output": usage.session.output,
            "cache_read": usage.session.cache_read,
            "cache_write": usage.session.cache_write,
        },
        "context": usage.context.map(|fill| json!({ "used": fill.used, "total": fill.total })),
        "limits": limits(&usage.limits),
        "fleet": usage.fleet.as_ref().map(|fleet| json!({
            "group": fleet.group,
            "limits": limits(&fleet.limits),
            "blocked": fleet.blocked,
        })),
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{Agent, Fill, Limit, Tokens};
    use crate::workspace::Tab;

    #[test]
    fn the_interface_description_names_every_method() {
        for method in [
            "State", "Text", "Type", "Key", "Focus", "NewTab", "CloseTab", "Usage", "Commands",
        ] {
            assert!(
                XML.contains(&format!("<method name=\"{method}\">")),
                "{method}"
            );
        }
        assert!(XML.contains(INTERFACE));
    }

    #[test]
    fn a_text_ask_is_held_between_one_line_and_the_cap() {
        assert_eq!(text_lines(0), 1);
        assert_eq!(text_lines(40), 40);
        assert_eq!(text_lines(u32::MAX), TEXT_LINES_MAX as usize);
    }

    #[test]
    fn a_key_name_is_modifiers_then_one_key() {
        let plain = |base| Chord {
            base,
            ctrl: false,
            alt: false,
            shift: false,
        };
        assert_eq!(chord("esc"), Some(plain(Base::Escape)));
        assert_eq!(chord("up"), Some(plain(Base::Up)));
        assert_eq!(chord("1"), Some(plain(Base::Char('1'))));
        assert_eq!(
            chord("shift-tab"),
            Some(Chord {
                shift: true,
                ..plain(Base::Tab)
            })
        );
        assert_eq!(
            chord("alt-ctrl-o"),
            Some(Chord {
                ctrl: true,
                alt: true,
                ..plain(Base::Char('o'))
            })
        );
        for wrong in [
            "",
            "ctrl-",
            "ctrl-ctrl-c",
            "C",
            "ab",
            "f1",
            "enter-ctrl",
            "\n",
        ] {
            assert_eq!(chord(wrong), None, "{wrong:?}");
        }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("tuni-control-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn agent_offers_its_built_ins_and_the_skills_it_loads() {
        let home = scratch("agent");
        let skills = home.join(".agents/skills");
        write(
            &skills.join("tdd/SKILL.md"),
            "---\nname: tdd\ndescription: Test first.\n---\nbody",
        );
        write(
            &skills.join("web/SKILL.md"),
            "---\nname: web\ndescription: >\n  Folded\n  over two lines\n---\n",
        );
        write(&skills.join("off/SKILL.md"), "---\nname: off\n---\n");
        write(&skills.join("empty/notes.md"), "no SKILL.md here");
        write(
            &home.join(".agents/settings.json"),
            r#"{"disabled_skills":["off"],"skill_profiles":{"w":["web"]},"active_profiles":[]}"#,
        );
        write(&home.join(".agents/prompts/review.md"), "Review the diff\n");

        let commands = commands(Agent::Rokartur, &home, &home);
        assert_eq!(commands[0].name, "/clear");
        assert!(commands.iter().all(|c| !c.name.contains(' ')));
        let found: Vec<_> = commands
            .iter()
            .filter(|c| c.source != "builtin")
            .map(|c| (c.name.as_str(), c.description.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                ("/tdd", "Test first."),
                ("/prompt:review", "Review the diff")
            ],
            "a disabled skill and one in a profile that is off are left out"
        );

        write(
            &home.join(".agents/settings.json"),
            r#"{"skill_profiles":{"w":["web"]},"active_profiles":["w"]}"#,
        );
        let web = super::commands(Agent::Rokartur, &home, &home)
            .into_iter()
            .find(|c| c.name == "/web")
            .expect("its profile is on");
        assert_eq!(web.description, "Folded over two lines");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn claude_code_offers_project_commands_before_its_own() {
        let home = scratch("claude-home");
        let project = scratch("claude-project");
        write(
            &home.join(".claude/commands/ship.md"),
            "---\ndescription: \"Mine\"\n---\n",
        );
        write(
            &project.join(".claude/commands/git/ship.md"),
            "---\ndescription: The project's\n---\n",
        );
        write(
            &home.join(".claude/skills/review/SKILL.md"),
            "---\nname: review\ndescription: Look it over\n---\n",
        );
        write(
            &project.join(".claude/commands/clear.md"),
            "# Not the built-in\n",
        );

        let commands = commands(Agent::Claude, &home, &project);
        let ship: Vec<_> = commands.iter().filter(|c| c.name == "/ship").collect();
        assert_eq!(ship.len(), 1);
        assert_eq!(ship[0].description, "The project's");
        assert_eq!(ship[0].source, "command");
        assert!(
            commands
                .iter()
                .any(|c| c.name == "/review" && c.source == "skill")
        );
        let clear: Vec<_> = commands.iter().filter(|c| c.name == "/clear").collect();
        assert_eq!(clear.len(), 1, "a built-in is listed once");
        assert_eq!(clear[0].source, "builtin");
        assert!(super::commands(Agent::Codex, &home, &project).is_empty());

        let read: serde_json::Value =
            serde_json::from_str(&commands_json(Some(Agent::Claude), &commands)).unwrap();
        assert_eq!(read["agent"], "Claude Code");
        assert_eq!(read["commands"][0]["name"], "/clear");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn the_state_follows_the_sidebar_and_marks_what_is_selected() {
        let mut workspace = Workspace::new();
        let first = workspace.open_project();
        let second = workspace.open_project();
        for project in [first, second] {
            workspace
                .project_mut(project)
                .unwrap()
                .insert_tab(0, Tab::new());
        }
        workspace.select(second);
        let busy = workspace.project(first).unwrap().tabs()[0]
            .layout()
            .focused();

        let state = describe(3, &workspace, |pane| Live {
            working: pane == busy,
            finished: false,
        });
        assert_eq!(state.window, 3);
        assert_eq!(state.projects.len(), 2);
        assert!(!state.projects[0].selected);
        assert!(state.projects[1].selected);
        let pane = &state.projects[0].tabs[0].panes[0];
        assert_eq!(pane.kind, "terminal");
        assert!(pane.focused);
        assert!(pane.working);
        assert!(!state.projects[1].tabs[0].panes[0].working);

        let text = state_json(&[state]);
        let read: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(read["windows"][0]["projects"][1]["selected"], true);
        assert_eq!(read["windows"][0]["projects"][0]["id"], first.raw());
    }

    #[test]
    fn usage_carries_tokens_and_bars_and_no_price() {
        let usage = Snapshot {
            agent: Some(Agent::Rokartur),
            model: Some("claude-opus-5-5".to_owned()),
            source: Some("anthropic".to_owned()),
            session: Tokens {
                input: 4,
                output: 693,
                cache_read: 32_596,
                cache_write: 9_555,
            },
            context: Some(Fill {
                used: 50_000,
                total: 200_000,
            }),
            limits: vec![Limit {
                window: "7 days".to_owned(),
                used_percent: 72.0,
                resets_at: Some(1_795_000_000),
            }],
            fleet: None,
        };
        let read: serde_json::Value = serde_json::from_str(&usage_json(&usage)).unwrap();
        assert_eq!(read["agent"], "agent");
        assert_eq!(read["tokens"]["cache_read"], 32_596);
        assert_eq!(read["limits"][0]["used_percent"], 72.0);
        assert_eq!(read["context"]["total"], 200_000);
        assert!(read["fleet"].is_null());
        assert!(!usage_json(&usage).contains("cost"));
    }
}
