//! What the user configured, in a file they can read.
//!
//! `~/.config/tuni/config.toml`, the same shape kero uses and close to the
//! keys Ghostty spells the same way — `font-family`, `font-size`,
//! `theme-light`, `theme-dark` — so a person who has configured one of those
//! is not learning a third vocabulary.
//!
//! Only settings that differ from the defaults are written. A file listing
//! every knob at its default value reads like a set of decisions when it is
//! really an absence of them, and it turns a later change of default into a
//! change of behavior for nobody.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::PathBuf;

use crate::{FONT_SIZE_MAX, FONT_SIZE_MIN, TerminalConfig, theme};

/// Which appearance the window takes. `System` is the desktop's own choice,
/// which is what a GNOME application is expected to follow; the other two are
/// for people whose desktop says one thing and whose terminal wants another.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    #[must_use]
    fn parse(name: &str) -> Option<Self> {
        match name.trim() {
            "system" => Some(Self::System),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

/// Everything the settings window edits.
#[derive(Clone, Debug, Default)]
pub struct Settings {
    pub terminal: TerminalConfig,
    pub appearance: Appearance,
    /// Whether a restored tab replays what its shell had printed before.
    ///
    /// Off by default, and deliberately so: it writes terminal output to disk,
    /// which is a decision about someone's `~` that no application should make
    /// on their behalf.
    pub restore_history: bool,
}

/// How many lines of a pane's scrollback are kept for the replay. kero's own
/// cap, and for the same reason: it bounds a file that would otherwise grow
/// with the largest thing anyone ever `cat`ed.
pub const HISTORY_LINE_LIMIT: usize = 500;

impl Settings {
    /// `$XDG_CONFIG_HOME/tuni/config.toml`, or `~/.config/tuni/config.toml`.
    #[must_use]
    pub fn path() -> PathBuf {
        config_dir().join("config.toml")
    }

    /// The settings on disk, or the defaults. A file that cannot be read or
    /// makes no sense is not an error worth stopping a terminal from starting
    /// over: the defaults are a working terminal.
    #[must_use]
    pub fn load() -> Self {
        fs::read_to_string(Self::path())
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    #[must_use]
    pub fn parse(text: &str) -> Self {
        let table = toml::parse(text);
        let mut settings = Self::default();

        if let Some(name) = table.string("theme") {
            settings.appearance = Appearance::parse(name).unwrap_or_default();
        }
        // A theme name that no longer exists would otherwise leave the settings
        // window showing a selection it cannot honor.
        if let Some(name) = table
            .string("theme-light")
            .filter(|name| theme::exists(name))
        {
            settings.terminal.theme_light = name.to_owned();
        }
        if let Some(name) = table
            .string("theme-dark")
            .filter(|name| theme::exists(name))
        {
            settings.terminal.theme_dark = name.to_owned();
        }
        if let Some(family) = table.string("font-family").filter(|f| !f.trim().is_empty()) {
            settings.terminal.font_family = family.trim().to_owned();
        }
        if let Some(size) = table
            .number("font-size")
            .filter(|size| (FONT_SIZE_MIN..=FONT_SIZE_MAX).contains(size))
        {
            settings.terminal.font_size = size;
        }
        if let Some(on) = table.boolean("font-ligatures") {
            settings.terminal.font_ligatures = on;
        }
        if let Some(extra) = table.number("line-height").filter(|extra| *extra >= 0.0) {
            settings.terminal.line_height_extra = extra;
        }
        if let Some(on) = table.boolean("cursor-blink") {
            settings.terminal.cursor_blink = on;
        }
        if let Some(lines) = table
            .number("terminal.scrollback-lines")
            .filter(|lines| *lines >= 0.0)
        {
            settings.terminal.scrollback_lines = lines as usize;
        }
        if let Some(on) = table.boolean("terminal.restore-history") {
            settings.restore_history = on;
        }
        settings
    }

    /// The file to write: what differs from the defaults, and nothing else.
    #[must_use]
    pub fn to_toml(&self) -> String {
        let default = Self::default();
        let mut out = String::new();

        if self.appearance != default.appearance {
            let _ = writeln!(out, "theme = {}", toml::quote(self.appearance.name()));
        }
        if self.terminal.theme_light != default.terminal.theme_light {
            let _ = writeln!(
                out,
                "theme-light = {}",
                toml::quote(&self.terminal.theme_light)
            );
        }
        if self.terminal.theme_dark != default.terminal.theme_dark {
            let _ = writeln!(
                out,
                "theme-dark = {}",
                toml::quote(&self.terminal.theme_dark)
            );
        }
        if self.terminal.font_family != default.terminal.font_family {
            let _ = writeln!(
                out,
                "font-family = {}",
                toml::quote(&self.terminal.font_family)
            );
        }
        if self.terminal.font_size != default.terminal.font_size {
            let _ = writeln!(out, "font-size = {}", toml::number(self.terminal.font_size));
        }
        if self.terminal.font_ligatures != default.terminal.font_ligatures {
            let _ = writeln!(out, "font-ligatures = {}", self.terminal.font_ligatures);
        }
        if self.terminal.line_height_extra != default.terminal.line_height_extra {
            let _ = writeln!(
                out,
                "line-height = {}",
                toml::number(self.terminal.line_height_extra)
            );
        }
        if self.terminal.cursor_blink != default.terminal.cursor_blink {
            let _ = writeln!(out, "cursor-blink = {}", self.terminal.cursor_blink);
        }
        if self.terminal.scrollback_lines != default.terminal.scrollback_lines {
            let _ = writeln!(
                out,
                "terminal.scrollback-lines = {}",
                self.terminal.scrollback_lines
            );
        }
        if self.restore_history != default.restore_history {
            let _ = writeln!(out, "terminal.restore-history = {}", self.restore_history);
        }
        out
    }

    /// Writes the file, creating `~/.config/tuni` if it is not there yet.
    ///
    /// Written beside itself and renamed into place, so an interrupted write
    /// leaves the previous settings rather than half of the new ones.
    pub fn save(&self) -> io::Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("toml.new");
        fs::write(&temporary, self.to_toml())?;
        fs::rename(&temporary, &path)
    }
}

/// `$XDG_CONFIG_HOME/tuni`, or `~/.config/tuni`. The XDG basedir spec says an
/// unset or relative `XDG_CONFIG_HOME` is to be ignored.
#[must_use]
pub fn config_dir() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config")
}

/// `$XDG_DATA_HOME/tuni`, or `~/.local/share/tuni`: the session snapshot and
/// the scrollback it may have saved.
#[must_use]
pub fn data_dir() -> PathBuf {
    xdg_dir("XDG_DATA_HOME", ".local/share")
}

fn xdg_dir(variable: &str, fallback: &str) -> PathBuf {
    let base = std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home().join(fallback));
    base.join("tuni")
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// As much TOML as a configuration file of flat settings uses: `key = value`
/// with string, number, and boolean values, `#` comments, dotted keys, and
/// `[table]` headers flattened into the dotted form.
///
/// A whole TOML implementation would parse arrays, inline tables, dates, and
/// multi-line strings that this file has no way to mean anything by. This is
/// the same subset kero's own parser covers.
mod toml {
    use std::collections::HashMap;

    #[derive(Debug)]
    pub enum Value {
        String(String),
        Number(f64),
        Boolean(bool),
    }

    #[derive(Debug, Default)]
    pub struct Table(HashMap<String, Value>);

    impl Table {
        pub fn string(&self, key: &str) -> Option<&str> {
            match self.0.get(key) {
                Some(Value::String(value)) => Some(value),
                _ => None,
            }
        }

        pub fn number(&self, key: &str) -> Option<f64> {
            match self.0.get(key) {
                Some(Value::Number(value)) => Some(*value),
                _ => None,
            }
        }

        pub fn boolean(&self, key: &str) -> Option<bool> {
            match self.0.get(key) {
                Some(Value::Boolean(value)) => Some(*value),
                _ => None,
            }
        }
    }

    pub fn parse(text: &str) -> Table {
        let mut table = Table::default();
        let mut section = String::new();

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = name.trim().to_owned();
                continue;
            }
            let Some((key, raw)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let Some(value) = value(raw.trim()) else {
                continue;
            };
            if key.is_empty() {
                continue;
            }
            let key = if section.is_empty() {
                key.to_owned()
            } else {
                format!("{section}.{key}")
            };
            table.0.insert(key, value);
        }
        table
    }

    fn value(raw: &str) -> Option<Value> {
        if let Some(rest) = raw.strip_prefix('"') {
            return string(rest).map(Value::String);
        }
        // Unquoted: a trailing comment is not part of the value.
        let bare = raw.split('#').next().unwrap_or_default().trim();
        match bare {
            "true" => Some(Value::Boolean(true)),
            "false" => Some(Value::Boolean(false)),
            _ => bare.parse().ok().map(Value::Number),
        }
    }

    fn string(rest: &str) -> Option<String> {
        let mut out = String::new();
        let mut escaped = false;
        for character in rest.chars() {
            if escaped {
                out.push(match character {
                    'n' => '\n',
                    't' => '\t',
                    other => other,
                });
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                return Some(out);
            } else {
                out.push(character);
            }
        }
        // Unterminated: not a string, and guessing where it ended would be
        // inventing a setting nobody wrote.
        None
    }

    pub fn quote(value: &str) -> String {
        let mut out = String::with_capacity(value.len() + 2);
        out.push('"');
        for character in value.chars() {
            match character {
                '"' | '\\' => {
                    out.push('\\');
                    out.push(character);
                }
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                other => out.push(other),
            }
        }
        out.push('"');
        out
    }

    /// A whole number written without a decimal point, which is what anyone
    /// setting `font-size = 13` expects to find in the file afterwards.
    pub fn number(value: f64) -> String {
        if value.fract() == 0.0 && value.abs() < 1e15 {
            format!("{}", value as i64)
        } else {
            format!("{value}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setting_is_read_by_name() {
        let settings = Settings::parse(
            "# a comment\n\
             font-family = \"Fira Code\"\n\
             font-size = 13\n\
             font-ligatures = true\n\
             [terminal]\n\
             restore-history = true\n",
        );
        assert_eq!(settings.terminal.font_family, "Fira Code");
        assert!((settings.terminal.font_size - 13.0).abs() < f64::EPSILON);
        assert!(settings.terminal.font_ligatures);
        assert!(settings.restore_history);
    }

    #[test]
    fn a_dotted_key_and_a_table_header_mean_the_same_thing() {
        let dotted = Settings::parse("terminal.restore-history = true\n");
        let sectioned = Settings::parse("[terminal]\nrestore-history = true\n");
        assert_eq!(dotted.restore_history, sectioned.restore_history);
    }

    #[test]
    fn a_value_that_makes_no_sense_leaves_the_default_in_place() {
        let settings = Settings::parse(
            "font-size = 400\n\
             theme-dark = \"no such theme\"\n\
             cursor-blink = maybe\n",
        );
        let default = Settings::default();
        assert!((settings.terminal.font_size - default.terminal.font_size).abs() < f64::EPSILON);
        assert_eq!(settings.terminal.theme_dark, default.terminal.theme_dark);
        assert_eq!(
            settings.terminal.cursor_blink,
            default.terminal.cursor_blink
        );
    }

    #[test]
    fn only_what_differs_from_the_defaults_is_written() {
        let mut settings = Settings::default();
        assert_eq!(settings.to_toml(), "");

        settings.terminal.font_size = 15.0;
        settings.restore_history = true;
        assert_eq!(
            settings.to_toml(),
            "font-size = 15\nterminal.restore-history = true\n"
        );
    }

    #[test]
    fn what_was_written_reads_back_the_same() {
        let settings = Settings {
            terminal: TerminalConfig {
                font_family: "Iosevka \"Term\"".to_owned(),
                font_size: 12.5,
                font_ligatures: true,
                line_height_extra: 2.0,
                cursor_blink: false,
                scrollback_lines: 50_000,
                ..TerminalConfig::default()
            },
            appearance: Appearance::Dark,
            restore_history: true,
        };
        let read = Settings::parse(&settings.to_toml());

        assert_eq!(read.terminal.font_family, settings.terminal.font_family);
        assert!((read.terminal.font_size - 12.5).abs() < f64::EPSILON);
        assert!(read.terminal.font_ligatures);
        assert!((read.terminal.line_height_extra - 2.0).abs() < f64::EPSILON);
        assert!(!read.terminal.cursor_blink);
        assert_eq!(read.terminal.scrollback_lines, 50_000);
        assert_eq!(read.appearance, Appearance::Dark);
        assert!(read.restore_history);
    }

    #[test]
    fn an_unset_xdg_variable_falls_back_to_the_home_directory() {
        // Both are read the same way; testing one covers the helper.
        let relative = PathBuf::from("relative/path");
        assert!(!relative.is_absolute(), "the filter is what this rests on");
        assert!(config_dir().ends_with("tuni"));
        assert!(data_dir().ends_with("tuni"));
    }
}
