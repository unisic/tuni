//! What the coding agent running in a pane has spent.
//!
//! Claude Code, Codex, OpenCode and Pi each keep a record of their own turns on
//! disk: a JSONL log per session for all but OpenCode, which uses SQLite.
//! Tokens come from those files, and so do Codex's plan bars, which it writes
//! into its own log. Claude Code's plan is the one thing its log does not
//! carry, so those bars are asked of the account's usage endpoint, the same
//! numbers its website shows, with the login the agent already keeps on disk.
//! Nothing here signs in, and nothing but that one request leaves the machine.
//! The exception is UsageFleet, where it is installed: its server alone knows
//! how much of a group's slice of the plan is spent, so that is asked of it,
//! with the device token its collector keeps.
//!
//! The logs are appended to and the panel polls, so a [`Reader`] remembers how
//! far into each file it read and what that came to. A session log runs to tens
//! of megabytes over an afternoon, and re-reading one every couple of seconds
//! would be the whole cost of the panel.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::info::Process;
use crate::settings::{Agents, home};

/// How long an answer from the plan endpoint keeps being the answer, in
/// seconds. The bars move slowly and the panel polls often; a failure is held
/// just as long, so being offline costs one attempt a minute.
const PLAN_FRESH: i64 = 60;

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
    Pi,
}

impl Agent {
    /// The agent running under a shell, if one of them is, and its process.
    ///
    /// The process list is the one the Info page already polls, so recognising
    /// an agent costs nothing beyond a walk of names that were read anyway.
    /// The comm is checked as well as the executable's name, because Claude
    /// Code's executable is a file named after its version and the comm is
    /// where `claude` survives.
    #[must_use]
    pub fn running(processes: &[Process]) -> Option<(Self, u32)> {
        processes.iter().find_map(|process| {
            [process.comm.as_str(), process.name.as_str()]
                .into_iter()
                .find_map(|name| match name {
                    "claude" => Some(Self::Claude),
                    "codex" => Some(Self::Codex),
                    "opencode" => Some(Self::OpenCode),
                    "pi" => Some(Self::Pi),
                    _ => None,
                })
                .map(|agent| (agent, process.pid))
        })
    }

    /// Whether this one is watched at all. An agent turned off in the settings
    /// is a pane like any other: nothing is read and nothing is asked.
    #[must_use]
    pub fn watched(self, agents: &Agents) -> bool {
        match self {
            Self::Claude => agents.claude,
            Self::Codex => agents.codex,
            Self::OpenCode => agents.opencode,
            Self::Pi => agents.pi,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
            Self::Pi => "Pi",
        }
    }
}

/// Whether a terminal title says the agent in that pane is working on a turn.
///
/// An agent that is thinking spins something, and it spins it in the terminal
/// title as well as on the screen. Claude Code writes `✳ Claude Code` while it
/// waits for a turn and swaps the star for a braille frame while it works:
/// `⠂ Claude Code`, `⠐ Claude Code`, and so on until the answer lands. Measured
/// from its own output rather than assumed, on 2.1.221. By 2.1.311 the frames
/// are two half-filled circles, `◐` and `◑`, swapped about once a second, read
/// out of its bundle, and both are spinners here.
///
/// Reading the title is what makes this affordable. It arrives as an escape
/// sequence the terminal parses anyway, so nothing polls `/proc` for it and
/// nothing scrapes the screen; a pane whose agent says nothing simply never
/// reports, which is also what every non-agent pane does.
///
/// Braille and those two circles are the whole test on purpose. A spinner is
/// drawn out of them and nothing else is: no directory, no command line and no
/// hostname a prompt puts in a title starts with one.
#[must_use]
pub fn thinking(title: &str) -> bool {
    title.chars().next().is_some_and(spinner_frame)
}

fn spinner_frame(glyph: char) -> bool {
    ('\u{2800}'..='\u{28ff}').contains(&glyph) || glyph == '◐' || glyph == '◑'
}

/// The same title with the agent's own status glyph taken off the front.
///
/// Having read the state out of the glyph, tuni draws it as a spinner on the
/// tab, and leaving the glyph in place would say it twice: a tab reading
/// `⠂ Claude Code` beside a spinner is one animation too many, and the frame
/// changing four times a second under a name is what makes a tab strip restless
/// to sit next to. The name stays; the flicker goes.
///
/// The idle star comes off with the spinner frames, not because it animates but
/// because leaving it would make the tab jump between two names as a turn starts
/// and ends. What is left is what the agent calls itself.
#[must_use]
pub fn strip_spinner(title: &str) -> &str {
    let mut rest = title.chars();
    let Some(first) = rest.next() else {
        return title;
    };
    if !spinner_frame(first) && first != '✳' {
        return title;
    }
    let rest = rest.as_str().trim_start();
    // A title that is nothing but the glyph is left alone: an empty one would
    // send the tab back to whatever names an untitled pane, which is a bigger
    // change than the flicker this is here to stop.
    if rest.is_empty() { title } else { rest }
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
    /// Who serves the model, in the agent's own word for it: the same Gemini
    /// is `google` under Pi on an API key and `antigravity` on a Google login.
    pub source: Option<String>,
    /// The session running in this directory.
    pub session: Tokens,
    pub context: Option<Fill>,
    /// The plan's windows, each a share used. The account's, not the pane's.
    pub limits: Vec<Limit>,
    /// This device's UsageFleet group, where there is one.
    pub fleet: Option<Fleet>,
}

