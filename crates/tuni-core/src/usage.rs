//! What the coding agent running in a pane has spent.
//!
//! Claude Code, Codex and OpenCode each keep a record of their own turns on
//! disk: the first two a JSONL log per session, the third a SQLite database.
//! Every number here comes from those files, so nothing in this module needs a
//! login, a stored token, or the network.
//!
//! The logs are appended to and the panel polls, so a [`Reader`] remembers how
//! far into each file it read and what that came to. A session log runs to tens
//! of megabytes over an afternoon, and re-reading one every couple of seconds
//! would be the whole cost of the panel.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::info::Process;
use crate::settings::home;

/// The window Claude Code's own usage is counted over, in seconds.
const WINDOW: i64 = 5 * 60 * 60;

/// How many session logs are opened looking for the one that belongs to a
/// directory. Codex writes them a day at a time and the newest are the ones a
/// running session could be in; past this the directory is not the pane's.
const CANDIDATES: usize = 20;

/// One of the agents whose spending can be read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Agent {
    Claude,
    Codex,
    OpenCode,
}

impl Agent {
    /// The agent running under a shell, if one of them is.
    ///
    /// The process list is the one the Info page already polls, so recognising
    /// an agent costs nothing beyond a walk of names that were read anyway.
    /// The comm is checked as well as the executable's name, because Claude
    /// Code's executable is a file named after its version and the comm is
    /// where `claude` survives.
    #[must_use]
    pub fn running(processes: &[Process]) -> Option<Self> {
        processes.iter().find_map(|process| {
            [process.comm.as_str(), process.name.as_str()]
                .into_iter()
                .find_map(|name| match name {
                    "claude" => Some(Self::Claude),
                    "codex" => Some(Self::Codex),
                    "opencode" => Some(Self::OpenCode),
                    _ => None,
                })
        })
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
        }
    }
}

/// What a turn cost, in the four kinds every one of these agents counts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    /// Context handed back from the cache rather than sent again, which is most
    /// of what a long conversation moves.
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Tokens {
    #[must_use]
    pub fn total(self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    fn add(&mut self, other: Self) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }
}

/// How much of the model's context the last turn was carrying.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Fill {
    pub used: u64,
    pub total: u64,
}

impl Fill {
    #[must_use]
    pub fn percent(self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss)]
        {
            self.used as f64 / self.total as f64 * 100.0
        }
    }
}

/// A plan window, as the agent itself reported it.
#[derive(Clone, Debug, PartialEq)]
pub struct Limit {
    /// How long the window is: "7 days", "5 hours".
    pub window: String,
    pub used_percent: f64,
    /// When it starts over, in Unix seconds.
    pub resets_at: Option<i64>,
}

/// Everything the Agent section draws.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub agent: Option<Agent>,
    pub model: Option<String>,
    /// The session running in this directory.
    pub session: Tokens,
    /// Everything the agent spent in the last five hours, wherever it was
    /// working. Claude Code's own limits are counted over a window like this
    /// one, and one pane is never the whole of it.
    pub recent: Option<Tokens>,
    pub context: Option<Fill>,
    pub limits: Vec<Limit>,
}

impl Snapshot {
    /// Whether there is anything worth a row. An agent that has just started
    /// has a log with no turns in it yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.session.total() == 0
            && self.recent.is_none_or(|recent| recent.total() == 0)
            && self.context.is_none()
            && self.limits.is_empty()
    }
}

/// Reads the agents' own records, remembering where each file was left.
#[derive(Debug, Default)]
pub struct Reader {
    logs: HashMap<PathBuf, Tail>,
}

/// What a log has said so far, and how much of it has been read. Which fields
/// are filled depends on the agent that wrote the file.
#[derive(Debug, Default)]
struct Tail {
    /// Bytes already counted. A file that shrank was replaced rather than
    /// appended to, so it is counted again from the start.
    read: u64,
    tokens: Tokens,
    model: Option<String>,
    /// The directory the session was started in, for the logs that say.
    directory: Option<PathBuf>,
    /// One entry per turn, kept only as long as the rolling window needs it.
    turns: Vec<(i64, Tokens)>,
    /// Turns already counted, so a line the agent wrote twice is not paid for
    /// twice.
    seen: HashSet<String>,
    context: Option<Fill>,
    limits: Vec<Limit>,
}

impl Tail {
    /// What the turns inside the window come to.
    fn since(&self, cutoff: i64) -> Tokens {
        let mut tokens = Tokens::default();
        for (_, turn) in self.turns.iter().filter(|(at, _)| *at >= cutoff) {
            tokens.add(*turn);
        }
        tokens
    }

