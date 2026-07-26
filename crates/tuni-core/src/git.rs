//! Repository state, read by running `git`.
//!
//! Kero shells out to the `git` binary rather than linking a library, and this
//! does the same for the same reason: the panel has to agree with the command
//! line the user types beside it, and the only thing that is certain to agree
//! with `git` is `git`. Every read is `--porcelain=v2 -z`, which is the format
//! written to be parsed and the only one that survives a file name containing
//! a space, a quote, or a newline.
//!
//! Nothing here is asynchronous: every call blocks on a subprocess, and the
//! caller is expected to be off the main thread. What can be decided without
//! running anything — which files are staged, whether a commit is possible,
//! what a discard would have to do — is decided here, where it can be tested,
//! and handed back as a [`Task`] for the caller to run.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// How many commits the history list shows.
const RECENT_COMMITS: usize = 8;

/// What a `git` run produced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Output {
    /// The exit status, or -1 when the process could not be started at all.
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.code == 0
    }

    /// The first line worth showing, else `fallback`.
    #[must_use]
    pub fn message(&self, fallback: &str) -> String {
        self.stderr
            .lines()
            .chain(self.stdout.lines())
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or(fallback)
            .to_owned()
    }
}

/// Runs `git` in `directory` and waits for it.
///
/// `GIT_OPTIONAL_LOCKS=0` keeps a background poll from taking `index.lock` out
/// from under the shell beside it, `GIT_TERMINAL_PROMPT=0` turns a credential
/// prompt into a failure rather than a process waiting forever on a terminal
/// nobody can see, and `LC_ALL=C` is what makes reading the error text safe.
pub fn run<S: AsRef<OsStr>>(args: &[S], directory: &Path) -> Output {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
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

/// One changed file, as porcelain v2 reports it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    /// Relative to the repository root, which is not necessarily the shell's
    /// working directory.
    pub path: String,
    /// The index side of the pair: `.` when the index matches HEAD, `?` when
    /// the file is untracked.
    pub staged: char,
    /// The worktree side.
    pub unstaged: char,
    pub conflict: bool,
    /// Where a rename or a copy came from.
    pub original: Option<String>,
}

impl Entry {
    #[must_use]
    pub fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// The directory the file is in, relative to the root, or `""` at the top.
    #[must_use]
    pub fn directory(&self) -> &str {
        match self.path.rfind('/') {
            Some(cut) => &self.path[..cut],
            None => "",
        }
    }

    /// `git add -N`: a path the index knows about with nothing in it.
    ///
    /// It has to be told apart from an ordinary modification, because
    /// restoring it from the index would empty the file the user just wrote.
    #[must_use]
    pub fn is_intent_to_add(&self) -> bool {
        self.staged == '.' && self.unstaged == 'A'
    }

    #[must_use]
    pub fn is_untracked(&self) -> bool {
        self.staged == '?' || self.is_intent_to_add()
    }

    #[must_use]
    pub fn is_worktree_rename(&self) -> bool {
        self.unstaged == 'R' && self.original.is_some()
    }

    #[must_use]
    pub fn is_worktree_copy(&self) -> bool {
        self.unstaged == 'C' && self.original.is_some()
    }

    /// What the row says out loud. Color alone cannot carry the state.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.conflict {
            return "Conflicted".to_owned();
        }
        if self.staged == '?' {
            return "Untracked".to_owned();
        }
        let mut parts = Vec::new();
        if self.staged != '.' {
            parts.push(format!("{} in the index", letter(self.staged)));
        }
        if self.unstaged != '.' {
            parts.push(format!("{} in the working tree", letter(self.unstaged)));
        }
        if parts.is_empty() {
            "Unchanged".to_owned()
        } else {
            parts.join(", ")
        }
    }
}

/// The word behind a porcelain status letter.
#[must_use]
pub fn letter(status: char) -> &'static str {
    match status {
        'M' => "Modified",
        'T' => "Type changed",
        'A' => "Added",
        'D' => "Deleted",
        'R' => "Renamed",
        'C' => "Copied",
        'U' => "Conflicted",
        '?' => "Untracked",
        '!' => "Ignored",
        _ => "Changed",
    }
}

/// One line of the history list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Commit {
    pub hash: String,
    pub short: String,
    pub subject: String,
    pub author: String,
    /// Git's own relative wording — "3 days ago".
    pub when: String,
}