/// UsageFleet's view of this device's group: how much of the slice of the plan
/// the group was given it has spent, window by window.
#[derive(Clone, Debug, PartialEq)]
pub struct Fleet {
    /// The group, as the dashboard names it.
    pub group: String,
    /// "5 hours" and "7 days", and a model's own window when that is the one
    /// refusing prompts.
    pub limits: Vec<Limit>,
    /// The window in `limits` whose share is spent, while UsageFleet refuses
    /// new prompts.
    pub blocked: Option<String>,
}

impl Snapshot {
    /// Whether there is anything worth a row. An agent that has just started
    /// has a log with no turns in it yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.session.total() == 0
            && self.context.is_none()
            && self.limits.is_empty()
            && self.fleet.is_none()
    }
}

/// Reads the agents' own records, remembering where each file was left.
#[derive(Debug, Default)]
pub struct Reader {
    logs: HashMap<PathBuf, Tail>,
    /// The last word from Claude Code's plan endpoint and when it arrived,
    /// because a request a minute is plenty for bars that move by the hour.
    plan: Option<Plan>,
    /// The last word from UsageFleet and when it came, held as long as a plan
    /// reading and for the same reason.
    fleet: Option<(i64, Option<Fleet>)>,
    /// Who serves Claude Code, for the process it was read from. Its
    /// environment cannot change once it runs, so it is read once a process.
    claude_source: Option<(u32, &'static str)>,
}

/// A fetched reading of the plan, held until it is stale.
#[derive(Debug)]
struct Plan {
    at: i64,
    login: Login,
    limits: Vec<Limit>,
}

/// Whose Claude login the plan is asked with: the one Claude Code keeps, or
/// the one Pi signed in with itself when it is on Anthropic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Login {
    Claude,
    Pi,
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
    source: Option<String>,
    /// The directory the session was started in, for the logs that say.
    directory: Option<PathBuf>,
    /// Turns already counted, so a line the agent wrote twice is not paid for
    /// twice.
    seen: HashSet<String>,
    context: Option<Fill>,
    limits: Vec<Limit>,
}

impl Reader {
    /// What the agent working in `cwd` has spent.
    ///
    /// `plan` decides the one part of this that is not a file read: Claude
    /// Code's plan bars, which come off the account's usage page. Everything
    /// else is the agent's own logs and stays on the machine either way.
    /// `fleet` decides whether UsageFleet is asked for the group's budget.
    /// `pid` is the agent's own process.
    #[must_use]
    pub fn read(
        &mut self,
        agent: Agent,
        pid: u32,
        cwd: &Path,
        plan: bool,
        fleet: bool,
    ) -> Snapshot {
        let mut snapshot = match agent {
            Agent::Claude => self.claude(pid, cwd, plan),
            Agent::Codex => self.codex(cwd),
            Agent::OpenCode => opencode(cwd),
            Agent::Pi => self.pi(pid, cwd, plan),
        };
        if !fleet {
            self.fleet = None;
        }
        // UsageFleet meters these two agents' logs, and its guard sits in
        // front of their prompts.
        if fleet && matches!(agent, Agent::Claude | Agent::Pi) {
            snapshot.fleet = self.fleet_budget();
        }
        snapshot
    }

    /// The group's budget, asked at most once a minute. A failed ask keeps the
    /// previous answer, as the plan's does.
    fn fleet_budget(&mut self) -> Option<Fleet> {
        let at = now();
        if let Some((asked, fleet)) = &self.fleet
            && at - asked < PLAN_FRESH
        {
            return fleet.clone();
        }
        let fleet = fetch_fleet().or_else(|| self.fleet.take().and_then(|(_, fleet)| fleet));
        self.fleet = Some((at, fleet.clone()));
        fleet
    }

    /// Claude Code: one log per session, under a directory named after the
    /// directory the session works in.
    fn claude(&mut self, pid: u32, cwd: &Path, plan: bool) -> Snapshot {
        let mut snapshot = Snapshot {
            agent: Some(Agent::Claude),
            source: Some(self.claude_source(pid).to_owned()),
            ..Snapshot::default()
        };
        if let Some(log) = claude_log(&home().join(".claude"), pid, cwd) {
            let tail = self.claude_tail(&log);
            snapshot.session = tail.tokens;
            snapshot.model.clone_from(&tail.model);
        }

        // No bars rather than stale ones: a cached answer redrawn after the
        // switch went off would look like the request was still being made.
        snapshot.limits = if plan {
            self.claude_plan(Login::Claude)
        } else {
            self.plan = None;
            Vec::new()
        };
        snapshot
    }