    fn forget_before(&mut self, cutoff: i64) {
        self.turns.retain(|(at, _)| *at >= cutoff);
    }
}

impl Reader {
    /// What the agent working in `cwd` has spent.
    #[must_use]
    pub fn read(&mut self, agent: Agent, cwd: &Path) -> Snapshot {
        match agent {
            Agent::Claude => self.claude(cwd),
            Agent::Codex => self.codex(cwd),
            Agent::OpenCode => opencode(cwd),
        }
    }

    /// Claude Code: one log per session, under a directory named after the
    /// directory the session works in.
    fn claude(&mut self, cwd: &Path) -> Snapshot {
        let mut snapshot = Snapshot {
            agent: Some(Agent::Claude),
            ..Snapshot::default()
        };
        let root = home().join(".claude/projects");

        if let Some(log) = newest(&root.join(claude_slug(cwd))) {
            let tail = self.claude_tail(&log);
            snapshot.session = tail.tokens;
            snapshot.model.clone_from(&tail.model);
        }

        // The window is the account's, not the pane's, so every log the agent
        // has touched inside it counts. Only those can hold a turn that recent,
        // which is what makes this a handful of files rather than the thousand
        // a year of sessions leaves behind.
        let cutoff = now() - WINDOW;
        let mut recent = Tokens::default();
        for log in touched_since(&root, cutoff) {
            recent.add(self.claude_tail(&log).since(cutoff));
        }
        snapshot.recent = Some(recent);
        snapshot
    }

    fn claude_tail(&mut self, log: &Path) -> &Tail {
        let cutoff = now() - WINDOW;
        let tail = self.logs.entry(log.to_path_buf()).or_default();
        read_lines(log, tail, |tail, line| {
            let Ok(entry) = serde_json::from_str::<ClaudeLine>(line) else {
                return;
            };
            let Some(message) = entry.message else {
                return;
            };
            let Some(usage) = message.usage else {
                return;
            };
            // A turn is written out again when the session is resumed, and the
            // pair of identifiers is what tells the copy from the turn.
            let key = format!(
                "{}:{}",
                message.id.unwrap_or_default(),
                entry.request_id.unwrap_or_default()
            );
            if !tail.seen.insert(key) {
                return;
            }
            let tokens = Tokens {
                input: usage.input_tokens,
                output: usage.output_tokens,
                cache_read: usage.cache_read_input_tokens,
                cache_write: usage.cache_creation_input_tokens,
            };
            tail.tokens.add(tokens);
            if let Some(at) = unix_time(&entry.timestamp) {
                tail.turns.push((at, tokens));
            }
            // The last model to answer is the one the session is on: a session
            // that changed models mid-way is on the one it changed to.
            if message.model.is_some() {
                tail.model = message.model;
            }
        });
        tail.forget_before(cutoff);
        tail
    }

    /// Codex: one rollout log per session, filed by date, each carrying the
    /// account's own reading of its plan.
    fn codex(&mut self, cwd: &Path) -> Snapshot {
        let mut snapshot = Snapshot {
            agent: Some(Agent::Codex),
            ..Snapshot::default()
        };

        let mut logs = newest_first(&home().join(".codex/sessions"));
        logs.truncate(CANDIDATES);
        let mut account = None;
        for log in logs {
            let tail = self.codex_tail(&log);
            if account.is_none() && !tail.limits.is_empty() {
                account = Some(tail.limits.clone());
            }
            if tail.directory.as_deref() != Some(cwd) {
                continue;
            }
            snapshot.session = tail.tokens;
            snapshot.model.clone_from(&tail.model);
            snapshot.context = tail.context;
            snapshot.limits.clone_from(&tail.limits);
            break;
        }

        // The plan is the account's rather than this directory's, so the newest
        // session still answers for it when nothing here is Codex's own.
        if snapshot.limits.is_empty() {
            snapshot.limits = account.unwrap_or_default();
        }
        snapshot
    }