/// Everything the panel draws, for one repository at one moment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Status {
    /// The repository root, which is what every path here is relative to.
    pub root: PathBuf,
    pub branch: Option<String>,
    pub head: Option<String>,
    /// False before the first commit, when there is no HEAD to restore from.
    pub has_head: bool,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub conflicts: Vec<Entry>,
    pub staged: Vec<Entry>,
    pub changed: Vec<Entry>,
    pub branches: Vec<String>,
    pub remotes: Vec<String>,
    pub commits: Vec<Commit>,
    pub stashes: usize,
    /// "Rebase in progress" and its like, read from the git directory.
    pub operation: Option<String>,
    /// Whether the fields below the working tree were read this time round.
    pub details: bool,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            root: PathBuf::new(),
            branch: None,
            head: None,
            // A repository with no commits is the exception, so the field
            // reads true until the status says otherwise.
            has_head: true,
            upstream: None,
            ahead: 0,
            behind: 0,
            conflicts: Vec::new(),
            staged: Vec::new(),
            changed: Vec::new(),
            branches: Vec::new(),
            remotes: Vec::new(),
            commits: Vec::new(),
            stashes: 0,
            operation: None,
            details: false,
        }
    }
}

impl Status {
    #[must_use]
    pub fn change_count(&self) -> usize {
        self.conflicts.len() + self.staged.len() + self.changed.len()
    }

    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.change_count() == 0
    }

    #[must_use]
    pub fn has_upstream(&self) -> bool {
        self.upstream.is_some()
    }

    /// The branch as a header shows it.
    #[must_use]
    pub fn branch_name(&self) -> &str {
        self.branch.as_deref().unwrap_or("no branch")
    }

    /// The remote to publish to when the branch has no upstream: only when
    /// there is exactly one, because picking for the user is not this panel's
    /// decision to make.
    #[must_use]
    pub fn only_remote(&self) -> Option<&str> {
        match self.remotes.as_slice() {
            [only] => Some(only.as_str()),
            _ => None,
        }
    }

    /// Carries the parts a plain poll does not re-read, so a refresh that
    /// skipped them does not blank the branch list and the history.
    pub fn keep_details_from(&mut self, previous: &Status) {
        if self.details || !previous.details {
            return;
        }
        self.branches = previous.branches.clone();
        self.remotes = previous.remotes.clone();
        self.commits = previous.commits.clone();
        self.stashes = previous.stashes;
        self.operation = previous.operation.clone();
        self.details = true;
    }
}

/// What a read of a directory found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Load {
    /// An ordinary directory. Not an error, and the panel offers to make it
    /// one rather than complaining.
    NotRepository,
    Repository(Box<Status>),
    /// A repository that could not be read: broken metadata, a bad
    /// permission, a `git` that is not installed.
    Failed(String),
}

/// Reads a directory's repository, if it has one.
///
/// `details` asks for the branch list, the remotes, the history, the stash
/// count, and any operation in progress. Those change on a human's timescale
/// rather than a build's, so a poll leaves them out and only a move to a
/// different repository or a user's own action asks for them.
#[must_use]
pub fn load(directory: &Path, details: bool) -> Load {
    let top = run(&["rev-parse", "--show-toplevel"], directory);
    if !top.is_ok() {
        let message = top.message("Unable to locate the git repository.");
        // Git says the same thing about a plain directory and about a
        // repository whose metadata is unreadable. The difference is whether
        // there is any metadata at or above it to be broken.
        if top.code == 128
            && message.to_lowercase().contains("not a git repository")
            && !has_git_metadata(directory)
        {
            return Load::NotRepository;
        }
        return Load::Failed(message);
    }

    let root = PathBuf::from(top.stdout.trim_end_matches(['\n', '\r']));
    if root.as_os_str().is_empty() {
        return Load::Failed("Git returned an empty repository path.".to_owned());
    }

    let status = run(
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=all",
        ],
        &root,
    );
    if !status.is_ok() {
        return Load::Failed(status.message("Unable to read git status."));
    }

    let mut result = parse_status(&status.stdout);
    result.root = root.clone();
    if details {
        read_details(&mut result, &root);
    }
    Load::Repository(Box::new(result))
}

fn read_details(status: &mut Status, root: &Path) {
    let refs = run(
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
        root,
    );
    if refs.is_ok() {
        status.branches = refs.stdout.lines().map(str::to_owned).collect();
        status.branches.sort();
    }

    let remotes = run(&["remote"], root);
    if remotes.is_ok() {
        status.remotes = remotes.stdout.lines().map(str::to_owned).collect();
        status.remotes.sort();
    }

    let log = run(
        &[
            "log",
            "-n",
            &RECENT_COMMITS.to_string(),
            "--pretty=format:%H%x1f%h%x1f%s%x1f%an%x1f%ar%x1e",
        ],
        root,
    );
    if log.is_ok() {
        status.commits = parse_commits(&log.stdout);
    }

    let stash = run(
        &["rev-list", "--walk-reflogs", "--count", "refs/stash"],
        root,
    );
    if stash.is_ok() {
        status.stashes = stash.stdout.trim().parse().unwrap_or(0);
    }

    let git_dir = run(&["rev-parse", "--absolute-git-dir"], root);
    if git_dir.is_ok() {
        let path = Path::new(git_dir.stdout.trim_end_matches(['\n', '\r']));
        status.operation = detect_operation(path);
    }
    status.details = true;
}

