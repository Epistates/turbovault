//! Regression cover for the line numbers on `ContentBlock::Code`, reported as
//! part of <https://github.com/Epistates/turbovault/issues/68>.
//!
//! `current_line` was set once when the parse started and never advanced, so
//! every fenced block in every document reported the line the parse began at.
//! For `parse_blocks` that was a hard-coded 0, which put every block before the
//! start of its own document for anything filtering on position. A quoted fence
//! was the case that got reported, because the quote is re-parsed as a fragment
//! and the fragment genuinely does start at zero.
//!
//! Lines are 1-based, matching the rest of the crate, and a fenced block spans
//! from its opening fence to its closing fence.

use turbovault_core::ContentBlock;
use turbovault_parser::{parse_blocks, parse_blocks_from_line};

/// Every code block reachable from a set of blocks, as
/// `(language, content, start_line, end_line)`.
fn code_spans(blocks: &[ContentBlock]) -> Vec<(Option<String>, String, usize, usize)> {
    fn walk(blocks: &[ContentBlock], out: &mut Vec<(Option<String>, String, usize, usize)>) {
        for block in blocks {
            match block {
                ContentBlock::Code {
                    language,
                    content,
                    start_line,
                    end_line,
                } => out.push((language.clone(), content.clone(), *start_line, *end_line)),
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

/// Just the spans, for the cases where the content is not the point.
fn spans(markdown: &str) -> Vec<(usize, usize)> {
    code_spans(&parse_blocks(markdown))
        .into_iter()
        .map(|(_, _, start, end)| (start, end))
        .collect()
}

// ---------------------------------------------------------------------------
// The reported case
// ---------------------------------------------------------------------------

/// The fixture from the report: a top-level fence and a quoted one in the same
/// document. Both reported 0 before the fix.
#[test]
fn a_quoted_fence_reports_its_place_in_the_document() {
    //   1  # T
    //   2
    //   3  ```rust
    //   4  top
    //   5  ```
    //   6
    //   7  > [!NOTE] N
    //   8  >
    //   9  > ```py
    //  10  > quoted
    //  11  > ```
    let blocks =
        parse_blocks("# T\n\n```rust\ntop\n```\n\n> [!NOTE] N\n>\n> ```py\n> quoted\n> ```\n");
    assert_eq!(
        code_spans(&blocks),
        vec![
            (Some("rust".to_string()), "top".to_string(), 3, 5),
            (Some("py".to_string()), "quoted".to_string(), 9, 11),
        ]
    );
}

#[test]
fn no_block_reports_a_line_before_the_document_starts() {
    for markdown in [
        "```rust\nA();\n```\n",
        "> ```rust\n> A();\n> ```\n",
        "para\n\n> quote\n>\n> ```sh\n> deep\n> ```\n",
        "- item\n\n  ```sh\n  nested\n  ```\n",
    ] {
        for (start, end) in spans(markdown) {
            assert!(
                start >= 1 && end >= start,
                "{markdown:?} reported {start}..{end}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The numbering contract
// ---------------------------------------------------------------------------

/// A fence spans opening to closing, so a one-line body is three lines.
#[test]
fn a_fence_spans_its_opening_and_closing_lines() {
    assert_eq!(spans("```rust\nA();\n```\n"), vec![(1, 3)]);
    assert_eq!(spans("```rust\nA();\nB();\nC();\n```\n"), vec![(1, 5)]);
}

#[test]
fn later_blocks_report_later_lines() {
    //  1 ```a          4 ```b          7 ```c
    let markdown = "```a\n1\n```\n```b\n2\n```\n```c\n3\n```\n";
    assert_eq!(spans(markdown), vec![(1, 3), (4, 6), (7, 9)]);
}

/// The fragment entry point exists so nested content can be numbered against
/// the document it came from. It is what the blockquote re-parse now uses.
#[test]
fn parsing_from_a_line_offsets_every_block() {
    let fragment = "```rust\nA();\n```\n";
    let at_one = code_spans(&parse_blocks_from_line(fragment, 1));
    let at_fifty = code_spans(&parse_blocks_from_line(fragment, 50));
    assert_eq!(at_one[0].2, 1);
    assert_eq!(at_one[0].3, 3);
    assert_eq!(at_fifty[0].2, 50);
    assert_eq!(at_fifty[0].3, 52);
}

/// A `<details>` block is replaced by a one-line placeholder before parsing, so
/// without padding it back to the same height every line after it reports a
/// number from higher up the document.
#[test]
fn a_details_block_does_not_shift_the_lines_after_it() {
    //   1  para
    //   2
    //   3  <details>
    //   4  <summary>S</summary>
    //   5
    //   6  body
    //   7
    //   8  </details>
    //   9
    //  10  ```rust
    //  11  after
    //  12  ```
    let with_details =
        "para\n\n<details>\n<summary>S</summary>\n\nbody\n\n</details>\n\n```rust\nafter\n```\n";
    assert_eq!(spans(with_details), vec![(10, 12)]);
}
