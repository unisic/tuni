//! Reading a unified diff, and cutting one hunk back out of it.
//!
//! What `git diff` prints is a text format, and everything the viewer shows is
//! read from it rather than from a second run of git: the lines, their numbers
//! on both sides, and the patch that stages a single hunk. Staging a hunk is
//! the reason the parse has to keep the original text intact — the patch handed
//! back to `git apply` is the input's own bytes, not a re-rendering of what was
//! understood.

use similar::{ChangeTag, TextDiff};

/// What one line of a hunk is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    /// Unchanged, and shown on both sides.
    Context,
    Added,
    Removed,
    /// `\ No newline at end of file`. Not a line of the file at all — a note
    /// about the one above it — but it belongs in the patch.
    Note,
}

/// One line, with its number on whichever sides it exists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Line {
    pub kind: Kind,
    /// The text without the leading marker.
    pub text: String,
    pub old: Option<u32>,
    pub new: Option<u32>,
}

impl Line {
    /// The line as it appears in a patch, marker included and newline-free.
    #[must_use]
    pub fn to_patch_line(&self) -> String {
        let marker = match self.kind {
            Kind::Context => ' ',
            Kind::Added => '+',
            Kind::Removed => '-',
            Kind::Note => '\\',
        };
        format!("{marker}{}", self.text)
    }
}

/// A run of changed lines with the context around it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    /// Whatever git wrote after the second `@@` — the enclosing function, when
    /// it could work one out.
    pub heading: String,
    pub lines: Vec<Line>,
}

impl Hunk {
    /// The `@@ -1,4 +1,6 @@` line this hunk was read from.
    #[must_use]
    pub fn header(&self) -> String {
        let old = span(self.old_start, self.old_count);
        let new = span(self.new_start, self.new_count);
        let mut header = format!("@@ -{old} +{new} @@");
        if !self.heading.is_empty() {
            header.push(' ');
            header.push_str(&self.heading);
        }
        header
    }

    /// How many lines this hunk adds and how many it removes.
    #[must_use]
    pub fn counts(&self) -> (usize, usize) {
        let added = self
            .lines
            .iter()
            .filter(|line| line.kind == Kind::Added)
            .count();
        let removed = self
            .lines
            .iter()
            .filter(|line| line.kind == Kind::Removed)
            .count();
        (added, removed)
    }

    /// The hunk as two columns.
    ///
    /// A run of removals is paired with the run of additions that follows it,
    /// one for one, because that is what a change to a line looks like in a
    /// unified diff: the old line, then the new one. What is left over when the
    /// runs are different lengths is a line added or removed outright, and it
    /// stands alone on its side.
    #[must_use]
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        let mut removed: Vec<Line> = Vec::new();
        let mut added: Vec<Line> = Vec::new();

        for line in &self.lines {
            match line.kind {
                Kind::Removed => removed.push(line.clone()),
                Kind::Added => added.push(line.clone()),
                Kind::Context => {
                    flush(&mut rows, &mut removed, &mut added);
                    rows.push(Row {
                        old: Some(line.clone()),
                        new: Some(line.clone()),
                    });
                }
                // A note belongs to the line above it, which is already on the
                // side it belongs to; a column of its own would only be an
                // empty row.
                Kind::Note => (),
            }
        }
        flush(&mut rows, &mut removed, &mut added);
        rows
    }
}

/// One line of a two-column view. At least one side is always there.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Row {
    pub old: Option<Line>,
    pub new: Option<Line>,
}

impl Row {
    /// Whether this row is a line that changed rather than one that only moved.
    #[must_use]
    pub fn is_change(&self) -> bool {
        !matches!(
            (&self.old, &self.new),
            (Some(old), Some(new)) if old.kind == Kind::Context && new.kind == Kind::Context
        )
    }
}

fn flush(rows: &mut Vec<Row>, removed: &mut Vec<Line>, added: &mut Vec<Line>) {
    let pairs = removed.len().max(added.len());
    for index in 0..pairs {
        rows.push(Row {
            old: removed.get(index).cloned(),
            new: added.get(index).cloned(),
        });
    }
    removed.clear();
    added.clear();
}

