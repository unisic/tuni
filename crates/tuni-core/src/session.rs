//! The workspace as it was when the window closed, so opening it again lands
//! where the work was left.
//!
//! A snapshot holds shape and place, never state: projects, their tabs, the
//! columns and panes inside each tab, the weights, which pane had the
//! keyboard, and what each shell said its working directory was. Restoring
//! starts fresh shells in those directories — a shell is a process, and a
//! process cannot be saved to disk.
//!
//! Everything that is not shape is optional, and a field that fails to read
//! falls back rather than failing the whole file. A snapshot is a convenience;
//! refusing to open a window because one number in it went missing would make
//! it the opposite.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::panes::{Column, Layout, Pane};
use crate::settings::data_dir;
use crate::workspace::{Id, Project, Tab, Workspace};

/// One pane: what it held, where that was, and how much room it had.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PaneSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    #[serde(default = "one")]
    pub weight: f64,
    /// Into the sidecar history file, and only when history restore was on.
    /// Kept out of the snapshot itself so this file stays small enough to read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<String>,
    /// The file the pane was showing, when it was showing one instead of a
    /// shell. Absent on every pane written before there was an editor, which is
    /// what makes those restore as terminals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Where the cursor was in that file, as a character offset. Unsaved edits
    /// are not kept — the file on disk is the file, and a session snapshot is
    /// not a place to hide a copy of one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<usize>,
}

/// What the window knows about a pane that the model does not: the scrollback
/// its terminal is holding, and where the cursor is in its editor.
#[derive(Clone, Debug, Default)]
pub struct PaneState {
    pub history: Option<String>,
    pub cursor: Option<usize>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ColumnSnapshot {
    pub panes: Vec<PaneSnapshot>,
    #[serde(default = "one")]
    pub weight: f64,
}

/// One tab's layout, plus which pane in it had the keyboard.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct TabSnapshot {
    pub columns: Vec<ColumnSnapshot>,
    #[serde(default)]
    pub focused_column: usize,
    #[serde(default)]
    pub focused_row: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_name: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ProjectSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_name: Option<String>,
    /// Only a pinned directory is saved. An automatic one is derived from
    /// wherever the shell is, and saving a derived value means restoring a
    /// decision the user never made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_directory: Option<String>,
    pub tabs: Vec<TabSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_tab: Option<usize>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Snapshot {
    pub projects: Vec<ProjectSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_project: Option<usize>,
    /// Whether the sidebar was showing. Optional: a snapshot written before
    /// the window remembered this leaves it at the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar: Option<bool>,
    /// Whether the panel was showing, on the same terms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel: Option<bool>,
    /// Which of the panel's pages was showing, by name. A name this version
    /// does not have is ignored rather than refused, so a session written by a
    /// version with more pages still opens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel_page: Option<String>,
}

fn one() -> f64 {
    1.0
}

/// A restored workspace, and what its panes are waiting for.
pub struct Restored {
    pub workspace: Workspace,
    /// Pane id to the key its saved scrollback is under. Only the panes that
    /// have one appear.
    pub histories: HashMap<Id, String>,
    /// Pane id to where the cursor was in the file it is holding.
    pub cursors: HashMap<Id, usize>,
    pub sidebar: Option<bool>,
    pub panel: Option<bool>,
    pub panel_page: Option<String>,
}

impl Snapshot {
    /// The workspace as it stands. `state` is asked, per pane, for the key its
    /// scrollback was saved under — `None` for panes whose output was not
    /// saved, which is every pane when the setting is off — and for where the
    /// cursor is in the file it is holding.
    #[must_use]
    pub fn of(workspace: &Workspace, state: impl Fn(Id) -> PaneState) -> Self {
        let projects = workspace
            .projects()
            .iter()
            .map(|project| ProjectSnapshot {
                custom_name: project.custom_name.clone(),
                custom_directory: project.custom_directory.clone(),
                selected_tab: project.selected_id().and_then(|id| project.index_of(id)),
                tabs: project
                    .tabs()
                    .iter()
                    .map(|tab| snap_tab(tab, &state))
                    .collect(),
            })
            .collect();

        Self {
            projects,
            selected_project: workspace
                .selected_id()
                .and_then(|id| workspace.index_of(id)),
            sidebar: None,
            panel: None,
            panel_page: None,
        }
    }

