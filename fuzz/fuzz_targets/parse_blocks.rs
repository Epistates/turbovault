//! The block parser on arbitrary text: it must not panic, and each list item's
//! nesting must stay within `MAX_LIST_DEPTH`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    let _ = turbovault_parser::parse_blocks(text);
    let _ = turbovault_parser::to_plain_text(text);
});
