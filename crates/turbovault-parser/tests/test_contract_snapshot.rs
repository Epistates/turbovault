//! One snapshot of everything the parser extracts from a fixture covering
//! every construct that has broken here.
//!
//! The narrow tests beside this one each pin a specific defect, and each was
//! written after that defect shipped. Three of them shipped while the suite was
//! green ([#55], [#68], [#71]), every time because the assertion was narrower
//! than the failure: a test asserting an image keeps its `src` says nothing
//! about the heading that image leaked into, and a test asserting a fence is
//! reported says nothing about the line it claims to be on.
//!
//! So this asserts the full extracted shape rather than properties of it. A
//! change anywhere in the output fails it, which is the point. It is not a
//! statement that the current shape is correct, only that it is known: when a
//! fix changes it, the diff is the review.
//!
//! Re-bless deliberately, after reading the diff:
//!
//! ```text
//! UPDATE_PARSER_SNAPSHOT=1 cargo test -p turbovault-parser --test test_contract_snapshot
//! ```
//!
//! [#55]: https://github.com/Epistates/turbovault/pull/55
//! [#68]: https://github.com/Epistates/turbovault/issues/68
//! [#71]: https://github.com/Epistates/turbovault/issues/71

use std::collections::BTreeMap;

use turbovault_parser::parse_blocks;

const FIXTURE: &str = include_str!("fixtures/contract.md");
const SNAPSHOT: &str = include_str!("fixtures/contract.json");

/// Object keys sorted at every depth, so a map that serializes in a different
/// order between runs cannot present as a change.
fn canonical(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, canonical(value)))
                .collect::<serde_json::Map<_, _>>()
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(canonical).collect())
        }
        other => other,
    }
}

fn render(markdown: &str) -> String {
    let blocks = parse_blocks(markdown);
    let value = canonical(serde_json::to_value(&blocks).expect("blocks to json"));
    serde_json::to_string_pretty(&value).expect("serialize blocks")
}

/// Lines present in one side and not the other, counted rather than positioned.
///
/// Comparing positionally makes a single inserted line cascade into a diff of
/// everything below it, which buries the actual change. As multisets, an
/// inserted line is one `new:` entry.
fn line_delta(expected: &str, actual: &str) -> Vec<String> {
    fn counts(text: &str) -> BTreeMap<&str, isize> {
        let mut counts = BTreeMap::new();
        for line in text.lines() {
            *counts.entry(line.trim()).or_insert(0) += 1;
        }
        counts
    }

    let expected_counts = counts(expected);
    let actual_counts = counts(actual);
    let mut delta = Vec::new();

    for (line, count) in &expected_counts {
        let surplus = count - actual_counts.get(line).copied().unwrap_or(0);
        for _ in 0..surplus.max(0) {
            delta.push(format!("  gone: {line}"));
        }
    }
    for (line, count) in &actual_counts {
        let surplus = count - expected_counts.get(line).copied().unwrap_or(0);
        for _ in 0..surplus.max(0) {
            delta.push(format!("  new:  {line}"));
        }
    }
    delta
}

#[test]
fn the_extracted_shape_of_every_construct_is_unchanged() {
    let actual = render(FIXTURE);
    let expected = SNAPSHOT.trim_end();

    if actual == expected {
        return;
    }

    if std::env::var_os("UPDATE_PARSER_SNAPSHOT").is_some() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/contract.json");
        std::fs::write(&path, format!("{actual}\n")).expect("rewrite the snapshot");
        panic!("snapshot rewritten at {path:?}; re-run without UPDATE_PARSER_SNAPSHOT");
    }

    let delta = line_delta(expected, &actual);
    panic!(
        "the parser's output for tests/fixtures/contract.md changed in {} place(s):\n{}\n\n\
         If the change is intended, read it, then re-bless with:\n  \
         UPDATE_PARSER_SNAPSHOT=1 cargo test -p turbovault-parser --test test_contract_snapshot",
        delta.len(),
        delta.join("\n")
    );
}

