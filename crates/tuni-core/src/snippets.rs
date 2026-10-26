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
//!
//! A body can leave blanks, `{{name}}`, which are asked for before anything is
//! typed. See [`blanks`].

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

/// The blanks a body asks to have filled before it is typed, each once, in the
/// order they first appear.
///
/// A blank is `{{name}}`: a letter or an underscore, then letters, digits,
/// underscores and hyphens. Anything else between double braces is text, which
/// keeps `docker ps --format '{{.Names}}'` and a template's `{{ .Field }}`
/// typing as written. There is no escape, because no shell spells anything
/// that way.
#[must_use]
pub fn blanks(body: &str) -> Vec<&str> {
    let mut found: Vec<&str> = Vec::new();
    for piece in pieces(body) {
        if let Piece::Blank(name) = piece
            && !found.contains(&name)
        {
            found.push(name);
        }
    }
    found
}

/// The body with every blank replaced by its value, or the name of the blank
/// whose value cannot be typed.
///
/// A value goes to a shell as keystrokes, so a control character in one is
/// refused rather than typed: a newline would run the line before the rest of
/// it arrived, and the others are keys of their own, Ctrl+C and Escape among
/// them. A blank nobody gave a value for is left as written.
pub fn fill(body: &str, values: &[(String, String)]) -> Result<String, String> {
    let mut filled = String::with_capacity(body.len());
    for piece in pieces(body) {
        match piece {
            Piece::Text(text) => filled.push_str(text),
            Piece::Blank(name) => match values.iter().find(|(blank, _)| blank == name) {
                Some((_, value)) if value.chars().any(char::is_control) => {
                    return Err(name.to_owned());
                }
                Some((_, value)) => filled.push_str(value),
                None => {
                    filled.push_str("{{");
                    filled.push_str(name);
                    filled.push_str("}}");
                }
            },
        }
    }
    Ok(filled)
}

enum Piece<'a> {
    Text(&'a str),
    Blank(&'a str),
}

/// A body cut at its blanks. The scan is one pass and what it skips over is
/// never looked at again, so a value that happens to contain braces cannot
/// become a blank of its own.
fn pieces(body: &str) -> Vec<Piece<'_>> {
    let mut pieces = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        match after
            .find("}}")
            .map(|close| &after[..close])
            .filter(|name| is_name(name))
        {
            Some(name) => {
                pieces.push(Piece::Text(&rest[..open]));
                pieces.push(Piece::Blank(name));
                rest = &after[name.len() + 2..];
            }
            None => {
                pieces.push(Piece::Text(&rest[..open + 2]));
                rest = after;
            }
        }
    }
    pieces.push(Piece::Text(rest));
    pieces
}

fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|next| next.is_ascii_alphanumeric() || next == '_' || next == '-')
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

    fn given(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(blank, value)| ((*blank).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn a_blank_is_asked_for_once_however_often_it_appears() {
        let body = "ping -c {{count}} {{host}} && ssh {{host}}\n";
        assert_eq!(blanks(body), ["count", "host"]);
    }

    #[test]
    fn double_braces_around_something_that_is_not_a_name_are_typed_as_written() {
        let body = "docker ps --format '{{.Names}}' {{ .Id }} {{}} {{{x}}}\n";
        assert!(blanks(body).is_empty());
        assert_eq!(fill(body, &[]).as_deref(), Ok(body));
    }

    #[test]
    fn values_go_where_their_blanks_were() {
        let filled = fill(
            "tail -n {{lines}} {{file}} | grep {{file}}\n",
            &given(&[("lines", "50"), ("file", "/var/log/{{x}}")]),
        );
        assert_eq!(
            filled.as_deref(),
            Ok("tail -n 50 /var/log/{{x}} | grep /var/log/{{x}}\n")
        );
    }

    #[test]
    fn a_value_that_would_press_a_key_is_refused_by_the_blank_it_was_for() {
        let body = "echo {{a}} {{b}}";
        assert_eq!(
            fill(body, &given(&[("a", "fine"), ("b", "x\nrm -rf ~")])),
            Err("b".to_owned())
        );
        assert_eq!(
            fill(body, &given(&[("a", "\u{3}"), ("b", "")])),
            Err("a".to_owned())
        );
    }

    #[test]
    fn a_blank_with_no_value_is_left_as_written() {
        assert_eq!(fill("cd {{dir}}", &[]).as_deref(), Ok("cd {{dir}}"));
    }

    #[test]
    fn one_that_is_removed_stays_removed() {
        let mut snippets = Snippets::default();
        snippets.set(None, snippet("disk", "df -h\n"));
        snippets.remove("disk");
        assert!(snippets.all().is_empty());
    }
}
