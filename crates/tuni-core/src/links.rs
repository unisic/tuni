//! Finding URLs in a line of terminal output.
//!
//! Ghostty matches URLs with a regex it feeds to oniguruma, and this is that
//! regex's scheme branch, ported pattern for pattern; `fancy-regex` is the
//! Rust engine that has the lookbehind it ends on. Only the scheme branch:
//! Ghostty also matches bare file paths, on heuristics its own comments call
//! breakable, and a path in this workspace already has better ways to open
//! than a guess at `xdg-open`.
//!
//! The scheme list is liberal in what it accepts after the scheme, with two
//! exceptions Ghostty documents: a URL does not end with `.` or `,`, because
//! sentences do, and it does not end with `)` unless it contains a matching
//! `(`, because Markdown wraps links in parentheses far more often than a URL
//! ends in one.

use std::ops::Range;
use std::sync::OnceLock;

use fancy_regex::Regex;

/// One URL found in a line: where it sits, in characters, and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub range: Range<usize>,
    pub uri: String,
}

/// Ghostty's scheme-URL pattern, verbatim apart from the string escaping.
const PATTERN: &str = concat!(
    r"(?:https?://|mailto:|ftp://|file:|ssh:|git://|ssh://|tel:|magnet:|ipfs://|ipns://|gemini://|gopher://|news:)",
    r"(?:\[[:0-9a-fA-F]+(?:[:0-9a-fA-F]*)+\](?::[0-9]+)?|[\w\-.~:/?#@!$&*+,;=%]+(?:[(\[]\w*[)\]])?)+",
    r"(?<![,.])",
);

fn regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(PATTERN).expect("the URL pattern compiles"))
}

/// The URL under one character of a line, if that character is part of one.
///
/// Positions count characters, not bytes, because the caller's line is one
/// character per terminal cell and the answer has to name cells.
#[must_use]
pub fn url_at(line: &str, cell: usize) -> Option<Found> {
    for found in regex().find_iter(line) {
        // An engine error is a pathological input, and a line that cannot be
        // scanned holds no link anyone can click.
        let found = found.ok()?;
        let start = line[..found.start()].chars().count();
        let end = start + line[found.start()..found.end()].chars().count();
        if cell < start {
            return None;
        }
        if cell < end {
            return Some(Found {
                range: start..end,
                uri: found.as_str().to_owned(),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::url_at;

    /// The position of `needle` in `haystack`, in characters.
    fn char_at(haystack: &str, needle: &str) -> usize {
        let byte = haystack.find(needle).expect("the needle is in the line");
        haystack[..byte].chars().count()
    }

    /// Ghostty's own test corpus for the scheme branch: hovering the first
    /// character of the expected URL finds exactly that URL.
    #[test]
    fn the_ghostty_corpus_matches_the_same_urls() {
        let cases: &[(&str, &str)] = &[
            ("hello https://example.com world", "https://example.com"),
            (
                "https://example.com/foo(bar) more",
                "https://example.com/foo(bar)",
            ),
            (
                "https://example.com/foo(bar)baz more",
                "https://example.com/foo(bar)baz",
            ),
            (
                "Link inside (https://example.com) parens",
                "https://example.com",
            ),
            (
                "Link period https://example.com. More text.",
                "https://example.com",
            ),
            (
                "Link trailing comma https://example.com, more text.",
                "https://example.com",
            ),
            (
                "Link in double quotes \"https://example.com\" and more",
                "https://example.com",
            ),
            (
                "Link in single quotes 'https://example.com' and more",
                "https://example.com",
            ),
            (
                "also match http://example.com non-secure links",
                "http://example.com",
            ),
            (
                "match tel://+12123456789 phone numbers",
                "tel://+12123456789",
            ),
            (
                "match with query url https://example.com?query=1&other=2 and more text.",
                "https://example.com?query=1&other=2",
            ),
            (
                "url with dashes [mode 2027](https://github.com/contour-terminal/terminal-unicode-core) for better unicode support",
                "https://github.com/contour-terminal/terminal-unicode-core",
            ),
            ("dot.http://example.com", "http://example.com"),
            (
                "weird characters https://example.com/~user/?query=1&other=2#hash and more",
                "https://example.com/~user/?query=1&other=2#hash",
            ),
            (
                "square brackets https://example.com/[foo] and more",
                "https://example.com/[foo]",
            ),
            (
                "[13]:TooManyStatements: TempFile#assign_temp_file_to_entity has approx 7 statements [https://example.com/docs/Too-Many-Statements.md]",
                "https://example.com/docs/Too-Many-Statements.md",
            ),
            ("match ftp://example.com ftp links", "ftp://example.com"),
            ("match ssh://example.com ssh links", "ssh://example.com"),
            ("match git://example.com git links", "git://example.com"),
            ("match tel:+18005551234 tel links", "tel:+18005551234"),
            (
                "match magnet:?xt=urn:btih:1234567890 magnet links",
                "magnet:?xt=urn:btih:1234567890",
            ),
            (
                "match ipfs://QmSomeHashValue ipfs links",
                "ipfs://QmSomeHashValue",
            ),
            (
                "match gemini://example.com gemini links",
                "gemini://example.com",
            ),
            (
                "match news:comp.infosystems.www.servers.unix news links",
                "news:comp.infosystems.www.servers.unix",
            ),
        ];
        for (line, expected) in cases {
            let at = char_at(line, expected);
            let found = url_at(line, at).unwrap_or_else(|| panic!("no URL in {line:?}"));
            assert_eq!(found.uri, *expected, "in {line:?}");
            assert_eq!(found.range.start, at, "in {line:?}");
        }
    }

    #[test]
    fn every_character_of_the_url_answers_and_the_rest_do_not() {
        let line = "see https://example.com now";
        assert!(url_at(line, 3).is_none(), "the space before");
        assert!(url_at(line, 4).is_some(), "the first character");
        assert!(url_at(line, 22).is_some(), "the last character");
        assert!(url_at(line, 23).is_none(), "the space after");
        assert!(url_at(line, 100).is_none(), "past the end");
    }

    #[test]
    fn positions_count_characters_not_bytes() {
        // Multibyte characters before the URL: byte and char offsets diverge.
        let line = "画面 https://example.com 端";
        let at = char_at(line, "https");
        let found = url_at(line, at).expect("found");
        assert_eq!(found.range.start, at);
        assert_eq!(found.uri, "https://example.com");
    }

    #[test]
    fn the_second_url_on_a_line_is_reachable() {
        let line = "some file with https://google.com https://duckduckgo.com links.";
        let at = char_at(line, "https://duckduckgo.com");
        let found = url_at(line, at).expect("found");
        assert_eq!(found.uri, "https://duckduckgo.com");
    }
}
