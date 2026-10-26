//! The conversations a coding agent had in a directory, so one can be picked
//! up again once the tab it ran in is gone: kero's Resume Conversation.
//!
//! Read from Claude Code's own records under `~/.claude/projects`, which it
//! keeps whether tuni was running or not, so a conversation from before tuni
//! was installed is there too. Nothing is written and nothing is remembered:
//! every palette opening asks the directory again.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Deserialize;

use crate::settings::home;
use crate::usage::claude_slug;

/// How much of either end of a record is read. Claude Code writes the title
/// again on every turn, and the last copy was within 30 KB of the end in every
/// record measured; the records themselves run to tens of megabytes.
const WINDOW: u64 = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub modified: SystemTime,
}

/// The `limit` conversations Claude Code had in `directory` most recently,
/// newest first. A record with nothing asked in it yet is left out.
#[must_use]
pub fn recent(directory: &Path, limit: usize) -> Vec<Conversation> {
    in_store(
        &home().join(".claude/projects").join(claude_slug(directory)),
        limit,
    )
}

fn in_store(store: &Path, limit: usize) -> Vec<Conversation> {
    let Ok(entries) = std::fs::read_dir(store) else {
        return Vec::new();
    };
    let mut records: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .filter_map(|path| Some((path.metadata().and_then(|meta| meta.modified()).ok()?, path)))
        .collect();
    records.sort_unstable_by(|(left, _), (right, _)| right.cmp(left));
    // Bounded before anything is opened: a busy checkout has hundreds.
    records.truncate(limit * 2);
    records
        .into_iter()
        .filter_map(|(modified, path)| {
            let id = path.file_stem()?.to_str()?.to_owned();
            resume_line(&id)?;
            Some(Conversation {
                title: title(&path)?,
                id,
                modified,
            })
        })
        .take(limit)
        .collect()
}

/// The line that picks a conversation up again, typed into a shell in its
/// directory. `None` for anything an id is not made of, since it goes to a
/// shell as it is.
#[must_use]
pub fn resume_line(id: &str) -> Option<String> {
    let valid = !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    valid.then(|| format!("claude --resume {id}"))
}

/// When something happened, the way a list of recent things says it.
#[must_use]
pub fn ago(then: SystemTime, now: SystemTime) -> String {
    let seconds = now.duration_since(then).unwrap_or(Duration::ZERO).as_secs();
    let (count, unit) = match seconds {
        0..60 => return "just now".to_owned(),
        60..3_600 => (seconds / 60, "minute"),
        3_600..86_400 => (seconds / 3_600, "hour"),
        _ => (seconds / 86_400, "day"),
    };
    let plural = if count == 1 { "" } else { "s" };
    format!("{count} {unit}{plural} ago")
}

/// The title Claude Code gave the conversation, else the first thing asked in
/// it.
fn title(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let tail = read_at(&mut file, length.saturating_sub(WINDOW))?;
    // The window's first line is cut in two, and fails to parse as one.
    tail.lines()
        .rev()
        .filter(|line| line.contains("\"ai-title\""))
        .filter_map(|line| serde_json::from_str::<Line>(line).ok())
        .find_map(|line| line.ai_title)
        .or_else(|| {
            read_at(&mut file, 0)?
                .lines()
                .filter_map(|line| serde_json::from_str::<Line>(line).ok())
                .find_map(Line::prompt)
        })
        .map(|title| title.trim().to_owned())
        .filter(|title| !title.is_empty())
}

fn read_at(file: &mut File, offset: u64) -> Option<String> {
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut bytes = Vec::new();
    file.take(WINDOW).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "aiTitle")]
    ai_title: Option<String>,
    #[serde(rename = "isMeta", default)]
    is_meta: bool,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    content: Option<Content>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Parts(Vec<Part>),
}

#[derive(Deserialize)]
struct Part {
    text: Option<String>,
}

impl Line {
    /// Something typed by a person: not a slash command's wrapping, a hook's
    /// note or a tool's result, which are user lines too.
    fn prompt(self) -> Option<String> {
        if self.kind.as_deref() != Some("user") || self.is_meta {
            return None;
        }
        let text = match self.message?.content? {
            Content::Text(text) => text,
            Content::Parts(parts) => parts.into_iter().find_map(|part| part.text)?,
        };
        let first = text.lines().map(str::trim).find(|line| !line.is_empty())?;
        (!first.starts_with('<')).then(|| first.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Store(PathBuf);

    impl Store {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("tuni-conversations-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn write(&self, id: &str, lines: &[&str], age: u64) {
            let path = self.0.join(format!("{id}.jsonl"));
            std::fs::write(&path, lines.join("\n") + "\n").unwrap();
            let file = File::options().write(true).open(&path).unwrap();
            file.set_modified(SystemTime::now() - Duration::from_secs(age))
                .unwrap();
        }
    }

    impl Drop for Store {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_conversation_is_named_by_its_last_title_else_by_what_was_asked_first() {
        let store = Store::new("titles");
        store.write(
            "aaaa-1",
            &[
                r#"{"type":"user","isMeta":true,"message":{"content":"<local-command-caveat>x"}}"#,
                r#"{"type":"user","message":{"content":"<command-name>/clear</command-name>"}}"#,
                r#"{"type":"user","message":{"content":[{"type":"text","text":"\nfix the build\nplease"}]}}"#,
            ],
            60,
        );
        store.write(
            "bbbb-2",
            &[
                r#"{"type":"user","message":{"content":"first"}}"#,
                r#"{"type":"ai-title","aiTitle":"Old name","sessionId":"bbbb-2"}"#,
                r#"{"type":"ai-title","aiTitle":"Port to macOS","sessionId":"bbbb-2"}"#,
            ],
            0,
        );
        store.write("cccc-3", &[r#"{"type":"mode"}"#], 30);
        store.write(
            "not;an-id",
            &[r#"{"type":"user","message":{"content":"x"}}"#],
            0,
        );

        let found = in_store(&store.0, 5);
        let titles: Vec<(&str, &str)> = found
            .iter()
            .map(|each| (each.id.as_str(), each.title.as_str()))
            .collect();
        assert_eq!(
            titles,
            [("bbbb-2", "Port to macOS"), ("aaaa-1", "fix the build")]
        );
        assert_eq!(in_store(&store.0, 1).len(), 1);
    }

    #[test]
    fn only_an_id_is_typed_into_the_shell() {
        assert_eq!(
            resume_line("51314179-5847-4cd8-8922-aa707aa9d5bf").as_deref(),
            Some("claude --resume 51314179-5847-4cd8-8922-aa707aa9d5bf")
        );
        assert_eq!(resume_line("x; rm -rf ~"), None);
        assert_eq!(resume_line(""), None);
    }

    #[test]
    fn a_time_is_said_in_the_largest_whole_unit() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let at = |seconds| now - Duration::from_secs(seconds);
        assert_eq!(ago(at(5), now), "just now");
        assert_eq!(ago(at(60), now), "1 minute ago");
        assert_eq!(ago(at(7_200), now), "2 hours ago");
        assert_eq!(ago(at(3 * 86_400), now), "3 days ago");
        assert_eq!(ago(now + Duration::from_secs(9), now), "just now");
    }
}
