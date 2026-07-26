//! Portable workspace models — no GTK, no libghostty.
//!
//! The terminal's own configuration and colors, the projects and tabs around
//! them, the pane layout inside a tab, what all of it is restored from, and the
//! directory tree and repository state beside it.

pub mod diff;
pub mod editor;
pub mod files;
pub mod fuzzy;
pub mod git;
pub mod info;
pub mod panes;
pub mod session;
pub mod settings;
pub mod ssh;
pub mod theme;
pub mod usage;
pub mod workspace;

/// Point sizes a font may be zoomed to. Below the first the grid stops being
/// legible; above the second a single cell no longer fits a sane window.
pub const FONT_SIZE_MIN: f64 = 4.0;
pub const FONT_SIZE_MAX: f64 = 96.0;

/// How transparent the background is allowed to get. Past this the text is
/// being read against whatever happens to be behind the window rather than
/// against a terminal.
pub const OPACITY_MIN: f64 = 0.2;

/// How much blank space may be left around the grid, in pixels.
pub const PADDING_MAX: f64 = 40.0;

/// The shape the cursor takes until the program running in the terminal asks
/// for another one. DECSCUSR is the application's to send, so this is only what
/// a screen nobody has asked anything of shows.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CursorStyle {
    #[default]
    Block,
    /// A vertical line between cells, which is what most editors show.
    Bar,
    Underline,
    /// A block drawn as an outline, which reads as a cursor without hiding the
    /// character under it.
    BlockHollow,
}

impl CursorStyle {
    /// Ghostty's spellings, since this is Ghostty's `cursor-style` key.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim() {
            "block" => Some(Self::Block),
            "bar" => Some(Self::Bar),
            "underline" => Some(Self::Underline),
            "block_hollow" => Some(Self::BlockHollow),
            _ => None,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Bar => "bar",
            Self::Underline => "underline",
            Self::BlockHollow => "block_hollow",
        }
    }
}

/// Terminal appearance and behavior. Read from and written to the config file
/// by [`settings::Settings`].
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalConfig {
    pub font_family: String,
    /// Point size, as Pango understands it.
    pub font_size: f64,
    /// Whether the font's ligatures are allowed to fire.
    ///
    /// Off by default: a terminal is a grid, and a ligature is one glyph
    /// spanning what the terminal still counts as several cells. Coding fonts
    /// are the reason to turn it on, and the reason it is a knob rather than a
    /// decision.
    pub font_ligatures: bool,
    pub scrollback_lines: usize,
    /// Extra space between rows, in pixels. Ghostty exposes the same knob.
    pub line_height_extra: f64,
    /// Whether the cursor blinks when the application has not asked for a
    /// particular style. The desktop's own blink preference still wins.
    pub cursor_blink: bool,
    /// The shape it takes under the same condition.
    pub cursor_style: CursorStyle,
    /// Whether highlighting text also puts it on the clipboard. Off by default:
    /// the primary selection already carries it to a middle click, and a
    /// terminal that overwrites the clipboard on every drag loses whatever was
    /// copied to paste into it.
    pub copy_on_select: bool,
    /// Whether an application is allowed to take the mouse at all. On, because
    /// that is what an application asking for the mouse expects, and a knob
    /// because the ones that follow the pointer take the drag that would have
    /// selected text with it. Ghostty spells it `mouse-reporting` and means the
    /// same thing.
    pub mouse_reporting: bool,
    /// Whether `BEL` reaches the desktop at all. The widget's own alert sound
    /// and the notification a background tab raises. On, because a bell is a
    /// program asking for attention; a knob, because a build that ends in one
    /// is not always worth a sound.
    pub bell: bool,
    /// What to run instead of the login shell. Empty is the login shell, which
    /// is `$SHELL`, then the passwd entry. A bare name is looked up on `PATH`.
    pub command: String,
    /// How opaque the background is, from [`OPACITY_MIN`] to 1.0. Only the
    /// page color takes it: a cell an application colored itself is drawn
    /// solid, or reverse video and a selection would come out as a tint of the
    /// wallpaper.
    ///
    /// Whether anything is visible through the window is the compositor's to
    /// decide, and a compositor that does not composite leaves the background
    /// black. That is the same deal Ghostty's `background-opacity` offers.
    pub background_opacity: f64,
    /// Blank space between the edges of the terminal and the grid, in pixels,
    /// up to [`PADDING_MAX`], across and down. Zero, because the grid is the
    /// terminal and the space around it is a preference rather than a default.
    /// Two numbers rather than one, which is how Ghostty spells it: a line of
    /// text has more room to spare above it than beside it.
    pub padding_x: f64,
    pub padding_y: f64,
    /// Bundled theme names, one per desktop appearance. The desktop decides
    /// which of the two is in use, so both are configured, as Ghostty and kero
    /// both do.
    pub theme_light: String,
    pub theme_dark: String,
}

