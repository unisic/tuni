//! What another program running as the same user can read and do through
//! `dev.unisic.Tuni.Remote`, the D-Bus interface Tuni offers while
//! Preferences - Session - Remote Control is on: the projects, tabs and panes,
//! a pane's text, typing into a shell, and what the coding agent in one has
//! spent.
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

use serde::Serialize;
use serde_json::json;

use crate::panes::{Content, Pane};
use crate::usage::Snapshot;
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

/// What a named key sends. Only keys whose bytes do not depend on a mode the
/// application set: the arrows are `ESC [ A` or `ESC O A` depending on one,
/// and guessing wrong moves a cursor somewhere nobody asked.
#[must_use]
pub fn key(name: &str) -> Option<&'static str> {
    match name {
        "enter" => Some("\r"),
        "esc" => Some("\x1b"),
        "tab" => Some("\t"),
        "ctrl-c" => Some("\x03"),
        "ctrl-d" => Some("\x04"),
        _ => None,
    }
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
            "State", "Text", "Type", "Key", "Focus", "NewTab", "CloseTab", "Usage",
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
    fn only_keys_that_mean_one_thing_are_sent() {
        assert_eq!(key("esc"), Some("\x1b"));
        assert_eq!(key("ctrl-c"), Some("\x03"));
        assert_eq!(key("up"), None, "the arrows depend on the cursor key mode");
        assert_eq!(key(""), None);
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
