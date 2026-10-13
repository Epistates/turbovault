//! The full note parse a vault runs on every file: frontmatter, headings,
//! wikilinks, embeds, Markdown and reference links, tags, tasks, callouts.
//! Every reported position must be a span of the text, on character
//! boundaries, because the link rewriter splices at exactly those offsets.
#![no_main]

use libfuzzer_sys::fuzz_target;
use std::path::{Path, PathBuf};

fuzz_target!(|text: &str| {
    let parser = turbovault_parser::Parser::new(PathBuf::from("/vault"));
    let Ok(file) = parser.parse_file(Path::new("/vault/note.md"), text) else {
        return;
    };
    for link in &file.links {
        let start = link.position.offset;
        let end = start + link.position.length;
        assert!(
            text.get(start..end).is_some(),
            "link span {start}..{end} is not a span of the {}-byte note",
            text.len()
        );
    }
    for definition in turbovault_parser::parse_reference_definitions(text) {
        assert!(text.get(definition.destination.clone()).is_some());
    }
});
