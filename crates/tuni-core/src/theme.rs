//! Color themes in Ghostty's format, and the bundled catalog of them.
//!
//! A theme file is `key = value` lines, one color per line, with the ANSI
//! palette written as `palette = 0=#21222c`. Ghostty publishes 574 of them,
//! converted from the iTerm2 collection; they are vendored under `data/themes`
//! and baked into the binary by `build.rs`.
//!
//! The parser ignores keys it does not know rather than failing on them, so a
//! theme written for a newer Ghostty still loads here with its colors intact.

include!(concat!(env!("OUT_DIR"), "/themes.rs"));

/// An sRGB color, the only kind a theme file can express.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// `#rrggbb`, `rrggbb`, or the three-digit short form.
    pub fn parse(text: &str) -> Option<Self> {
        let digits = text.trim().strip_prefix('#').unwrap_or(text.trim());
        if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }

        let byte = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
        match digits.len() {
            6 => Some(Self::new(byte(0)?, byte(2)?, byte(4)?)),
            // "#fff" is "#ffffff": each digit doubles.
            3 => {
                let nibble = |at: usize| {
                    u8::from_str_radix(&digits[at..at + 1], 16)
                        .ok()
                        .map(|v| v << 4 | v)
                };
                Some(Self::new(nibble(0)?, nibble(1)?, nibble(2)?))
            }
            _ => None,
        }
    }

    /// This color moved `amount` of the way toward `other`, 0.0 to 1.0.
    pub fn blend(self, other: Self, amount: f64) -> Self {
        let amount = amount.clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| (f64::from(a) + (f64::from(b) - f64::from(a)) * amount) as u8;
        Self::new(
            mix(self.r, other.r),
            mix(self.g, other.g),
            mix(self.b, other.b),
        )
    }

    /// Relative luminance by the Rec. 709 weights, 0.0 to 1.0. Good enough to
    /// answer "is this a dark theme"; not a perceptual lightness.
    pub fn luminance(self) -> f64 {
        (0.2126 * f64::from(self.r) + 0.7152 * f64::from(self.g) + 0.0722 * f64::from(self.b))
            / 255.0
    }

    /// `#rrggbb`, which is what both the theme files and CSS want.
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Black or white, whichever stays legible on top of this color.
    pub fn contrasting(self) -> Self {
        if self.luminance() < 0.5 {
            Self::new(0xff, 0xff, 0xff)
        } else {
            Self::new(0x00, 0x00, 0x00)
        }
    }
}

/// One theme: the terminal's default colors and its sixteen ANSI slots.
///
/// The 240 remaining slots of the 256-color palette are the standard cube and
/// grayscale ramp, which no theme overrides and which the terminal already
/// knows; only the first sixteen are theme data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub name: String,
    pub background: Rgb,
    pub foreground: Rgb,
    pub cursor: Option<Rgb>,
    /// The text under a block cursor. `None` means invert the cell.
    pub cursor_text: Option<Rgb>,
    pub selection_background: Option<Rgb>,
    pub selection_foreground: Option<Rgb>,
    pub palette: [Rgb; 16],
}

impl Theme {
    /// Parse a theme file. Returns `None` only when the file names neither a
    /// background nor a foreground, which no usable theme omits.
    pub fn parse(name: &str, text: &str) -> Option<Self> {
        let mut background = None;
        let mut foreground = None;
        let mut cursor = None;
        let mut cursor_text = None;
        let mut selection_background = None;
        let mut selection_foreground = None;
        let mut palette = [Rgb::default(); 16];
        let mut seen = [false; 16];

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());

            if key == "palette" {
                // The value is itself "index=color".
                if let Some((index, color)) = value.split_once('=')
                    && let Ok(index) = index.trim().parse::<usize>()
                    && let Some(color) = Rgb::parse(color)
                    && index < palette.len()
                {
                    palette[index] = color;
                    seen[index] = true;
                }
                continue;
            }

