//! Splitting note text into chunks, and diffing one note's chunk set against
//! another by content hash.
//!
//! The splitting algorithm and the diff strategy below started as
//! ForrestThump's prototype in [#29](https://github.com/Epistates/turbovault/issues/29)
//! (`crates/turbovault-vector/src/chunks.rs` and `build.rs` on his fork,
//! `forrest/prototype-vector-e2e`); both are carried over close to verbatim
//! because they were already correct and already had the regression tests to
//! prove it. What changed is what surrounds them: this crate stores full
//! chunk text and a dense vector per chunk instead of a byte range into a
//! SQLite row, because persistence here goes through `PluginStore` rather
//! than a database file.

use std::collections::{HashMap, VecDeque};

use sha2::{Digest, Sha256};

/// SHA-256 of `data`, as lowercase hex.
pub fn content_hash(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Split `text` into `(start_byte, end_byte)` chunks with optional overlap.
///
/// Algorithm:
/// 1. Split on `\n\n` to get paragraphs.
/// 2. If a paragraph fits in `max_chars`, it becomes one segment.
/// 3. If too long, split on sentence boundaries (`. `, `! `, `? `, or the CJK
///    period `。`).
/// 4. If a sentence is still too long, hard-split at `max_chars` char
///    boundaries.
/// 5. Apply overlap: extend each segment's start back by up to
///    `overlap_chars`, walking real char boundaries in `text` so a multi-byte
///    character is never split.
pub fn chunk_text(text: &str, max_chars: usize, overlap_chars: usize) -> Vec<(usize, usize)> {
    if text.is_empty() || max_chars == 0 {
        return vec![];
    }

    let mut segments: Vec<(usize, usize)> = Vec::new();

    // Walk paragraph boundaries (`\n\n`) manually to keep byte offsets exact.
    let mut para_start: usize = 0;
    let mut remaining = text;
    loop {
        match remaining.find("\n\n") {
            Some(pos) => {
                let para = &remaining[..pos];
                if !para.is_empty() {
                    split_paragraph(para, para_start, max_chars, &mut segments);
                }
                para_start += pos + 2;
                remaining = &remaining[pos + 2..];
            }
            None => {
                if !remaining.is_empty() {
                    split_paragraph(remaining, para_start, max_chars, &mut segments);
                }
                break;
            }
        }
    }

    if segments.is_empty() || overlap_chars == 0 {
        return segments;
    }

    let mut result: Vec<(usize, usize)> = Vec::with_capacity(segments.len());
    result.push(segments[0]);
    for &(cur_start, cur_end) in &segments[1..] {
        // Extend the start back by up to `overlap_chars` characters, walking
        // actual char boundaries in `text` rather than subtracting raw byte
        // counts, so the new start always lands on a valid boundary even
        // amid multi-byte characters.
        let new_start = text[..cur_start]
            .char_indices()
            .rev()
            .take(overlap_chars)
            .map(|(idx, _)| idx)
            .last()
            .unwrap_or(cur_start);
        result.push((new_start, cur_end));
    }
    result
}

/// Split a single paragraph into segments of at most `max_chars` chars,
/// appending `(start_byte, end_byte)` pairs relative to the original text.
fn split_paragraph(para: &str, base: usize, max_chars: usize, segments: &mut Vec<(usize, usize)>) {
    let para_chars = para.chars().count();
    if para_chars <= max_chars {
        segments.push((base, base + para.len()));
        return;
    }

    let split_chars = [". ", "! ", "? ", "。"];
    let mut sent_start_byte: usize = 0;
    let mut sent_remaining = para;

    loop {
        match sent_remaining
            .find(split_chars[0])
            .or_else(|| sent_remaining.find(split_chars[1]))
            .or_else(|| sent_remaining.find(split_chars[2]))
            .or_else(|| sent_remaining.find(split_chars[3]))
        {
            Some(pos) => {
                // The CJK period is 3 bytes with no trailing space; the ASCII
                // punctuation marks are 1 byte plus a 1-byte space.
                let is_cjk_period = sent_remaining.as_bytes()[pos] == 0xE3;
                let punct_end = if is_cjk_period { 3 } else { 1 };
                let delimiter_len = if is_cjk_period { 3 } else { 2 };
                let sentence_end = pos + punct_end;
                let sentence = &sent_remaining[..sentence_end];
                let abs_start = base + sent_start_byte;
                push_hard_chunks(sentence, abs_start, max_chars, segments);

                sent_start_byte += pos + delimiter_len;
                sent_remaining = &sent_remaining[pos + delimiter_len..];
            }
            None => {
                if !sent_remaining.is_empty() {
                    let abs_start = base + sent_start_byte;
                    push_hard_chunks(sent_remaining, abs_start, max_chars, segments);
                }
                break;
            }
        }
    }
}

/// Push one or more hard-split chunks from `text` (absolute start byte
/// `base`), each at most `max_chars` characters.
fn push_hard_chunks(text: &str, base: usize, max_chars: usize, segments: &mut Vec<(usize, usize)>) {
    let mut char_count = 0usize;
    let mut chunk_start_byte: usize = 0;

    for (byte_idx, _character) in text.char_indices() {
        if char_count == max_chars {
            segments.push((base + chunk_start_byte, base + byte_idx));
            chunk_start_byte = byte_idx;
            char_count = 0;
        }
        char_count += 1;
    }
    if chunk_start_byte < text.len() {
        segments.push((base + chunk_start_byte, base + text.len()));
    }
}

/// Match new chunk hashes against a previous chunk set by content hash.
///
/// Returns `(reuse, stale)` where:
/// - `reuse[i] = Some(id)`: new chunk `i` has the same content as an old
///   chunk; its embedding can be reused unchanged, no re-embedding needed.
/// - `reuse[i] = None`: new chunk `i` is new or changed content; it needs
///   embedding.
/// - `stale`: old chunk ids not matched by any new chunk; remove them from
///   the dense and lexical indices.
///
/// Duplicate content (the same hash appearing more than once) is handled with
/// a per-hash queue: each old id is consumed at most once, so as many
/// vectors as possible are preserved even when a note repeats a paragraph.
pub fn diff_chunks(
    old_hashes: &[(u64, String)],
    new_hashes: &[String],
) -> (Vec<Option<u64>>, Vec<u64>) {
    let mut old_by_hash: HashMap<&str, VecDeque<u64>> = HashMap::new();
    for (id, hash) in old_hashes {
        old_by_hash.entry(hash.as_str()).or_default().push_back(*id);
    }

    let reuse: Vec<Option<u64>> = new_hashes
        .iter()
        .map(|hash| {
            old_by_hash
                .get_mut(hash.as_str())
                .and_then(VecDeque::pop_front)
        })
        .collect();

    let stale: Vec<u64> = old_by_hash.into_values().flatten().collect();
    (reuse, stale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_text_ranges_are_char_boundaries_with_multibyte_and_overlap() {
        // A long run of multi-byte chars (em-dash = 3 bytes) with no sentence
        // breaks forces hard-splitting + overlap. Regression: overlap once
        // subtracted raw byte counts and could land mid-char.
        let text = "word—word ".repeat(400);
        let ranges = chunk_text(&text, 100, 20);
        assert!(!ranges.is_empty());
        for (start, end) in ranges {
            assert!(text.is_char_boundary(start), "start {start} not a boundary");
            assert!(text.is_char_boundary(end), "end {end} not a boundary");
            let _ = &text[start..end]; // must not panic
        }
    }

    #[test]
    fn empty_text_has_no_chunks() {
        assert!(chunk_text("", 800, 100).is_empty());
    }

    #[test]
    fn short_paragraph_is_one_chunk() {
        let text = "A short paragraph that fits easily.";
        let ranges = chunk_text(text, 800, 100);
        assert_eq!(ranges, vec![(0, text.len())]);
    }

    #[test]
    fn paragraphs_split_on_blank_lines() {
        let text = "First paragraph.\n\nSecond paragraph.";
        let ranges = chunk_text(text, 800, 0);
        assert_eq!(ranges.len(), 2);
        assert_eq!(&text[ranges[0].0..ranges[0].1], "First paragraph.");
        assert_eq!(&text[ranges[1].0..ranges[1].1], "Second paragraph.");
    }

    #[test]
    fn long_paragraph_splits_on_sentences() {
        let sentence = "Word word word word word. ";
        let text = sentence.repeat(10); // ~270 chars, over a 50-char budget
        let ranges = chunk_text(&text, 50, 0);
        assert!(ranges.len() > 1);
        for (start, end) in &ranges {
            assert!(end - start <= sentence.len(), "chunk exceeded one sentence");
        }
    }

    // ── diff_chunks ──────────────────────────────────────────────────────

    fn old(pairs: &[(u64, &str)]) -> Vec<(u64, String)> {
        pairs
            .iter()
            .map(|(id, hash)| (*id, hash.to_string()))
            .collect()
    }

    fn hashes(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn diff_empty_old_and_new() {
        let (reuse, stale) = diff_chunks(&[], &[]);
        assert!(reuse.is_empty());
        assert!(stale.is_empty());
    }

    #[test]
    fn diff_all_new_embeds_everything() {
        let (reuse, stale) = diff_chunks(&[], &hashes(&["a", "b", "c"]));
        assert_eq!(reuse, vec![None, None, None]);
        assert!(stale.is_empty());
    }

    #[test]
    fn diff_all_unchanged_reuses_everything() {
        let (reuse, stale) = diff_chunks(
            &old(&[(1, "a"), (2, "b"), (3, "c")]),
            &hashes(&["a", "b", "c"]),
        );
        assert_eq!(reuse, vec![Some(1), Some(2), Some(3)]);
        assert!(stale.is_empty());
    }

    #[test]
    fn diff_one_chunk_changed_only_that_one_is_re_embedded() {
        let (reuse, mut stale) = diff_chunks(
            &old(&[(1, "a"), (2, "b"), (3, "c")]),
            &hashes(&["a", "b2", "c"]),
        );
        assert_eq!(reuse, vec![Some(1), None, Some(3)]);
        stale.sort_unstable();
        assert_eq!(stale, vec![2]);
    }

    #[test]
    fn diff_paragraph_inserted_in_the_middle() {
        let (reuse, stale) = diff_chunks(&old(&[(1, "a"), (2, "b")]), &hashes(&["a", "new", "b"]));
        assert_eq!(reuse, vec![Some(1), None, Some(2)]);
        assert!(stale.is_empty());
    }

    #[test]
    fn diff_paragraph_deleted() {
        let (reuse, mut stale) =
            diff_chunks(&old(&[(1, "a"), (2, "b"), (3, "c")]), &hashes(&["a", "c"]));
        assert_eq!(reuse, vec![Some(1), Some(3)]);
        stale.sort_unstable();
        assert_eq!(stale, vec![2]);
    }

    #[test]
    fn diff_everything_deleted() {
        let (reuse, mut stale) = diff_chunks(&old(&[(1, "a"), (2, "b")]), &[]);
        assert!(reuse.is_empty());
        stale.sort_unstable();
        assert_eq!(stale, vec![1, 2]);
    }

    #[test]
    fn diff_duplicate_content_partial_match_reuses_one_deletes_the_other() {
        let (reuse, mut stale) = diff_chunks(&old(&[(10, "dup"), (11, "dup")]), &hashes(&["dup"]));
        assert_eq!(reuse.len(), 1);
        let reused_id = reuse[0].expect("one of the duplicates is reused");
        stale.sort_unstable();
        assert_eq!(stale.len(), 1);
        assert!(!stale.contains(&reused_id));
    }

    #[test]
    fn diff_duplicate_content_full_match_reuses_both() {
        let (reuse, stale) =
            diff_chunks(&old(&[(10, "dup"), (11, "dup")]), &hashes(&["dup", "dup"]));
        assert_eq!(reuse.len(), 2);
        assert!(reuse.iter().all(Option::is_some));
        assert!(stale.is_empty());
    }
}
