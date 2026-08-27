//! Regression cover for two reports from treemd against turbovault-parser 1.6.0.
//!
//! - <https://github.com/Epistates/treemd/issues/79> (images)
//! - <https://github.com/Epistates/treemd/issues/80> (blockquotes)
//!
//! Every case here failed before the fix, and none of them could be worked
//! around downstream: the information was already gone by the time a consumer
//! received the blocks.

use turbovault_parser::{ContentBlock, InlineElement, parse_blocks};

/// Every image reachable from a block, whether it is a block of its own or an
/// inline element, including inside list items and blockquotes.
fn collect_images(blocks: &[ContentBlock]) -> Vec<(String, String, Option<String>)> {
    fn walk_inline(inline: &[InlineElement], out: &mut Vec<(String, String, Option<String>)>) {
        for element in inline {
            if let InlineElement::Image {
                alt, src, title, ..
            } = element
            {
                out.push((alt.clone(), src.clone(), title.clone()));
            }
        }
    }

    fn walk(blocks: &[ContentBlock], out: &mut Vec<(String, String, Option<String>)>) {
        for block in blocks {
            match block {
                ContentBlock::Image { alt, src, title } => {
                    out.push((alt.clone(), src.clone(), title.clone()));
                }
                ContentBlock::Paragraph { inline, .. } => walk_inline(inline, out),
                ContentBlock::Heading { inline, .. } => walk_inline(inline, out),
                ContentBlock::Blockquote { blocks, .. } => walk(blocks, out),
                ContentBlock::List { items, .. } => {
                    for item in items {
                        walk_inline(&item.inline, out);
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

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

/// A badge row is the single most common shape of this: an image wrapped in a
/// link. The image used to be dropped outright, so a README reported none.
#[test]
fn image_wrapped_in_a_link_is_still_an_image() {
    let blocks = parse_blocks("[![badge](b.png)](https://ci.example)\n");
    let images = collect_images(&blocks);

    assert_eq!(
        images,
        vec![("badge".to_string(), "b.png".to_string(), None)],
        "a linked image must still be reported as an image"
    );
}

/// The enclosing link keeps pointing at the link's own destination, not the
/// image's. These two are easy to cross-wire, since both pass through
/// `link_url`.
#[test]
fn a_linked_image_keeps_both_destinations_distinct() {
    let blocks = parse_blocks("[![badge](b.png)](https://ci.example)\n");
    let ContentBlock::Paragraph { inline, .. } = &blocks[0] else {
        panic!("expected a paragraph, got {:?}", blocks[0]);
    };

    let link = inline
        .iter()
        .find_map(|e| match e {
            InlineElement::Link { url, .. } => Some(url.as_str()),
            _ => None,
        })
        .expect("expected the enclosing link");
    let image = inline
        .iter()
        .find_map(|e| match e {
            InlineElement::Image { src, .. } => Some(src.as_str()),
            _ => None,
        })
        .expect("expected the wrapped image");

    assert_eq!(link, "https://ci.example");
    assert_eq!(image, "b.png");
}

/// A title is not part of the destination. The spaced-link preprocessor used
/// to wrap `x.png "Title"` whole in angle brackets, which made the title part
/// of the URL and left `title: None`.
#[test]
fn an_image_title_is_not_folded_into_the_source() {
    let images = collect_images(&parse_blocks("![a](x.png \"Title\")\n"));

    assert_eq!(
        images,
        vec![(
            "a".to_string(),
            "x.png".to_string(),
            Some("Title".to_string())
        )]
    );
}

/// The preprocessor still has a job to do: a destination that genuinely
/// contains a space needs the angle brackets, and a title alongside it must
/// survive that rewrite.
#[test]
fn a_spaced_destination_still_works_and_keeps_its_title() {
    let images = collect_images(&parse_blocks("![a](my file.png \"Title\")\n"));

    assert_eq!(
        images,
        vec![(
            "a".to_string(),
            "my file.png".to_string(),
            Some("Title".to_string())
        )]
    );
}

/// Tight and loose lists differ only by a blank line, so they must not differ
/// in what they report. The tight form used to hoist the image out to a
/// top-level block and discard the item's own text with it.
#[test]
fn a_list_item_image_reports_the_same_tight_or_loose() {
    let tight = parse_blocks("- item ![a](a.png)\n- second item\n");
    let loose = parse_blocks("- item ![a](a.png)\n\n- second item\n");

    assert_eq!(
        collect_images(&tight),
        vec![("a".to_string(), "a.png".to_string(), None)],
        "tight list item image"
    );
    assert_eq!(
        collect_images(&tight),
        collect_images(&loose),
        "a blank line between items must not change what is reported"
    );

    for (label, blocks) in [("tight", &tight), ("loose", &loose)] {
        assert!(
            matches!(blocks.first(), Some(ContentBlock::List { .. })),
            "{label}: the image must stay inside the list, got {:?}",
            blocks.first()
        );
    }
}

/// Text before an image in the same item is part of that item. The image's
/// title used to be parked in the paragraph buffer, overwriting it.
#[test]
fn text_before_an_image_survives_the_image() {
    let blocks = parse_blocks("- item ![a](a.png)\n");
    let ContentBlock::List { items, .. } = &blocks[0] else {
        panic!("expected a list, got {:?}", blocks[0]);
    };

    assert!(
        items[0].content.starts_with("item "),
        "expected the item text to survive, got {:?}",
        items[0].content
    );
}

// ---------------------------------------------------------------------------
// Blockquotes
// ---------------------------------------------------------------------------

/// The highest-impact case: every multi-line blockquote. Body lines were
/// concatenated with no separator, so a GFM alert or Obsidian callout, which
/// takes the first line as its marker and the rest as the body, swallowed the
/// entire body into the title.
#[test]
fn blockquote_body_lines_stay_separable() {
    let (content, _) = {
        let blocks = parse_blocks("> [!NOTE] Heads up\n> Some text.\n> More text.\n");
        let (c, b) = first_blockquote(&blocks);
        (c.to_string(), b.to_vec())
    };

    assert_eq!(content, "[!NOTE] Heads up\nSome text.\nMore text.");

    let mut lines = content.lines();
    assert_eq!(lines.next(), Some("[!NOTE] Heads up"));
    assert_eq!(lines.next(), Some("Some text."));
    assert_eq!(lines.next(), Some("More text."));
    assert_eq!(lines.next(), None);
}

/// The stray whitespace-only paragraph that used to be emitted alongside a
/// blockquote, because soft breaks inside the quote landed in the paragraph
/// buffer.
#[test]
fn a_blockquote_emits_no_stray_paragraph() {
    let blocks = parse_blocks("> [!NOTE] Heads up\n> Some text.\n> More text.\n");

    assert_eq!(
        blocks.len(),
        1,
        "expected only the blockquote, got {blocks:?}"
    );
}

/// A fenced block inside a quote was emitted as a top-level sibling *ahead of*
/// the blockquote, so it rendered above the callout header instead of within
/// the callout.
#[test]
fn a_fenced_block_inside_a_blockquote_stays_inside_it() {
    let blocks = parse_blocks("> [!NOTE] Hi\n> Text.\n>\n> ```rust\n> fn main() {}\n> ```\n");

    assert_eq!(
        blocks.len(),
        1,
        "the code block must not escape the quote, got {blocks:?}"
    );

    let (_, nested) = first_blockquote(&blocks);
    let code = nested
        .iter()
        .find_map(|b| match b {
            ContentBlock::Code {
                language, content, ..
            } => Some((language.clone(), content.clone())),
            _ => None,
        })
        .expect("expected the fenced block nested in the quote");

    assert_eq!(code.0, Some("rust".to_string()));
    assert_eq!(code.1, "fn main() {}");

    // Order matters: the prose introduces the code, so it has to come first.
    assert!(
        matches!(nested.first(), Some(ContentBlock::Paragraph { .. })),
        "expected prose before the code block, got {nested:?}"
    );
}

/// A quote with no code and one line is the ordinary case, and must not have
/// picked up a trailing separator from the paragraph-break handling.
#[test]
fn a_single_line_blockquote_has_no_trailing_whitespace() {
    let blocks = parse_blocks("> Just one line.\n");
    let (content, _) = first_blockquote(&blocks);

    assert_eq!(content, "Just one line.");
}
