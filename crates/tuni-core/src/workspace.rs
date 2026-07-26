//! Projects and their tabs.
//!
//! A project is one row in the left sidebar and one strip of tabs; a tab is a
//! layout of panes. The rules here are kero's: a new tab opens next to the selected
//! one rather than at the end, closing the selected tab falls to its neighbor,
//! selection wraps at both ends, and a project whose tabs are all closed stays
//! in the sidebar until it is closed on purpose.
//!
//! Nothing here knows about widgets. The tab strip is an `AdwTabView`, which
//! implements the tab-level rules itself and reports what it did; this model
//! mirrors it so the project name, the working directory a new tab starts in,
//! and — from Etap 4 — the session file have one place to read from. Projects
//! have no widget that owns them, so their rules live here outright.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::panes::{Layout, Pane};

/// Identity for a project or a tab: unique for the life of the process, and
/// unrelated to position, so a reorder cannot invalidate a reference.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, PartialOrd, Ord)]
pub struct Id(u64);

impl Id {
    /// The next unused identity.
    #[must_use]
    pub fn next() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    /// The identity as a plain number, for the GVariant an action carries.
    #[must_use]
    pub fn raw(self) -> u64 {
        self.0
    }
}

impl From<u64> for Id {
    fn from(raw: u64) -> Self {
        Self(raw)
    }
}

/// Shown when a tab has neither a name of its own nor a title from the shell.
const UNTITLED_TAB: &str = "Terminal";

/// One entry in a project's strip: a layout of panes, which for a tab that was
/// never split is one terminal.
///
/// A tab holds no title or directory of its own — both belong to whichever pane
/// has the keyboard, so a split tab is named by the half being worked in.
#[derive(Clone, Debug)]
pub struct Tab {
    id: Id,
    /// Set by "Rename…", cleared by "Use Automatic Title".
    pub custom_name: Option<String>,
    layout: Layout,
}

impl Tab {
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: Id::next(),
            custom_name: None,
            layout: Layout::new(Pane::new()),
        }
    }

    #[must_use]
    pub fn id(&self) -> Id {
        self.id
    }

    #[must_use]
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    pub fn layout_mut(&mut self) -> &mut Layout {
        &mut self.layout
    }

    /// The name the user gave it, else the focused shell's own title. Nothing
    /// when neither has anything to say — which is what lets a project fall
    /// back to its own name rather than to a placeholder.
    #[must_use]
    pub fn display_name(&self) -> Option<&str> {
        non_empty(self.custom_name.as_deref()).or_else(|| {
            self.layout
                .focused_pane()
                .and_then(|pane| non_empty(pane.title.as_deref()))
        })
    }

    /// What the strip shows.
    #[must_use]
    pub fn name(&self) -> &str {
        self.display_name().unwrap_or(UNTITLED_TAB)
    }

    /// Where the focused shell last said it is.
    #[must_use]
    pub fn directory(&self) -> Option<&str> {
        self.layout
            .focused_pane()
            .and_then(|pane| non_empty(pane.directory.as_deref()))
    }
}

impl Default for Tab {
    fn default() -> Self {
        Self::new()
    }
}

/// A project: a sidebar row, a strip of tabs, and a directory the file tree and
/// the git panel will anchor to.
#[derive(Clone, Debug)]
pub struct Project {
    id: Id,
    fallback_name: String,
    /// Set by "Rename…", cleared by "Use Automatic Title".
    pub custom_name: Option<String>,
    /// Pinned by "Set Project Directory…", cleared by "Use Automatic
    /// Directory". While it is unset the root is derived from the shell's own
    /// working directory on every read — see [`Project::panel_root`].
    pub custom_directory: Option<String>,
    tabs: Vec<Tab>,
    selected: Option<Id>,
}

impl Project {
    #[must_use]
    pub fn new(fallback_name: impl Into<String>) -> Self {
        Self {
            id: Id::next(),
            fallback_name: fallback_name.into(),
            custom_name: None,
            custom_directory: None,
            tabs: Vec::new(),
            selected: None,
        }
    }

    #[must_use]
    pub fn id(&self) -> Id {
        self.id
    }

