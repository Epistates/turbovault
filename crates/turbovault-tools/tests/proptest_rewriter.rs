//! Property tests for the link rewriter that move_note and delete_note run
//! over every note linking to the one that changed. Moving a note there and
//! back restores the linking note byte for byte, a second rewrite finds
//! nothing left to change, and text outside the links is never touched.

use proptest::prelude::*;
use turbovault_parser::parsers::link_utils::markdown_link_path;
use turbovault_tools::wikilink_rewriter::{rewrite_wikilinks, wrap_wikilinks_as_stale};

/// A note name: letters, digits, spaces and a dash, never empty and never
/// starting or ending with a space (Obsidian trims those).
fn note_name() -> impl Strategy<Value = String> {
    "[A-Za-z0-9][A-Za-z0-9 -]{0,10}[A-Za-z0-9]".prop_filter("no double spaces", |s| !s.contains("  "))
}

/// One link to `name` (in folder `dir`) in a form a vault holds it in.
fn link_to(name: &str, dir: &str, form: u8) -> String {
    let path = format!("{dir}/{name}");
    match form % 10 {
        0 => format!("[[{name}]]"),
        1 => format!("[[{path}]]"),
        2 => format!("[[{name}|Shown text]]"),
        3 => format!("[[{name}#A heading]]"),
        4 => format!("![[{name}#^block1]]"),
        5 => format!("[x]({})", markdown_link_path(&format!("{name}.md"))),
        6 => format!("[x]({}#part)", markdown_link_path(&format!("{path}.md"))),
        7 => format!("[x](<{name}.md> \"Title\")"),
        8 => format!("[x][r{form}]\n\n[r{form}]: {}\n", markdown_link_path(&format!("{path}.md"))),
        _ => format!("| [[{name}\\|cell]] |"),
    }
}

/// Prose around the links, with nothing in it that could itself be a link.
fn prose() -> impl Strategy<Value = String> {
    "[a-z ,.\n]{0,20}"
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn moving_a_note_there_and_back_restores_its_links(
        old in note_name(),
        new in note_name(),
        forms in prop::collection::vec(any::<u8>(), 1..6),
        gaps in prop::collection::vec(prose(), 7),
    ) {
        prop_assume!(!old.eq_ignore_ascii_case(&new));
        let mut content = gaps[0].clone();
        for (i, form) in forms.iter().enumerate() {
            content.push_str(&link_to(&old, "notes", *form));
            content.push_str(&gaps[i + 1]);
        }
        let old_path = format!("notes/{old}.md");
        let new_path = format!("archive/{new}.md");

        let moved = rewrite_wikilinks(&content, &old_path, &new_path);
        prop_assert_ne!(&moved, &content, "a link to the moved note was not rewritten");
        prop_assert_eq!(
            rewrite_wikilinks(&moved, &old_path, &new_path),
            moved.clone(),
            "a second rewrite changed the note again"
        );
        let back = rewrite_wikilinks(&moved, &new_path, &old_path);
        prop_assert_eq!(back, content);
    }

    #[test]
    fn links_to_other_notes_are_left_alone(
        old in note_name(),
        other in note_name(),
        forms in prop::collection::vec(any::<u8>(), 1..6),
        gap in prose(),
    ) {
        prop_assume!(!old.eq_ignore_ascii_case(&other));
        let content: String = forms
            .iter()
            .map(|form| format!("{}{gap}", link_to(&other, "notes", *form)))
            .collect();
        prop_assert_eq!(
            rewrite_wikilinks(&content, &format!("notes/{old}.md"), "archive/x.md"),
            content.clone()
        );
        prop_assert_eq!(wrap_wikilinks_as_stale(&content, &format!("notes/{old}.md")), content);
    }

    #[test]
    fn marking_links_stale_is_idempotent(
        name in note_name(),
        forms in prop::collection::vec(any::<u8>(), 1..6),
        gap in prose(),
    ) {
        let content: String = forms
            .iter()
            .map(|form| format!("{}{gap}", link_to(&name, "notes", *form)))
            .collect();
        let path = format!("notes/{name}.md");
        let once = wrap_wikilinks_as_stale(&content, &path);
        prop_assert_eq!(wrap_wikilinks_as_stale(&once, &path), once);
    }
}