fn span(start: u32, count: u32) -> String {
    if count == 1 {
        start.to_string()
    } else {
        format!("{start},{count}")
    }
}

/// One file's diff: the header git wrote for it, and the hunks under it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Diff {
    /// Everything from `diff --git` down to `+++`, kept verbatim because it is
    /// what a patch has to carry back to `git apply`.
    pub preamble: Vec<String>,
    pub hunks: Vec<Hunk>,
    /// Why there are no hunks, when there are none and there should have been:
    /// a binary file, or one git would not diff.
    pub note: Option<String>,
}

impl Diff {
    /// Reads what `git diff` printed for one file.
    ///
    /// Anything after a second `diff --git` is ignored: the viewer asks for one
    /// path at a time, and a second file in the output would be a pathspec that
    /// matched more than was meant.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut diff = Self::default();
        let mut old = 0;
        let mut new = 0;
        let mut started = false;

        for line in text.lines() {
            if line.starts_with("diff --git") || line.starts_with("diff --no-index") {
                if started {
                    break;
                }
                started = true;
                diff.preamble.push(line.to_owned());
                continue;
            }
            if let Some(hunk) = parse_header(line) {
                // The counters name the line last read, so they start one
                // before the hunk does. A hunk against an empty side starts at
                // zero and stays there.
                old = hunk.old_start.saturating_sub(1);
                new = hunk.new_start.saturating_sub(1);
                diff.hunks.push(hunk);
                continue;
            }
            let Some(hunk) = diff.hunks.last_mut() else {
                // Still in the header. `Binary files ... differ` is git saying
                // there will be no hunks at all.
                if line.starts_with("Binary files") || line.ends_with("binary files differ") {
                    diff.note = Some("This is a binary file.".to_owned());
                }
                diff.preamble.push(line.to_owned());
                continue;
            };

            let mut characters = line.chars();
            let (kind, text) = match characters.next() {
                Some(' ') => (Kind::Context, characters.as_str()),
                Some('+') => (Kind::Added, characters.as_str()),
                Some('-') => (Kind::Removed, characters.as_str()),
                Some('\\') => (Kind::Note, characters.as_str()),
                // An empty line inside a hunk is a context line whose trailing
                // space some tool trimmed. Git itself never writes one, but a
                // patch that has been through an editor may.
                None => (Kind::Context, ""),
                // Anything else has ended the hunks.
                Some(_) => break,
            };
            let line = match kind {
                Kind::Context => {
                    old += 1;
                    new += 1;
                    Line {
                        kind,
                        text: text.to_owned(),
                        old: Some(old),
                        new: Some(new),
                    }
                }
                Kind::Removed => {
                    old += 1;
                    Line {
                        kind,
                        text: text.to_owned(),
                        old: Some(old),
                        new: None,
                    }
                }
                Kind::Added => {
                    new += 1;
                    Line {
                        kind,
                        text: text.to_owned(),
                        old: None,
                        new: Some(new),
                    }
                }
                Kind::Note => Line {
                    kind,
                    text: text.to_owned(),
                    old: None,
                    new: None,
                },
            };
            hunk.lines.push(line);
        }

        diff
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hunks.is_empty()
    }

    /// How many lines the whole file gains and loses.
    #[must_use]
    pub fn counts(&self) -> (usize, usize) {
        self.hunks
            .iter()
            .map(Hunk::counts)
            .fold((0, 0), |(added, removed), (a, r)| (added + a, removed + r))
    }

    /// A patch holding one hunk, ready for `git apply`.
    ///
    /// The file header travels with it, and the hunk's own line counts are
    /// written back out rather than copied, so a hunk that was read from a
    /// diff of the whole file still applies on its own.
    #[must_use]
    pub fn patch(&self, index: usize) -> Option<String> {
        let hunk = self.hunks.get(index)?;
        let mut patch = String::new();
        for line in &self.preamble {
            patch.push_str(line);
            patch.push('\n');
        }
        patch.push_str(&hunk.header());
        patch.push('\n');
        for line in &hunk.lines {
            patch.push_str(&line.to_patch_line());
            patch.push('\n');
        }
        Some(patch)
    }
}