    /// The plan's windows, the way the account's own usage page draws them,
    /// cached so the answer is asked for at most once a minute. The one thing
    /// the log cannot say, since the plan is spent by every session at once.
    fn claude_plan(&mut self, login: Login) -> Vec<Limit> {
        let at = now();
        if let Some(plan) = &self.plan
            && plan.login == login
            && at - plan.at < PLAN_FRESH
        {
            return plan.limits.clone();
        }
        // A failed ask keeps the previous answer on the bars: percentages a
        // minute or two old are still the picture, and the sky being out is no
        // reason to blank the panel. Only the same login's, though: another
        // one can be another account.
        let previous = self.plan.take().filter(|plan| plan.login == login);
        let limits = fetch_plan(login).or_else(|| previous.map(|plan| plan.limits));
        let limits = limits.unwrap_or_default();
        self.plan = Some(Plan {
            at,
            login,
            limits: limits.clone(),
        });
        limits
    }

    /// Claude Code's log names the model and not who served it, so this asks
    /// the switches it chooses that by, in the environment it was started in.
    fn claude_source(&mut self, pid: u32) -> &'static str {
        match self.claude_source {
            Some((read, source)) if read == pid => source,
            _ => {
                let environ = std::fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
                let source = claude_platform(&environ);
                self.claude_source = Some((pid, source));
                source
            }
        }
    }

    fn claude_tail(&mut self, log: &Path) -> &Tail {
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
            // The last model to answer is the one the session is on: a session
            // that changed models mid-way is on the one it changed to.
            if message.model.is_some() {
                tail.model = message.model;
            }
        });
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
            snapshot.source.clone_from(&tail.source);
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
            if let Some(provider) = payload.model_provider {
                tail.source = Some(provider);
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

    /// Pi: one log per session, filed under a directory named after the working
    /// directory, which the log's own opening line names outright.
    ///
    /// Neither the context window nor a plan is written down anywhere in there,
    /// so the session's spending is all this can honestly report.
    fn pi(&mut self, pid: u32, cwd: &Path, plan: bool) -> Snapshot {
        let mut snapshot = Snapshot {
            agent: Some(Agent::Pi),
            ..Snapshot::default()
        };

        // Pi writes its log with the first answer and not before, so until
        // then the newest one here is the run before this one, with its model
        // and its total. A log older than the process is not the process's.
        // ponytail: a session resumed with `pi -c` is older too, and shows
        // nothing until its next answer; the session id on Pi's command line
        // would name it, if that wait ever matters.
        let started = crate::info::age(pid).map(|age| SystemTime::now() - Duration::from_secs(age));
        let mut logs = newest_first(&home().join(".pi/agent/sessions"));
        logs.truncate(CANDIDATES);
        let Some(log) = logs
            .into_iter()
            .take_while(|log| started.is_none_or(|started| modified(log) >= Some(started)))
            .find(|log| self.pi_works_in(log, cwd))
        else {
            return snapshot;
        };
        let tail = self.pi_tail(&log);
        snapshot.session = tail.tokens;
        snapshot.model.clone_from(&tail.model);
        snapshot.source.clone_from(&tail.source);
        // On Anthropic with a Claude login, Pi spends the plan Claude Code
        // does, and its own login can ask for the same bars.
        if plan && snapshot.source.as_deref() == Some("anthropic") {
            snapshot.limits = self.claude_plan(Login::Pi);
        }
        snapshot
    }

    /// Whether a Pi log is this directory's, off the header it opens with.
    ///
    /// One line of each candidate rather than the whole of every one: a session
    /// Pi has been in for a day runs to tens of megabytes and only one of them
    /// belongs to the pane. The answer is kept with the log, so the line is read
    /// once however long the session lasts.
    fn pi_works_in(&mut self, log: &Path, cwd: &Path) -> bool {
        let tail = self.logs.entry(log.to_path_buf()).or_default();
        if tail.directory.is_none() {
            let mut header = String::new();
            if let Ok(file) = File::open(log) {
                let _ = BufReader::new(file).read_line(&mut header);
            }
            tail.directory = serde_json::from_str::<PiLine>(&header)
                .ok()
                .and_then(|entry| entry.cwd)
                .map(PathBuf::from);
        }
        tail.directory.as_deref() == Some(cwd)
    }

    fn pi_tail(&mut self, log: &Path) -> &Tail {
        let tail = self.logs.entry(log.to_path_buf()).or_default();
        read_lines(log, tail, |tail, line| {
            let Ok(entry) = serde_json::from_str::<PiLine>(line) else {
                return;
            };
            // A switch is written the moment it is made, a turn only once it
            // is answered, so this is what says the model changed in between.
            if entry.kind.as_deref() == Some("model_change") && entry.model_id.is_some() {
                tail.model = entry.model_id;
                tail.source = entry.provider;
                return;
            }
            let Some(message) = entry.message else {
                return;
            };
            let Some(usage) = message.usage else {
                return;
            };
            // Pi counts the turn rather than the running total, and the entry's
            // own identifier tells a turn from a copy of it.
            if !tail.seen.insert(entry.id.unwrap_or_default()) {
                return;
            }
            // Reasoning is part of the output rather than beside it: these four
            // are the four that come to the `totalTokens` Pi writes next to
            // them, and adding the fifth would count it twice.
            tail.tokens.add(Tokens {
                input: usage.input,
                output: usage.output,
                cache_read: usage.cache_read,
                cache_write: usage.cache_write,
            });
            if message.model.is_some() {
                tail.model = message.model;
                tail.source = message.provider;
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
        if let Some(model) = model {
            (snapshot.model, snapshot.source) = opencode_model(&model);
        }
        snapshot.session = tokens;
    }
    snapshot
}

/// OpenCode names the model in a column of JSON, `{"id":..,"providerID":..}`:
/// the model, and who serves it.
fn opencode_model(column: &str) -> (Option<String>, Option<String>) {
    serde_json::from_str::<OpenCodeModel>(column).map_or_else(
        |_| (Some(column.to_owned()), None),
        |model| (Some(model.id), model.provider),
    )
}

/// Who serves Claude Code, in its own word for it, from the environment it
/// runs in: Anthropic, unless one of the switches below sends it to a cloud.
/// The order and the words are Claude Code's own, read out of 2.1.311.
///
/// ponytail: only what it was started with. A switch set under `env` in its
/// settings file, or a gateway login, is applied inside it and reads as
/// `anthropic` here.
fn claude_platform(environ: &[u8]) -> &'static str {
    let on = |name: &str| {
        environ.split(|byte| *byte == 0).any(|pair| {
            pair.strip_prefix(name.as_bytes())
                .and_then(|rest| rest.strip_prefix(b"="))
                .and_then(|value| std::str::from_utf8(value).ok())
                .is_some_and(|value| {
                    ["1", "true", "yes", "on"].contains(&value.trim().to_lowercase().as_str())
                })
        })
    };
    [
        ("CLAUDE_CODE_USE_BEDROCK", "bedrock"),
        ("CLAUDE_CODE_USE_FOUNDRY", "foundry"),
        ("CLAUDE_CODE_USE_ANTHROPIC_AWS", "anthropicAws"),
        (
            "CLAUDE_CODE_USE_ANTHROPIC_GOOGLE_CLOUD",
            "anthropicGoogleCloud",
        ),
        ("CLAUDE_CODE_USE_MANTLE", "mantle"),
        ("CLAUDE_CODE_USE_VERTEX", "vertex"),
    ]
    .into_iter()
    .find(|(name, _)| on(name))
    .map_or("anthropic", |(_, source)| source)
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

/// The record the Claude Code in `pid` is writing, under `claude` (its
/// `~/.claude`).
///
/// Each running Claude Code names its conversation in `sessions/<pid>.json`
/// and writes the new one there when `/clear` or a resume moves it, so a pane
/// gets its own and not whichever other one in the same checkout was written
/// to last. Read on 2.1.310 and 2.1.311. Where that file is missing the newest
/// record stands in, which is the guess this made before.
fn claude_log(claude: &Path, pid: u32, cwd: &Path) -> Option<PathBuf> {
    let store = claude.join("projects").join(claude_slug(cwd));
    let Ok(live) = std::fs::read_to_string(claude.join(format!("sessions/{pid}.json"))) else {
        return newest(&store);
    };
    let id = serde_json::from_str::<ClaudeLive>(&live).ok()?.session_id;
    // Joined onto a path, so nothing an id is not made of.
    let valid = !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    valid.then(|| store.join(format!("{id}.jsonl")))
}

fn modified(path: &Path) -> Option<SystemTime> {
    path.metadata().and_then(|meta| meta.modified()).ok()
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

/// Asks the account's usage endpoint how full the plan's windows are, with the
/// login the agent keeps on disk. The request goes
/// to the same place the agent itself talks to, and it carries nothing but the
/// token that was already there.
fn fetch_plan(login: Login) -> Option<Vec<Limit>> {
    let (token, expires) = match login {
        Login::Claude => {
            let credentials = home().join(".claude/.credentials.json");
            let credentials = std::fs::read_to_string(credentials).ok()?;
            let oauth = serde_json::from_str::<Credentials>(&credentials)
                .ok()?
                .claude_ai_oauth;
            (oauth.access_token, oauth.expires_at)
        }
        // Pi keeps its own sign-in, made with Claude Code's client and the
        // same `user:profile` scope the usage page wants. A key in its place
        // has no plan behind it, and no expiry either.
        Login::Pi => {
            let auth = std::fs::read_to_string(home().join(".pi/agent/auth.json")).ok()?;
            let login = serde_json::from_str::<PiAuth>(&auth).ok()?.anthropic?;
            (login.access, login.expires)
        }
    };
    // An expired token would only buy a 401: the agent refreshes it while it
    // runs, and the bars are only drawn while it runs.
    if expires <= now().saturating_mul(1000) {
        return None;
    }

    let body = get(
        "https://api.anthropic.com/api/oauth/usage",
        &format!("Authorization: Bearer {token}"),
        &["anthropic-beta: oauth-2025-04-20"],
    )?;
    Some(plan_limits(&body))
}

/// Asks UsageFleet's server for this device's group, the endpoint its own
/// prompt guard reads before every prompt and which it keeps for status bars.
/// The device token is the one its collector keeps in
/// `~/.config/usagefleet/config.json`, picked the way the collector picks it:
/// the one bound to the Claude account signed in here, else the default.
fn fetch_fleet() -> Option<Fleet> {
    // Beside Tuni's own directory, which already honours `XDG_CONFIG_HOME`.
    let config = crate::settings::config_dir().with_file_name("usagefleet");
    let config = std::fs::read_to_string(config.join("config.json")).ok()?;
    let config = serde_json::from_str::<FleetConfig>(&config).ok()?;
    let account = std::fs::read_to_string(home().join(".claude.json"))
        .ok()
        .and_then(|state| serde_json::from_str::<ClaudeState>(&state).ok())
        .and_then(|state| state.oauth_account)
        .map(|account| account.account_uuid);
    let token = account
        .and_then(|account| config.account_tokens.get(&account))
        .or(config.token.as_ref())?;
    let body = get(
        "https://usagefleet.com/api/v1/limits",
        &format!("x-api-key: {token}"),
        &[],
    )?;
    fleet(&body)
}

/// A GET through `curl`, with the header that carries the token written to its
/// stdin, where `/proc` cannot read it off the argument list the way it could a
/// flag. These are the only requests the module sends anywhere.
fn get(url: &str, secret: &str, headers: &[&str]) -> Option<String> {
    // Named for what it is rather than as curl, whose own `curl/8.15.0` the
    // plan endpoint refused with a 429 every time, measured on 2026-10-26.
    let agent = concat!("tuni/", env!("CARGO_PKG_VERSION"));
    let mut args = vec!["-sf", "--max-time", "4", "-A", agent, "-H", "@-"];
    for header in headers {
        args.extend(["-H", header]);
    }
    args.push(url);
    let mut curl = Command::new("curl")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    curl.stdin
        .take()?
        .write_all(format!("{secret}\n").as_bytes())
        .ok()?;
    let output = curl.wait_with_output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The group's reading, as UsageFleet's limits endpoint answers it.
fn fleet(json: &str) -> Option<Fleet> {
    let view = serde_json::from_str::<FleetView>(json).ok()?;
    // Until a collector has reported, the percentages are zeros standing in
    // for nothing, and a device in no group has no share to be a part of.
    view.reported_at?;
    let group = view.group?;
    let window = |label: &str, used: Option<f64>| Limit {
        window: label.to_owned(),
        used_percent: used.unwrap_or(0.0),
        resets_at: None,
    };
    let mut limits = vec![
        window("5 hours", view.session_pct),
        window("7 days", view.weekly_pct),
    ];
    let blocked = (view.blocked == Some(true)).then(|| {
        let tripped = usize::from(view.blocked_window.as_deref() == Some("weekly"));
        let resets_at = view.blocked_until.as_deref().and_then(unix_time);
        if let Some(model) = view.blocked_model {
            // A model's own window ran out rather than the group's share of
            // the plan, so it gets a bar of its own.
            let label = format!("{model} {}", limits[tripped].window);
            limits.push(Limit {
                resets_at,
                ..window(&label, view.blocked_pct.or(Some(100.0)))
            });
            label
        } else {
            limits[tripped].resets_at = resets_at;
            limits[tripped].window.clone()
        }
    });
    Some(Fleet {
        group,
        limits,
        blocked,
    })
}

/// The endpoint's windows, shortest first. Every window with a reading becomes
/// a bar, whatever it is called: the names are minted per model, `seven_day`,
/// `seven_day_opus`, whatever the next model's will be, and a list written
/// here would go stale with the next launch.
fn plan_limits(json: &str) -> Vec<Limit> {
    let Ok(serde_json::Value::Object(windows)) = serde_json::from_str(json) else {
        return Vec::new();
    };
    let mut limits: Vec<(u64, Limit)> = windows
        .iter()
        .filter_map(|(key, window)| {
            let utilization = window.get("utilization")?.as_f64()?;
            let (minutes, label) = plan_window(key);
            Some((
                minutes,
                Limit {
                    window: label,
                    used_percent: utilization,
                    resets_at: window.get("resets_at").and_then(reset_time),
                },
            ))
        })
        .collect();
    limits.sort_by(|(left, a), (right, b)| left.cmp(right).then_with(|| a.window.cmp(&b.window)));
    limits.into_iter().map(|(_, limit)| limit).collect()
}

/// A window's name, read as the endpoint writes them: a number word, a unit,
/// and sometimes the model the window is scoped to, as in `five_hour` and
/// `seven_day_opus`. Returns the window's length for sorting and the label to
/// draw; a name shaped some other way is shown as it stands, at the end.
fn plan_window(key: &str) -> (u64, String) {
    let parts: Vec<&str> = key.split('_').collect();
    let spelled = (u64::MAX, key.replace('_', " "));
    let Some((count, unit)) = parts.first().zip(parts.get(1)) else {
        return spelled;
    };
    let count = match count.parse::<u64>() {
        Ok(count) => count,
        Err(_) => match *count {
            "one" => 1,
            "two" => 2,
            "three" => 3,
            "four" => 4,
            "five" => 5,
            "six" => 6,
            "seven" => 7,
            "eight" => 8,
            "nine" => 9,
            "ten" => 10,
            _ => return spelled,
        },
    };
    let unit = match unit.trim_end_matches('s') {
        "minute" => 1,
        "hour" => 60,
        "day" => 1440,
        "week" => 10080,
        _ => return spelled,
    };
    let minutes = count * unit;
    let mut label = window_label(minutes);
    // The rest of the name is what the window is scoped to, usually a model,
    // and a model's name starts with a capital.
    let scope = parts.get(2..).unwrap_or_default().join(" ");
    let mut letters = scope.chars();
    if let Some(first) = letters.next() {
        label = format!(
            "{label} ({}{})",
            first.to_ascii_uppercase(),
            letters.as_str()
        );
    }
    (minutes, label)
}

/// When a window starts over, however the endpoint said it: a timestamp
/// written out, or seconds already counted.
fn reset_time(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::String(text) => unix_time(text),
        other => other.as_i64(),
    }
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
    model_provider: Option<String>,
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
    #[serde(rename = "providerID", default)]
    provider: Option<String>,
}

/// A line of a Pi session log: the header that opens it, one message, or a
/// switch of model.
#[derive(Deserialize)]
struct PiLine {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    message: Option<PiMessage>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default, rename = "modelId")]
    model_id: Option<String>,
}

#[derive(Deserialize)]
struct PiMessage {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    usage: Option<PiUsage>,
}

#[derive(Deserialize)]
struct PiUsage {
    #[serde(default)]
    input: u64,
    #[serde(default)]
    output: u64,
    #[serde(default, rename = "cacheRead")]
    cache_read: u64,
    #[serde(default, rename = "cacheWrite")]
    cache_write: u64,
}

/// UsageFleet's configuration, for its device tokens.
#[derive(Deserialize)]
struct FleetConfig {
    token: Option<String>,
    /// A token per Claude account, for a machine that signs in to more than
    /// one and files each one's usage in its own fleet.
    #[serde(default, rename = "accountTokens")]
    account_tokens: HashMap<String, String>,
}

/// `~/.claude/sessions/<pid>.json`, for which conversation a Claude Code is in.
#[derive(Deserialize)]
struct ClaudeLive {
    #[serde(rename = "sessionId")]
    session_id: String,
}

/// `~/.claude.json`, for which account Claude Code is signed in to.
#[derive(Deserialize)]
struct ClaudeState {
    #[serde(rename = "oauthAccount")]
    oauth_account: Option<OauthAccount>,
}

#[derive(Deserialize)]
struct OauthAccount {
    #[serde(rename = "accountUuid")]
    account_uuid: String,
}

/// What UsageFleet's limits endpoint answers. Every field may be missing,
/// which is how a server older than one of them answers.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FleetView {
    group: Option<String>,
    session_pct: Option<f64>,
    weekly_pct: Option<f64>,
    blocked: Option<bool>,
    blocked_window: Option<String>,
    blocked_model: Option<String>,
    blocked_pct: Option<f64>,
    blocked_until: Option<String>,
    reported_at: Option<String>,
}

/// The file Claude Code keeps its login in.
#[derive(Deserialize)]
struct Credentials {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Oauth,
}

#[derive(Deserialize)]
struct Oauth {
    #[serde(rename = "accessToken")]
    access_token: String,
    /// In milliseconds, the way JavaScript keeps a moment.
    #[serde(default, rename = "expiresAt")]
    expires_at: i64,
}

/// The file Pi keeps its logins and keys in, of which only Anthropic's is read.
#[derive(Deserialize)]
struct PiAuth {
    anthropic: Option<PiLogin>,
}

#[derive(Deserialize)]
struct PiLogin {
    #[serde(default)]
    access: String,
    /// In milliseconds, as Claude Code's.
    #[serde(default)]
    expires: i64,
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
    fn a_spinner_in_the_title_says_the_agent_is_working() {
        // Measured from Claude Code itself, both states.
        assert!(thinking("⠂ Claude Code"));
        assert!(thinking("⠐ Claude Code"));
        assert!(thinking("◐ Claude Code"));
        assert!(thinking("◑ Claude Code"));
        assert!(!thinking("✳ Claude Code"));
    }

    #[test]
    fn the_agents_own_spinner_comes_off_the_tab() {
        assert_eq!(strip_spinner("⠂ Claude Code"), "Claude Code");
        assert_eq!(strip_spinner("◑ Claude Code"), "Claude Code");
        assert_eq!(strip_spinner("✳ Claude Code"), "Claude Code");
        assert_eq!(strip_spinner("Claude Code"), "Claude Code");
        assert_eq!(strip_spinner("~/Projects/tuni"), "~/Projects/tuni");
        // Nothing left to name the tab with, so nothing is taken.
        assert_eq!(strip_spinner("⠂"), "⠂");
        assert_eq!(strip_spinner(""), "");
    }

    #[test]
    fn an_ordinary_title_is_not_mistaken_for_a_spinner() {
        assert!(!thinking(""));
        assert!(!thinking("dean@fedora:~/Projects/tuni"));
        assert!(!thinking("vim src/main.rs"));
        assert!(!thinking("~/Projects/tuni"));
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
    fn a_pi_session_is_found_by_its_header_and_its_turns_added_up() {
        let directory = tempdir();
        let mine = directory.join("mine.jsonl");
        let theirs = directory.join("theirs.jsonl");
        let header = |cwd: &str| {
            format!(
                r#"{{"type":"session","version":3,"id":"019fc7b1","timestamp":"2026-08-03T12:55:10.016Z","cwd":"{cwd}"}}"#
            )
        };
        let turn = |id: &str| {
            format!(
                r#"{{"type":"message","id":"{id}","timestamp":"2026-08-03T15:04:24.287Z","message":{{"role":"assistant","model":"gpt-5.6-sol","provider":"antigravity","usage":{{"input":10,"output":20,"cacheRead":30,"cacheWrite":40,"reasoning":15,"totalTokens":100}}}}}}"#
            )
        };
        std::fs::write(
            &mine,
            format!(
                "{}\n{}\n{}\n{}\n",
                header("/home/dean/Projects/tuni"),
                turn("a"),
                // The same turn again: a resumed session writes it out twice.
                turn("a"),
                turn("b"),
            ),
        )
        .expect("write");
        std::fs::write(&theirs, format!("{}\n", header("/home/dean/elsewhere"))).expect("write");

        let mut reader = Reader::default();
        let cwd = Path::new("/home/dean/Projects/tuni");
        assert!(reader.pi_works_in(&mine, cwd));
        assert!(!reader.pi_works_in(&theirs, cwd));

        let tail = reader.pi_tail(&mine);
        // Reasoning is inside the output, so the total is what Pi's own
        // `totalTokens` comes to for two turns rather than that plus 30.
        assert_eq!(
            tail.tokens,
            Tokens {
                input: 20,
                output: 40,
                cache_read: 60,
                cache_write: 80,
            }
        );
        assert_eq!(tail.tokens.total(), 200);
        assert_eq!(tail.model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(tail.source.as_deref(), Some("antigravity"));

        // A switch shows before anything is asked of the new model, the line
        // as Pi wrote it.
        let switch = r#"{"type":"model_change","id":"c","parentId":"b","timestamp":"2026-08-03T15:05:00.000Z","provider":"anthropic","modelId":"claude-haiku-5-5"}"#;
        let mut log = std::fs::read_to_string(&mine).expect("read");
        log.push_str(switch);
        log.push('\n');
        std::fs::write(&mine, log).expect("append");
        let tail = reader.pi_tail(&mine);
        assert_eq!(tail.model.as_deref(), Some("claude-haiku-5-5"));
        assert_eq!(tail.source.as_deref(), Some("anthropic"));
        assert_eq!(tail.tokens.total(), 200);

        std::fs::remove_dir_all(&directory).expect("clean up");
    }

    #[test]
    fn a_claude_code_reads_the_conversation_it_says_it_is_in() {
        let claude = tempdir();
        let cwd = Path::new("/home/dean/Projects/tuni");
        let store = claude.join("projects").join(claude_slug(cwd));
        std::fs::create_dir_all(&store).expect("store");
        std::fs::create_dir_all(claude.join("sessions")).expect("sessions");
        let mine = store.join("8b3993e8-8068.jsonl");
        std::fs::write(&mine, "").expect("mine");
        File::options()
            .write(true)
            .open(&mine)
            .and_then(|file| file.set_modified(SystemTime::now() - Duration::from_secs(60)))
            .expect("age");
        // Another conversation in the same checkout, written to since.
        std::fs::write(store.join("51314179-5847.jsonl"), "").expect("theirs");

        std::fs::write(
            claude.join("sessions/343430.json"),
            r#"{"pid":343430,"sessionId":"8b3993e8-8068","cwd":"/home/dean/Projects/tuni","status":"idle"}"#,
        )
        .expect("live");
        assert_eq!(claude_log(&claude, 343_430, cwd), Some(mine));
        // One with no file of its own falls back to the newest.
        assert_eq!(
            claude_log(&claude, 1, cwd),
            Some(store.join("51314179-5847.jsonl"))
        );
        std::fs::write(
            claude.join("sessions/2.json"),
            r#"{"sessionId":"../../.credentials"}"#,
        )
        .expect("bad");
        assert_eq!(claude_log(&claude, 2, cwd), None);

        std::fs::remove_dir_all(&claude).expect("clean up");
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
    fn every_plan_window_with_a_reading_becomes_a_bar_shortest_first() {
        let limits = plan_limits(
            r#"{"seven_day_fable":{"utilization":33.0},
                "seven_day":{"utilization":61.0,"resets_at":1785069161},
                "five_hour":{"utilization":12.5,"resets_at":"2026-07-26T15:00:00Z"},
                "seven_day_opus":{"utilization":null},
                "seven_day_sonnet":null,
                "unheard_of":{"utilization":5.0}}"#,
        );
        assert_eq!(
            limits,
            vec![
                Limit {
                    window: "5 hours".to_owned(),
                    used_percent: 12.5,
                    resets_at: Some(1_785_078_000),
                },
                Limit {
                    window: "7 days".to_owned(),
                    used_percent: 61.0,
                    resets_at: Some(1_785_069_161),
                },
                Limit {
                    window: "7 days (Fable)".to_owned(),
                    used_percent: 33.0,
                    resets_at: None,
                },
                Limit {
                    window: "unheard of".to_owned(),
                    used_percent: 5.0,
                    resets_at: None,
                },
            ],
            "a window without a reading is not a bar, and a window named for \
             a model not yet released still is"
        );
        assert!(plan_limits("not json").is_empty());
    }

    #[test]
    fn claude_code_is_served_by_anthropic_unless_a_switch_says_otherwise() {
        assert_eq!(claude_platform(b""), "anthropic");
        assert_eq!(
            claude_platform(b"HOME=/h\0CLAUDE_CODE_USE_BEDROCK=0\0"),
            "anthropic"
        );
        assert_eq!(claude_platform(b"CLAUDE_CODE_USE_VERTEX=1\0"), "vertex");
        // Bedrock is asked before Vertex, so both on is Bedrock.
        assert_eq!(
            claude_platform(b"CLAUDE_CODE_USE_VERTEX=true\0CLAUDE_CODE_USE_BEDROCK=Yes\0"),
            "bedrock"
        );
        // A longer name that starts with a switch's is not that switch.
        assert_eq!(
            claude_platform(b"CLAUDE_CODE_USE_BEDROCK_X=1\0"),
            "anthropic"
        );
    }

    #[test]
    fn opencode_names_the_model_and_its_provider_in_one_column() {
        assert_eq!(
            opencode_model(r#"{"id":"claude-opus-5-5","providerID":"github-copilot"}"#),
            (
                Some("claude-opus-5-5".to_owned()),
                Some("github-copilot".to_owned())
            )
        );
        assert_eq!(opencode_model("plain"), (Some("plain".to_owned()), None));
    }

    #[test]
    fn codex_reports_a_running_total_and_the_account_it_is_against() {
        let directory = tempdir();
        let log = directory.join("rollout-2026-07-26T12-00-00-abc.jsonl");
        std::fs::write(
            &log,
            concat!(
                r#"{"type":"session_meta","payload":{"cwd":"/work","model":"gpt-5-codex","model_provider":"openai"}}"#,
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
        assert_eq!(tail.source.as_deref(), Some("openai"));
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

    #[test]
    fn a_group_reading_becomes_a_bar_per_window() {
        let json = r#"{"group":"laptops","sessionPct":20,"weeklyPct":10.4,"blocked":false,
            "blockedWindow":null,"blockedModel":null,"blockedPct":null,"blockedUntil":null,
            "reportedAt":"2026-10-10T16:47:50.808Z"}"#;
        let fleet = fleet(json).expect("a reading");
        assert_eq!(fleet.group, "laptops");
        let bars: Vec<(&str, f64)> = fleet
            .limits
            .iter()
            .map(|limit| (limit.window.as_str(), limit.used_percent))
            .collect();
        assert_eq!(bars, [("5 hours", 20.0), ("7 days", 10.4)]);
        assert_eq!(fleet.blocked, None);
    }

    #[test]
    fn a_spent_share_names_its_window_and_when_it_resets() {
        let json = r#"{"group":"laptops","sessionPct":40,"weeklyPct":101,"blocked":true,
            "blockedWindow":"weekly","blockedPct":101,"blockedUntil":"2026-10-12T08:00:00.000Z",
            "reportedAt":"2026-10-10T16:47:50.808Z"}"#;
        let fleet = fleet(json).expect("a reading");
        assert_eq!(fleet.blocked.as_deref(), Some("7 days"));
        assert_eq!(fleet.limits[1].resets_at, unix_time("2026-10-12T08:00:00Z"));
        assert_eq!(fleet.limits[0].resets_at, None);
    }

    #[test]
    fn a_model_window_that_refuses_prompts_gets_a_bar_of_its_own() {
        let json = r#"{"group":"laptops","sessionPct":40,"weeklyPct":50,"blocked":true,
            "blockedWindow":"session","blockedModel":"Fable","blockedPct":100,
            "reportedAt":"2026-10-10T16:47:50.808Z"}"#;
        let fleet = fleet(json).expect("a reading");
        assert_eq!(fleet.limits.len(), 3);
        assert_eq!(fleet.limits[2].window, "Fable 5 hours");
        assert_eq!(fleet.blocked.as_deref(), Some("Fable 5 hours"));
    }

    #[test]
    fn no_report_yet_or_no_group_draws_nothing() {
        let unreported = r#"{"group":"laptops","sessionPct":0,"weeklyPct":0,"reportedAt":null}"#;
        let groupless = r#"{"group":null,"sessionPct":0,"weeklyPct":0,"reportedAt":"2026-10-10T16:47:50.808Z"}"#;
        assert_eq!(fleet(unreported), None);
        assert_eq!(fleet(groupless), None);
        assert_eq!(fleet("not json"), None);
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
