//! The directory tree behind the Files panel.
//!
//! A flat list rather than a tree of nodes: only the directories that have been
//! opened are read, and what comes out is exactly the rows to draw, in order,
//! each carrying its own indent. A list view wants a list, a diff between two
//! of them is a comparison of two vectors, and nothing has to walk a structure
//! to answer "what is the third row".
//!
//! Everything here is filesystem and ordering. Trash, clipboards, and file
//! managers are the desktop's business and live in the widget.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// How deep an expansion may go. A symlink pointing at one of its own parents
/// is a cycle the tree would otherwise follow forever.
const MAX_DEPTH: usize = 32;

/// One row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Item {
    pub name: String,
    pub path: PathBuf,
    pub is_directory: bool,
    /// Rows under the root are at zero; the indent is drawn from this.
    pub depth: usize,
}

impl Item {
    /// Whether the name is one the shell hides. Shown, but shown quietly.
    #[must_use]
    pub fn is_hidden(&self) -> bool {
        self.name.starts_with('.')
    }
}

/// Why an operation could not be carried out, in the two parts a dialog wants.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Failure {
    pub message: String,
    pub detail: String,
}

impl Failure {
    fn new(message: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            detail: detail.into(),
        }
    }
}

/// The rows, and which directories are open.
#[derive(Clone, Debug, Default)]
pub struct Tree {
    root: PathBuf,
    items: Vec<Item>,
    expanded: HashSet<PathBuf>,
}

impl Tree {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The last component of the root, which is what the panel is titled.
    #[must_use]
    pub fn root_name(&self) -> &str {
        self.root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_else(|| self.root.to_str().unwrap_or_default())
    }

    #[must_use]
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    #[must_use]
    pub fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(path)
    }

    /// Points the tree at `root` and re-reads whatever is open. Answers whether
    /// the rows changed, so a caller polling on a timer can leave the list view
    /// alone when nothing moved — which is nearly always.
    pub fn sync(&mut self, root: &Path) -> bool {
        if root != self.root {
            self.root = root.to_path_buf();
            // The open directories belonged to the tree that was here before.
            self.expanded.clear();
        }
        self.rebuild()
    }

    /// Opens a directory if it was closed, closes it if it was open.
    pub fn toggle(&mut self, path: &Path) -> bool {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_path_buf());
        }
        self.rebuild()
    }

    pub fn expand(&mut self, path: &Path) -> bool {
        self.expanded.insert(path.to_path_buf());
        self.rebuild()
    }

    /// Forgets a path and everything under it, after it stopped existing.
    pub fn forget(&mut self, path: &Path) -> bool {
        self.expanded.retain(|open| !open.starts_with(path));
        self.rebuild()
    }

    /// Follows a rename, so a directory that was open stays open under its new
    /// name — and so do the directories opened inside it.
    pub fn remap(&mut self, from: &Path, to: &Path) -> bool {
        self.expanded = self
            .expanded
            .drain()
            .map(|open| match open.strip_prefix(from) {
                Ok(rest) => to.join(rest),
                Err(_) => open,
            })
            .collect();
        self.rebuild()
    }

    /// Reads the open directories again. Answers whether anything changed.
    pub fn rebuild(&mut self) -> bool {
        let mut items = Vec::new();
        if self.root.as_os_str().is_empty() {
            let changed = !self.items.is_empty();
            self.items.clear();
            return changed;
        }
        let root = self.root.clone();
        self.append_children(&root, 0, &mut items);
        if items == self.items {
            return false;
        }
        self.items = items;
        true
    }

    fn append_children(&self, directory: &Path, depth: usize, out: &mut Vec<Item>) {
        if depth >= MAX_DEPTH {
            return;
        }
        for child in read_directory(directory) {
            let expanded = child.is_directory && self.expanded.contains(&child.path);
            let path = child.path.clone();
            out.push(Item { depth, ..child });
            if expanded {
                self.append_children(&path, depth + 1, out);
            }
        }
    }
}

