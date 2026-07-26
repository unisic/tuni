//! Ranking for the command palette.
//!
//! A subsequence match, scored so that the ways a person actually types a
//! command name win: initials of words, then a run of letters that follow one
//! another, then anything else. kero scores its palette the same way, and the
//! numbers here are its numbers, so the same query picks the same command.

/// How well `pattern` matches `candidate`, or `None` if it does not match at
/// all.
///
/// Both are folded to lowercase, so a query is never case-sensitive. Every
/// character of the pattern has to appear in the candidate, in order, but not
/// next to each other: `sp` finds "Split Right".
#[must_use]
pub fn score(candidate: &str, pattern: &str) -> Option<i32> {
    let text: Vec<char> = candidate.to_lowercase().chars().collect();
    let mut score = 0;
    let mut index = 0usize;
    // Where the previous character of the pattern matched, so a run of letters
    // that follow one another can be told from letters scattered about.
    let mut last: Option<usize> = None;

    for wanted in pattern.to_lowercase().chars() {
        let mut found = false;
        while index < text.len() {
            if text[index] == wanted {
                score += if index == 0 || is_boundary(text[index - 1]) {
                    // The start of a word: what someone typing initials means.
                    10
                } else if last == Some(index - 1) {
                    5
                } else {
                    1
                };
                last = Some(index);
                index += 1;
                found = true;
                break;
            }
            index += 1;
        }
        if !found {
            return None;
        }
    }
    Some(score)
}

/// Whether a character ends a word, so the next one starts one. kero only
/// counts spaces; a palette entry here can carry a path or a hyphenated name,
/// and the first letter after either reads as the start of a word too.
fn is_boundary(c: char) -> bool {
    c.is_whitespace() || matches!(c, '-' | '_' | '/' | '.' | ':')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_pattern_matches_anything() {
        assert_eq!(score("Split Right", ""), Some(0));
    }

    #[test]
    fn a_character_that_is_not_there_does_not_match() {
        assert_eq!(score("Split Right", "z"), None);
    }

    #[test]
    fn the_pattern_has_to_appear_in_order() {
        assert!(score("Split Right", "sr").is_some());
        assert_eq!(score("Split Right", "rs"), None);
    }

    #[test]
    fn initials_beat_letters_from_the_middle() {
        let initials = score("Split Right", "sr").expect("initials match");
        let middle = score("Split Right", "pi").expect("middle match");
        assert!(initials > middle, "{initials} should beat {middle}");
    }

    #[test]
    fn a_run_beats_the_same_letters_scattered() {
        let run = score("abcxx", "abc").expect("run match");
        let scattered = score("axbxc", "abc").expect("scattered match");
        assert!(run > scattered, "{run} should beat {scattered}");
    }

    #[test]
    fn a_word_after_a_separator_starts_a_word() {
        // "up" in "~/src/upstream" is at the start of a path segment, which is
        // where someone typing a directory name expects the match to count.
        let boundary = score("~/src/upstream", "up").expect("boundary match");
        let inner = score("~/src/cupboard", "up").expect("inner match");
        assert!(boundary > inner, "{boundary} should beat {inner}");
    }

    #[test]
    fn case_is_ignored_on_both_sides() {
        assert_eq!(score("Split Right", "SPLIT"), score("split right", "split"));
    }
}
