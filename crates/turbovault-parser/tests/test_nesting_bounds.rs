//! Bounds on what deeply nested input costs.
//!
//! A quote is rebuilt as markdown and re-parsed, and markdown can only say a
//! list is nested by indenting it. `> - > - > - x` nests a list a level per
//! four bytes with no indentation in the source, so rebuilding it wrote a
//! line per level indented two columns more than the last: quadratic text
//! from linear input, then scanned once per block. Twenty kilobytes took
//! seconds and two hundred never finished, from a document small enough to
//! sit in any vault and be parsed on every read.
//!
//! Lists nest no deeper than [`MAX_LIST_DEPTH`] now, in the document and in a
//! quote alike, and an item past that depth contributes its text to the
//! deepest one kept instead of opening a level of its own.

use turbovault_parser::{ContentBlock, ListItem, MAX_LIST_DEPTH, parse_blocks};

/// Deepest run of lists nested inside one another anywhere in `blocks`,
/// looking through quotes.
fn list_depth(blocks: &[ContentBlock]) -> usize {
    blocks
        .iter()
        .map(|block| match block {
            ContentBlock::List { items, .. } => {
                1 + items
                    .iter()
                    .map(|item| list_depth(&item.blocks))
                    .max()
                    .unwrap_or(0)
            }
            ContentBlock::Blockquote { blocks, .. } => list_depth(blocks),
            _ => 0,
        })
        .max()
        .unwrap_or(0)
}

/// The innermost item reached by always following an item's first block.
fn deepest_item(blocks: &[ContentBlock]) -> Option<&ListItem> {
    let mut current = None;
    let mut blocks = blocks;
    loop {
        let next = match blocks.first() {
            Some(ContentBlock::List { items, .. }) => items.first(),
            Some(ContentBlock::Blockquote { blocks: inner, .. }) => {
                blocks = inner;
                continue;
            }
            _ => None,
        };
        match next {
            Some(item) => {
                current = Some(item);
                blocks = &item.blocks;
            }
            None => return current,
        }
    }
}

/// `count` bullets, each nested one level under the last, with text on each.
fn indented_list(count: usize, prefix: &str) -> String {
    (0..count)
        .map(|level| format!("{prefix}{}- l{level}\n", "  ".repeat(level)))
        .collect()
}

const DEEP: usize = 20_000;

#[test]
fn deep_lists_in_a_quote_stay_within_the_limit() {
    let markdown = format!("# T\n\n{}x\n", "> - ".repeat(DEEP));
    let blocks = parse_blocks(&markdown);
    assert_eq!(list_depth(&blocks), MAX_LIST_DEPTH);

    let [
        ContentBlock::Heading { .. },
        ContentBlock::Blockquote { content, blocks },
    ] = blocks.as_slice()
    else {
        panic!("expected a heading and a quote, got {blocks:?}");
    };
    // The rebuilt text is what the quote costs to re-parse and what every
    // consumer receives, so it has to stay in proportion to the source.
    assert!(
        content.len() < markdown.len(),
        "a {}-byte quote was rebuilt as {} bytes",
        markdown.len(),
        content.len()
    );
    assert_eq!(deepest_item(blocks).map(|i| i.content.as_str()), Some("x"));
}

#[test]
fn deep_lists_stay_within_the_limit() {
    let markdown = format!("# T\n\n{}x\n", "- ".repeat(DEEP));
    let blocks = parse_blocks(&markdown);
    assert_eq!(list_depth(&blocks), MAX_LIST_DEPTH);
    assert_eq!(
        deepest_item(&blocks[1..]).map(|i| i.content.as_str()),
        Some("x")
    );
}

#[test]
fn deep_lists_alternating_with_quotes_stay_within_the_limit() {
    let markdown = format!("# T\n\n{}x\n", "- > ".repeat(DEEP));
    let blocks = parse_blocks(&markdown);
    // The outer list is the document's own and every level below it is
    // inside its first item's quote, which is capped separately.
    assert_eq!(list_depth(&blocks), MAX_LIST_DEPTH + 1);
    assert_eq!(
        deepest_item(&blocks[1..]).map(|i| i.content.as_str()),
        Some("x")
    );
}

/// The limit is a backstop, not a change to documents anyone writes.
#[test]
fn nesting_up_to_the_limit_is_unchanged() {
    for prefix in ["", "> "] {
        let blocks = parse_blocks(&indented_list(MAX_LIST_DEPTH, prefix));
        assert_eq!(list_depth(&blocks), MAX_LIST_DEPTH, "prefix {prefix:?}");
        let last = format!("l{}", MAX_LIST_DEPTH - 1);
        assert_eq!(
            deepest_item(&blocks).map(|i| i.content.as_str()),
            Some(last.as_str()),
            "prefix {prefix:?}"
        );
    }
}

/// Past the limit an item's text is kept, on the deepest item that is.
#[test]
fn items_past_the_limit_keep_their_text() {
    for prefix in ["", "> "] {
        let blocks = parse_blocks(&indented_list(MAX_LIST_DEPTH + 2, prefix));
        assert_eq!(list_depth(&blocks), MAX_LIST_DEPTH, "prefix {prefix:?}");
        let expected = (MAX_LIST_DEPTH - 1..MAX_LIST_DEPTH + 2)
            .map(|level| format!("l{level}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            deepest_item(&blocks).map(|i| i.content.as_str()),
            Some(expected.as_str()),
            "prefix {prefix:?}"
        );
    }
}

/// A checkbox past the limit has no item of its own to mark, and must not
/// overwrite the state of the item it was folded into.
#[test]
fn a_checkbox_past_the_limit_leaves_the_kept_item_alone() {
    let mut markdown = indented_list(MAX_LIST_DEPTH, "");
    markdown.push_str(&format!("{}- [x] past\n", "  ".repeat(MAX_LIST_DEPTH)));
    let blocks = parse_blocks(&markdown);
    let deepest = deepest_item(&blocks).expect("an item");
    assert_eq!(deepest.checked, None);
    assert!(deepest.content.ends_with("past"), "{:?}", deepest.content);
}

/// Wall-clock cover for the shapes above and for a long flat quote, whose
/// every block used to recount the lines of everything before it. Timing is
/// only meaningful in an optimized build, so this is opt-in:
///
/// ```text
/// cargo test --release -p turbovault-parser --test test_nesting_bounds -- --ignored
/// ```
#[test]
#[ignore = "timing; run with --release -- --ignored"]
fn adversarial_nesting_parses_in_linear_time() {
    let cases = [
        ("> - ", format!("# T\n\n{}x\n", "> - ".repeat(50_000))),
        ("- ", format!("# T\n\n{}x\n", "- ".repeat(50_000))),
        ("- > ", format!("# T\n\n{}x\n", "- > ".repeat(50_000))),
        ("flat quote", "> a\n>\n".repeat(50_000)),
    ];
    for (name, markdown) in cases {
        let started = std::time::Instant::now();
        let blocks = parse_blocks(&markdown);
        let elapsed = started.elapsed();
        drop(blocks);
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "{name}: {} bytes took {elapsed:?}",
            markdown.len()
        );
    }
}
