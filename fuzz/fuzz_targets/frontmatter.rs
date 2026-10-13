//! Frontmatter YAML from a note: arbitrary input must give a value or an
//! error, never a panic, a hang or unbounded memory (the parser caps size and
//! depth, MAX_FRONTMATTER_BYTES and MAX_FRONTMATTER_DEPTH).
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    let _ = turbovault_parser::parse_frontmatter_yaml::<serde_json::Value>(text);
});