/// Whether anything at or above `directory` claims to be a repository, which
/// is what tells a plain directory from a broken one.
fn has_git_metadata(directory: &Path) -> bool {
    let mut current = Some(directory);
    while let Some(path) = current {
        if path.join(".git").exists() {
            return true;
        }
        current = path.parent();
    }
    false
}

/// What the repository is in the middle of, read from the files git leaves in
/// its own directory while it is going on.
#[must_use]
pub fn detect_operation(git_directory: &Path) -> Option<String> {
    let exists = |name: &str| git_directory.join(name).exists();
    if exists("rebase-merge") || exists("rebase-apply") {
        Some("Rebase in progress".to_owned())
    } else if exists("MERGE_HEAD") {
        Some("Merge in progress".to_owned())
    } else if exists("CHERRY_PICK_HEAD") {
        Some("Cherry-pick in progress".to_owned())
    } else if exists("REVERT_HEAD") {
        Some("Revert in progress".to_owned())
    } else if exists("BISECT_LOG") {
        Some("Bisect in progress".to_owned())
    } else {
        None
    }
}

/// Reads NUL-delimited porcelain v2.
///
/// The records are fixed-width up to the path, which is the last field and the
/// only one that can hold anything, so every line is split a bounded number of
/// times and whatever is left is the name — spaces, quotes, newlines and all.
#[must_use]
pub fn parse_status(output: &str) -> Status {
    let records: Vec<&str> = output.split('\0').filter(|part| !part.is_empty()).collect();
    let mut status = Status::default();
    let mut entries: Vec<Entry> = Vec::new();
    let mut index = 0;

    while index < records.len() {
        let record = records[index];
        if let Some(oid) = record.strip_prefix("# branch.oid ") {
            status.has_head = oid != "(initial)";
            status.head = status.has_head.then(|| oid.to_owned());
        } else if let Some(name) = record.strip_prefix("# branch.head ") {
            status.branch = Some(if name == "(detached)" {
                "detached HEAD".to_owned()
            } else {
                name.to_owned()
            });
        } else if let Some(name) = record.strip_prefix("# branch.upstream ") {
            status.upstream = Some(name.to_owned());
        } else if let Some(counts) = record.strip_prefix("# branch.ab ") {
            for part in counts.split(' ') {
                if let Some(ahead) = part.strip_prefix('+') {
                    status.ahead = ahead.parse().unwrap_or(0);
                } else if let Some(behind) = part.strip_prefix('-') {
                    status.behind = behind.parse().unwrap_or(0);
                }
            }
        } else if record.starts_with("1 ") {
            if let Some(entry) = ordinary(record) {
                entries.push(entry);
            }
        } else if record.starts_with("2 ") {
            // With -z the path is in this record and the one it came from is
            // the next record, rather than the two being joined by a tab.
            if let Some(mut entry) = renamed(record) {
                entry.original = records.get(index + 1).map(|path| (*path).to_owned());
                if entry.original.is_some() {
                    index += 1;
                }
                entries.push(entry);
            }
        } else if record.starts_with("u ") {
            if let Some(entry) = unmerged(record) {
                entries.push(entry);
            }
        } else if let Some(path) = record.strip_prefix("? ") {
            entries.push(Entry {
                path: path.to_owned(),
                staged: '?',
                unstaged: '?',
                conflict: false,
                original: None,
            });
        }
        index += 1;
    }

    // A file can be in two sections at once — staged one way and changed
    // another — which is what "MM" means and why these are filters rather
    // than a partition.
    status.conflicts = entries.iter().filter(|e| e.conflict).cloned().collect();
    status.staged = entries
        .iter()
        .filter(|e| !e.conflict && e.staged != '.' && e.staged != '?')
        .cloned()
        .collect();
    status.changed = entries
        .iter()
        .filter(|e| !e.conflict && e.unstaged != '.')
        .cloned()
        .collect();
    status
}

/// `1 XY sub mH mI mW hH hI path`
fn ordinary(record: &str) -> Option<Entry> {
    let fields: Vec<&str> = record.splitn(9, ' ').collect();
    let [_, xy, .., path] = fields.as_slice() else {
        return None;
    };
    if fields.len() != 9 {
        return None;
    }
    let (staged, unstaged) = pair(xy)?;
    Some(Entry {
        path: (*path).to_owned(),
        staged,
        unstaged,
        conflict: false,
        original: None,
    })
}