/// Line numbers checked against the fixture rather than against themselves.
///
/// A snapshot only says the numbers have not moved. This says they are right:
/// every reported line, once its quote markers and indentation are stripped,
/// has to be a line that actually opens or closes a fence. It covers the
/// fences inside blockquotes and list items, which are the ones that have to
/// travel furthest to get their position.
#[test]
fn every_code_block_points_at_a_real_fence() {
    fn collect(blocks: &[turbovault_core::ContentBlock], out: &mut Vec<(usize, usize)>) {
        use turbovault_core::ContentBlock;
        for block in blocks {
            match block {
                ContentBlock::Code {
                    start_line,
                    end_line,
                    ..
                } => out.push((*start_line, *end_line)),
                ContentBlock::Blockquote { blocks, .. } => collect(blocks, out),
                ContentBlock::List { items, .. } => {
                    for item in items {
                        collect(&item.blocks, out);
                    }
                }
                _ => {}
            }
        }
    }

    /// Strips `>` quote markers and surrounding whitespace, leaving the line as
    /// the nested parse sees it.
    fn unquote(line: &str) -> &str {
        let mut rest = line.trim();
        while let Some(stripped) = rest.strip_prefix('>') {
            rest = stripped.trim_start();
        }
        rest
    }

    let lines: Vec<&str> = FIXTURE.lines().collect();
    let mut spans = Vec::new();
    collect(&parse_blocks(FIXTURE), &mut spans);
    assert!(!spans.is_empty(), "the fixture reported no code blocks");

    for (start, end) in spans {
        for (which, number) in [("start", start), ("end", end)] {
            assert!(
                number >= 1 && number <= lines.len(),
                "{which}_line {number} is outside the fixture's {} lines",
                lines.len()
            );
            let line = lines[number - 1];
            let bare = unquote(line);
            // An indented block has no fence, so its own single line is the
            // only thing that can identify it.
            let is_fence = bare.starts_with("```");
            let is_indented = line.starts_with("    ") && !bare.is_empty();
            assert!(
                is_fence || is_indented,
                "a code block claims {which}_line {number}, which is {line:?}"
            );
        }
    }
}

/// The fixture is only worth what it covers, so a construct going missing from
/// it has to fail rather than quietly shrink what the snapshot protects.
#[test]
fn the_fixture_still_covers_every_construct_that_has_broken() {
    for (what, needle) in [
        ("an image in a heading", "![alt-in-heading]"),
        ("a link in a heading", "[link-in-heading]"),
        ("a linked image in a heading", "[![badge-in-heading]"),
        (
            "a heading that is only an image",
            "#### ![banner-only-heading]",
        ),
        ("a titled image", "\"The Title\""),
        ("a spaced destination", "<spaced path.png>"),
        ("a wikilink", "[[wikilink]]"),
        ("an image in a blockquote", "![img-in-quote]"),
        ("a linked image in a blockquote", "[![badge-in-quote]"),
        ("a list in a blockquote", "> - quoted tight one"),
        ("an ordered list in a blockquote", "> 1. quoted ordered one"),
        ("a task list in a blockquote", "> - [x] quoted done"),
        ("a nested list in a blockquote", ">   - quoted nested"),
        (
            "a fence after a list in a quote",
            "fence_after_a_list_in_a_callout",
        ),
        ("a fence first in a quote", "fence_first_in_a_quote"),
        (
            "a continuation paragraph in a quoted item",
            ">   a continuation paragraph",
        ),
        ("a fence in a quoted item", "fence_inside_a_quoted_item"),
        ("a nested blockquote", "> > nested quote"),
        ("a nested task list", "  - [x] nested task"),
        ("a nested ordered list", "   1. nested ordered"),
        ("a top-level fence", "fn top_level()"),
        ("an indented code block", "    an indented code block"),
        ("a fence in a list item", "in_a_list_item()"),
        ("a details block", "<summary>A details block</summary>"),
        ("a table", "|:---------|:--------:|---------:|"),
    ] {
        assert!(
            FIXTURE.contains(needle),
            "the fixture no longer covers {what} (looked for {needle:?})"
        );
    }
}