    /// What the sidebar shows: the name the user gave it, else the selected
    /// tab's title — a project follows the shell that is on screen — else the
    /// name it was born with.
    #[must_use]
    pub fn name(&self) -> &str {
        non_empty(self.custom_name.as_deref())
            .or_else(|| self.selected_tab().and_then(Tab::display_name))
            .unwrap_or(&self.fallback_name)
    }

    #[must_use]
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    #[must_use]
    pub fn tab(&self, id: Id) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    pub fn tab_mut(&mut self, id: Id) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    #[must_use]
    pub fn index_of(&self, id: Id) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    #[must_use]
    pub fn selected_id(&self) -> Option<Id> {
        self.selected
    }

    #[must_use]
    pub fn selected_tab(&self) -> Option<&Tab> {
        self.selected.and_then(|id| self.tab(id))
    }

    pub fn selected_tab_mut(&mut self) -> Option<&mut Tab> {
        self.selected.and_then(|id| self.tab_mut(id))
    }

    /// Selects a tab, ignoring an id this project does not hold.
    pub fn select(&mut self, id: Option<Id>) {
        match id {
            Some(id) if self.index_of(id).is_some() => self.selected = Some(id),
            None => self.selected = None,
            Some(_) => {}
        }
    }

    /// Places a tab at `index`, clamped to the end of the strip.
    pub fn insert_tab(&mut self, index: usize, tab: Tab) {
        let index = index.min(self.tabs.len());
        self.tabs.insert(index, tab);
    }

    /// Removes a tab and hands it back, leaving selection to the caller — the
    /// strip has already picked the neighbor by the time this is called.
    pub fn remove_tab(&mut self, id: Id) -> Option<Tab> {
        let index = self.index_of(id)?;
        if self.selected == Some(id) {
            self.selected = None;
        }
        Some(self.tabs.remove(index))
    }

