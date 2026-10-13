//! The link rewriter a note move runs over every note linking to it:
//! wikilinks, embeds, inline Markdown links, reference definitions and links
//! in properties, with arbitrary note paths on both sides of the move.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use turbovault_tools::wikilink_rewriter::{links_in, rewrite_links_from, wrap_links_as_stale};

#[derive(Arbitrary, Debug)]
struct Move<'a> {
    content: &'a str,
    source: &'a str,
    old: &'a str,
    new: &'a str,
}

fuzz_target!(|m: Move<'_>| {
    let links = links_in(m.content);
    let _ = rewrite_links_from(m.source, m.content, &links, m.old, m.new);
    let _ = wrap_links_as_stale(m.content, &links);
});