fn parse_header(line: &str) -> Option<Hunk> {
    let rest = line.strip_prefix("@@ ")?;
    let (ranges, heading) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(' ')?;
    let (old_start, old_count) = range(old.strip_prefix('-')?)?;
    let (new_start, new_count) = range(new.strip_prefix('+')?)?;
    Some(Hunk {
        old_start,
        old_count,
        new_start,
        new_count,
        heading: heading.trim().to_owned(),
        lines: Vec::new(),
    })
}

/// `12,4`, or `12` when the range is one line long.
fn range(text: &str) -> Option<(u32, u32)> {
    match text.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((text.parse().ok()?, 1)),
    }
}

/// A stretch of a line, and whether it is part of what changed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Span {
    pub text: String,
    pub changed: bool,
}

/// The longest line the word-level comparison will run on. Past it the
/// comparison costs more than the highlight is worth, and a minified file is
/// one line long.
const MAX_SPAN_BYTES: usize = 2048;

/// How much of the longer line has to survive the change for the difference to
/// be worth pointing at, in hundredths. Below it the two are different lines
/// rather than one line edited, and marking the handful of spaces and brackets
/// they happen to share says nothing about either.
const MIN_SHARED_PERCENT: usize = 30;

/// What changed inside a pair of lines, as spans for each side.
///
/// Both sides come back whole, so the caller can draw them either way round.
#[must_use]
pub fn spans(old: &str, new: &str) -> (Vec<Span>, Vec<Span>) {
    let whole = || {
        (
            vec![Span {
                text: old.to_owned(),
                changed: true,
            }],
            vec![Span {
                text: new.to_owned(),
                changed: true,
            }],
        )
    };
    if old.is_empty() || new.is_empty() {
        return whole();
    }
    if old.len() > MAX_SPAN_BYTES || new.len() > MAX_SPAN_BYTES {
        return whole();
    }

    let mut before = Vec::new();
    let mut after = Vec::new();
    for change in TextDiff::from_words(old, new).iter_all_changes() {
        let text = change.value();
        match change.tag() {
            ChangeTag::Equal => {
                push(&mut before, text, false);
                push(&mut after, text, false);
            }
            ChangeTag::Delete => push(&mut before, text, true),
            ChangeTag::Insert => push(&mut after, text, true),
        }
    }

    // Whitespace is shared by every pair of lines ever written, so it is not
    // evidence that these two are the same line.
    let kept = printing(&before, false);
    let longest = printing(&before, true).max(printing(&after, true));
    if kept * 100 < MIN_SHARED_PERCENT * longest {
        return whole();
    }
    (before, after)
}

/// How many characters that are not whitespace these spans hold — all of them,
/// or only the ones that came through the change unaltered.
fn printing(spans: &[Span], all: bool) -> usize {
    spans
        .iter()
        .filter(|span| all || !span.changed)
        .flat_map(|span| span.text.chars())
        .filter(|character| !character.is_whitespace())
        .count()
}

