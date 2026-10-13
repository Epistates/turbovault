//! The SEARCH/REPLACE edit engine an LLM drives with `edit_note`: parsing the
//! blocks and applying them, fuzzy matching included, to a note.
//!
//! Input is the note, a NUL byte, then the edit text.
#![no_main]

use libfuzzer_sys::fuzz_target;
use turbovault_vault::EditEngine;

fuzz_target!(|input: &str| {
    let Some((content, edits)) = input.split_once('\0') else {
        return;
    };
    let engine = EditEngine::new();
    let Ok(blocks) = engine.parse_blocks(edits) else {
        return;
    };
    if let Ok((result, new_content)) = engine.apply_edits(content, &blocks, true) {
        assert_eq!(result.blocks_applied, blocks.len());
        assert_eq!(
            result.new_hash,
            turbovault_vault::compute_hash(&new_content)
        );
    }
});