/// One directory's entries, sorted the way a file manager sorts them:
/// directories first, then by name the way a person reads names, so `file2`
/// comes before `file10`.
///
/// A directory that cannot be read — a permission, a mount that went away —
/// yields nothing rather than an error: the row above it is still a row, and
/// there is nothing the panel could usefully say about it.
fn read_directory(directory: &Path) -> Vec<Item> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut items: Vec<Item> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            // `.git` is the repository's own bookkeeping, and nobody opens it
            // from a file tree. Every other dotfile is shown.
            if name == ".git" {
                return None;
            }
            Some(Item {
                // Follows symlinks, so a link to a directory opens like one.
                is_directory: entry.path().is_dir(),
                name,
                path: entry.path(),
                depth: 0,
            })
        })
        .collect();
    items.sort_by(|a, b| {
        b.is_directory
            .cmp(&a.is_directory)
            .then_with(|| natural_cmp(&a.name, &b.name))
    });
    items
}

/// Compares names the way a person reads them: a run of digits counts as one
/// number, and case only decides between names that are otherwise the same.
#[must_use]
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut left, mut right) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (left.peek().copied(), right.peek().copied()) {
            (None, None) => break,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let one = take_digits(&mut left);
                let two = take_digits(&mut right);
                // Compared as numbers: length first, since leading zeros are
                // already gone, then digit by digit.
                let ordering = one
                    .len()
                    .cmp(&two.len())
                    .then_with(|| one.as_str().cmp(two.as_str()));
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            (Some(x), Some(y)) => {
                left.next();
                right.next();
                let ordering = x
                    .to_lowercase()
                    .cmp(y.to_lowercase())
                    .then_with(|| x.cmp(&y));
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
        }
    }
    // Equal to the eye: `a1` and `a01` differ only in what was dropped.
    a.cmp(b)
}

fn take_digits(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut digits = String::new();
    while chars.peek().is_some_and(char::is_ascii_digit) {
        digits.push(chars.next().unwrap_or_default());
    }
    let trimmed = digits.trim_start_matches('0');
    if trimmed.is_empty() {
        String::from("0")
    } else {
        trimmed.to_owned()
    }
}

/// A name that can be one component of a path, and not one of the two that
/// mean somewhere else.
fn check_name(name: &str, verb: &str) -> Result<(), Failure> {
    if name.contains('/') || name == "." || name == ".." {
        return Err(Failure::new(
            format!("Couldn't {verb} to “{name}”."),
            "A name can't contain “/” or be “.” or “..”.",
        ));
    }
    Ok(())
}

/// Renames a file or directory in place, answering with where it went.
///
/// `Ok(None)` is the nothing-to-do case: an empty name, or the name it already
/// has. Neither is an error worth a dialog — it is what pressing Enter on an
/// untouched field means.
pub fn rename(path: &Path, new_name: &str) -> Result<Option<PathBuf>, Failure> {
    let trimmed = new_name.trim();
    let current = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if trimmed.is_empty() || trimmed == current {
        return Ok(None);
    }
    check_name(trimmed, "rename")?;
    let Some(parent) = path.parent() else {
        return Err(Failure::new(
            format!("Couldn't rename to “{trimmed}”."),
            "It has no parent directory.",
        ));
    };
    let destination = parent.join(trimmed);
    // On a case-insensitive mount `foo` and `Foo` are the same file, so a
    // change of case only would otherwise look like a collision with itself.
    let case_only = trimmed.to_lowercase() == current.to_lowercase();
    if !case_only && destination.symlink_metadata().is_ok() {
        return Err(Failure::new(
            format!("Couldn't rename to “{trimmed}”."),
            format!("An item named “{trimmed}” already exists here."),
        ));
    }
    fs::rename(path, &destination).map_err(|error| {
        Failure::new(
            format!("Couldn't rename to “{trimmed}”."),
            error.to_string(),
        )
    })?;
    Ok(Some(destination))
}