    fn codex_tail(&mut self, log: &Path) -> &Tail {
        let tail = self.logs.entry(log.to_path_buf()).or_default();
        read_lines(log, tail, |tail, line| {
            let Ok(entry) = serde_json::from_str::<CodexLine>(line) else {
                return;
            };
            let payload = entry.payload;
            if let Some(cwd) = payload.cwd {
                tail.directory = Some(PathBuf::from(cwd));
            }
            if let Some(model) = payload.model {
                tail.model = Some(model);
            }
            // Codex reports the running total rather than the turn, so the
            // newest count replaces what the last one said.
            if let Some(info) = payload.info {
                let used = info.total_token_usage;
                tail.tokens = Tokens {
                    input: used.input_tokens.saturating_sub(used.cached_input_tokens),
                    output: used.output_tokens,
                    cache_read: used.cached_input_tokens,
                    cache_write: 0,
                };
                if info.model_context_window > 0 {
                    tail.context = Some(Fill {
                        used: info.last_token_usage.total_tokens,
                        total: info.model_context_window,
                    });
                }
            }
            if let Some(limits) = payload.rate_limits {
                tail.limits = [limits.primary, limits.secondary]
                    .into_iter()
                    .flatten()
                    .map(CodexWindow::into_limit)
                    .collect();
            }
        });
        tail
    }
}

/// OpenCode: a database rather than a log, with the totals already added up
/// per session, so there is nothing to remember between reads.
fn opencode(cwd: &Path) -> Snapshot {
    let mut snapshot = Snapshot {
        agent: Some(Agent::OpenCode),
        ..Snapshot::default()
    };

    let path = data_home().join("opencode/opencode.db");
    // Read-only: this is another program's live database, and the panel has no
    // business writing to it or waiting on its lock.
    let Ok(database) = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    ) else {
        return snapshot;
    };

    let row = database.query_one(
        "select model, tokens_input, tokens_output, tokens_cache_read, tokens_cache_write \
         from session where directory = ?1 order by time_updated desc limit 1",
        [cwd.to_string_lossy()],
        |row| {
            // SQLite counts in signed integers and these are counts, so the
            // conversion has one answer and no interesting failure.
            let count = |index| -> rusqlite::Result<u64> {
                Ok(u64::try_from(row.get::<_, i64>(index)?).unwrap_or_default())
            };
            Ok((
                row.get::<_, Option<String>>(0)?,
                Tokens {
                    input: count(1)?,
                    output: count(2)?,
                    cache_read: count(3)?,
                    cache_write: count(4)?,
                },
            ))
        },
    );
    if let Ok((model, tokens)) = row {
        snapshot.model = model.map(|model| opencode_model(&model));
        snapshot.session = tokens;
    }
    snapshot
}

/// OpenCode names the model in a column of JSON, `{"id":..,"providerID":..}`,
/// and the identifier is the part anyone reads.
fn opencode_model(column: &str) -> String {
    serde_json::from_str::<OpenCodeModel>(column)
        .map(|model| model.id)
        .unwrap_or_else(|_| column.to_owned())
}

// --- the files ------------------------------------------------------------

/// Read whatever has been appended since the last read, a line at a time.
///
/// A line still being written has no newline yet, so the read stops at the last
/// complete one and picks the rest up next time.
fn read_lines(log: &Path, tail: &mut Tail, mut line: impl FnMut(&mut Tail, &str)) {
    let Ok(file) = File::open(log) else {
        return;
    };
    let length = file.metadata().map(|meta| meta.len()).unwrap_or_default();
    if length < tail.read {
        *tail = Tail::default();
    }
    if length == tail.read {
        return;
    }

    let mut reader = BufReader::new(file);
    if reader.seek(SeekFrom::Start(tail.read)).is_err() {
        return;
    }
    let mut text = String::new();
    loop {
        text.clear();
        match reader.read_line(&mut text) {
            Ok(0) => break,
            Ok(read) => {
                if !text.ends_with('\n') {
                    break;
                }
                tail.read += read as u64;
                line(tail, text.trim_end());
            }
            Err(_) => break,
        }
    }
}

/// The newest file in a directory, which for a directory of session logs is the
/// session still being written.
fn newest(directory: &Path) -> Option<PathBuf> {
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(directory).ok()?.flatten() {
        let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) else {
            continue;
        };
        if newest.as_ref().is_none_or(|(at, _)| modified > *at) {
            newest = Some((modified, entry.path()));
        }
    }
    newest.map(|(_, path)| path)
}

/// Every log under a tree, newest first.
fn newest_first(root: &Path) -> Vec<PathBuf> {
    let mut logs: Vec<(SystemTime, PathBuf)> = walk(root)
        .into_iter()
        .filter_map(|path| {
            let modified = path.metadata().and_then(|meta| meta.modified()).ok()?;
            Some((modified, path))
        })
        .collect();
    logs.sort_unstable_by(|(left, _), (right, _)| right.cmp(left));
    logs.into_iter().map(|(_, path)| path).collect()
}

/// Every log under a tree written since `cutoff`. A file older than that cannot
/// hold a turn newer than that.
fn touched_since(root: &Path, cutoff: i64) -> Vec<PathBuf> {
    walk(root)
        .into_iter()
        .filter(|path| {
            path.metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
                .is_some_and(|since| since.as_secs() as i64 >= cutoff)
        })
        .collect()
}

