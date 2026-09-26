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

use turbovault_parser::{ContentBlock, InlineElement, parse_blocks};

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
///
/// <https://github.com/Epistates/turbovault/issues/79> tracked this the other
/// way too: the parser used to recognise the nesting but have nowhere to put
/// it, so it flattened the nested item's marker and text into the outer
/// item's own `content` instead of giving it a nested list block.
#[test]
fn a_nested_list_in_a_quote_stays_nested() {
    for markdown in [
        "> - one\n>   - deep\n> - two\n",
        "> 1. one\n>    - deep\n> 2. two\n",
    ] {
        let (_, items) = quoted_list(markdown);
        assert_eq!(items.len(), 2, "nesting flattened for {markdown:?}");
        assert_eq!(
            items[0].0, "one",
            "the nested item leaked into the outer item's own text for {markdown:?}"
        );

        let (content, blocks) = first_quoted_item(markdown);
        assert_eq!(content, "one");
        let [ContentBlock::List { items: nested, .. }] = &blocks[..] else {
            panic!("expected a nested list block for {markdown:?}, got {blocks:?}");
        };
        assert_eq!(nested.len(), 1);
        assert_eq!(
            nested[0].content, "deep",
            "nested item lost for {markdown:?}: {nested:?}"
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

// ---------------------------------------------------------------------------
// Continuation blocks inside a quoted list item
// <https://github.com/Epistates/turbovault/issues/77>
// ---------------------------------------------------------------------------
//
// An item's second block is written to the quote buffer at the point the first
// one stopped, so it needs both the line it was written on and the item's own
// content column. It was getting neither: the continuation ran straight onto
// the item's text with no separator at all, and a fence landed back at column
// zero, which closed the list and made the fence the list's sibling.

/// The item that owns a continuation paragraph, as `(item content, item
/// blocks)`, from the first list in the first blockquote.
fn first_quoted_item(markdown: &str) -> (String, Vec<ContentBlock>) {
    let blocks = parse_blocks(markdown);
    let (_, nested) = first_blockquote(&blocks);
    let list = nested
        .iter()
        .find_map(|b| match b {
            ContentBlock::List { items, .. } => Some(items),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a list inside the quote, got {nested:?}"));
    let item = list.first().expect("expected at least one item");
    (item.content.clone(), item.blocks.clone())
}

/// The reported case. The blank line makes the list loose and the indent makes
/// the text a second block of the item, and neither survived the round trip.
#[test]
fn a_quoted_item_keeps_its_continuation_paragraph_separate() {
    let (content, blocks) = first_quoted_item("> - step\n>\n>   text\n");
    assert_eq!(content, "step", "the continuation ran onto the item's text");
    assert!(
        matches!(&blocks[..], [ContentBlock::Paragraph { content, .. }] if content == "text"),
        "expected the continuation as a nested paragraph, got {blocks:?}"
    );
}

/// An ordered item's content column is wider than a bullet's, so the indent has
/// to come from the marker that was actually written.
#[test]
fn a_quoted_ordered_item_keeps_its_continuation_paragraph_separate() {
    let (content, blocks) = first_quoted_item("> 1. step\n>\n>    text\n");
    assert_eq!(content, "step", "the continuation ran onto the item's text");
    assert!(
        matches!(&blocks[..], [ContentBlock::Paragraph { content, .. }] if content == "text"),
        "expected the continuation as a nested paragraph, got {blocks:?}"
    );
}

/// A nested item's content column is its parent's plus its own marker, so a
/// continuation there is the case that catches an indent computed from depth
/// rather than from the buffer it is actually being written into.
///
/// Asserted against the same input parsed outside a quote rather than against
/// a literal, so a future change to either path cannot drift the other one
/// without this test noticing. [#79] was exactly that kind of drift: the
/// continuation belongs to `inner`, the innermost item enclosing it, but the
/// parser had only one blocks buffer shared by every depth, so it landed on
/// `outer` instead, in and out of a quote alike.
///
/// [#79]: https://github.com/Epistates/turbovault/issues/79
#[test]
fn a_nested_quoted_item_keeps_its_continuation_paragraph_separate() {
    let quoted = parse_blocks("> - outer\n>   - inner\n>\n>     text\n");
    let (_, nested) = first_blockquote(&quoted);
    assert_eq!(
        nested,
        parse_blocks("- outer\n  - inner\n\n    text\n"),
        "the quote reported a different shape than the same list outside one"
    );
    let ContentBlock::List { items, .. } = &nested[0] else {
        panic!("expected a list inside the quote, got {nested:?}");
    };
    assert_eq!(items[0].content, "outer");
    let ContentBlock::List { items: inner, .. } = &items[0].blocks[0] else {
        panic!(
            "expected a nested list inside the outer item, got {:?}",
            items[0].blocks
        );
    };
    assert_eq!(inner[0].content, "inner");
    assert_eq!(
        inner[0].blocks,
        vec![ContentBlock::Paragraph {
            content: "text".to_string(),
            inline: vec![InlineElement::Text {
                value: "text".to_string()
            }],
        }],
        "expected the continuation as the nested item's own block, got {:?}",
        inner[0].blocks
    );
}

/// The same shape with a fence, which is what a quoted step-with-a-command
/// looks like. Written at column zero it ends the list, so the fence came back
/// as the list's sibling instead of the item's own block.
#[test]
fn a_quoted_item_keeps_a_fenced_block_inside_it() {
    let (content, blocks) = first_quoted_item("> - step\n>\n>   ```sh\n>   run()\n>   ```\n");
    assert_eq!(content, "step");
    assert!(
        matches!(
            &blocks[..],
            [ContentBlock::Code { language, content, .. }]
                if language.as_deref() == Some("sh") && content == "run()"
        ),
        "expected the fence inside the item, got {blocks:?}"
    );
}

/// A quoted fence still reports the document line it was written on, which is
/// the guarantee #76 added and the indent must not disturb.
#[test]
fn a_fence_inside_a_quoted_item_still_reports_its_own_line() {
    let blocks = parse_blocks("intro\n\n> - step\n>\n>   ```sh\n>   run()\n>   ```\n");
    let (_, nested) = first_blockquote(&blocks);
    let ContentBlock::List { items, .. } = &nested[0] else {
        panic!("expected a list inside the quote, got {nested:?}");
    };
    assert!(
        matches!(
            &items[0].blocks[..],
            [ContentBlock::Code {
                start_line: 5,
                end_line: 7,
                ..
            }]
        ),
        "expected the fence on lines 5 to 7, got {:?}",
        items[0].blocks
    );
}

/// An unindented paragraph after a quoted list is a sibling of the list, not a
/// continuation of its last item, and it was already right. It has to stay
/// right once continuations start indenting themselves.
#[test]
fn an_unindented_paragraph_after_a_quoted_list_stays_a_sibling() {
    let blocks = parse_blocks("> - step\n>\n> text\n");
    let (_, nested) = first_blockquote(&blocks);
    assert!(
        matches!(
            nested,
            [ContentBlock::List { .. }, ContentBlock::Paragraph { content, .. }] if content == "text"
        ),
        "expected a list then a paragraph, got {nested:?}"
    );
}