/// `2 XY sub mH mI mW hH hI Xscore path`
fn renamed(record: &str) -> Option<Entry> {
    let fields: Vec<&str> = record.splitn(10, ' ').collect();
    if fields.len() != 10 {
        return None;
    }
    let (staged, unstaged) = pair(fields[1])?;
    Some(Entry {
        path: fields[9].to_owned(),
        staged,
        unstaged,
        conflict: false,
        original: None,
    })
}

/// `u XY sub m1 m2 m3 mW h1 h2 h3 path`
fn unmerged(record: &str) -> Option<Entry> {
    let fields: Vec<&str> = record.splitn(11, ' ').collect();
    if fields.len() != 11 {
        return None;
    }
    let (staged, unstaged) = pair(fields[1])?;
    Some(Entry {
        path: fields[10].to_owned(),
        staged,
        unstaged,
        conflict: true,
        original: None,
    })
}

fn pair(field: &str) -> Option<(char, char)> {
    let mut letters = field.chars();
    let staged = letters.next()?;
    let unstaged = letters.next()?;
    letters.next().is_none().then_some((staged, unstaged))
}

/// Reads the history list, which is written with unit and record separators so
/// that a subject holding anything at all still parses.
#[must_use]
pub fn parse_commits(output: &str) -> Vec<Commit> {
    output
        .split('\u{1e}')
        .filter_map(|record| {
            let fields: Vec<&str> = record.trim_matches(['\n', '\r']).split('\u{1f}').collect();
            let [hash, short, subject, author, when] = fields.as_slice() else {
                return None;
            };
            Some(Commit {
                hash: (*hash).to_owned(),
                short: (*short).to_owned(),
                subject: (*subject).to_owned(),
                author: (*author).to_owned(),
                when: (*when).to_owned(),
            })
        })
        .collect()
}

/// One thing the user asked for: a name to show while it runs and the commands
/// to run, in order, stopping at the first that fails.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Task {
    pub label: String,
    pub commands: Vec<Vec<String>>,
    /// Paths, relative to the root, for the caller to move to the desktop's
    /// trash once every command has succeeded. Git has no such thing as a
    /// recoverable delete, so discarding a file it does not track is the one
    /// operation it cannot be asked to do.
    pub trash: Vec<String>,
}

impl Task {
    fn new(label: impl Into<String>, commands: Vec<Vec<&str>>) -> Self {
        Self {
            label: label.into(),
            commands: commands
                .into_iter()
                .map(|command| command.into_iter().map(str::to_owned).collect())
                .collect(),
            trash: Vec::new(),
        }
    }
}

/// Runs a task's commands in order. `Err` carries what to show the user.
pub fn run_task(task: &Task, root: &Path) -> Result<String, String> {
    let mut transcript = String::new();
    for command in &task.commands {
        let output = run(command, root);
        transcript.push_str(&output.stdout);
        transcript.push_str(&output.stderr);
        if !output.is_ok() {
            return Err(output.message(&format!("{} failed.", task.label)));
        }
    }
    Ok(transcript)
}

/// `--literal-pathspecs`, because a file may be named `*` or `[a]` and the
/// panel is passing a path rather than a pattern.
fn literal(command: &[&str], paths: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = std::iter::once("--literal-pathspecs")
        .chain(command.iter().copied())
        .chain(std::iter::once("--"))
        .map(str::to_owned)
        .collect();
    args.extend(paths.iter().map(|path| (*path).to_owned()));
    args
}

/// The paths one entry stands for: a rename is two, and both have to move
/// together or the index ends up describing a rename that half happened.
fn paths_of(entry: &Entry, renamed_side: char) -> Vec<&str> {
    let mut paths = vec![entry.path.as_str()];
    if renamed_side == 'R'
        && let Some(original) = entry.original.as_deref()
    {
        paths.push(original);
    }
    paths
}

#[must_use]
pub fn stage(entry: &Entry) -> Task {
    Task {
        label: format!("Stage {}", entry.file_name()),
        commands: vec![literal(&["add"], &paths_of(entry, entry.unstaged))],
        trash: Vec::new(),
    }
}

#[must_use]
pub fn unstage(entry: &Entry, has_head: bool) -> Task {
    let paths = paths_of(entry, entry.staged);
    // Before the first commit there is nothing to restore a path from, so
    // unstaging means taking it back out of the index.
    let command = if has_head {
        literal(&["restore", "--staged"], &paths)
    } else {
        literal(&["rm", "--cached", "-f"], &paths)
    };
    Task {
        label: format!("Unstage {}", entry.file_name()),
        commands: vec![command],
        trash: Vec::new(),
    }
}

#[must_use]
pub fn stage_all() -> Task {
    Task::new("Stage all changes", vec![vec!["add", "-A"]])
}