/// Every `.jsonl` file under a directory, however deep. Both agents file their
/// logs in directories of their own making: by project, or by date.
fn walk(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                directories.push(path);
            } else if path.extension().is_some_and(|kind| kind == "jsonl") {
                found.push(path);
            }
        }
    }
    found
}

/// What Claude Code calls the directory it keeps a project's sessions in: the
/// path with everything that is not a letter or a digit turned into a dash.
#[must_use]
pub fn claude_slug(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect()
}

/// `$XDG_DATA_HOME`, or `~/.local/share`. Unlike [`crate::settings::data_dir`]
/// this is the directory itself: what lives under it belongs to other programs.
fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home().join(".local/share"))
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or_default()
}

/// Unix seconds out of the timestamp Claude Code writes, which is RFC 3339 in
/// UTC: `2026-07-26T12:32:41.297Z`. Anything else is not a turn's time.
#[must_use]
pub fn unix_time(text: &str) -> Option<i64> {
    let (date, rest) = text.split_once('T')?;
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;

    let clock = rest.split(['.', 'Z', '+']).next()?;
    let mut parts = clock.split(':');
    let hour: i64 = parts.next()?.parse().ok()?;
    let minute: i64 = parts.next()?.parse().ok()?;
    let second: i64 = parts.next()?.parse().ok()?;

    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days between a date and 1970-01-01, by Howard Hinnant's calendar algorithm:
/// the year starts in March so that the leap day lands at the end of it and
/// falls out of the arithmetic.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// How long a window is, said the way a person would: Codex reports it in
/// minutes, and 10080 of them is a week.
#[must_use]
pub fn window_label(minutes: u64) -> String {
    let (count, unit) = if minutes.is_multiple_of(1440) {
        (minutes / 1440, "day")
    } else if minutes.is_multiple_of(60) {
        (minutes / 60, "hour")
    } else {
        (minutes, "minute")
    };
    if count == 1 {
        format!("1 {unit}")
    } else {
        format!("{count} {unit}s")
    }
}

// --- what the logs say ----------------------------------------------------

#[derive(Deserialize)]
struct ClaudeLine {
    #[serde(default)]
    timestamp: String,
    #[serde(default, rename = "requestId")]
    request_id: Option<String>,
    #[serde(default)]
    message: Option<ClaudeMessage>,
}

#[derive(Deserialize)]
struct ClaudeMessage {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<ClaudeUsage>,
}

#[derive(Deserialize)]
struct ClaudeUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

#[derive(Deserialize)]
struct CodexLine {
    payload: CodexPayload,
}

#[derive(Deserialize)]
struct CodexPayload {
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    info: Option<CodexInfo>,
    #[serde(default)]
    rate_limits: Option<CodexLimits>,
}

#[derive(Deserialize)]
struct CodexInfo {
    #[serde(default)]
    total_token_usage: CodexUsage,
    #[serde(default)]
    last_token_usage: CodexUsage,
    #[serde(default)]
    model_context_window: u64,
}

#[derive(Default, Deserialize)]
struct CodexUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cached_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
}

#[derive(Deserialize)]
struct CodexLimits {
    #[serde(default)]
    primary: Option<CodexWindow>,
    #[serde(default)]
    secondary: Option<CodexWindow>,
}

#[derive(Deserialize)]
struct CodexWindow {
    #[serde(default)]
    used_percent: f64,
    #[serde(default)]
    window_minutes: u64,
    #[serde(default)]
    resets_at: Option<i64>,
}

#[derive(Deserialize)]
struct OpenCodeModel {
    id: String,
}

