//! Regression cover for <https://github.com/Epistates/turbovault/issues/79>.
//!
//! A nested list had no structure of its own. `item_depth` counted how deep
//! the parser was inside pulldown-cmark's own events, but every item still
//! landed in the single top-level item buffer: a nested item's marker and
//! text were hand-flattened into indented text and appended to the *parent*
//! item's `content`, with no `ContentBlock::List` to hold it. That lost three
//! things at once: the nested marker (bullet and ordered were both just
//! indentation, so they read the same), the source indent (always rewritten
//! to two spaces a level regardless of what the author wrote), and a nested
//! item's own continuation paragraph, which landed in the *outer* item's
//! blocks because there was only one blocks buffer to put it in.
//!
//! `- [ ] outer` / `  - [x] inner` used to come back as one item:
//! `ListItem { checked: Some(false), content: "outer\n  [x] inner", blocks: [] }`.
//! It now comes back as an outer item whose `blocks` holds a nested
//! `ContentBlock::List` with its own item, checkbox and all.

use turbovault_parser::{ContentBlock, InlineElement, ListItem, parse_blocks};

/// The single top-level list in `markdown`, as `(ordered, items)`.
fn top_list(markdown: &str) -> (bool, Vec<ListItem>) {
    let blocks = parse_blocks(markdown);
    match blocks.as_slice() {
        [ContentBlock::List { ordered, items }] => (*ordered, items.clone()),
        other => panic!("expected a single top-level list, got {other:?}"),
    }
}

/// The nested list inside `item`'s own blocks, as `(ordered, items)`.
fn nested_list(item: &ListItem) -> (bool, Vec<ListItem>) {
    match item.blocks.as_slice() {
        [ContentBlock::List { ordered, items }] => (*ordered, items.clone()),
        other => panic!(
            "expected item {:?} to hold a single nested list, got {other:?}",
            item.content
        ),
    }
}