            let Some(color) = Rgb::parse(value) else {
                continue;
            };
            match key {
                "background" => background = Some(color),
                "foreground" => foreground = Some(color),
                "cursor-color" => cursor = Some(color),
                "cursor-text" => cursor_text = Some(color),
                "selection-background" => selection_background = Some(color),
                "selection-foreground" => selection_foreground = Some(color),
                _ => {}
            }
        }

        // A theme that sets only one of the two still says something; the other
        // takes the obvious contrast so the result is at least readable.
        let (background, foreground) = match (background, foreground) {
            (Some(bg), Some(fg)) => (bg, fg),
            (Some(bg), None) => (bg, bg.contrasting()),
            (None, Some(fg)) => (fg.contrasting(), fg),
            (None, None) => return None,
        };

        // A palette slot the theme skipped falls back to the xterm color, not
        // to black, which would make that ANSI color invisible.
        for (index, slot) in palette.iter_mut().enumerate() {
            if !seen[index] {
                *slot = XTERM_ANSI[index];
            }
        }

        Some(Self {
            name: name.to_owned(),
            background,
            foreground,
            cursor,
            cursor_text,
            selection_background,
            selection_foreground,
            palette,
        })
    }

    /// Whether this theme is meant for a dark desktop.
    pub fn is_dark(&self) -> bool {
        self.background.luminance() < 0.5
    }

    /// The color for focus rings, active icons, and links. ANSI blue is what a
    /// palette means by "this is interactive"; the cursor and then the plain
    /// foreground stand in for palettes that make blue unreadable.
    pub fn accent(&self) -> Rgb {
        let blue = self.palette[4];
        if blue != self.background {
            return blue;
        }
        self.cursor.unwrap_or(self.foreground)
    }

    /// The background lifted toward the foreground: sidebars, gutters, the
    /// editor's current-line highlight. Deriving it from the theme's own two
    /// colors keeps light themes light and dark themes dark.
    pub fn surface(&self, elevation: f64) -> Rgb {
        self.background.blend(self.foreground, elevation)
    }
}

/// The theme used on a dark desktop when the configured one is unknown.
pub const DEFAULT_DARK: &str = "GitHub Dark Default";
/// The theme used on a light desktop when the configured one is unknown.
pub const DEFAULT_LIGHT: &str = "GitHub Light Default";

/// Every bundled theme's name, sorted.
pub fn names() -> impl ExactSizeIterator<Item = &'static str> {
    THEMES.iter().map(|(name, _)| *name)
}

/// A bundled theme by name. Names are exact, as Ghostty's own config treats
/// them.
pub fn theme(name: &str) -> Option<Theme> {
    let at = THEMES.binary_search_by_key(&name, |(name, _)| name).ok()?;
    let (name, text) = THEMES[at];
    Theme::parse(name, text)
}

/// A bundled theme by name, falling back to the default for the desktop's
/// light or dark setting, and to a plain built-in pair if even that is gone.
pub fn theme_or_default(name: &str, dark: bool) -> Theme {
    let fallback = if dark { DEFAULT_DARK } else { DEFAULT_LIGHT };
    theme(name)
        .or_else(|| theme(fallback))
        .unwrap_or_else(|| builtin(dark))
}

/// Last resort should the catalog ever fail to load: GitHub's Default colors,
/// written out so there is always something to draw with.
fn builtin(dark: bool) -> Theme {
    let (background, foreground) = if dark {
        (Rgb::new(0x0d, 0x11, 0x17), Rgb::new(0xe6, 0xed, 0xf3))
    } else {
        (Rgb::new(0xff, 0xff, 0xff), Rgb::new(0x1f, 0x23, 0x28))
    };
    Theme {
        name: if dark { DEFAULT_DARK } else { DEFAULT_LIGHT }.to_owned(),
        background,
        foreground,
        cursor: None,
        cursor_text: None,
        selection_background: None,
        selection_foreground: None,
        palette: XTERM_ANSI,
    }
}

