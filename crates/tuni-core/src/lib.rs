//! Portable workspace models — no GTK, no libghostty.
//!
//! Etap 0 only needs the terminal's own configuration and its colors. Projects,
//! pane layout, session persistence, the file tree, and git status land here in
//! Etapy 2–6.

pub mod theme;

/// Point sizes a font may be zoomed to. Below the first the grid stops being
/// legible; above the second a single cell no longer fits a sane window.
pub const FONT_SIZE_MIN: f64 = 4.0;
pub const FONT_SIZE_MAX: f64 = 96.0;

/// Terminal appearance and behavior. Serialization and a settings UI arrive in
/// Etap 4; until then the defaults are the whole story.
#[derive(Clone, Debug)]
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
    /// Bundled theme names, one per desktop appearance. The desktop decides
    /// which of the two is in use, so both are configured, as Ghostty and kero
    /// both do.
    pub theme_light: String,
    pub theme_dark: String,
}

impl TerminalConfig {
    /// The colors for one desktop appearance.
    pub fn theme(&self, dark: bool) -> theme::Theme {
        let name = if dark { &self.theme_dark } else { &self.theme_light };
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