    /// Builds the workspace this snapshot describes. Panes come back with the
    /// directory their shell was in, so the caller can start one there.
    #[must_use]
    pub fn restore(&self) -> Restored {
        let mut workspace = Workspace::new();
        let mut histories = HashMap::new();
        let mut cursors = HashMap::new();

        for saved in &self.projects {
            let tabs: Vec<Tab> = saved
                .tabs
                .iter()
                .filter_map(|tab| restore_tab(tab, &mut histories, &mut cursors))
                .collect();
            // A project with nothing in it would restore as a row that shows
            // the empty state; the tab it lost is what made it a project.
            if tabs.is_empty() {
                continue;
            }

            let id = workspace.open_project();
            let Some(project) = workspace.project_mut(id) else {
                continue;
            };
            project.custom_name.clone_from(&saved.custom_name);
            project.custom_directory.clone_from(&saved.custom_directory);
            for (index, tab) in tabs.into_iter().enumerate() {
                project.insert_tab(index, tab);
            }
            let selected = saved
                .selected_tab
                .and_then(|index| project.tabs().get(index))
                .or_else(|| project.tabs().first())
                .map(Tab::id);
            project.select(selected);
        }

        if let Some(id) = self
            .selected_project
            .and_then(|index| workspace.projects().get(index))
            .map(Project::id)
        {
            workspace.select(id);
        }

        Restored {
            workspace,
            histories,
            cursors,
            sidebar: self.sidebar,
            panel: self.panel,
            panel_page: self.panel_page.clone(),
        }
    }

    /// `$XDG_DATA_HOME/tuni/session.json`.
    #[must_use]
    pub fn path() -> PathBuf {
        data_dir().join("session.json")
    }

    /// The snapshot on disk, if there is one that still reads.
    #[must_use]
    pub fn load() -> Option<Self> {
        let text = fs::read_to_string(Self::path()).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Writes the snapshot, replacing whatever was there.
    ///
    /// Written beside itself and renamed into place: the previous session is
    /// better than half of this one, and this runs while the window is closing.
    pub fn save(&self) -> io::Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string(self).map_err(io::Error::other)?;
        let temporary = path.with_extension("json.new");
        fs::write(&temporary, text)?;
        fs::rename(&temporary, &path)
    }

    /// Forgets the saved session. What "close the window and mean it" does when
    /// there is nothing left to restore.
    pub fn forget() {
        let _ = fs::remove_file(Self::path());
        History::forget();
    }
}

fn snap_tab(tab: &Tab, state: &impl Fn(Id) -> PaneState) -> TabSnapshot {
    let layout = tab.layout();
    let (focused_column, focused_row) = layout.location(layout.focused()).unwrap_or((0, 0));

    TabSnapshot {
        columns: layout
            .columns()
            .iter()
            .map(|column| ColumnSnapshot {
                weight: column.weight,
                panes: column
                    .panes()
                    .iter()
                    .map(|pane| {
                        let state = state(pane.id());
                        PaneSnapshot {
                            directory: pane.directory.clone(),
                            weight: pane.weight,
                            history: state.history,
                            file: pane.path().map(|path| path.to_string_lossy().into_owned()),
                            cursor: state.cursor,
                        }
                    })
                    .collect(),
            })
            .collect(),
        focused_column,
        focused_row,
        custom_name: tab.custom_name.clone(),
    }
}

fn restore_tab(
    saved: &TabSnapshot,
    histories: &mut HashMap<Id, String>,
    cursors: &mut HashMap<Id, usize>,
) -> Option<Tab> {
    let columns: Vec<Column> = saved
        .columns
        .iter()
        .filter_map(|column| {
            let panes: Vec<Pane> = column
                .panes
                .iter()
                .map(|saved| {
                    let mut pane = match &saved.file {
                        Some(path) => Pane::file(PathBuf::from(path)),
                        None => Pane::new(),
                    };
                    if saved.directory.is_some() {
                        pane.directory.clone_from(&saved.directory);
                    }
                    pane.weight = saved.weight;
                    if let Some(key) = &saved.history {
                        histories.insert(pane.id(), key.clone());
                    }
                    if let Some(cursor) = saved.cursor {
                        cursors.insert(pane.id(), cursor);
                    }
                    pane
                })
                .collect();
            Column::from_panes(panes, column.weight)
        })
        .collect();

    let layout = Layout::from_columns(columns, saved.focused_column, saved.focused_row)?;
    let mut tab = Tab::new();
    tab.custom_name.clone_from(&saved.custom_name);
    *tab.layout_mut() = layout;
    Some(tab)
}

/// The scrollback panes had when the window closed, kept beside the snapshot
/// rather than inside it.
///
/// Terminal output is the largest thing here by far and the least often read:
/// the window needs the layout to open at all, and the history only once, per
/// pane, as its shell starts. Keys are opaque and belong to one saved session —
/// the whole file is rewritten on every save, so a pane that is gone takes its
/// history with it.
#[derive(Debug, Default)]
pub struct History(HashMap<String, String>);

impl History {
    #[must_use]
    pub fn path() -> PathBuf {
        data_dir().join("history.json")
    }