#[must_use]
pub fn unstage_all(has_head: bool) -> Task {
    let command = if has_head {
        vec!["restore", "--staged", "--", "."]
    } else {
        vec!["rm", "--cached", "-r", "-f", "--", "."]
    };
    Task::new("Unstage all changes", vec![command])
}

/// Throws away a working-tree change.
///
/// A tracked file is restored from the index. Anything git is not keeping a
/// copy of — an untracked file, an intent-to-add, the new name of a rename —
/// goes to the trash instead, because there would be nothing to restore it
/// from afterwards.
#[must_use]
pub fn discard(entry: &Entry) -> Task {
    let name = entry.file_name().to_owned();
    if entry.is_intent_to_add() {
        Task {
            label: format!("Discard {name}"),
            commands: vec![literal(&["rm", "--cached", "-f"], &[&entry.path])],
            trash: vec![entry.path.clone()],
        }
    } else if entry.is_untracked() || entry.is_worktree_copy() {
        Task {
            label: format!("Move {name} to the trash"),
            commands: Vec::new(),
            trash: vec![entry.path.clone()],
        }
    } else if entry.is_worktree_rename() {
        let original = entry.original.clone().unwrap_or_default();
        Task {
            label: format!("Discard the rename of {name}"),
            commands: vec![literal(&["restore", "--worktree"], &[&original])],
            trash: vec![entry.path.clone()],
        }
    } else {
        Task {
            label: format!("Discard changes in {name}"),
            commands: vec![literal(&["restore", "--worktree"], &[&entry.path])],
            trash: Vec::new(),
        }
    }
}

/// Discards a whole set at once, over the snapshot the user was shown rather
/// than over whatever the working tree holds by the time they answer: a build
/// running beside the dialog must not have its output swept up by an answer
/// given before it existed.
#[must_use]
pub fn discard_all(entries: &[Entry]) -> Task {
    let mut restore: Vec<String> = Vec::new();
    let mut forget: Vec<String> = Vec::new();
    let mut trash: Vec<String> = Vec::new();

    for entry in entries {
        if entry.is_intent_to_add() {
            forget.push(entry.path.clone());
            trash.push(entry.path.clone());
        } else if entry.is_untracked() || entry.is_worktree_copy() {
            trash.push(entry.path.clone());
        } else if entry.is_worktree_rename() {
            restore.push(entry.original.clone().unwrap_or_default());
            trash.push(entry.path.clone());
        } else {
            restore.push(entry.path.clone());
        }
    }

    let mut commands = Vec::new();
    if !restore.is_empty() {
        let paths: Vec<&str> = restore.iter().map(String::as_str).collect();
        commands.push(literal(&["restore", "--worktree"], &paths));
    }
    if !forget.is_empty() {
        let paths: Vec<&str> = forget.iter().map(String::as_str).collect();
        commands.push(literal(&["rm", "--cached", "-f"], &paths));
    }

    Task {
        label: "Discard all changes".to_owned(),
        commands,
        trash,
    }
}

/// Commits, once there is something to commit and something to call it.
pub fn commit(
    message: &str,
    include_all: bool,
    amend: bool,
    status: &Status,
) -> Result<Task, String> {
    let message = message.trim();
    if message.is_empty() {
        return Err("Enter a commit message.".to_owned());
    }
    if !include_all && !amend && status.staged.is_empty() {
        return Err("Stage changes before committing.".to_owned());
    }

    let mut commands: Vec<Vec<String>> = Vec::new();
    if include_all {
        commands.push(vec!["add".to_owned(), "-A".to_owned()]);
    }
    let mut commit = vec!["commit".to_owned()];
    if amend {
        commit.push("--amend".to_owned());
    }
    commit.push("-m".to_owned());
    commit.push(message.to_owned());
    commands.push(commit);

    let label = if amend {
        "Amend the last commit"
    } else if include_all {
        "Stage everything and commit"
    } else {
        "Commit the staged changes"
    };
    Ok(Task {
        label: label.to_owned(),
        commands,
        trash: Vec::new(),
    })
}

pub fn fetch(status: &Status) -> Result<Task, String> {
    if status.remotes.is_empty() {
        return Err("This repository has no remote to fetch from.".to_owned());
    }
    Ok(Task::new("Fetch", vec![vec!["fetch", "--all", "--prune"]]))
}

pub fn pull(status: &Status) -> Result<Task, String> {
    if !status.has_upstream() {
        return Err("This branch has no upstream to pull from.".to_owned());
    }
    // Fast-forward only: a merge commit is a decision, and one made by a
    // button that says "Pull" is one nobody asked for.
    Ok(Task::new("Pull", vec![vec!["pull", "--ff-only"]]))
}