/// xterm's sixteen ANSI colors, for palette slots a theme leaves out.
const XTERM_ANSI: [Rgb; 16] = [
    Rgb::new(0x00, 0x00, 0x00),
    Rgb::new(0xcd, 0x00, 0x00),
    Rgb::new(0x00, 0xcd, 0x00),
    Rgb::new(0xcd, 0xcd, 0x00),
    Rgb::new(0x00, 0x00, 0xee),
    Rgb::new(0xcd, 0x00, 0xcd),
    Rgb::new(0x00, 0xcd, 0xcd),
    Rgb::new(0xe5, 0xe5, 0xe5),
    Rgb::new(0x7f, 0x7f, 0x7f),
    Rgb::new(0xff, 0x00, 0x00),
    Rgb::new(0x00, 0xff, 0x00),
    Rgb::new(0xff, 0xff, 0x00),
    Rgb::new(0x5c, 0x5c, 0xff),
    Rgb::new(0xff, 0x00, 0xff),
    Rgb::new(0x00, 0xff, 0xff),
    Rgb::new(0xff, 0xff, 0xff),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_is_sorted_and_every_theme_parses() {
        assert!(names().len() > 500, "the catalog did not get embedded");
        assert!(names().is_sorted(), "binary search needs a sorted table");

        for name in names() {
            let theme = theme(name).unwrap_or_else(|| panic!("{name} did not parse"));
            assert_eq!(theme.name, name);
            assert_ne!(
                theme.background, theme.foreground,
                "{name} is unreadable: one color for text and page"
            );
        }
    }

    #[test]
    fn a_theme_file_becomes_colors() {
        let theme = theme("Dracula").expect("Dracula is in the catalog");
        assert_eq!(theme.background, Rgb::new(0x28, 0x2a, 0x36));
        assert_eq!(theme.foreground, Rgb::new(0xf8, 0xf8, 0xf2));
        assert_eq!(theme.cursor, Some(Rgb::new(0xf8, 0xf8, 0xf2)));
        assert_eq!(theme.selection_background, Some(Rgb::new(0x44, 0x47, 0x5a)));
        assert_eq!(theme.palette[1], Rgb::new(0xff, 0x55, 0x55));
        assert_eq!(theme.palette[15], Rgb::new(0xff, 0xff, 0xff));
        assert!(theme.is_dark());
    }

    #[test]
    fn light_and_dark_themes_are_told_apart() {
        assert!(theme(DEFAULT_DARK).expect("bundled").is_dark());
        assert!(!theme(DEFAULT_LIGHT).expect("bundled").is_dark());
    }

    #[test]
    fn an_unknown_name_falls_back_to_the_desktop_default() {
        assert_eq!(theme_or_default("no such theme", true).name, DEFAULT_DARK);
        assert_eq!(theme_or_default("no such theme", false).name, DEFAULT_LIGHT);
        assert_eq!(theme_or_default("Dracula", false).name, "Dracula");
    }

    #[test]
    fn colors_parse_in_every_form_a_theme_might_write_them() {
        assert_eq!(Rgb::parse("#a1b2c3"), Some(Rgb::new(0xa1, 0xb2, 0xc3)));
        assert_eq!(Rgb::parse("a1b2c3"), Some(Rgb::new(0xa1, 0xb2, 0xc3)));
        assert_eq!(Rgb::parse("#fff"), Some(Rgb::new(0xff, 0xff, 0xff)));
        assert_eq!(Rgb::parse("#12345"), None);
        assert_eq!(Rgb::parse("rebeccapurple"), None);
        assert_eq!(Rgb::new(0x0d, 0x11, 0x17).to_hex(), "#0d1117");
    }

    #[test]
    fn unknown_keys_are_skipped_rather_than_fatal() {
        let text = "\
            # a comment\n\
            font-family = Whatever\n\
            background = #101010\n\
            foreground = #f0f0f0\n\
            palette = 4=#0000ff\n\
            palette = 999=#ff0000\n\
            not a key value line\n";
        let theme = Theme::parse("Test", text).expect("background and foreground are enough");
        assert_eq!(theme.background, Rgb::new(0x10, 0x10, 0x10));
        assert_eq!(theme.palette[4], Rgb::new(0x00, 0x00, 0xff));
        // The slot the theme skipped keeps a visible color rather than black.
        assert_eq!(theme.palette[1], XTERM_ANSI[1]);
        assert_eq!(theme.cursor, None);
    }

    #[test]
    fn a_theme_naming_one_color_gets_a_readable_partner() {
        let dark = Theme::parse("Dark", "background = #000000").expect("one color is enough");
        assert_eq!(dark.foreground, Rgb::new(0xff, 0xff, 0xff));
        let light = Theme::parse("Light", "foreground = #000000").expect("one color is enough");
        assert_eq!(light.background, Rgb::new(0xff, 0xff, 0xff));
        assert_eq!(Theme::parse("Empty", "# nothing here\n"), None);
    }

    #[test]
    fn chrome_colors_come_from_the_theme_itself() {
        let theme = theme("Dracula").expect("bundled");
        assert_eq!(theme.accent(), theme.palette[4]);
        // A surface sits between the two, never outside them.
        let surface = theme.surface(0.08);
        assert_ne!(surface, theme.background);
        assert!(surface.luminance() > theme.background.luminance());
        assert_eq!(theme.surface(0.0), theme.background);
        assert_eq!(theme.surface(1.0), theme.foreground);
    }
}
