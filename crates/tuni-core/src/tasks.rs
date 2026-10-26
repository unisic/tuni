//! The tasks a checkout already declares, for the palette to run.
//!
//! A project says how it is built and served in a file of its own: the scripts
//! in `package.json`, a `justfile`, a `Makefile`, a `Taskfile.yml`, a
//! `mise.toml`. Listing them puts `pnpm run dev` a name away instead of a file
//! away. Zentty does the same.
//!
//! Each file is read the way its own tool lists it, and no further: a recipe
//! that needs an argument, a private one, a Makefile rule that builds a file
//! rather than doing a job, are all left out, because running them by name
//! alone would fail or surprise.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The most of one file read for tasks. A build file past this was generated,
/// and the palette opens on a keystroke rather than waiting for it.
const LARGEST: u64 = 256 * 1024;

/// The most tasks handed to the palette, which lists every one of them.
const MOST: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    /// What the file declaring it calls it.
    pub name: String,
    /// The line that runs it, quoted for a shell started in `directory`.
    pub command: String,
    /// Where the file declaring it is.
    pub directory: PathBuf,
}

/// The tasks declared in `cwd` and in every directory above it up to the
/// repository holding it, nearest first; outside a repository, only `cwd`'s.
/// Inside a package of a monorepo that is the package's scripts and then the
/// ones at the top.
#[must_use]
pub fn discover(cwd: &Path) -> Vec<Task> {
    let top = crate::workspace::closest_git_repository(cwd);
    let mut tasks = Vec::new();
    for directory in cwd.ancestors() {
        scan(directory, top.as_deref().unwrap_or(cwd), &mut tasks);
        if top.as_deref().is_none_or(|top| top == directory) || tasks.len() >= MOST {
            break;
        }
    }
    tasks.truncate(MOST);
    tasks
}

fn scan(directory: &Path, top: &Path, tasks: &mut Vec<Task>) {
    let mut add = |name: &str, argv: &[&str]| {
        let argv: Vec<String> = argv.iter().map(|&word| word.to_owned()).collect();
        tasks.push(Task {
            name: name.to_owned(),
            // Quoted: a name is whatever the file says, and the file came with
            // the checkout.
            command: crate::ssh::shell_line(&argv),
            directory: directory.to_path_buf(),
        });
    };
    if let Some(text) = read(directory, &["package.json"])
        && let Ok(package) = serde_json::from_str::<Value>(&text)
    {
        let runner = package_runner(&package, directory, top);
        if let Some(scripts) = package.get("scripts").and_then(Value::as_object) {
            for name in scripts.keys() {
                add(name, &[runner, "run", name]);
            }
        }
    }
    if let Some(text) = read(directory, &["justfile", ".justfile", "Justfile"]) {
        for name in just_recipes(&text) {
            add(name, &["just", name]);
        }
    }
    if let Some(text) = read(directory, &["GNUmakefile", "makefile", "Makefile"]) {
        for name in make_targets(&text) {
            add(&name, &["make", &name]);
        }
    }
    if let Some(text) = read(
        directory,
        &[
            "Taskfile.yml",
            "taskfile.yml",
            "Taskfile.yaml",
            "taskfile.yaml",
        ],
    ) {
        for name in taskfile_tasks(&text) {
            add(name, &["task", name]);
        }
    }
    if let Some(text) = read(directory, &["mise.toml", ".mise.toml"]) {
        for name in mise_tasks(&text) {
            add(name, &["mise", "run", name]);
        }
    }
}

/// The first of `names` in `directory` that exists, read whole unless it is
/// past `LARGEST`. The order is the tool's own: `make` takes `GNUmakefile`
/// before `Makefile`.
fn read(directory: &Path, names: &[&str]) -> Option<String> {
    let file = names
        .iter()
        .find_map(|name| fs::File::open(directory.join(name)).ok())?;
    let mut text = String::new();
    file.take(LARGEST + 1).read_to_string(&mut text).ok()?;
    (text.len() as u64 <= LARGEST).then_some(text)
}