/// Appends to the span already there when it is the same kind, so a line that
/// changed in one place is three spans rather than one per word.
fn push(spans: &mut Vec<Span>, text: &str, changed: bool) {
    match spans.last_mut() {
        Some(last) if last.changed == changed => last.text.push_str(text),
        _ => spans.push(Span {
            text: text.to_owned(),
            changed,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 83db48f..bf269f4 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,5 +1,6 @@ fn main() {
 use std::fs;
-let one = 1;
+let one = 2;
+let two = 3;
 use std::io;
 fn main() {
@@ -20,3 +21,2 @@
 last
-gone
 tail
";

    #[test]
    fn a_hunk_carries_the_numbers_of_both_sides() {
        let diff = Diff::parse(SAMPLE);
        assert_eq!(diff.hunks.len(), 2);
        assert_eq!(
            diff.preamble.len(),
            4,
            "everything down to +++ is the header"
        );

        let hunk = &diff.hunks[0];
        assert_eq!(hunk.heading, "fn main() {");
        assert_eq!((hunk.old_start, hunk.old_count), (1, 5));
        assert_eq!((hunk.new_start, hunk.new_count), (1, 6));

        let numbers: Vec<_> = hunk
            .lines
            .iter()
            .map(|line| (line.kind, line.old, line.new))
            .collect();
        assert_eq!(
            numbers,
            vec![
                (Kind::Context, Some(1), Some(1)),
                (Kind::Removed, Some(2), None),
                (Kind::Added, None, Some(2)),
                (Kind::Added, None, Some(3)),
                (Kind::Context, Some(3), Some(4)),
                (Kind::Context, Some(4), Some(5)),
            ]
        );
    }

    #[test]
    fn the_counts_are_of_the_whole_file() {
        assert_eq!(Diff::parse(SAMPLE).counts(), (2, 2));
    }

    #[test]
    fn one_hunk_comes_back_out_as_a_patch_of_its_own() {
        let diff = Diff::parse(SAMPLE);
        assert_eq!(
            diff.patch(1).expect("there are two hunks"),
            "\
diff --git a/src/main.rs b/src/main.rs
index 83db48f..bf269f4 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -20,3 +21,2 @@
 last
-gone
 tail
"
        );
        assert!(diff.patch(2).is_none());
    }

    #[test]
    fn a_missing_final_newline_stays_in_the_patch() {
        let text = "\
diff --git a/a b/a
--- a/a
+++ b/a
@@ -1 +1 @@
-one
\\ No newline at end of file
+one
";
        let diff = Diff::parse(text);
        assert_eq!(diff.hunks[0].lines[1].kind, Kind::Note);
        assert!(
            diff.patch(0)
                .expect("one hunk")
                .contains("\\ No newline at end of file"),
            "git apply needs the note or it writes the newline back"
        );
    }

    #[test]
    fn a_change_pairs_the_line_it_replaced() {
        let diff = Diff::parse(SAMPLE);
        let rows = diff.hunks[0].rows();
        assert_eq!(rows.len(), 5, "one row per line of the taller side");
        assert_eq!(
            rows[1].old.as_ref().map(|line| line.text.as_str()),
            Some("let one = 1;")
        );
        assert_eq!(
            rows[1].new.as_ref().map(|line| line.text.as_str()),
            Some("let one = 2;")
        );
        assert!(
            rows[2].old.is_none(),
            "the second addition has nothing to pair with"
        );
        assert!(!rows[0].is_change());
        assert!(rows[1].is_change());
    }

    #[test]
    fn a_deletion_alone_keeps_its_side() {
        let rows = Diff::parse(SAMPLE).hunks[1].rows();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[1].old.as_ref().map(|line| line.text.as_str()),
            Some("gone")
        );
        assert!(rows[1].new.is_none());
    }

    #[test]
    fn a_binary_file_says_so_instead_of_showing_nothing() {
        let diff = Diff::parse(
            "diff --git a/logo.png b/logo.png\nBinary files a/logo.png and b/logo.png differ\n",
        );
        assert!(diff.is_empty());
        assert_eq!(diff.note.as_deref(), Some("This is a binary file."));
    }

    #[test]
    fn a_second_file_in_the_output_is_not_read() {
        let text = format!("{SAMPLE}diff --git a/other b/other\n@@ -1 +1 @@\n-a\n+b\n");
        assert_eq!(Diff::parse(&text).hunks.len(), 2);
    }

    #[test]
    fn a_word_that_changed_is_marked_and_the_rest_is_not() {
        let (before, after) = spans("let one = 1;", "let one = 2;");
        assert_eq!(
            before.iter().filter(|span| span.changed).count(),
            1,
            "only the number differs"
        );
        assert_eq!(
            before
                .iter()
                .map(|span| span.text.as_str())
                .collect::<String>(),
            "let one = 1;",
            "the line comes back whole"
        );
        let changed: String = after
            .iter()
            .filter(|span| span.changed)
            .map(|span| span.text.as_str())
            .collect();
        assert_eq!(changed, "2;", "the number and what follows it on its token");
    }

    #[test]
    fn two_lines_with_nothing_in_common_are_marked_whole() {
        let (before, after) = spans("alpha beta gamma", "wholly different text here");
        assert_eq!(before.len(), 1);
        assert!(before[0].changed);
        assert_eq!(after.len(), 1);
        assert!(after[0].changed);
    }
}