/// The blockquote's own nested blocks.
fn quoted_blocks(markdown: &str) -> Vec<ContentBlock> {
    let blocks = parse_blocks(markdown);
    match blocks.as_slice() {
        [ContentBlock::Blockquote { blocks, .. }] => blocks.clone(),
        other => panic!("expected a single blockquote, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Control
// ---------------------------------------------------------------------------

/// A flat list has no nesting to lose, so it has to come back exactly as it
/// always has. Any change here means the fix touched more than nesting.
#[test]
fn a_flat_list_is_unchanged() {
    let (ordered, items) = top_list("- one\n- two\n- three\n");
    assert!(!ordered);
    assert_eq!(items.len(), 3);
    for (item, expected) in items.iter().zip(["one", "two", "three"]) {
        assert_eq!(item.content, expected);
        assert!(
            item.blocks.is_empty(),
            "a flat item picked up blocks: {item:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The issue's own example
// ---------------------------------------------------------------------------

/// The literal shape from the report. The outer item's `content` must not
/// contain the inner item's text or marker at all: it belongs in a nested
/// list, not folded into the parent's own words.
#[test]
fn a_nested_bullet_under_a_bullet_becomes_a_nested_list_block() {
    let (ordered, items) = top_list("- [ ] outer\n  - [x] inner\n");
    assert!(!ordered);
    assert_eq!(items.len(), 1);

    let outer = &items[0];
    assert_eq!(outer.checked, Some(false));
    assert_eq!(
        outer.content, "outer",
        "the inner item leaked into the outer item's own text"
    );

    let (inner_ordered, inner_items) = nested_list(outer);
    assert!(!inner_ordered, "a nested bullet list reported ordered");
    assert_eq!(inner_items.len(), 1);
    assert_eq!(inner_items[0].checked, Some(true));
    assert_eq!(inner_items[0].content, "inner");
    assert!(
        inner_items[0].blocks.is_empty(),
        "the innermost item picked up blocks: {inner_items:?}"
    );
}

// ---------------------------------------------------------------------------
// Marker kind and indent width
// ---------------------------------------------------------------------------

/// An ordered nested list needs its marker's own content column, three spaces
/// under `1. `, not the two-space width a bullet would use. Getting the
/// indent wrong is exactly what used to make every nesting depth read as flat
/// text at whatever column the flattening code invented.
#[test]
fn an_ordered_list_nested_under_an_ordered_list_keeps_its_own_numbering() {
    let (ordered, items) = top_list("1. outer\n   1. inner\n");
    assert!(ordered);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].content, "outer");

    let (inner_ordered, inner_items) = nested_list(&items[0]);
    assert!(inner_ordered, "a nested ordered list reported unordered");
    assert_eq!(inner_items.len(), 1);
    assert_eq!(
        inner_items[0].content, "inner",
        "the three-space indent was not recognised as nesting"
    );
}

/// A nested list's marker kind is independent of its parent's. Flattening
/// used to erase this distinction entirely: an inner bullet and an inner
/// ordered item both just became indented text, indistinguishable from each
/// other.
#[test]
fn a_nested_list_can_switch_marker_kind_from_its_parent() {
    let (outer_ordered, items) = top_list("- outer\n  1. inner\n");
    assert!(!outer_ordered);
    assert_eq!(items[0].content, "outer");
    let (inner_ordered, inner_items) = nested_list(&items[0]);
    assert!(
        inner_ordered,
        "a nested ordered list under a bullet reported unordered"
    );
    assert_eq!(inner_items[0].content, "inner");

    let (outer_ordered, items) = top_list("1. outer\n   - inner\n");
    assert!(outer_ordered);
    assert_eq!(items[0].content, "outer");
    let (inner_ordered, inner_items) = nested_list(&items[0]);
    assert!(
        !inner_ordered,
        "a nested bullet list under an ordered list reported ordered"
    );
    assert_eq!(inner_items[0].content, "inner");
}

// ---------------------------------------------------------------------------
// Depth
// ---------------------------------------------------------------------------

/// Three levels deep, so a fix that only special-cases "one level of nesting"
/// shows up here instead of quietly passing the report's own two-level case.
#[test]
fn three_levels_of_nesting_are_all_reachable_as_structure() {
    let (_, items) = top_list("- level one\n  - level two\n    - level three\n");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].content, "level one");

    let (_, level_two_items) = nested_list(&items[0]);
    assert_eq!(level_two_items.len(), 1);
    assert_eq!(level_two_items[0].content, "level two");

    let (_, level_three_items) = nested_list(&level_two_items[0]);
    assert_eq!(level_three_items.len(), 1);
    assert_eq!(level_three_items[0].content, "level three");
    assert!(level_three_items[0].blocks.is_empty());
}

// ---------------------------------------------------------------------------
// A nested item's own continuation paragraph
// ---------------------------------------------------------------------------

/// The third thing the report named. The blank line makes the list loose and
/// the four-space indent is the *nested* item's own content column (two for
/// the outer marker, two for the inner), so `text` belongs to `inner`, not to
/// `outer`. The flattening code had only one blocks buffer, shared by every
/// depth, so this used to land on `outer` instead.
#[test]
fn a_nested_items_continuation_paragraph_lands_in_the_nested_items_blocks() {
    let (_, items) = top_list("- outer\n  - inner\n\n    text\n");
    let outer = &items[0];
    assert_eq!(outer.content, "outer");

    let (_, inner_items) = nested_list(outer);
    let inner = &inner_items[0];
    assert_eq!(inner.content, "inner");
    assert!(
        !inner.content.contains("text"),
        "the continuation ran onto the nested item's own text: {:?}",
        inner.content
    );
    assert_eq!(
        inner.blocks,
        vec![ContentBlock::Paragraph {
            content: "text".to_string(),
            inline: vec![InlineElement::Text {
                value: "text".to_string()
            }],
        }],
        "expected the continuation as the nested item's own block, got {:?}",
        inner.blocks
    );
    assert!(
        outer.blocks.iter().all(|b| !matches!(
            b,
            ContentBlock::Paragraph { content, .. } if content == "text"
        )),
        "the continuation also landed on the outer item"
    );
}

// ---------------------------------------------------------------------------
// Real source lines
// ---------------------------------------------------------------------------

/// A code block nested inside a nested item still has to report the document
/// lines its fence actually sits on, not a count relative to whichever buffer
/// it happened to land in. This is the same `current_line`/`current_end_line`
/// span tracking every other block uses; nesting must not disturb it.
#[test]
fn a_code_block_nested_inside_a_nested_item_reports_real_lines() {
    let markdown = "- outer\n  - inner\n    ```rust\n    code()\n    ```\n";
    let (_, items) = top_list(markdown);
    let (_, inner_items) = nested_list(&items[0]);
    let inner = &inner_items[0];
    assert_eq!(inner.content, "inner");
    assert_eq!(
        inner.blocks,
        vec![ContentBlock::Code {
            language: Some("rust".to_string()),
            content: "code()".to_string(),
            start_line: 3,
            end_line: 5,
        }]
    );
}

// ---------------------------------------------------------------------------
// Inside a blockquote
// ---------------------------------------------------------------------------
//
// A quote is rebuilt by re-parsing its raw text (see
// tests/test_lists_in_blockquotes.rs), so once the underlying list logic
// nests correctly, a quoted nested list gets the fix for free through that
// re-parse. These pin that it actually does, rather than assuming it.

#[test]
fn a_nested_bullet_in_a_quote_becomes_a_nested_list_block() {
    let blocks = quoted_blocks("> - outer\n>   - inner\n");
    let [ContentBlock::List { ordered, items }] = blocks.as_slice() else {
        panic!("expected a single list inside the quote, got {blocks:?}");
    };
    assert!(!ordered);
    assert_eq!(items[0].content, "outer");
    let (inner_ordered, inner_items) = nested_list(&items[0]);
    assert!(!inner_ordered);
    assert_eq!(inner_items[0].content, "inner");
}

#[test]
fn a_nested_ordered_list_in_a_quote_keeps_its_own_numbering() {
    let blocks = quoted_blocks("> 1. outer\n>    1. inner\n");
    let [ContentBlock::List { ordered, items }] = blocks.as_slice() else {
        panic!("expected a single list inside the quote, got {blocks:?}");
    };
    assert!(ordered);
    let (inner_ordered, inner_items) = nested_list(&items[0]);
    assert!(
        inner_ordered,
        "a nested ordered list in a quote lost its kind"
    );
    assert_eq!(inner_items[0].content, "inner");
}

#[test]
fn a_nested_task_checkbox_in_a_quote_keeps_its_own_state() {
    let blocks = quoted_blocks("> - [ ] outer\n>   - [x] inner\n");
    let [ContentBlock::List { items, .. }] = blocks.as_slice() else {
        panic!("expected a single list inside the quote, got {blocks:?}");
    };
    assert_eq!(items[0].checked, Some(false));
    assert_eq!(items[0].content, "outer");
    let (_, inner_items) = nested_list(&items[0]);
    assert_eq!(inner_items[0].checked, Some(true));
    assert_eq!(inner_items[0].content, "inner");
}