pub fn push(status: &Status) -> Result<Task, String> {
    if status.has_upstream() {
        return Ok(Task::new("Push", vec![vec!["push"]]));
    }
    if status.branch.as_deref() == Some("detached HEAD") {
        return Err("Switch to a branch before pushing.".to_owned());
    }
    match status.only_remote() {
        Some(remote) => Ok(Task::new(
            format!("Publish the branch to {remote}"),
            vec![vec!["push", "-u", remote, "HEAD"]],
        )),
        None if status.remotes.is_empty() => {
            Err("Add a git remote before publishing this branch.".to_owned())
        }
        None => Err("Choose which remote should receive this branch.".to_owned()),
    }
}

pub fn stash_push(status: &Status, include_untracked: bool) -> Result<Task, String> {
    if status.is_clean() {
        return Err("There is nothing to stash.".to_owned());
    }
    let mut command = vec!["stash", "push"];
    if include_untracked {
        command.push("--include-untracked");
    }
    Ok(Task::new("Stash the changes", vec![command]))
}

pub fn stash_pop(status: &Status) -> Result<Task, String> {
    if status.stashes == 0 {
        return Err("There is no stash to pop.".to_owned());
    }
    Ok(Task::new("Pop the stash", vec![vec!["stash", "pop"]]))
}

pub fn switch_branch(name: &str) -> Task {
    Task {
        label: format!("Switch to {name}"),
        commands: vec![vec!["switch".to_owned(), name.to_owned()]],
        trash: Vec::new(),
    }
}

pub fn create_branch(name: &str) -> Result<Task, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Enter a branch name.".to_owned());
    }
    Ok(Task {
        label: format!("Create {name}"),
        commands: vec![vec!["switch".to_owned(), "-c".to_owned(), name.to_owned()]],
        trash: Vec::new(),
    })
}