impl CodexWindow {
    fn into_limit(self) -> Limit {
        Limit {
            window: window_label(self.window_minutes),
            used_percent: self.used_percent,
            resets_at: self.resets_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_becomes_the_name_claude_files_it_under() {
        assert_eq!(
            claude_slug(Path::new("/home/dean/.config/ghostty")),
            "-home-dean--config-ghostty"
        );
    }

    #[test]
    fn timestamps_are_read_as_the_second_they_name() {
        assert_eq!(unix_time("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(unix_time("2026-07-26T12:32:41.297Z"), Some(1_785_069_161));
        assert_eq!(unix_time("2024-02-29T00:00:00Z"), Some(1_709_164_800));
        assert_eq!(unix_time("nothing like a time"), None);
    }

    #[test]
    fn a_window_is_named_in_the_largest_unit_that_divides_it() {
        assert_eq!(window_label(10080), "7 days");
        assert_eq!(window_label(300), "5 hours");
        assert_eq!(window_label(1440), "1 day");
        assert_eq!(window_label(90), "90 minutes");
    }

    #[test]
    fn claude_turns_are_added_up_and_counted_once() {
        let directory = tempdir();
        let log = directory.join("session.jsonl");
        let line = |id: &str, request: &str, timestamp: &str| {
            format!(
                r#"{{"timestamp":"{timestamp}","requestId":"{request}","message":{{"id":"{id}","model":"claude-opus-5","usage":{{"input_tokens":10,"output_tokens":20,"cache_read_input_tokens":30,"cache_creation_input_tokens":40}}}}}}"#
            )
        };
        std::fs::write(
            &log,
            format!(
                "{}\n{}\n{}\n",
                line("a", "one", "2026-07-26T12:00:00Z"),
                // The same turn again: a resumed session writes it out twice.
                line("a", "one", "2026-07-26T12:00:00Z"),
                line("b", "two", "2026-07-26T12:00:01Z"),
            ),
        )
        .expect("write");

        let mut reader = Reader::default();
        let tail = reader.claude_tail(&log);
        assert_eq!(
            tail.tokens,
            Tokens {
                input: 20,
                output: 40,
                cache_read: 60,
                cache_write: 80,
            }
        );
        assert_eq!(tail.model.as_deref(), Some("claude-opus-5"));

        // Appending is read from where the last read stopped, and comes to the
        // same as reading the file whole would have.
        std::fs::write(
            &log,
            format!(
                "{}\n{}\n{}\n{}\n",
                line("a", "one", "2026-07-26T12:00:00Z"),
                line("a", "one", "2026-07-26T12:00:00Z"),
                line("b", "two", "2026-07-26T12:00:01Z"),
                line("c", "three", "2026-07-26T12:00:02Z"),
            ),
        )
        .expect("append");
        assert_eq!(reader.claude_tail(&log).tokens.total(), 300);

        std::fs::remove_dir_all(&directory).expect("clean up");
    }

    #[test]
    fn a_half_written_line_is_left_for_the_next_read() {
        let directory = tempdir();
        let log = directory.join("session.jsonl");
        let turn = r#"{"timestamp":"2026-07-26T12:00:00Z","requestId":"one","message":{"id":"a","usage":{"output_tokens":5}}}"#;
        std::fs::write(&log, format!("{turn}\n{{\"timestamp\":\"2026")).expect("write");

        let mut reader = Reader::default();
        assert_eq!(reader.claude_tail(&log).tokens.output, 5);

        std::fs::write(&log, format!("{turn}\n{turn}\n")).expect("finish the line");
        assert_eq!(reader.claude_tail(&log).tokens.output, 5, "same turn twice");

        std::fs::remove_dir_all(&directory).expect("clean up");
    }

    #[test]
    fn codex_reports_a_running_total_and_the_account_it_is_against() {
        let directory = tempdir();
        let log = directory.join("rollout-2026-07-26T12-00-00-abc.jsonl");
        std::fs::write(
            &log,
            concat!(
                r#"{"type":"session_meta","payload":{"cwd":"/work","model":"gpt-5-codex"}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":60,"output_tokens":10,"total_tokens":110},"last_token_usage":{"total_tokens":80},"model_context_window":200},"rate_limits":{"primary":{"used_percent":78.0,"window_minutes":10080,"resets_at":1784539081}}}}"#,
                "\n",
            ),
        )
        .expect("write");

        let mut reader = Reader::default();
        let tail = reader.codex_tail(&log);
        assert_eq!(tail.directory.as_deref(), Some(Path::new("/work")));
        assert_eq!(tail.model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(
            tail.tokens,
            Tokens {
                input: 40,
                output: 10,
                cache_read: 60,
                cache_write: 0,
            },
            "cached input is part of the input Codex reports, not another kind"
        );
        assert_eq!(
            tail.context,
            Some(Fill {
                used: 80,
                total: 200
            })
        );
        assert_eq!(
            tail.limits,
            vec![Limit {
                window: "7 days".to_owned(),
                used_percent: 78.0,
                resets_at: Some(1_784_539_081),
            }]
        );

        std::fs::remove_dir_all(&directory).expect("clean up");
    }

    /// A directory of this test's own, named after the test binary and the
    /// nanosecond it asked, since these tests write files and run in parallel.
    fn tempdir() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("tuni-usage-{stamp}"));
        std::fs::create_dir_all(&path).expect("temporary directory");
        path
    }
}