impl TerminalConfig {
    /// The colors for one desktop appearance.
    pub fn theme(&self, dark: bool) -> theme::Theme {
        let name = if dark {
            &self.theme_dark
        } else {
            &self.theme_light
        };
        theme::theme_or_default(name, dark)
    }

    /// The font as a family list, most wanted first.
    ///
    /// Pango reads a comma-separated family as a fallback chain, so a machine
    /// without the configured face lands on the Nerd Font symbols — the
    /// powerline glyphs a prompt is likely to use — and then on whatever
    /// fontconfig calls monospace, rather than on a proportional default.
    ///
    /// No space after the commas: Pango splits the list on commas and looks up
    /// the pieces as they stand, and a leading space is part of the name it
    /// asks fontconfig for. `" monospace"` is not `"monospace"` and resolves to
    /// a different face.
    #[must_use]
    pub fn font_stack(&self) -> String {
        format!(
            "{},Symbols Nerd Font Mono,monospace",
            self.font_family.trim()
        )
    }

    /// Read a font the way Pango writes one: a family, then an optional point
    /// size. `"JetBrains Mono 13"`, `"Fira Code"`, `"Cascadia Code 11.5"`.
    ///
    /// An empty family, or a size outside what a terminal can show, leaves that
    /// half of the setting alone.
    pub fn set_font(&mut self, spec: &str) {
        let spec = spec.trim();
        let (family, size) = match spec.rsplit_once(' ') {
            Some((family, tail)) => match tail.parse::<f64>() {
                Ok(size) => (family.trim(), Some(size)),
                Err(_) => (spec, None),
            },
            None => (spec, None),
        };

        if !family.is_empty() {
            self.font_family = family.to_owned();
        }
        if let Some(size) = size.filter(|s| (FONT_SIZE_MIN..=FONT_SIZE_MAX).contains(s)) {
            self.font_size = size;
        }
    }
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            font_family: "JetBrains Mono".to_owned(),
            font_size: 11.0,
            font_ligatures: false,
            scrollback_lines: 10_000,
            line_height_extra: 0.0,
            cursor_blink: true,
            cursor_style: CursorStyle::Block,
            copy_on_select: false,
            mouse_reporting: true,
            bell: true,
            command: String::new(),
            background_opacity: 1.0,
            padding_x: 0.0,
            padding_y: 0.0,
            theme_light: theme::DEFAULT_LIGHT.to_owned(),
            theme_dark: theme::DEFAULT_DARK.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_font_spec_carries_a_family_and_a_size() {
        let mut config = TerminalConfig::default();
        config.set_font("Fira Code 13.5");
        assert_eq!(config.font_family, "Fira Code");
        assert!((config.font_size - 13.5).abs() < f64::EPSILON);
    }

    #[test]
    fn a_font_spec_without_a_size_keeps_the_one_in_effect() {
        let mut config = TerminalConfig {
            font_size: 14.0,
            ..TerminalConfig::default()
        };
        config.set_font("Cascadia Code");
        assert_eq!(config.font_family, "Cascadia Code");
        assert!((config.font_size - 14.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_family_that_ends_in_a_number_is_still_a_family() {
        let mut config = TerminalConfig::default();
        config.set_font("PT Mono 2");
        // Ambiguous by construction, and Pango resolves it the same way: the
        // trailing number is a size unless it is one no terminal would use.
        assert_eq!(config.font_family, "PT Mono");
        assert!((config.font_size - 11.0).abs() < f64::EPSILON);
    }

    #[test]
    fn an_absurd_size_is_ignored() {
        let mut config = TerminalConfig::default();
        config.set_font("Iosevka 400");
        assert_eq!(config.font_family, "Iosevka");
        assert!((config.font_size - 11.0).abs() < f64::EPSILON);
    }

    #[test]
    fn the_font_stack_falls_back_past_the_configured_family() {
        let config = TerminalConfig::default();
        assert_eq!(
            config.font_stack(),
            "JetBrains Mono,Symbols Nerd Font Mono,monospace"
        );
    }
}
