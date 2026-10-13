//! Property tests for the note parser: whatever the input, parsing returns
//! rather than panicking, and every position it reports is a span of the
//! note on character boundaries. The link rewriter and the outline tools
//! slice notes at exactly these offsets, so an off-by-one here is a panic or
//! a corrupted note there.

use proptest::prelude::*;
use std::path::{Path, PathBuf};
use turbovault_parser::{Parser, parse_blocks, parse_reference_definitions, to_plain_text};

/// Text made of the pieces Markdown and Obsidian syntax are built from, so
/// generated notes hit link, embed, code, quote, list, table and frontmatter
/// paths far more often than uniformly random text would.
fn markdownish() -> impl Strategy<Value = String> {
    let piece = prop_oneof![
        Just("[[".to_string()),
        Just("]]".to_string()),
        Just("![[".to_string()),
        Just("|".to_string()),
        Just("\\|".to_string()),
        Just("#".to_string()),
        Just("#^".to_string()),
        Just("[".to_string()),
        Just("]".to_string()),
        Just("](".to_string()),
        Just(")".to_string()),
        Just("<".to_string()),
        Just(">".to_string()),
        Just("![".to_string()),
        Just("]: ".to_string()),
        Just("[]".to_string()),
        Just("`".to_string()),
        Just("```\n".to_string()),
        Just("---\n".to_string()),
        Just("\n".to_string()),
        Just("\n\n".to_string()),
        Just("> ".to_string()),
        Just("> [!note] ".to_string()),
        Just("- ".to_string()),
        Just("- [ ] ".to_string()),
        Just("    ".to_string()),
        Just("%%".to_string()),
        Just("%20".to_string()),
        Just(".md".to_string()),
        Just("tags: [a, b]\n".to_string()),
        "[a-zA-Z0-9 _/.-]{1,8}",
        "[éßİ日本😀\u{301}]{1,3}",
    ];
    prop::collection::vec(piece, 0..60).prop_map(|pieces| pieces.concat())
}

fn assert_span(text: &str, offset: usize, length: usize, what: &str) {
    assert!(
        text.get(offset..offset + length).is_some(),
        "{what} span {offset}..{} is not a span of the {}-byte note {text:?}",
        offset + length,
        text.len()
    );
}

fn check_note(text: &str) {
    let parser = Parser::new(PathBuf::from("/vault"));
    let file = parser
        .parse_file(Path::new("/vault/note.md"), text)
        .expect("parse_file does not fail on text");
    for link in &file.links {
        assert_span(text, link.position.offset, link.position.length, "link");
    }
    for heading in &file.headings {
        assert_span(text, heading.position.offset, heading.position.length, "heading");
    }
    for tag in &file.tags {
        assert_span(text, tag.position.offset, tag.position.length, "tag");
    }
    for definition in parse_reference_definitions(text) {
        assert!(
            text.get(definition.destination.clone()).is_some(),
            "definition destination {:?} is not a span of {text:?}",
            definition.destination
        );
    }
    let _ = parse_blocks(text);
    let _ = to_plain_text(text);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn parsing_markdownish_text_reports_valid_spans(text in markdownish()) {
        check_note(&text);
    }

    #[test]
    fn parsing_arbitrary_text_reports_valid_spans(text in any::<String>()) {
        check_note(&text);
    }

    #[test]
    fn a_frontmatter_block_parses_with_its_body(
        body in markdownish(),
        key in "[a-z]{1,6}",
        value in markdownish(),
    ) {
        let text = format!("---\n{key}: {value:?}\n---\n{body}");
        check_note(&text);
    }
}