    #[must_use]
    pub fn load() -> Self {
        let Ok(text) = fs::read_to_string(Self::path()) else {
            return Self::default();
        };
        Self(serde_json::from_str(&text).unwrap_or_default())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn insert(&mut self, key: String, contents: String) {
        self.0.insert(key, contents);
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    /// Writes the file, or removes it when there is nothing to save — so a
    /// window closed with history restore turned off leaves no trace of the
    /// last time it was on.
    pub fn save(&self) -> io::Result<()> {
        let path = Self::path();
        if self.0.is_empty() {
            match fs::remove_file(&path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => return Ok(()),
            }
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string(&self.0).map_err(io::Error::other)?;
        let temporary = path.with_extension("json.new");
        fs::write(&temporary, text)?;
        fs::rename(&temporary, &path)
    }

    pub fn forget() {
        let _ = fs::remove_file(Self::path());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panes::Edge;
    use std::path::Path;

    /// A workspace with one project, one tab, and the shape `shape` describes:
    /// one entry per column, each saying how many panes are stacked in it.
    fn workspace(shape: &[usize]) -> Workspace {
        let mut workspace = Workspace::new();
        let id = workspace.open_project();
        let project = workspace.project_mut(id).expect("just opened");
        project.insert_tab(0, Tab::new());
        let tab = project.tabs().first().map(Tab::id).expect("just inserted");
        project.select(Some(tab));
        let layout = project.tab_mut(tab).expect("just inserted").layout_mut();

        for _ in 1..shape[0] {
            layout.split(Pane::new(), Edge::Down);
        }
        for column in &shape[1..] {
            layout.split(Pane::new(), Edge::Right);
            for _ in 1..*column {
                layout.split(Pane::new(), Edge::Down);
            }
        }
        workspace
    }

    fn shape(workspace: &Workspace) -> Vec<usize> {
        workspace.projects()[0].tabs()[0]
            .layout()
            .columns()
            .iter()
            .map(|column| column.panes().len())
            .collect()
    }

    #[test]
    fn a_layout_comes_back_the_shape_it_was_saved_in() {
        let saved = workspace(&[2, 1, 3]);
        let restored = Snapshot::of(&saved, |_| PaneState::default())
            .restore()
            .workspace;

        assert_eq!(shape(&restored), vec![2, 1, 3]);
        assert_eq!(restored.projects().len(), 1);
    }

    #[test]
    fn the_pane_that_had_the_keyboard_has_it_again() {
        let mut saved = workspace(&[2, 2]);
        let project = saved.projects()[0].id();
        let tab = saved.projects()[0].tabs()[0].id();
        let target = saved.projects()[0].tabs()[0].layout().columns()[0].panes()[1].id();
        saved
            .project_mut(project)
            .and_then(|project| project.tab_mut(tab))
            .expect("just built")
            .layout_mut()
            .focus(target);

        let restored = Snapshot::of(&saved, |_| PaneState::default())
            .restore()
            .workspace;
        let layout = restored.projects()[0].tabs()[0].layout();
        assert_eq!(layout.location(layout.focused()), Some((0, 1)));
    }

    #[test]
    fn names_directories_and_weights_survive_the_trip() {
        let mut saved = workspace(&[2]);
        let project_id = saved.projects()[0].id();
        let tab_id = saved.projects()[0].tabs()[0].id();
        {
            let project = saved.project_mut(project_id).expect("just built");
            project.custom_name = Some("Backend".to_owned());
            project.custom_directory = Some("/srv/app".to_owned());
            let tab = project.tab_mut(tab_id).expect("just built");
            tab.custom_name = Some("logs".to_owned());
            let panes: Vec<Id> = tab.layout().panes().map(Pane::id).collect();
            let top = tab.layout_mut().pane_mut(panes[0]).expect("just built");
            top.directory = Some("/var/log".to_owned());
            top.weight = 0.25;
        }

        let restored = Snapshot::of(&saved, |_| PaneState::default())
            .restore()
            .workspace;
        let project = &restored.projects()[0];
        assert_eq!(project.custom_name.as_deref(), Some("Backend"));
        assert_eq!(project.custom_directory.as_deref(), Some("/srv/app"));

        let tab = &project.tabs()[0];
        assert_eq!(tab.custom_name.as_deref(), Some("logs"));
        let top = &tab.layout().columns()[0].panes()[0];
        assert_eq!(top.directory.as_deref(), Some("/var/log"));
        assert!((top.weight - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn a_history_key_follows_the_pane_it_was_saved_for() {
        let saved = workspace(&[1, 1]);
        let ids: Vec<Id> = saved.projects()[0].tabs()[0]
            .layout()
            .panes()
            .map(Pane::id)
            .collect();
        let second = ids[1];

        let restored = Snapshot::of(&saved, |id| PaneState {
            history: (id == second).then(|| "key-for-the-second".to_owned()),
            ..PaneState::default()
        })
        .restore();

        let panes: Vec<Id> = restored.workspace.projects()[0].tabs()[0]
            .layout()
            .panes()
            .map(Pane::id)
            .collect();
        assert_eq!(restored.histories.len(), 1);
        assert_eq!(
            restored.histories.get(&panes[1]).map(String::as_str),
            Some("key-for-the-second")
        );
    }

    #[test]
    fn a_file_pane_comes_back_holding_its_file_and_its_cursor() {
        let mut saved = workspace(&[1]);
        let project = saved.projects()[0].id();
        let tab = saved.projects()[0].tabs()[0].id();
        let layout = saved
            .project_mut(project)
            .and_then(|project| project.tab_mut(tab))
            .expect("just built")
            .layout_mut();
        layout.split(Pane::file(PathBuf::from("/src/main.rs")), Edge::Right);
        let opened = layout
            .panes()
            .find(|pane| pane.path().is_some())
            .map(Pane::id)
            .expect("just split one in");

        let restored = Snapshot::of(&saved, |id| PaneState {
            cursor: (id == opened).then_some(42),
            ..PaneState::default()
        })
        .restore();

        let pane = restored.workspace.projects()[0].tabs()[0]
            .layout()
            .panes()
            .find(|pane| pane.path().is_some())
            .expect("the file pane is still a file pane");
        assert_eq!(pane.path(), Some(Path::new("/src/main.rs")));
        assert_eq!(pane.directory.as_deref(), Some("/src"));
        assert_eq!(restored.cursors.get(&pane.id()), Some(&42));
    }

    #[test]
    fn a_snapshot_reads_back_out_of_its_own_json() {
        let saved = workspace(&[1, 2]);
        let snapshot = Snapshot::of(&saved, |_| PaneState::default());
        let text = serde_json::to_string(&snapshot).expect("plain data");
        let read: Snapshot = serde_json::from_str(&text).expect("what we just wrote");

        assert_eq!(shape(&read.restore().workspace), vec![1, 2]);
    }

    #[test]
    fn a_snapshot_missing_everything_optional_still_opens() {
        let read: Snapshot =
            serde_json::from_str(r#"{"projects":[{"tabs":[{"columns":[{"panes":[{}]}]}]}]}"#)
                .expect("every optional field has a fallback");
        let restored = read.restore().workspace;

        assert_eq!(shape(&restored), vec![1]);
        let pane = &restored.projects()[0].tabs()[0].layout().columns()[0].panes()[0];
        assert!(pane.directory.is_none());
        assert!((pane.weight - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_project_whose_tabs_all_failed_to_read_is_not_restored_empty() {
        let read: Snapshot = serde_json::from_str(r#"{"projects":[{"tabs":[{"columns":[]}]}]}"#)
            .expect("shape is what it is");
        assert!(read.restore().workspace.is_empty());
    }
}
