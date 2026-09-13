//! Regression cover for <https://github.com/Epistates/turbovault/issues/71>.
//!
//! A blockquote is rebuilt by re-parsing its raw text, so everything inside one
//! has to be written back into that buffer as markdown. Fenced blocks and
//! inline elements already were. Lists were not: they went to the top-level
//! block list instead, which put an item-less list ahead of the quote and left
//! the items' text bare in the quote's content, with no markers and no line
//! breaks between them.
//!
//! The reported symptom was worse than a loss. A fence following a list ran
//! onto the last item's line, so the whole run re-parsed as one paragraph and
//! the code block vanished, while a different paragraph elsewhere in the quote
//! came back reported as source code.

use turbovault_parser::{ContentBlock, parse_blocks};

/// Every code block reachable from a set of blocks, including inside quotes
/// and list items.
fn collect_code(blocks: &[ContentBlock]) -> Vec<(Option<String>, String)> {
    fn walk(blocks: &[ContentBlock], out: &mut Vec<(Option<String>, String)>) {
        for block in blocks {
            match block {
                ContentBlock::Code {
                    language, content, ..
                } => out.push((language.clone(), content.clone())),
                ContentBlock::Blockquote { blocks, .. } => walk(blocks, out),
                ContentBlock::List { items, .. } => {
                    for item in items {
                        walk(&item.blocks, out);
                    }
                }
                _ => {}
            }
        }
    }

    let mut out = Vec::new();
    walk(blocks, &mut out);
    out
}

fn first_blockquote(blocks: &[ContentBlock]) -> (&str, &[ContentBlock]) {
    blocks
        .iter()
        .find_map(|b| match b {
            ContentBlock::Blockquote { content, blocks } => Some((content.as_str(), &blocks[..])),
            _ => None,
        })
        .expect("expected a blockquote")
}

/// The list inside the first blockquote, as `(ordered, items)` where each item
/// is `(content, checked)`.
fn quoted_list(markdown: &str) -> (bool, Vec<(String, Option<bool>)>) {
    let blocks = parse_blocks(markdown);
    let (_, nested) = first_blockquote(&blocks);
    nested
        .iter()
        .find_map(|b| match b {
            ContentBlock::List { ordered, items } => Some((
                *ordered,
                items
                    .iter()
                    .map(|i| (i.content.clone(), i.checked))
                    .collect(),
            )),
            _ => None,
        })
        .expect("expected a list inside the blockquote")
}

// ---------------------------------------------------------------------------
// The reported case
// ---------------------------------------------------------------------------

/// The shape from the report. A list immediately before the fence was the
/// discriminator: the fence was appended with no separator, so it stopped
/// being a fence at all.
#[test]
fn a_fence_after_a_list_in_a_quote_is_still_a_code_block() {
    let blocks = parse_blocks("> - bullet\n>\n> ```rust\n> A();\n> ```\n");
    assert_eq!(
        collect_code(&blocks),
        vec![(Some("rust".to_string()), "A();".to_string())]
    );
}

#[test]
fn a_fence_after_an_ordered_list_in_a_quote_is_still_a_code_block() {
    let blocks = parse_blocks("> 1. bullet\n>\n> ```rust\n> A();\n> ```\n");
    assert_eq!(
        collect_code(&blocks),
        vec![(Some("rust".to_string()), "A();".to_string())]
    );
}

/// The rest of the report's table. These already worked, and are here so a
/// future change to the quote buffer cannot fix one row by breaking another.
#[test]
fn every_ordering_of_a_list_and_a_fence_in_a_quote_reports_the_code() {
    for markdown in [
        "> para\n>\n> ```rust\n> A();\n> ```\n",
        "> ```rust\n> A();\n> ```\n>\n> - bullet\n",
        "> - bullet\n>\n> para\n>\n> ```rust\n> A();\n> ```\n",
        "> - bullet\n>\n> ```rust\n> A();\n> ```\n",
        "> 1. bullet\n>\n> ```rust\n> A();\n> ```\n",
    ] {
        assert_eq!(
            collect_code(&parse_blocks(markdown)),
            vec![(Some("rust".to_string()), "A();".to_string())],
            "no code block for {markdown:?}"
        );
    }
}