    /// Moves a tab to `index`, as a drag across the strip does. Selection
    /// follows the tab, not the position.
    pub fn move_tab(&mut self, id: Id, index: usize) {
        let Some(from) = self.index_of(id) else {
            return;
        };
        let to = index.min(self.tabs.len() - 1);
        if from == to {
            return;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
    }

    /// Where a new tab's shell should start: where the selected one is, else
    /// the pinned project directory. `None` leaves it to the caller — the
    /// process's own working directory.
    #[must_use]
    pub fn directory_for_new_tab(&self) -> Option<&str> {
        self.selected_tab()
            .and_then(Tab::directory)
            .or_else(|| non_empty(self.custom_directory.as_deref()))
    }

    /// One pane, addressed the way the window addresses it: which tab, which
    /// pane inside it.
    #[must_use]
    pub fn pane(&self, tab: Id, pane: Id) -> Option<&Pane> {
        self.tab(tab)?.layout().pane(pane)
    }

    pub fn pane_mut(&mut self, tab: Id, pane: Id) -> Option<&mut Pane> {
        self.tab_mut(tab)?.layout_mut().pane_mut(pane)
    }

    /// The root the file tree and the git panel anchor to, and whether it was
    /// derived rather than pinned.
    ///
    /// A pinned directory wins, but only while it still exists — a pin left
    /// behind by a deleted checkout falls back to automatic rather than to
    /// nothing. Automatic is the closest git repository containing `cwd`, and
    /// it is re-derived on every call, so it follows the shell in and out of
    /// repositories instead of sticking to the first one seen.
    #[must_use]
    pub fn panel_root(&self, cwd: &Path) -> (PathBuf, bool) {
        if let Some(pinned) = non_empty(self.custom_directory.as_deref()) {
            let pinned = Path::new(pinned);
            if pinned.is_dir() {
                return (pinned.to_path_buf(), false);
            }
        }
        (
            closest_git_repository(cwd).unwrap_or_else(|| cwd.to_path_buf()),
            true,
        )
    }
}

/// The directory of the nearest enclosing git repository: walks up from `path`
/// looking for a `.git` entry — a directory in a normal checkout, a file in a
/// worktree or a submodule, so existence is the test rather than being a
/// directory.
fn closest_git_repository(path: &Path) -> Option<PathBuf> {
    let mut dir = path;
    loop {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// Every project in the window, and which one is on screen.
#[derive(Clone, Debug, Default)]
pub struct Workspace {
    projects: Vec<Project>,
    selected: Option<Id>,
    /// Numbers the automatic names. Never decremented: a project called
    /// "Project 2" keeps that name after "Project 1" is closed, and the next
    /// one is "Project 3" rather than a second "Project 2".
    counter: usize,
}

impl Workspace {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn projects(&self) -> &[Project] {
        &self.projects
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.projects.is_empty()
    }

    #[must_use]
    pub fn project(&self, id: Id) -> Option<&Project> {
        self.projects.iter().find(|project| project.id == id)
    }

    pub fn project_mut(&mut self, id: Id) -> Option<&mut Project> {
        self.projects.iter_mut().find(|project| project.id == id)
    }

    #[must_use]
    pub fn index_of(&self, id: Id) -> Option<usize> {
        self.projects.iter().position(|project| project.id == id)
    }

    #[must_use]
    pub fn selected_id(&self) -> Option<Id> {
        self.selected
    }

    #[must_use]
    pub fn selected_project(&self) -> Option<&Project> {
        self.selected.and_then(|id| self.project(id))
    }

    pub fn selected_project_mut(&mut self) -> Option<&mut Project> {
        self.selected.and_then(move |id| self.project_mut(id))
    }

    /// Opens a project next to the selected one — not at the end of the list,
    /// which is where a project opened from the one you are in does not belong
    /// — and selects it.
    pub fn open_project(&mut self) -> Id {
        self.counter += 1;
        let project = Project::new(format!("Project {}", self.counter));
        let id = project.id;
        match self.selected.and_then(|id| self.index_of(id)) {
            Some(index) => self.projects.insert(index + 1, project),
            None => self.projects.push(project),
        }
        self.selected = Some(id);
        id
    }

    /// Closes a project and hands it back so the caller can tear down its
    /// terminals. Selection falls to the neighbor that took its place, or to
    /// the last project when it was the last one.
    pub fn close_project(&mut self, id: Id) -> Option<Project> {
        let index = self.index_of(id)?;
        let project = self.projects.remove(index);
        if self.selected == Some(id) {
            self.selected = self
                .projects
                .get(index.min(self.projects.len().saturating_sub(1)))
                .map(Project::id);
        }
        Some(project)
    }

    /// Moves a project to `index`, as a drag down the sidebar does.
    pub fn move_project(&mut self, id: Id, index: usize) {
        let Some(from) = self.index_of(id) else {
            return;
        };
        let to = index.min(self.projects.len() - 1);
        if from == to {
            return;
        }
        let project = self.projects.remove(from);
        self.projects.insert(to, project);
    }

    /// Selects a project, ignoring an id this workspace does not hold.
    pub fn select(&mut self, id: Id) {
        if self.index_of(id).is_some() {
            self.selected = Some(id);
        }
    }

    pub fn select_index(&mut self, index: usize) {
        if let Some(project) = self.projects.get(index) {
            self.selected = Some(project.id);
        }
    }

    pub fn select_next(&mut self) {
        self.shift_selection(1);
    }

    pub fn select_previous(&mut self) {
        self.shift_selection(-1);
    }

    /// Wraps at both ends: the sidebar is a ring, and stopping at the last
    /// project would make the shortcut feel broken rather than safe.
    fn shift_selection(&mut self, offset: isize) {
        let count = self.projects.len();
        let Some(current) = self.selected.and_then(|id| self.index_of(id)) else {
            return;
        };
        let count_i = count as isize;
        let next = (current as isize + offset).rem_euclid(count_i) as usize;
        self.selected = Some(self.projects[next].id);
    }
}

/// A string that is worth showing, or nothing.
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|text| !text.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the shell in the focused pane says about itself.
    fn shell_says(tab: &mut Tab, title: &str, directory: &str) {
        let focused = tab.layout().focused();
        let pane = tab.layout_mut().pane_mut(focused).expect("focused pane");
        pane.title = Some(title.to_owned());
        pane.directory = Some(directory.to_owned());
    }

    /// A project with `count` tabs, the last one selected — the state the
    /// strip leaves behind after opening that many.
    fn project_with_tabs(count: usize) -> (Project, Vec<Id>) {
        let mut project = Project::new("Project 1");
        let mut ids = Vec::new();
        for index in 0..count {
            let tab = Tab::new();
            ids.push(tab.id());
            project.insert_tab(index, tab);
        }
        project.select(ids.last().copied());
        (project, ids)
    }

    #[test]
    fn a_tab_is_named_by_its_shell_until_it_is_renamed() {
        let mut tab = Tab::new();
        assert_eq!(tab.name(), UNTITLED_TAB);

        shell_says(&mut tab, "vim src/main.rs", "/home/me/src");
        assert_eq!(tab.name(), "vim src/main.rs");

        tab.custom_name = Some("editor".to_owned());
        assert_eq!(tab.name(), "editor");

        // "Use Automatic Title" hands the tab back to the shell.
        tab.custom_name = None;
        assert_eq!(tab.name(), "vim src/main.rs");
    }

    #[test]
    fn a_project_is_named_by_the_tab_that_is_on_screen() {
        let (mut project, ids) = project_with_tabs(2);
        assert_eq!(project.name(), "Project 1");

        shell_says(project.tab_mut(ids[0]).unwrap(), "build", "/src");
        shell_says(project.tab_mut(ids[1]).unwrap(), "test", "/src");
        assert_eq!(project.name(), "test");

        project.select(Some(ids[0]));
        assert_eq!(project.name(), "build");

        project.custom_name = Some("tuni".to_owned());
        assert_eq!(project.name(), "tuni");
    }

    #[test]
    fn a_renamed_tab_that_is_blank_is_not_a_name() {
        let mut tab = Tab::new();
        shell_says(&mut tab, "zsh", "/home/me");
        tab.custom_name = Some("   ".to_owned());
        assert_eq!(tab.name(), "zsh");
    }

    #[test]
    fn a_split_tab_is_named_by_the_pane_being_worked_in() {
        let mut tab = Tab::new();
        shell_says(&mut tab, "server", "/srv");

        let pane = crate::panes::Pane::new();
        let second = pane.id();
        tab.layout_mut().split(pane, crate::panes::Edge::Right);
        shell_says(&mut tab, "tests", "/srv/tests");

        assert_eq!(tab.name(), "tests");
        assert_eq!(tab.directory(), Some("/srv/tests"));

        tab.layout_mut().focus_previous();
        assert_eq!(tab.name(), "server");
        assert_eq!(tab.directory(), Some("/srv"));
        assert_eq!(tab.layout().focused(), tab.layout().columns()[0].panes()[0].id());
        assert_ne!(tab.layout().focused(), second);
    }

    #[test]
    fn removing_the_selected_tab_leaves_the_selection_to_the_strip() {
        let (mut project, ids) = project_with_tabs(3);
        project.select(Some(ids[1]));

        assert!(project.remove_tab(ids[1]).is_some());
        assert_eq!(project.selected_id(), None);
        assert_eq!(
            project.tabs().iter().map(Tab::id).collect::<Vec<_>>(),
            vec![ids[0], ids[2]]
        );
    }

    #[test]
    fn closing_every_tab_leaves_the_project_open_and_empty() {
        let (mut project, ids) = project_with_tabs(2);
        for id in ids {
            project.remove_tab(id);
        }
        assert!(project.is_empty());
        assert_eq!(project.name(), "Project 1");
    }

    #[test]
    fn a_dragged_tab_lands_where_it_was_dropped_and_stays_selected() {
        let (mut project, ids) = project_with_tabs(3);
        project.select(Some(ids[0]));

        project.move_tab(ids[0], 2);
        assert_eq!(
            project.tabs().iter().map(Tab::id).collect::<Vec<_>>(),
            vec![ids[1], ids[2], ids[0]]
        );
        assert_eq!(project.selected_id(), Some(ids[0]));

        // Past the end is the end, not a panic.
        project.move_tab(ids[0], 99);
        assert_eq!(project.index_of(ids[0]), Some(2));
    }

    #[test]
    fn a_new_tab_starts_where_the_selected_one_is() {
        let (mut project, ids) = project_with_tabs(2);
        project.custom_directory = Some("/srv/pinned".to_owned());
        assert_eq!(project.directory_for_new_tab(), Some("/srv/pinned"));

        shell_says(project.tab_mut(ids[1]).unwrap(), "zsh", "/home/me/src");
        assert_eq!(project.directory_for_new_tab(), Some("/home/me/src"));

        project.select(Some(ids[0]));
        assert_eq!(project.directory_for_new_tab(), Some("/srv/pinned"));
    }

    #[test]
    fn a_new_project_opens_next_to_the_selected_one() {
        let mut workspace = Workspace::new();
        let first = workspace.open_project();
        let second = workspace.open_project();
        workspace.select(first);
        let third = workspace.open_project();

        assert_eq!(
            workspace.projects().iter().map(Project::id).collect::<Vec<_>>(),
            vec![first, third, second]
        );
        assert_eq!(workspace.selected_id(), Some(third));
    }

    #[test]
    fn an_automatic_name_is_never_handed_out_twice() {
        let mut workspace = Workspace::new();
        let first = workspace.open_project();
        workspace.open_project();
        workspace.close_project(first);

        let third = workspace.open_project();
        assert_eq!(workspace.project(third).unwrap().name(), "Project 3");
    }

    #[test]
    fn closing_a_project_falls_to_its_neighbor() {
        let mut workspace = Workspace::new();
        let first = workspace.open_project();
        let second = workspace.open_project();
        let third = workspace.open_project();

        // The one that took its place, which is the one below it.
        workspace.select(second);
        workspace.close_project(second);
        assert_eq!(workspace.selected_id(), Some(third));

        // And at the end of the list, the one above it.
        workspace.close_project(third);
        assert_eq!(workspace.selected_id(), Some(first));

        workspace.close_project(first);
        assert_eq!(workspace.selected_id(), None);
        assert!(workspace.is_empty());
    }

    #[test]
    fn closing_a_project_that_is_not_selected_leaves_the_selection_alone() {
        let mut workspace = Workspace::new();
        let first = workspace.open_project();
        let second = workspace.open_project();

        workspace.close_project(first);
        assert_eq!(workspace.selected_id(), Some(second));
    }

    #[test]
    fn project_selection_wraps_at_both_ends() {
        let mut workspace = Workspace::new();
        let first = workspace.open_project();
        let second = workspace.open_project();

        workspace.select_next();
        assert_eq!(workspace.selected_id(), Some(first));
        workspace.select_previous();
        assert_eq!(workspace.selected_id(), Some(second));
        workspace.select_previous();
        assert_eq!(workspace.selected_id(), Some(first));
    }

    #[test]
    fn a_dragged_project_lands_where_it_was_dropped() {
        let mut workspace = Workspace::new();
        let first = workspace.open_project();
        let second = workspace.open_project();
        let third = workspace.open_project();

        workspace.move_project(third, 0);
        assert_eq!(
            workspace.projects().iter().map(Project::id).collect::<Vec<_>>(),
            vec![third, first, second]
        );
        assert_eq!(workspace.selected_id(), Some(third));
    }

    /// A directory tree that removes itself, so the root tests touch a real
    /// filesystem — the only way to test a lookup that asks whether `.git`
    /// exists.
    struct TempTree(PathBuf);

    impl TempTree {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("tuni-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("temp dir");
            Self(root)
        }

        fn dir(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            std::fs::create_dir_all(&path).expect("temp dir");
            path
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_panel_root_is_the_closest_repository_above_the_shell() {
        let tree = TempTree::new("panel-root");
        let repo = tree.dir("work/repo");
        let deep = tree.dir("work/repo/crates/core/src");
        tree.dir("work/repo/.git");
        let outside = tree.dir("work/notes");

        let project = Project::new("Project 1");
        assert_eq!(project.panel_root(&deep), (repo.clone(), true));

        // A worktree's `.git` is a file, and it counts too.
        let worktree = tree.dir("work/tree");
        std::fs::write(worktree.join(".git"), "gitdir: elsewhere\n").expect("worktree marker");
        assert_eq!(project.panel_root(&worktree), (worktree.clone(), true));

        // Outside a repository the shell's own directory is the root.
        assert_eq!(project.panel_root(&outside), (outside, true));
    }

    #[test]
    fn a_pinned_root_wins_until_it_stops_existing() {
        let tree = TempTree::new("pinned-root");
        let repo = tree.dir("repo");
        tree.dir("repo/.git");
        let deep = tree.dir("repo/src");
        let pinned = tree.dir("elsewhere");

        let mut project = Project::new("Project 1");
        project.custom_directory = Some(pinned.to_string_lossy().into_owned());
        assert_eq!(project.panel_root(&deep), (pinned.clone(), false));

        std::fs::remove_dir_all(&pinned).expect("remove pin");
        assert_eq!(project.panel_root(&deep), (repo, true));
    }
}