/// Which package manager runs the scripts: the one `packageManager` names,
/// otherwise the one whose lockfile sits beside the package or at the top of
/// the repository, which is where a workspace keeps it.
fn package_runner(package: &Value, directory: &Path, top: &Path) -> &'static str {
    const RUNNERS: [&str; 4] = ["pnpm", "yarn", "bun", "npm"];
    const LOCKS: [(&str, &str); 4] = [
        ("pnpm-lock.yaml", "pnpm"),
        ("yarn.lock", "yarn"),
        ("bun.lock", "bun"),
        ("bun.lockb", "bun"),
    ];
    let declared = package
        .get("packageManager")
        .and_then(Value::as_str)
        .and_then(|value| value.split('@').next());
    if let Some(runner) = RUNNERS.into_iter().find(|&runner| Some(runner) == declared) {
        return runner;
    }
    [directory, top]
        .into_iter()
        .find_map(|root| {
            LOCKS
                .into_iter()
                .find(|(lock, _)| root.join(lock).exists())
                .map(|(_, runner)| runner)
        })
        .unwrap_or("npm")
}

/// A name a shell would take as one plain word, which is every name a tool
/// lists as a thing to run. What it leaves out is a pattern rule, a variable,
/// a file path.
fn plain(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || "-_.:".contains(c))
}

/// The recipes `just --list` would show that run with no argument.
///
/// A recipe starts in the first column with its name, its parameters and a
/// colon. Everything else that starts there is an assignment, a setting or an
/// alias, all of which say `:=`, or an attribute in brackets. A parameter with
/// a default, or a variadic one that may be empty, does not need an argument.
fn just_recipes(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| {
            !line.starts_with(char::is_whitespace)
                && !line.starts_with(['#', '['])
                && !line.contains(":=")
        })
        .filter_map(|line| {
            let (header, _) = line.split_once(':')?;
            let mut words = header.split_whitespace();
            let name = words.next()?.trim_start_matches('@');
            let bare = words.all(|word| word.contains('=') || word.starts_with(['*', '$']));
            (bare && plain(name) && !name.starts_with('_')).then_some(name)
        })
        .collect()
}

/// The targets a Makefile marks as jobs rather than files: those named in
/// `.PHONY`, and those with a `## description`, the convention a `make help`
/// prints from.
fn make_targets(text: &str) -> Vec<String> {
    let text = text.replace("\\\n", " ");
    let mut names: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some(phony) = line.strip_prefix(".PHONY:") {
            names.extend(phony.split_whitespace().map(str::to_owned));
        } else if !line.starts_with(char::is_whitespace)
            && line.contains("##")
            && let Some((target, rest)) = line.split_once(':')
            && !rest.starts_with('=')
        {
            names.push(target.trim().to_owned());
        }
    }
    let mut seen = std::collections::HashSet::new();
    names.retain(|name| plain(name) && seen.insert(name.clone()));
    names
}

/// The keys under `tasks:` in a Taskfile, read as indentation rather than as
/// YAML: the first indented key sets the depth, and every key at it is a task.
fn taskfile_tasks(text: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut inside = false;
    let mut depth = None;
    for line in text.lines() {
        let key = line.trim_start();
        if key.is_empty() || key.starts_with('#') {
            continue;
        }
        let indent = line.len() - key.len();
        if indent == 0 {
            inside = key
                .split('#')
                .next()
                .is_some_and(|key| key.trim_end() == "tasks:");
            continue;
        }
        if !inside || *depth.get_or_insert(indent) != indent {
            continue;
        }
        if let Some((name, _)) = key.split_once(':') {
            let name = name.trim().trim_matches(['"', '\'']);
            if plain(name) {
                names.push(name);
            }
        }
    }
    names
}

