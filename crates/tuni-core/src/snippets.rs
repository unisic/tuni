//! Short pieces of shell, kept by name.
//!
//! A snippet is typed into the pane that has the keyboard, exactly as written,
//! rather than sent to the far end as a command of its own. So it works against
//! any shell, on this machine or another one, and the person who ran it can see
//! what ran.
//!
//! That is also the rule for the last character. A body ending in a newline
//! runs; one that does not lands on the prompt to be finished by hand, which is
//! how a snippet that would delete something can be kept without a mis-hit
//! Return being able to fire it.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One of them: what it is called, and what gets typed.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Snippet {
    pub name: String,
    pub body: String,
}

/// All of them, in the order they were written, which is the order they are
/// offered in.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Snippets(Vec<Snippet>);

impl Snippets {
    #[must_use]
    pub fn path() -> PathBuf {
        crate::settings::config_dir().join("ssh/snippets.json")
    }

    /// What is on disk, or nothing, which is what a machine that has never
    /// written one has.
    #[must_use]
    pub fn load() -> Self {
        fs::read_to_string(Self::path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn all(&self) -> &[Snippet] {
        &self.0
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Snippet> {
        self.0.iter().find(|snippet| snippet.name == name)
    }

    /// Files one under its name, replacing the one that had that name.
    ///
    /// `under` is the name it had before it was edited, so a rename replaces
    /// rather than leaving the old one behind.
    pub fn set(&mut self, under: Option<&str>, snippet: Snippet) {
        let existing = under
            .or(Some(snippet.name.as_str()))
            .and_then(|name| self.0.iter().position(|held| held.name == name));
        match existing {
            Some(index) => self.0[index] = snippet,
            None => self.0.push(snippet),
        }
    }

    pub fn remove(&mut self, name: &str) {
        self.0.retain(|snippet| snippet.name != name);
    }

    /// Written beside itself and renamed into place, the way every other file
    /// tuni owns is.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string(self).map_err(std::io::Error::other)?;
        let temporary = path.with_extension("json.new");
        fs::write(&temporary, text)?;
        fs::rename(&temporary, &path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snippet(name: &str, body: &str) -> Snippet {
        Snippet {
            name: name.to_owned(),
            body: body.to_owned(),
        }
    }

    #[test]
    fn a_snippet_comes_back_out_of_its_own_file() {
        let mut snippets = Snippets::default();
        snippets.set(None, snippet("tail the log", "sudo journalctl -fu nginx\n"));
        let text = serde_json::to_string(&snippets).expect("write");
        let read: Snippets = serde_json::from_str(&text).expect("read");
        assert_eq!(read.get("tail the log").map(|s| s.body.as_str()), {
            Some("sudo journalctl -fu nginx\n")
        });
    }

    #[test]
    fn editing_one_replaces_it_rather_than_adding_a_second() {
        let mut snippets = Snippets::default();
        snippets.set(None, snippet("disk", "df -h\n"));
        snippets.set(Some("disk"), snippet("free space", "df -h /\n"));
        assert_eq!(snippets.all().len(), 1);
        assert_eq!(snippets.all()[0].name, "free space");
    }

    #[test]
    fn one_that_is_removed_stays_removed() {
        let mut snippets = Snippets::default();
        snippets.set(None, snippet("disk", "df -h\n"));
        snippets.remove("disk");
        assert!(snippets.all().is_empty());
    }
}