#[must_use]
pub fn init() -> Task {
    Task::new("Start a repository here", vec![vec!["init"]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A repository to run against, removed when the test ends.
    struct Sandbox {
        path: PathBuf,
    }

    impl Sandbox {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("tuni-git-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("sandbox");
            let sandbox = Self { path };
            sandbox.git(&["init", "-b", "main"]);
            sandbox.git(&["config", "user.name", "Tuni"]);
            sandbox.git(&["config", "user.email", "tuni@example.invalid"]);
            sandbox
        }

        fn git(&self, args: &[&str]) -> Output {
            let output = run(args, &self.path);
            assert!(output.is_ok(), "git {args:?}: {}", output.stderr);
            output
        }

        fn write(&self, name: &str, contents: &str) {
            fs::write(self.path.join(name), contents).expect("write");
        }

        fn status(&self) -> Status {
            match load(&self.path, true) {
                Load::Repository(status) => *status,
                other => panic!("expected a repository, got {other:?}"),
            }
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn a_branch_header_carries_the_name_and_the_distance() {
        let status = parse_status(
            "# branch.oid abc123\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -3\0",
        );
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.head.as_deref(), Some("abc123"));
        assert!(status.has_head);
        assert_eq!(status.upstream.as_deref(), Some("origin/main"));
        assert_eq!(status.ahead, 2);
        assert_eq!(status.behind, 3);
    }

    #[test]
    fn a_repository_without_a_commit_has_no_head() {
        let status = parse_status("# branch.oid (initial)\0# branch.head main\0");
        assert!(!status.has_head);
        assert_eq!(status.head, None);
    }

    #[test]
    fn a_detached_head_is_named_as_one() {
        let status = parse_status("# branch.head (detached)\0");
        assert_eq!(status.branch.as_deref(), Some("detached HEAD"));
    }

    #[test]
    fn a_file_changed_both_ways_is_in_both_sections() {
        let status = parse_status("1 MM N... 100644 100644 100644 aaa bbb src/main.rs\0");
        assert_eq!(status.staged.len(), 1);
        assert_eq!(status.changed.len(), 1);
        assert_eq!(status.staged[0].path, "src/main.rs");
        assert_eq!(status.change_count(), 2);
    }

    #[test]
    fn a_name_with_spaces_survives_the_parse() {
        let status = parse_status("1 .M N... 100644 100644 100644 aaa bbb my notes.md\0");
        assert_eq!(status.changed[0].path, "my notes.md");
        assert_eq!(status.changed[0].file_name(), "my notes.md");
    }

    #[test]
    fn a_rename_carries_the_path_it_came_from() {
        let status = parse_status(
            "2 R. N... 100644 100644 100644 aaa bbb R100 new.rs\0old.rs\0? extra.txt\0",
        );
        assert_eq!(status.staged.len(), 1);
        assert_eq!(status.staged[0].path, "new.rs");
        assert_eq!(status.staged[0].original.as_deref(), Some("old.rs"));
        // The record after the original path is a record of its own, not a
        // continuation of the rename.
        assert_eq!(status.changed.len(), 1);
        assert_eq!(status.changed[0].path, "extra.txt");
    }

    #[test]
    fn a_conflict_is_its_own_section() {
        let status = parse_status(
            "u UU N... 100644 100644 100644 100644 aaa bbb ccc both.rs\x001 M. N... 100644 100644 100644 aaa bbb clean.rs\0",
        );
        assert_eq!(status.conflicts.len(), 1);
        assert_eq!(status.conflicts[0].path, "both.rs");
        assert!(status.conflicts[0].conflict);
        assert_eq!(status.staged.len(), 1);
        assert_eq!(status.changed.len(), 0);
    }

    #[test]
    fn an_untracked_file_is_a_change_rather_than_a_staged_one() {
        let status = parse_status("? new.txt\0");
        assert!(status.staged.is_empty());
        assert_eq!(status.changed.len(), 1);
        assert!(status.changed[0].is_untracked());
    }

    #[test]
    fn an_intent_to_add_counts_as_untracked() {
        let entry = Entry {
            path: "draft.rs".to_owned(),
            staged: '.',
            unstaged: 'A',
            conflict: false,
            original: None,
        };
        assert!(entry.is_intent_to_add());
        assert!(entry.is_untracked());
        // Restoring it would empty the file, so it is forgotten and trashed.
        let task = discard(&entry);
        assert_eq!(task.trash, vec!["draft.rs".to_owned()]);
        assert_eq!(
            task.commands,
            vec![vec![
                "--literal-pathspecs",
                "rm",
                "--cached",
                "-f",
                "--",
                "draft.rs"
            ]]
        );
    }

    #[test]
    fn discarding_a_tracked_file_restores_it_and_trashes_nothing() {
        let entry = Entry {
            path: "src/main.rs".to_owned(),
            staged: '.',
            unstaged: 'M',
            conflict: false,
            original: None,
        };
        let task = discard(&entry);
        assert!(task.trash.is_empty());
        assert_eq!(task.commands[0].last().unwrap(), "src/main.rs");
        assert!(task.commands[0].contains(&"--worktree".to_owned()));
    }

    #[test]
    fn discarding_a_rename_restores_the_old_name_and_trashes_the_new_one() {
        let entry = Entry {
            path: "new.rs".to_owned(),
            staged: '.',
            unstaged: 'R',
            conflict: false,
            original: Some("old.rs".to_owned()),
        };
        let task = discard(&entry);
        assert_eq!(task.trash, vec!["new.rs".to_owned()]);
        assert_eq!(task.commands[0].last().unwrap(), "old.rs");
    }

    #[test]
    fn discarding_a_set_restores_and_trashes_in_one_pass() {
        let entries = vec![
            Entry {
                path: "kept.rs".to_owned(),
                staged: '.',
                unstaged: 'M',
                conflict: false,
                original: None,
            },
            Entry {
                path: "loose.txt".to_owned(),
                staged: '?',
                unstaged: '?',
                conflict: false,
                original: None,
            },
        ];
        let task = discard_all(&entries);
        assert_eq!(task.trash, vec!["loose.txt".to_owned()]);
        assert_eq!(task.commands.len(), 1);
        assert!(task.commands[0].contains(&"kept.rs".to_owned()));
    }

    #[test]
    fn unstaging_before_the_first_commit_empties_the_index_instead() {
        let entry = Entry {
            path: "first.rs".to_owned(),
            staged: 'A',
            unstaged: '.',
            conflict: false,
            original: None,
        };
        assert!(unstage(&entry, true).commands[0].contains(&"restore".to_owned()));
        assert!(unstage(&entry, false).commands[0].contains(&"--cached".to_owned()));
    }

    #[test]
    fn staging_a_rename_moves_both_of_its_names() {
        let entry = Entry {
            path: "new.rs".to_owned(),
            staged: '.',
            unstaged: 'R',
            conflict: false,
            original: Some("old.rs".to_owned()),
        };
        let command = &stage(&entry).commands[0];
        assert!(command.contains(&"new.rs".to_owned()));
        assert!(command.contains(&"old.rs".to_owned()));
    }

    #[test]
    fn a_commit_needs_a_message_and_something_to_commit() {
        let mut status = Status::default();
        assert!(commit("  ", false, false, &status).is_err());
        assert!(commit("a message", false, false, &status).is_err());
        status.staged.push(Entry {
            path: "a.rs".to_owned(),
            staged: 'M',
            unstaged: '.',
            conflict: false,
            original: None,
        });
        let task = commit("a message", false, false, &status).expect("task");
        assert_eq!(task.commands, vec![vec!["commit", "-m", "a message"]]);
    }

    #[test]
    fn committing_everything_stages_first() {
        let task = commit("all of it", true, false, &Status::default()).expect("task");
        assert_eq!(task.commands[0], vec!["add", "-A"]);
        assert_eq!(task.commands.len(), 2);
    }

    #[test]
    fn pushing_without_an_upstream_publishes_to_the_only_remote() {
        let mut status = Status {
            branch: Some("main".to_owned()),
            ..Status::default()
        };
        assert!(push(&status).is_err());
        status.remotes = vec!["origin".to_owned(), "fork".to_owned()];
        assert!(push(&status).is_err());
        status.remotes = vec!["origin".to_owned()];
        let task = push(&status).expect("task");
        assert_eq!(task.commands[0], vec!["push", "-u", "origin", "HEAD"]);
    }

    #[test]
    fn a_poll_that_skipped_the_details_keeps_the_ones_it_had() {
        let previous = Status {
            branches: vec!["main".to_owned()],
            stashes: 2,
            details: true,
            ..Status::default()
        };
        let mut fresh = Status::default();
        fresh.keep_details_from(&previous);
        assert_eq!(fresh.branches, vec!["main".to_owned()]);
        assert_eq!(fresh.stashes, 2);
        assert!(fresh.details);
    }

    #[test]
    fn the_history_list_survives_a_subject_with_punctuation() {
        let output = "aaa\u{1f}aa\u{1f}Fix: a, b; c\u{1f}Someone\u{1f}2 days ago\u{1e}\nbbb\u{1f}bb\u{1f}Another\u{1f}Someone\u{1f}3 days ago\u{1e}";
        let commits = parse_commits(output);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].subject, "Fix: a, b; c");
        assert_eq!(commits[1].short, "bb");
    }

    #[test]
    fn an_ordinary_directory_is_not_a_failure() {
        let path = std::env::temp_dir().join(format!("tuni-plain-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("directory");
        // Only meaningful when nothing above the temp directory is a
        // repository, which is the normal arrangement.
        if !has_git_metadata(&path) {
            assert_eq!(load(&path, false), Load::NotRepository);
        }
        let _ = fs::remove_dir_all(&path);
    }

    #[test]
    fn a_real_repository_reports_what_was_done_to_it() {
        let sandbox = Sandbox::new("status");
        sandbox.write("README.md", "hello\n");

        let status = sandbox.status();
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert!(!status.has_head);
        assert_eq!(status.changed.len(), 1);
        assert!(status.changed[0].is_untracked());
        assert!(status.staged.is_empty());

        run_task(&stage(&status.changed[0]), &status.root).expect("stage");
        let status = sandbox.status();
        assert_eq!(status.staged.len(), 1);
        assert_eq!(status.staged[0].path, "README.md");

        let task = commit("Add a readme", false, false, &status).expect("task");
        run_task(&task, &status.root).expect("commit");

        let status = sandbox.status();
        assert!(status.is_clean());
        assert!(status.has_head);
        assert_eq!(status.commits.len(), 1);
        assert_eq!(status.commits[0].subject, "Add a readme");
        assert_eq!(status.branches, vec!["main".to_owned()]);
    }

    #[test]
    fn discarding_a_tracked_change_puts_the_file_back() {
        let sandbox = Sandbox::new("discard");
        sandbox.write("file.txt", "first\n");
        sandbox.git(&["add", "file.txt"]);
        sandbox.git(&["commit", "-m", "first"]);
        sandbox.write("file.txt", "second\n");

        let status = sandbox.status();
        assert_eq!(status.changed.len(), 1);
        let task = discard(&status.changed[0]);
        assert!(task.trash.is_empty());
        run_task(&task, &status.root).expect("discard");

        assert_eq!(
            fs::read_to_string(sandbox.path.join("file.txt")).expect("read"),
            "first\n"
        );
        assert!(sandbox.status().is_clean());
    }

    #[test]
    fn stashing_and_popping_come_back_to_the_same_place() {
        let sandbox = Sandbox::new("stash");
        sandbox.write("file.txt", "first\n");
        sandbox.git(&["add", "file.txt"]);
        sandbox.git(&["commit", "-m", "first"]);
        sandbox.write("file.txt", "second\n");

        let status = sandbox.status();
        let task = stash_push(&status, true).expect("task");
        run_task(&task, &status.root).expect("stash");

        let status = sandbox.status();
        assert!(status.is_clean());
        assert_eq!(status.stashes, 1);

        let task = stash_pop(&status).expect("task");
        run_task(&task, &status.root).expect("pop");
        assert_eq!(sandbox.status().changed.len(), 1);
    }
}
