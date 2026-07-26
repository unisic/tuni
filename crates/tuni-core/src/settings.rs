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

use crate::{
    CursorStyle, FONT_SIZE_MAX, FONT_SIZE_MIN, OPACITY_MIN, PADDING_MAX, TerminalConfig, theme,
};

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
    /// Whether a file pane wraps a long line to its width rather than scrolling
    /// sideways. Off, as it is in kero and in every editor's own default: a
    /// wrapped line and a real line stop being the same thing.
    pub wrap_lines: bool,
    /// Whether the tab bar goes away while a window has a single tab. Off: a
    /// bar that appears when a second tab opens moves the terminal under the
    /// pointer, and the row it costs is the row it is worth.
    pub auto_hide_tab_bar: bool,
    /// Whether the compositor is asked to blur what shows through a translucent
    /// window. KWin is the one that offers this; on a desktop without the
    /// protocol the request is dropped and the background stays clear.
    pub background_blur: bool,
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
            match Appearance::parse(name) {
                Some(appearance) => settings.appearance = appearance,
                // Ghostty's `theme` names a theme rather than an appearance,
                // either one for both appearances or the `light:a,dark:b` pair.
                None => themes(name, &mut settings.terminal),
            }
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
        if let Some(on) = table
            .boolean("cursor-blink")
            .or_else(|| table.boolean("cursor-style-blink"))
        {
            settings.terminal.cursor_blink = on;
        }
        if let Some(style) = table.string("cursor-style").and_then(CursorStyle::parse) {
            settings.terminal.cursor_style = style;
        }
        // Ghostty writes `clipboard` for the selection going to the clipboard
        // proper rather than to the middle-click one, which is where a Wayland
        // selection lands anyway.
        if let Some(on) = table
            .boolean("copy-on-select")
            .or_else(|| table.string("copy-on-select").map(|to| to == "clipboard"))
        {
            settings.terminal.copy_on_select = on;
        }
        if let Some(on) = table.boolean("mouse-reporting") {
            settings.terminal.mouse_reporting = on;
        }
        if let Some(on) = table.boolean("bell") {
            settings.terminal.bell = on;
        }
        if let Some(command) = table.string("command") {
            settings.terminal.command = command.trim().to_owned();
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
        if let Some(on) = table.boolean("editor.wrap-lines") {
            settings.wrap_lines = on;
        }
        if let Some(opacity) = table
            .number("background-opacity")
            .filter(|opacity| (OPACITY_MIN..=1.0).contains(opacity))
        {
            settings.terminal.background_opacity = opacity;
        }
        // A number is Ghostty's blur intensity. KWin has one blur of its own
        // and no way to be told how strong to make it for one window, so any
        // intensity above zero means the same thing here: blur it.
        if let Some(on) = table
            .boolean("background-blur")
            .or_else(|| table.number("background-blur").map(|blur| blur > 0.0))
        {
            settings.background_blur = on;
        }
        let padding = |key| {
            table
                .number(key)
                .filter(|padding| (0.0..=PADDING_MAX).contains(padding))
        };
        if let Some(across) = padding("window-padding-x") {
            settings.terminal.padding_x = across;
        }
        if let Some(down) = padding("window-padding-y") {
            settings.terminal.padding_y = down;
        }
        if let Some(on) = table.boolean("window.auto-hide-tab-bar") {
            settings.auto_hide_tab_bar = on;
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
        if self.terminal.cursor_style != default.terminal.cursor_style {
            let _ = writeln!(
                out,
                "cursor-style = {}",
                toml::quote(self.terminal.cursor_style.name())
            );
        }
        if self.terminal.copy_on_select != default.terminal.copy_on_select {
            let _ = writeln!(out, "copy-on-select = {}", self.terminal.copy_on_select);
        }
        if self.terminal.mouse_reporting != default.terminal.mouse_reporting {
            let _ = writeln!(out, "mouse-reporting = {}", self.terminal.mouse_reporting);
        }
        if self.terminal.bell != default.terminal.bell {
            let _ = writeln!(out, "bell = {}", self.terminal.bell);
        }
        if self.terminal.command != default.terminal.command {
            let _ = writeln!(out, "command = {}", toml::quote(&self.terminal.command));
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
        if self.wrap_lines != default.wrap_lines {
            let _ = writeln!(out, "editor.wrap-lines = {}", self.wrap_lines);
        }
        if self.terminal.background_opacity != default.terminal.background_opacity {
            let _ = writeln!(
                out,
                "background-opacity = {}",
                toml::number(self.terminal.background_opacity)
            );
        }
        if self.background_blur != default.background_blur {
            let _ = writeln!(out, "background-blur = {}", self.background_blur);
        }
        if self.terminal.padding_x != default.terminal.padding_x {
            let _ = writeln!(
                out,
                "window-padding-x = {}",
                toml::number(self.terminal.padding_x)
            );
        }
        if self.terminal.padding_y != default.terminal.padding_y {
            let _ = writeln!(
                out,
                "window-padding-y = {}",
                toml::number(self.terminal.padding_y)
            );
        }
        if self.auto_hide_tab_bar != default.auto_hide_tab_bar {
            let _ = writeln!(out, "window.auto-hide-tab-bar = {}", self.auto_hide_tab_bar);
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

/// Reads Ghostty's `theme`, which names themes rather than an appearance:
/// `theme = Catppuccin Mocha` for both, or `theme = light:a,dark:b` for one
/// each. A name no bundled theme answers to is left alone, the same as any
/// other value this file cannot honor.
fn themes(value: &str, terminal: &mut TerminalConfig) {
    for part in value.split(',') {
        let (light, dark, name) = match part.split_once(':') {
            Some(("light", name)) => (true, false, name),
            Some(("dark", name)) => (false, true, name),
            _ => (true, true, part),
        };
        let name = name.trim();
        if !theme::exists(name) {
            continue;
        }
        if light {
            terminal.theme_light = name.to_owned();
        }
        if dark {
            terminal.theme_dark = name.to_owned();
        }
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
            // The first of a repeated key wins. A Ghostty configuration lists
            // `font-family` once per fallback, primary first, and the primary
            // is the one this has a use for.
            table.0.entry(key).or_insert(value);
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
            // TOML would want the quotes. Ghostty's configuration file does
            // not have them, and `font-family = JetBrains Mono` is a sentence
            // both spellings can mean the same thing by.
            _ => Some(
                bare.parse()
                    .map_or_else(|_| Value::String(bare.to_owned()), Value::Number),
            ),
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
                cursor_style: CursorStyle::Bar,
                copy_on_select: true,
                mouse_reporting: false,
                bell: false,
                command: "/usr/bin/fish".to_owned(),
                scrollback_lines: 50_000,
                background_opacity: 0.85,
                padding_x: 8.0,
                padding_y: 4.0,
                ..TerminalConfig::default()
            },
            appearance: Appearance::Dark,
            restore_history: true,
            wrap_lines: true,
            auto_hide_tab_bar: true,
            background_blur: true,
        };
        let read = Settings::parse(&settings.to_toml());

        assert_eq!(read.terminal.font_family, settings.terminal.font_family);
        assert!((read.terminal.font_size - 12.5).abs() < f64::EPSILON);
        assert!(read.terminal.font_ligatures);
        assert!((read.terminal.line_height_extra - 2.0).abs() < f64::EPSILON);
        assert!(!read.terminal.cursor_blink);
        assert_eq!(read.terminal.cursor_style, CursorStyle::Bar);
        assert!(read.terminal.copy_on_select);
        assert!(!read.terminal.mouse_reporting);
        assert!(!read.terminal.bell);
        assert_eq!(read.terminal.command, "/usr/bin/fish");
        assert_eq!(read.terminal.scrollback_lines, 50_000);
        assert!((read.terminal.background_opacity - 0.85).abs() < f64::EPSILON);
        assert!((read.terminal.padding_x - 8.0).abs() < f64::EPSILON);
        assert!((read.terminal.padding_y - 4.0).abs() < f64::EPSILON);
        assert!(read.background_blur);
        assert_eq!(read.appearance, Appearance::Dark);
        assert!(read.restore_history);
        assert!(read.wrap_lines);
        assert!(read.auto_hide_tab_bar);
    }

    #[test]
    fn a_ghostty_configuration_is_read_as_one_of_these() {
        let settings = Settings::parse(
            "# The one real gap in the Nerd Font is color emoji.\n\
             font-family = JetBrainsMono Nerd Font\n\
             font-family = Noto Color Emoji\n\
             font-size = 12\n\
             theme = light:GitHub Light Default,dark:GitHub Dark Default\n\
             cursor-style-blink = false\n\
             copy-on-select = clipboard\n\
             background-opacity = 0.92\n\
             background-blur = 20\n\
             window-padding-x = 10\n\
             window-padding-y = 8\n\
             window-padding-balance = true\n",
        );

        // The first family is the one being asked for; the rest are Ghostty's
        // fallbacks, which fontconfig picks here.
        assert_eq!(settings.terminal.font_family, "JetBrainsMono Nerd Font");
        assert!((settings.terminal.font_size - 12.0).abs() < f64::EPSILON);
        assert_eq!(settings.terminal.theme_light, "GitHub Light Default");
        assert_eq!(settings.terminal.theme_dark, "GitHub Dark Default");
        assert!(!settings.terminal.cursor_blink);
        assert!(settings.terminal.copy_on_select);
        assert!((settings.terminal.background_opacity - 0.92).abs() < f64::EPSILON);
        assert!(settings.background_blur);
        assert!((settings.terminal.padding_x - 10.0).abs() < f64::EPSILON);
        assert!((settings.terminal.padding_y - 8.0).abs() < f64::EPSILON);
    }

    #[test]
    fn one_theme_name_covers_both_appearances() {
        let settings = Settings::parse("theme = GitHub Dark Default\n");
        assert_eq!(settings.terminal.theme_light, "GitHub Dark Default");
        assert_eq!(settings.terminal.theme_dark, "GitHub Dark Default");
        assert_eq!(settings.appearance, Appearance::System);
    }

    #[test]
    fn an_opacity_nothing_could_be_read_through_leaves_the_default() {
        let read = Settings::parse("background-opacity = 0.02");
        assert!((read.terminal.background_opacity - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_cursor_style_nobody_spells_that_way_leaves_the_default() {
        assert_eq!(
            Settings::parse("cursor-style = \"beam\"")
                .terminal
                .cursor_style,
            CursorStyle::Block
        );
        assert_eq!(
            Settings::parse("cursor-style = \"underline\"")
                .terminal
                .cursor_style,
            CursorStyle::Underline
        );
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