/// The tasks a `mise.toml` declares, either as `[tasks.name]` tables or as
/// `name = "..."` lines under `[tasks]`. A dotted table below a task, its
/// `env` say, is not one.
fn mise_tasks(text: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut section = "";
    for line in text.lines().map(str::trim) {
        if let Some(header) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            section = header.trim();
            if let Some(name) = section.strip_prefix("tasks.") {
                let name = name.trim_matches('"');
                if plain(name) && !name.contains('.') {
                    names.push(name);
                }
            }
        } else if section == "tasks"
            && !line.starts_with('#')
            && let Some((name, _)) = line.split_once('=')
        {
            let name = name.trim().trim_matches('"');
            if plain(name) {
                names.push(name);
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_justfile_lists_what_runs_without_an_argument() {
        let text = "set shell := [\"bash\", \"-c\"]\n\
                    alias b := build\n\
                    version := \"1\"\n\
                    # Build it\n\
                    [group('dev')]\n\
                    build: clean\n    cargo build\n\
                    @clean:\n    rm -rf target\n\
                    serve port=\"8080\" url=\"http://x\":\n    run\n\
                    deploy target:\n    push {{target}}\n\
                    test *args:\n    cargo test {{args}}\n\
                    _helper:\n    true\n";
        assert_eq!(just_recipes(text), ["build", "clean", "serve", "test"]);
    }

    #[test]
    fn a_makefile_lists_its_jobs_and_not_its_files() {
        let text = ".PHONY: all build \\\n\tcheck $(EXTRA)\n\
                    CC := gcc\n\
                    all: build\n\
                    build: ## compile it\n\tcargo build\n\
                    lint: ## run the linter\n\tclippy\n\
                    target/tuni: src/main.rs\n\tcargo build\n\
                    %.o: %.c\n\t$(CC) -c $<\n";
        assert_eq!(make_targets(text), ["all", "build", "check", "lint"]);
    }

    #[test]
    fn a_taskfile_lists_the_keys_under_tasks_and_nothing_deeper() {
        let text = "version: '3'\n\
                    vars:\n  NAME: x\n\
                    tasks:  # the jobs\n\
                    \x20 build:\n    cmds:\n      - go build\n\
                    \x20 \"lint\": golangci-lint run\n\
                    \x20 # not one\n\
                    \x20 test:\n    deps: [build]\n\
                    includes:\n  docs: ./docs\n";
        assert_eq!(taskfile_tasks(text), ["build", "lint", "test"]);
    }

    #[test]
    fn mise_tasks_come_from_tables_and_from_the_tasks_section() {
        let text = "[tools]\nnode = \"22\"\n\
                    [tasks]\nfmt = \"cargo fmt\"\n# skip = \"x\"\n\
                    [tasks.build]\nrun = \"cargo build\"\n\
                    [tasks.build.env]\nRUST_LOG = \"info\"\n\
                    [tasks.\"dev\"]\nrun = \"vite\"\n";
        assert_eq!(mise_tasks(text), ["fmt", "build", "dev"]);
    }

    #[test]
    fn a_package_inside_a_repository_lists_its_scripts_then_the_top_ones() {
        let base = std::env::temp_dir().join(format!("tuni-tasks-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("repository");
        let package = root.join("packages/web");
        fs::create_dir_all(&package).expect("package directory");
        fs::create_dir(root.join(".git")).expect("repository");
        fs::write(root.join("pnpm-lock.yaml"), "").expect("lockfile");
        fs::write(
            package.join("package.json"),
            r#"{"scripts": {"dev": "vite", "x; rm -rf ~": "oops"}}"#,
        )
        .expect("package.json");
        fs::write(root.join("justfile"), "check:\n    make check\n").expect("justfile");
        // Above the repository, and so never read.
        fs::write(base.join("justfile"), "outside:\n    true\n").expect("outside");

        let tasks = discover(&package);
        let listed: Vec<(&str, &str, &Path)> = tasks
            .iter()
            .map(|task| {
                (
                    task.name.as_str(),
                    task.command.as_str(),
                    task.directory.as_path(),
                )
            })
            .collect();
        assert_eq!(
            listed,
            [
                ("dev", "pnpm run dev", package.as_path()),
                ("x; rm -rf ~", "pnpm run 'x; rm -rf ~'", package.as_path()),
                ("check", "just check", root.as_path()),
            ]
        );

        let _ = fs::remove_dir_all(&base);
    }
}
