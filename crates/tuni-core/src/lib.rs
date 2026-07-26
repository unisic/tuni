//! Portable workspace models — no GTK, no libghostty.
//!
//! Etap 0 only needs the terminal's own configuration and its colors. Projects,
//! pane layout, session persistence, the file tree, and git status land here in
//! Etapy 2–6.

pub mod theme;

/// Terminal appearance and behavior. Serialization and a settings UI arrive in
/// Etap 4; until then the defaults are the whole story.
#[derive(Clone, Debug)]
pub struct TerminalConfig {
    pub font_family: String,
    /// Point size, as Pango understands it.
    pub font_size: f64,
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
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            font_family: "JetBrains Mono".to_owned(),
            font_size: 11.0,
            scrollback_lines: 10_000,
            line_height_extra: 0.0,
            cursor_blink: true,
            theme_light: theme::DEFAULT_LIGHT.to_owned(),
            theme_dark: theme::DEFAULT_DARK.to_owned(),
        }
    }
}
