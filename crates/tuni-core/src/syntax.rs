//! What a parse tree can say about a selection.
//!
//! One question so far: given what is selected, what is the next thing worth
//! selecting? The answer is the smallest named node that strictly contains the
//! selection, which is how a cursor inside a string grows to the string, then
//! the call around it, then the statement, on up to the file. A regex cannot
//! answer that and a language server round-trip is too slow for a key that is
//! held down; a tree-sitter parse of the whole buffer is neither, at well
//! under a millisecond for the files the editor will open.
//!
//! Offsets cross this boundary in characters, because that is what GTK buffers
//! count, and tree-sitter counts bytes; the conversion happens here, next to
//! the tests that keep it honest.

use tree_sitter::Parser;

/// The grammar for one of the editor's language ids, which are the LSP ids in
/// [`crate::lsp::LANGUAGES`]. `None` is a language nobody compiled a grammar
/// for, and means the selection stays as it is rather than an error.
fn grammar(language: &str) -> Option<tree_sitter::Language> {
    Some(match language {
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "c" => tree_sitter_c::LANGUAGE.into(),
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        // The JavaScript grammar carries JSX, which is all "react" adds.
        "javascript" | "javascriptreact" => tree_sitter_javascript::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "typescriptreact" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "shellscript" => tree_sitter_bash::LANGUAGE.into(),
        _ => return None,
    })
}

/// The next selection out from the given one: the smallest named node strictly
/// containing `start..end`, in character offsets. A caret is an empty range
/// and grows to the token under it first.
#[must_use]
pub fn grow_selection(
    language: &str,
    text: &str,
    start: usize,
    end: usize,
) -> Option<(usize, usize)> {
    let mut parser = Parser::new();
    parser.set_language(&grammar(language)?).ok()?;
    let tree = parser.parse(text, None)?;

    let from = byte_of_char(text, start);
    let to = byte_of_char(text, end);
    let mut node = tree.root_node().named_descendant_for_byte_range(from, to)?;
    loop {
        // Strictly larger, not merely covering: the node equal to the current
        // selection is the selection, and returning it would make the key do
        // nothing forever.
        if node.start_byte() < from || node.end_byte() > to {
            return Some((
                char_of_byte(text, node.start_byte()),
                char_of_byte(text, node.end_byte()),
            ));
        }
        node = node.parent()?;
    }
}

fn byte_of_char(text: &str, character: usize) -> usize {
    text.char_indices()
        .nth(character)
        .map_or(text.len(), |(byte, _)| byte)
}

fn char_of_byte(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUST: &str = "fn main() {\n    let x = (1 + 2);\n}\n";

    fn slice(text: &str, range: (usize, usize)) -> String {
        text.chars().skip(range.0).take(range.1 - range.0).collect()
    }

    #[test]
    fn a_caret_grows_outward_one_enclosing_node_at_a_time() {
        // The caret sits inside `1`, at the character just after `(`.
        let inside = RUST.find('1').unwrap();
        let first = grow_selection("rust", RUST, inside, inside).unwrap();
        assert_eq!(slice(RUST, first), "1");
        let second = grow_selection("rust", RUST, first.0, first.1).unwrap();
        assert_eq!(slice(RUST, second), "1 + 2");
        let third = grow_selection("rust", RUST, second.0, second.1).unwrap();
        assert_eq!(slice(RUST, third), "(1 + 2)");
        let fourth = grow_selection("rust", RUST, third.0, third.1).unwrap();
        assert_eq!(slice(RUST, fourth), "let x = (1 + 2);");
    }

    #[test]
    fn the_whole_file_grows_no_further() {
        let all = RUST.chars().count();
        assert_eq!(grow_selection("rust", RUST, 0, all), None);
    }

    #[test]
    fn a_language_without_a_grammar_grows_nothing() {
        assert_eq!(grow_selection("zig", RUST, 0, 0), None);
        assert_eq!(grow_selection("", RUST, 0, 0), None);
    }

    #[test]
    fn offsets_count_characters_even_past_a_multibyte_name() {
        // Every offset before the parens crosses "żółw", two bytes a letter;
        // a byte-counting caller would land inside the identifier and select
        // garbage.
        let text = "fn żółw() { print(1); }";
        let inside = text.chars().position(|c| c == '1').unwrap();
        let first = grow_selection("rust", text, inside, inside).unwrap();
        assert_eq!(slice(text, first), "1");
        let second = grow_selection("rust", text, first.0, first.1).unwrap();
        assert_eq!(slice(text, second), "(1)");
    }

    #[test]
    fn every_shipped_grammar_parses_a_line_of_its_language() {
        // The grammar and the runtime carry an ABI version each; this is the
        // test that fails when an update splits them.
        for (language, line) in [
            ("rust", "fn a() {}"),
            ("c", "int a(void) { return 0; }"),
            ("cpp", "int a() { return 0; }"),
            ("python", "def a():\n    return 1\n"),
            ("go", "package a\nfunc b() {}\n"),
            ("javascript", "function a() { return 1; }"),
            ("javascriptreact", "const a = <b c={1} />;"),
            ("typescript", "function a(b: number): number { return b; }"),
            ("typescriptreact", "const a = <b c={1} />;"),
            ("shellscript", "a() { echo hi; }"),
        ] {
            let one = line.find('1').or_else(|| line.find("hi")).unwrap_or(1);
            let grown = grow_selection(language, line, one, one);
            assert!(grown.is_some(), "{language} did not parse");
        }
    }
}