/// Creates an empty file or a directory inside `parent`, answering with it.
///
/// `Ok(None)` means an empty name, which is how the inline row is cancelled.
pub fn create(parent: &Path, name: &str, directory: bool) -> Result<Option<PathBuf>, Failure> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    check_name(trimmed, "create")?;
    let destination = parent.join(trimmed);
    if destination.symlink_metadata().is_ok() {
        return Err(Failure::new(
            format!("Couldn't create “{trimmed}”."),
            format!("An item named “{trimmed}” already exists here."),
        ));
    }
    let result = if directory {
        fs::create_dir(&destination)
    } else {
        // Fails rather than truncates if something appeared in between.
        fs::File::create_new(&destination).map(|_| ())
    };
    result.map_err(|error| {
        Failure::new(format!("Couldn't create “{trimmed}”."), error.to_string())
    })?;
    Ok(Some(destination))
}

/// A path as an argument to a shell command, safe whatever is in it.
#[must_use]
pub fn shell_quote(path: &Path) -> String {
    let text = path.to_string_lossy();
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory tree under a temporary root, removed when the test ends.
    struct Sandbox(PathBuf);

    impl Sandbox {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("tuni-files-{name}"));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("sandbox");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn directory(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(&path).expect("directory");
            path
        }

        fn file(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            fs::write(&path, b"").expect("file");
            path
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn names(tree: &Tree) -> Vec<(String, usize)> {
        tree.items()
            .iter()
            .map(|item| (item.name.clone(), item.depth))
            .collect()
    }

    #[test]
    fn a_closed_directory_shows_only_itself() {
        let sandbox = Sandbox::new("closed");
        sandbox.directory("src");
        sandbox.file("src/main.rs");
        sandbox.file("README.md");

        let mut tree = Tree::new();
        tree.sync(sandbox.path());
        assert_eq!(
            names(&tree),
            [("src".to_owned(), 0), ("README.md".to_owned(), 0)]
        );
    }

    #[test]
    fn an_opened_directory_shows_its_children_indented() {
        let sandbox = Sandbox::new("opened");
        let src = sandbox.directory("src");
        sandbox.file("src/main.rs");

        let mut tree = Tree::new();
        tree.sync(sandbox.path());
        assert!(tree.toggle(&src), "the rows changed");
        assert!(tree.is_expanded(&src));
        assert_eq!(
            names(&tree),
            [("src".to_owned(), 0), ("main.rs".to_owned(), 1)]
        );

        assert!(tree.toggle(&src));
        assert_eq!(names(&tree), [("src".to_owned(), 0)]);
    }

    #[test]
    fn directories_come_first_and_names_read_as_numbers() {
        let sandbox = Sandbox::new("order");
        sandbox.file("item10.txt");
        sandbox.file("item2.txt");
        sandbox.file("Apple.txt");
        sandbox.directory("zebra");

        let mut tree = Tree::new();
        tree.sync(sandbox.path());
        let listed: Vec<String> = tree.items().iter().map(|item| item.name.clone()).collect();
        assert_eq!(listed, ["zebra", "Apple.txt", "item2.txt", "item10.txt"]);
    }

    #[test]
    fn the_repositorys_own_directory_is_not_listed() {
        let sandbox = Sandbox::new("dotgit");
        sandbox.directory(".git");
        sandbox.file(".gitignore");

        let mut tree = Tree::new();
        tree.sync(sandbox.path());
        let listed: Vec<String> = tree.items().iter().map(|item| item.name.clone()).collect();
        assert_eq!(listed, [".gitignore"], "other dotfiles stay");
    }

    #[test]
    fn a_second_read_of_an_unchanged_tree_reports_nothing_to_do() {
        let sandbox = Sandbox::new("unchanged");
        sandbox.file("one");

        let mut tree = Tree::new();
        assert!(tree.sync(sandbox.path()));
        assert!(!tree.rebuild(), "nothing moved");

        sandbox.file("two");
        assert!(tree.rebuild(), "a file appeared");
    }

    #[test]
    fn moving_the_root_closes_what_was_open_under_the_old_one() {
        let first = Sandbox::new("root-a");
        let src = first.directory("src");
        first.file("src/main.rs");
        let second = Sandbox::new("root-b");
        second.file("other");

        let mut tree = Tree::new();
        tree.sync(first.path());
        tree.toggle(&src);
        assert!(tree.is_expanded(&src));

        tree.sync(second.path());
        assert!(!tree.is_expanded(&src));
        assert_eq!(names(&tree), [("other".to_owned(), 0)]);
    }

    #[test]
    fn renaming_a_directory_keeps_it_and_its_children_open() {
        let sandbox = Sandbox::new("remap");
        let src = sandbox.directory("src");
        let inner = sandbox.directory("src/inner");
        sandbox.file("src/inner/deep.rs");

        let mut tree = Tree::new();
        tree.sync(sandbox.path());
        tree.toggle(&src);
        tree.toggle(&inner);

        let moved = rename(&src, "lib").expect("renamed").expect("moved");
        tree.remap(&src, &moved);

        assert!(tree.is_expanded(&moved));
        assert!(tree.is_expanded(&moved.join("inner")));
        assert_eq!(
            names(&tree),
            [
                ("lib".to_owned(), 0),
                ("inner".to_owned(), 1),
                ("deep.rs".to_owned(), 2),
            ]
        );
    }

    #[test]
    fn a_name_that_is_unchanged_or_empty_is_not_a_rename() {
        let sandbox = Sandbox::new("no-op");
        let file = sandbox.file("one");

        assert_eq!(rename(&file, "one"), Ok(None));
        assert_eq!(rename(&file, "   "), Ok(None));
        assert!(file.exists());
    }

    #[test]
    fn a_name_that_would_land_somewhere_else_is_refused() {
        let sandbox = Sandbox::new("traversal");
        let file = sandbox.file("one");

        for name in ["../escape", "sub/one", "..", "."] {
            let failure = rename(&file, name).expect_err("refused");
            assert!(failure.detail.contains('/'), "says why: {failure:?}");
        }
        assert!(file.exists());
    }

    #[test]
    fn a_name_already_taken_is_refused() {
        let sandbox = Sandbox::new("collision");
        let one = sandbox.file("one");
        sandbox.file("two");

        assert!(rename(&one, "two").is_err());
        assert!(one.exists(), "the original is left alone");
        assert!(create(sandbox.path(), "two", false).is_err());
        assert!(create(sandbox.path(), "two", true).is_err());
    }

    #[test]
    fn creating_answers_with_what_was_created() {
        let sandbox = Sandbox::new("create");

        let file = create(sandbox.path(), "notes.md", false)
            .expect("created")
            .expect("named");
        assert!(file.is_file());

        let directory = create(sandbox.path(), " build ", true)
            .expect("created")
            .expect("named");
        assert!(directory.is_dir());
        assert_eq!(
            directory.file_name().and_then(|n| n.to_str()),
            Some("build")
        );

        assert_eq!(create(sandbox.path(), "  ", false), Ok(None));
    }

    #[test]
    fn a_quoted_path_survives_a_quote_in_it() {
        assert_eq!(shell_quote(Path::new("/tmp/plain")), "'/tmp/plain'");
        assert_eq!(
            shell_quote(Path::new("/tmp/it's here")),
            r"'/tmp/it'\''s here'"
        );
    }

    #[test]
    fn names_sort_the_way_they_are_read() {
        assert_eq!(natural_cmp("file2", "file10"), Ordering::Less);
        assert_eq!(
            natural_cmp("file010", "file10"),
            Ordering::Less,
            "then by text"
        );
        assert_eq!(natural_cmp("Apple", "banana"), Ordering::Less);
        assert_eq!(natural_cmp("a", "a"), Ordering::Equal);
        assert_eq!(natural_cmp("v1.9.0", "v1.10.0"), Ordering::Less);
    }
}
