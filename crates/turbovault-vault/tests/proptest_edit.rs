//! Property tests for the SEARCH/REPLACE edit engine behind edit_note: an
//! exact search that occurs once replaces exactly that passage and nothing
//! else, the reported hashes are the hashes of the text before and after, and
//! blocks written out in the edit format parse back unchanged.

use proptest::prelude::*;
use turbovault_vault::{EditEngine, SearchReplaceBlock, compute_hash};

/// Note text that never contains the sentinel `¤`, so a search carrying one
/// occurs exactly once in a note built around it.
fn text() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 \t\n#*_\\[\\]()|:.,é日-]{0,60}"
}

/// Lines with none of the characters edit fences are made of, so a block
/// body can never be read as a fence.
fn body() -> impl Strategy<Value = String> {
    prop::collection::vec("[a-zA-Z0-9 #*_.,é日-]{0,20}", 0..5).prop_map(|lines| lines.join("\n"))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn a_unique_exact_search_replaces_only_that_passage(
        prefix in text(),
        search in text(),
        suffix in text(),
        replace in text(),
    ) {
        let search = format!("¤{search}");
        let content = format!("{prefix}{search}{suffix}");
        let engine = EditEngine::new();
        let block = SearchReplaceBlock { search, replace: replace.clone() };

        let (result, new_content) = engine
            .apply_edits(&content, std::slice::from_ref(&block), false)
            .expect("a search that occurs once applies");
        prop_assert_eq!(&new_content, &format!("{prefix}{replace}{suffix}"));
        prop_assert!(result.warnings.is_empty(), "exact match reported {:?}", result.warnings);
        prop_assert_eq!(result.old_hash, compute_hash(&content));
        prop_assert_eq!(result.new_hash, compute_hash(&new_content));
    }

    #[test]
    fn a_search_that_occurs_twice_is_refused(
        prefix in text(),
        search in text(),
        middle in text(),
        suffix in text(),
    ) {
        let search = format!("¤{search}");
        let content = format!("{prefix}{search}{middle}{search}{suffix}");
        let block = SearchReplaceBlock { search, replace: "x".into() };
        prop_assert!(EditEngine::new().apply_blocks(&content, &[block]).is_err());
    }

    #[test]
    fn blocks_written_in_the_edit_format_parse_back(
        blocks in prop::collection::vec((body(), body()), 1..4),
    ) {
        let input: String = blocks
            .iter()
            .map(|(search, replace)| {
                format!("<<<<<<< SEARCH\n{search}\n=======\n{replace}\n>>>>>>> REPLACE\n")
            })
            .collect();
        let parsed = EditEngine::new().parse_blocks(&input).expect("well-formed blocks parse");
        let parsed: Vec<(String, String)> =
            parsed.into_iter().map(|b| (b.search, b.replace)).collect();
        prop_assert_eq!(parsed, blocks);
    }
}