/// The failure that made this worth fixing promptly. Losing a block would be
/// detectable downstream; reporting a paragraph as source code is not.
///
/// Two fences is what it takes. The first one runs onto the item's line and
/// goes inert, which leaves its *closing* fence sitting at the start of a line
/// with nothing open, so that one opens a block instead and swallows the prose
/// until the next fence closes it. The release-notes shape, list then example
/// then commentary then example, hits this directly.
#[test]
fn prose_between_two_fences_after_a_list_is_never_reported_as_code() {
    let blocks = parse_blocks(
        "> - bullet\n>\n> ```\n> A();\n> ```\n>\n> Results in:\n>\n> ```\n> B();\n> ```\n",
    );
    let code = collect_code(&blocks);
    assert!(
        !code
            .iter()
            .any(|(_, content)| content.contains("Results in:")),
        "prose came back as a code block: {code:?}"
    );
    assert_eq!(
        code,
        vec![(None, "A();".to_string()), (None, "B();".to_string())]
    );
}

// ---------------------------------------------------------------------------
// The list itself
// ---------------------------------------------------------------------------

/// The root cause, independent of any fence. Two items came back as the single
/// run "onetwo", and the list block that escaped to the top level had one empty
/// item in it.
#[test]
fn a_list_in_a_quote_keeps_its_items() {
    let (ordered, items) = quoted_list("> - one\n> - two\n");
    assert!(!ordered);
    assert_eq!(
        items,
        vec![("one".to_string(), None), ("two".to_string(), None)]
    );
}

#[test]
fn a_list_in_a_quote_does_not_escape_to_the_top_level() {
    let blocks = parse_blocks("> - one\n> - two\n");
    assert_eq!(
        blocks.len(),
        1,
        "expected only the blockquote, got {blocks:?}"
    );
    assert!(matches!(blocks[0], ContentBlock::Blockquote { .. }));
}

#[test]
fn a_task_list_in_a_quote_keeps_its_checkboxes() {
    let (_, items) = quoted_list("> - [x] done\n> - [ ] todo\n");
    assert_eq!(
        items,
        vec![
            ("done".to_string(), Some(true)),
            ("todo".to_string(), Some(false)),
        ]
    );
}

#[test]
fn an_ordered_list_in_a_quote_keeps_its_numbering() {
    let (ordered, items) = quoted_list("> 5. five\n> 6. six\n");
    assert!(ordered);
    assert_eq!(
        items,
        vec![("five".to_string(), None), ("six".to_string(), None)]
    );
}

/// A nested list has to indent to its parent's content column, which is three
/// characters under `1. ` and two under `- `. A fixed indent would leave the
/// nested items as siblings under the wider marker.
#[test]
fn a_nested_list_in_a_quote_stays_nested() {
    for markdown in [
        "> - one\n>   - deep\n> - two\n",
        "> 1. one\n>    - deep\n> 2. two\n",
    ] {
        let (_, items) = quoted_list(markdown);
        assert_eq!(items.len(), 2, "nesting flattened for {markdown:?}");
        assert!(
            items[0].0.contains("deep"),
            "nested item lost for {markdown:?}: {items:?}"
        );
    }
}

/// A callout is a blockquote whose first line is the marker, which is the shape
/// the report found this in.
#[test]
fn a_callout_keeps_a_list_and_the_fence_after_it() {
    let blocks = parse_blocks("> [!note] Title\n> - bullet\n>\n> ```rust\n> A();\n> ```\n");
    let (_, nested) = first_blockquote(&blocks);
    assert!(
        nested
            .iter()
            .any(|b| matches!(b, ContentBlock::List { .. })),
        "callout lost its list: {nested:?}"
    );
    assert_eq!(
        collect_code(&blocks),
        vec![(Some("rust".to_string()), "A();".to_string())]
    );
}

// ---------------------------------------------------------------------------
// Quote state
// ---------------------------------------------------------------------------

/// The quote's open flag was only cleared when it had text to emit, so a quote
/// that produced none never closed and every block after it was pulled inside.
#[test]
fn an_empty_quote_does_not_swallow_the_document_after_it() {
    let blocks = parse_blocks("> \n\nafter the quote\n");
    assert!(
        !blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Blockquote { .. })),
        "the text after an empty quote was pulled into one: {blocks:?}"
    );
    assert!(
        matches!(&blocks[..], [ContentBlock::Paragraph { content, .. }] if content == "after the quote"),
        "expected one paragraph, got {blocks:?}"
    );
}
