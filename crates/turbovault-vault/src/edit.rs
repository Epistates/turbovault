//! LLM-optimized file editing with SEARCH/REPLACE blocks
//!
//! Inspired by aider's proven approach that reduced GPT-4 laziness by 3X.
//! Uses git merge conflict syntax which LLMs know intimately from training data.
//!
//! ## Format (for LLMs):
//! ```text
//! <<<<<<< SEARCH
//! old content to find
//! =======
//! new content to replace with
//! >>>>>>> REPLACE
//! ```
//!
//! ## Editing content that contains a `=======` line (turbovault-74v):
//! The fence is variable-width. To match content that itself contains a
//! standard 7-char `=======` line, widen ALL three markers to 8+ chars so the
//! 7-char line is treated as content, not the divider:
//! ```text
//! <<<<<<<< SEARCH
//! above
//! =======
//! below
//! ========
//! new above
//! =======
//! new below
//! >>>>>>>> REPLACE
//! ```
//!
//! ## Fuzzy Matching Strategy (aider-inspired):
//! 1. Exact match (fastest)
//! 2. Whitespace-insensitive match
//! 3. Indentation-preserving match
//! 4. Fuzzy match with Levenshtein distance
//!
//! This tolerates minor LLM errors while remaining safe.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use turbovault_core::bytes_to_lower_hex;
use turbovault_core::{Error, Result};
use unicode_normalization::UnicodeNormalization;

/// A single SEARCH/REPLACE block
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchReplaceBlock {
    /// Text to search for (will be fuzzy-matched)
    pub search: String,
    /// Replacement text
    pub replace: String,
}

/// Result of applying edits
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditResult {
    /// Whether edits were applied successfully
    pub success: bool,
    /// Old content hash (SHA-256)
    pub old_hash: String,
    /// New content hash (SHA-256)
    pub new_hash: String,
    /// Number of blocks successfully applied
    pub blocks_applied: usize,
    /// Total blocks attempted
    pub total_blocks: usize,
    /// Preview of changes (if dry_run)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_preview: Option<String>,
    /// Warning messages (e.g., fuzzy match used)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Configuration for edit engine behavior
#[derive(Debug, Clone)]
pub struct EditConfig {
    /// Maximum allowed Levenshtein distance ratio (0.0-1.0)
    /// 0.8 means search can differ by up to 20%
    pub max_fuzzy_distance: f32,

    /// Enable whitespace-insensitive matching
    pub allow_whitespace_flex: bool,

    /// Enable indentation-preserving matching
    pub allow_indent_flex: bool,

    /// Enable fuzzy Levenshtein matching
    pub allow_fuzzy_match: bool,

    /// Maximum character-comparison budget for fuzzy matching (n × m).
    /// Inputs exceeding this skip the Levenshtein strategy entirely.
    pub max_fuzzy_budget: usize,

    /// Maximum search string length (in characters) for fuzzy matching.
    pub max_search_len: usize,
}

impl Default for EditConfig {
    fn default() -> Self {
        Self {
            max_fuzzy_distance: 0.85, // 85% similarity required
            allow_whitespace_flex: true,
            allow_indent_flex: true,
            allow_fuzzy_match: true,
            max_fuzzy_budget: 10_000_000,
            max_search_len: 10_000,
        }
    }
}

/// Edit engine with cascading fuzzy matching
pub struct EditEngine {
    config: EditConfig,
}

impl EditEngine {
    /// Create new edit engine with default config
    pub fn new() -> Self {
        Self {
            config: EditConfig::default(),
        }
    }

    /// Create edit engine with custom config
    pub fn with_config(config: EditConfig) -> Self {
        Self { config }
    }

    /// Parse SEARCH/REPLACE blocks from LLM-generated string
    ///
    /// Expected format:
    /// ```text
    /// <<<<<<< SEARCH
    /// old content
    /// =======
    /// new content
    /// >>>>>>> REPLACE
    /// ```
    pub fn parse_blocks(&self, input: &str) -> Result<Vec<SearchReplaceBlock>> {
        // turbovault-u9w: collect each block's body into a buffer, THEN detect
        // the divider, so a body that legitimately contains a `=======` line is
        // handled rather than silently mis-split.
        //
        // turbovault-74v (TV-006): the fence is variable-width (>= 7). A block
        // opened by a W-wide `<<<…< SEARCH` is delimited by a W-wide `===…=`
        // divider and closed by a W-wide `>>>…> REPLACE`; marker-shaped lines of
        // ANY OTHER width are body content. So to edit content that contains a
        // standard 7-char `=======` line, widen all three markers to 8+ chars
        // (the markdown "longer fence to escape" idiom). 7-wide stays the
        // default and is fully backward-compatible.
        let mut blocks = Vec::new();
        let mut buffer: Vec<&str> = Vec::new();
        let mut in_block = false;
        let mut width = 0usize;

        for line in input.lines() {
            let trimmed = line.trim();

            if !in_block {
                if let Some(w) = fence_width(trimmed, '<', " SEARCH") {
                    in_block = true;
                    width = w;
                    buffer.clear();
                } else if fence_width(trimmed, '>', " REPLACE").is_some() {
                    return Err(Error::ParseError {
                        reason:
                            "Unexpected >>>>>>> REPLACE outside a SEARCH block. Missing <<<<<<< SEARCH?"
                                .to_string(),
                    });
                }
                // Any other line outside a block is preamble/trailing — ignored.
            } else if fence_width(trimmed, '<', " SEARCH") == Some(width) {
                return Err(Error::ParseError {
                    reason:
                        "Nested SEARCH block: missing >>>>>>> REPLACE before the next <<<<<<< SEARCH"
                            .to_string(),
                });
            } else if fence_width(trimmed, '>', " REPLACE") == Some(width) {
                // Only a divider of THIS block's fence width separates the
                // halves; narrower/wider `=` runs in the body are content.
                let divider_count = buffer
                    .iter()
                    .filter(|l| fence_width(l.trim(), '=', "") == Some(width))
                    .count();
                if divider_count == 0 {
                    return Err(Error::ParseError {
                        reason: "SEARCH/REPLACE block is missing the ======= divider".to_string(),
                    });
                }
                if divider_count > 1 {
                    // turbovault-u9w / turbovault-74v: more than one fence-width
                    // divider line — the split is ambiguous. Fail loud and point
                    // at the wider-fence fix.
                    let eq = "=".repeat(width);
                    return Err(Error::ParseError {
                        reason: format!(
                            "SEARCH/REPLACE block contains {divider_count} `{eq}` divider lines but the divider must appear exactly once. \
                             If the matched content has a literal `{eq}` line, widen ALL three fence markers to {}+ characters (e.g. `{}` / `{}` / `{}`) so a {width}-character `=` line in your content is treated as content, not the divider (turbovault-u9w / turbovault-74v).",
                            width + 1,
                            "<".repeat(width + 1),
                            "=".repeat(width + 1),
                            ">".repeat(width + 1),
                        ),
                    });
                }
                let divider_idx = buffer
                    .iter()
                    .position(|l| fence_width(l.trim(), '=', "") == Some(width))
                    .unwrap();
                let search = buffer[..divider_idx].join("\n");
                let replace = buffer[divider_idx + 1..].join("\n");
                blocks.push(SearchReplaceBlock { search, replace });
                buffer.clear();
                in_block = false;
            } else {
                buffer.push(line);
            }
        }

        if in_block {
            return Err(Error::ParseError {
                reason: "Incomplete SEARCH/REPLACE block. Expected >>>>>>> REPLACE".to_string(),
            });
        }

        if blocks.is_empty() {
            return Err(Error::ParseError {
                reason: "No SEARCH/REPLACE blocks found in input".to_string(),
            });
        }

        Ok(blocks)
    }

    /// Apply SEARCH/REPLACE blocks to content
    ///
    /// Returns edited content and metadata about what was applied
    pub fn apply_blocks(
        &self,
        content: &str,
        blocks: &[SearchReplaceBlock],
    ) -> Result<(String, Vec<String>)> {
        let mut result = content.to_string();
        let mut warnings = Vec::new();

        for (idx, block) in blocks.iter().enumerate() {
            match self.find_and_replace(&result, &block.search, &block.replace) {
                Ok((new_content, match_type)) => {
                    result = new_content;
                    if match_type != MatchType::Exact {
                        warnings.push(format!(
                            "Block {} used {} matching",
                            idx + 1,
                            match_type.description()
                        ));
                    }
                }
                Err(e) => {
                    return Err(Error::Other(format!("Block {} failed: {}", idx + 1, e)));
                }
            }
        }

        Ok((result, warnings))
    }

    /// Apply edits with full result metadata.
    ///
    /// Returns `(EditResult, new_content)`. The caller can use `new_content`
    /// for writing without re-running `apply_blocks`.
    pub fn apply_edits(
        &self,
        content: &str,
        blocks: &[SearchReplaceBlock],
        dry_run: bool,
    ) -> Result<(EditResult, String)> {
        let old_hash = compute_hash(content);

        // Apply blocks exactly once
        let (new_content, warnings) = self.apply_blocks(content, blocks)?;

        let diff_preview = if dry_run {
            Some(Self::generate_diff(content, &new_content))
        } else {
            None
        };

        let new_hash = compute_hash(&new_content);

        Ok((
            EditResult {
                success: true,
                old_hash,
                new_hash,
                blocks_applied: blocks.len(),
                total_blocks: blocks.len(),
                diff_preview,
                warnings,
            },
            new_content,
        ))
    }

    /// Find and replace using cascading fuzzy matching strategies
    fn find_and_replace(
        &self,
        content: &str,
        search: &str,
        replace: &str,
    ) -> Result<(String, MatchType)> {
        // Strategy 1: Exact match
        if let Some(pos) = content.find(search) {
            let new_content = Self::replace_at(content, pos, search.len(), replace);
            return Ok((new_content, MatchType::Exact));
        }

        // Strategy 2: Whitespace-insensitive
        if self.config.allow_whitespace_flex
            && let Some((pos, len)) = self.fuzzy_find_whitespace(content, search)
        {
            let new_content = Self::replace_at(content, pos, len, replace);
            return Ok((new_content, MatchType::WhitespaceInsensitive));
        }

        // Strategy 3: Indentation-preserving
        if self.config.allow_indent_flex
            && let Some((pos, len)) = self.fuzzy_find_indentation(content, search)
        {
            let new_content = Self::replace_at(content, pos, len, replace);
            return Ok((new_content, MatchType::IndentationPreserving));
        }

        // Strategy 4: Fuzzy Levenshtein
        if self.config.allow_fuzzy_match
            && let Some((pos, len)) = self.fuzzy_find_levenshtein(content, search)
        {
            let new_content = Self::replace_at(content, pos, len, replace);
            return Ok((new_content, MatchType::FuzzyLevenshtein));
        }

        Err(Error::Other(format!(
            "Could not find search text (tried {} strategies). Search: {:?}",
            4,
            &search[..search.len().min(100)]
        )))
    }

    /// Replace text at specific position
    fn replace_at(content: &str, pos: usize, len: usize, replacement: &str) -> String {
        let mut result = String::with_capacity(content.len() + replacement.len());
        result.push_str(&content[..pos]);
        result.push_str(replacement);
        result.push_str(&content[pos + len..]);
        result
    }

    /// Find with whitespace normalization (line-based approach).
    /// Compares lines after collapsing all whitespace runs to single spaces.
    fn fuzzy_find_whitespace(&self, content: &str, search: &str) -> Option<(usize, usize)> {
        let search_lines: Vec<&str> = search.lines().collect();
        let content_lines: Vec<&str> = content.lines().collect();

        if search_lines.is_empty() {
            return None;
        }

        let normalized_search_lines: Vec<String> = search_lines
            .iter()
            .map(|l| normalize_whitespace(l))
            .collect();

        for start_idx in 0..content_lines.len() {
            if start_idx + search_lines.len() > content_lines.len() {
                break;
            }

            let mut matches = true;
            for (i, norm_search) in normalized_search_lines.iter().enumerate() {
                let norm_content = normalize_whitespace(content_lines[start_idx + i]);
                if *norm_search != norm_content {
                    matches = false;
                    break;
                }
            }

            if matches {
                let start_pos: usize = content_lines[..start_idx].iter().map(|l| l.len() + 1).sum();

                let match_len: usize = content_lines[start_idx..start_idx + search_lines.len()]
                    .iter()
                    .map(|l| l.len() + 1)
                    .sum::<usize>()
                    .saturating_sub(1);

                return Some((start_pos, match_len));
            }
        }

        None
    }

    /// Find with indentation flexibility
    fn fuzzy_find_indentation(&self, content: &str, search: &str) -> Option<(usize, usize)> {
        // Split into lines
        let search_lines: Vec<&str> = search.lines().collect();
        let content_lines: Vec<&str> = content.lines().collect();

        if search_lines.is_empty() {
            return None;
        }

        // Try to find matching sequence with flexible indentation
        for start_idx in 0..content_lines.len() {
            if start_idx + search_lines.len() > content_lines.len() {
                break;
            }

            let mut matches = true;
            for (i, search_line) in search_lines.iter().enumerate() {
                let content_line = content_lines[start_idx + i];
                if search_line.trim() != content_line.trim() {
                    matches = false;
                    break;
                }
            }

            if matches {
                // Calculate byte positions
                let start_pos = content_lines[..start_idx]
                    .iter()
                    .map(|l| l.len() + 1) // +1 for newline
                    .sum();

                let match_len = content_lines[start_idx..start_idx + search_lines.len()]
                    .iter()
                    .map(|l| l.len() + 1)
                    .sum::<usize>()
                    .saturating_sub(1); // Last line doesn't have trailing newline in match

                return Some((start_pos, match_len));
            }
        }

        None
    }

    /// Find using semi-global alignment DP (with size guards to prevent DoS).
    ///
    /// Uses a two-phase approach:
    /// 1. Forward pass: find the best end position where the search text aligns
    /// 2. Backward pass: find the optimal start position for that alignment
    ///
    /// Complexity: O(n * m) vs the previous sliding window O(n * m³)
    #[allow(clippy::needless_range_loop)] // DP loops index multiple arrays by j
    fn fuzzy_find_levenshtein(&self, content: &str, search: &str) -> Option<(usize, usize)> {
        let content_chars: Vec<char> = content.chars().collect();
        let search_chars: Vec<char> = search.chars().collect();
        let n = content_chars.len();
        let m = search_chars.len();

        if m == 0
            || n == 0
            || m > self.config.max_search_len
            || n * m > self.config.max_fuzzy_budget
        {
            return None;
        }

        let threshold = (m as f32 * (1.0 - self.config.max_fuzzy_distance)) as usize;

        // --- Phase 1: Forward semi-global DP to find best end position ---
        // dp[j] = min edit distance to align search[0..j] ending at current content position.
        // dp[0] = 0 for every row (free start gaps — pattern can begin anywhere in content).
        let mut dp = vec![0usize; m + 1];
        for j in 1..=m {
            dp[j] = j;
        }

        let mut best_end: Option<(usize, usize)> = None; // (end_char_idx, distance)

        for i in 1..=n {
            let mut prev_diag = 0;
            dp[0] = 0;

            for j in 1..=m {
                let old = dp[j];
                let cost = usize::from(content_chars[i - 1] != search_chars[j - 1]);
                dp[j] = (prev_diag + cost)
                    .min(dp[j] + 1) // deletion in search (skip content char)
                    .min(dp[j - 1] + 1); // insertion in search (skip search char)
                prev_diag = old;
            }

            if dp[m] <= threshold && best_end.is_none_or(|(_, d)| dp[m] < d) {
                best_end = Some((i, dp[m]));
            }
        }

        let (end_char_idx, _) = best_end?;

        // --- Phase 2: Backward DP to find optimal start position ---
        // Align reverse(search) against reverse(content[..end]) to find match length.
        let max_backward = (m + threshold).min(end_char_idx);
        let mut dp2 = vec![0usize; m + 1];
        for j in 1..=m {
            dp2[j] = j;
        }

        let mut best_len: Option<(usize, usize)> = None; // (match_char_len, distance)

        for i in 1..=max_backward {
            let ci = content_chars[end_char_idx - i];
            let mut prev_diag = 0;
            dp2[0] = 0;

            for j in 1..=m {
                let sj = search_chars[m - j];
                let old = dp2[j];
                let cost = usize::from(ci != sj);
                dp2[j] = (prev_diag + cost).min(dp2[j] + 1).min(dp2[j - 1] + 1);
                prev_diag = old;
            }

            if dp2[m] <= threshold && best_len.is_none_or(|(_, d)| dp2[m] < d) {
                best_len = Some((i, dp2[m]));
            }
        }

        let (match_char_len, _) = best_len?;
        let start_char_idx = end_char_idx - match_char_len;

        // Convert character indices to byte positions
        let start_byte: usize = content_chars[..start_char_idx]
            .iter()
            .map(|c| c.len_utf8())
            .sum();
        let match_byte_len: usize = content_chars[start_char_idx..end_char_idx]
            .iter()
            .map(|c| c.len_utf8())
            .sum();

        Some((start_byte, match_byte_len))
    }

    /// Generate a unified-style diff preview from old and new content.
    fn generate_diff(old_content: &str, new_content: &str) -> String {
        use similar::{ChangeTag, TextDiff};

        let diff = TextDiff::from_lines(old_content, new_content);
        let mut preview = String::new();

        for change in diff.iter_all_changes() {
            let sign = match change.tag() {
                ChangeTag::Delete => "-",
                ChangeTag::Insert => "+",
                ChangeTag::Equal => " ",
            };
            preview.push_str(&format!("{} {}", sign, change));
        }

        preview
    }
}

impl Default for EditEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Type of match found
#[derive(Debug, Clone, Copy, PartialEq)]
enum MatchType {
    Exact,
    WhitespaceInsensitive,
    IndentationPreserving,
    FuzzyLevenshtein,
}

impl MatchType {
    fn description(&self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::WhitespaceInsensitive => "whitespace-insensitive",
            Self::IndentationPreserving => "indentation-preserving",
            Self::FuzzyLevenshtein => "fuzzy (Levenshtein)",
        }
    }
}

/// Compute SHA-256 hash of content (with Unicode NFC normalization)
pub fn compute_hash(content: &str) -> String {
    let normalized: String = content.nfc().collect();
    let hash = Sha256::digest(normalized.as_bytes());
    bytes_to_lower_hex(hash)
}

/// Normalize whitespace for comparison
fn normalize_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Width of a SEARCH/REPLACE fence marker line (turbovault-74v).
///
/// Returns `Some(w)` when `line` (already trimmed) is exactly `w` copies of
/// `ch` (with `w >= 7`) immediately followed by `suffix` — e.g.
/// `fence_width("<<<<<<<< SEARCH", '<', " SEARCH") == Some(8)`. For the divider
/// `suffix` is `""`. Returns `None` otherwise. The `>= 7` floor keeps the
/// canonical 7-wide git-conflict markers as the default fence while letting
/// callers widen the fence to escape content that itself contains a marker line.
fn fence_width(line: &str, ch: char, suffix: &str) -> Option<usize> {
    let run = line.strip_suffix(suffix)?;
    if run.len() >= 7 && run.chars().all(|c| c == ch) {
        Some(run.len())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_single_block() {
        let engine = EditEngine::new();
        let input = r#"<<<<<<< SEARCH
old content
=======
new content
>>>>>>> REPLACE"#;

        let blocks = engine.parse_blocks(input).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].search, "old content");
        assert_eq!(blocks[0].replace, "new content");
    }

    #[test]
    fn test_parse_multiple_blocks() {
        let engine = EditEngine::new();
        let input = r#"<<<<<<< SEARCH
first old
=======
first new
>>>>>>> REPLACE
<<<<<<< SEARCH
second old
=======
second new
>>>>>>> REPLACE"#;

        let blocks = engine.parse_blocks(input).unwrap();
        assert_eq!(blocks.len(), 2);
    }

    /// turbovault-u9w: SEARCH content containing a literal `=======` line
    /// must produce a LOUD parse error, not silent mis-splitting.
    #[test]
    fn parse_block_with_divider_in_search_content_errors_loudly() {
        let engine = EditEngine::new();
        let input = "<<<<<<< SEARCH\nbefore divider\n=======\ncontent containing\n=======\nfooter\n>>>>>>> REPLACE";
        let err = engine.parse_blocks(input).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("`=======`") && msg.contains("turbovault-u9w"),
            "expected u9w ambiguity error, got: {msg}"
        );
    }

    /// turbovault-u9w: REPLACE content with a literal `=======` is the
    /// same shape (the parser sees N+1 dividers total, where N is the
    /// number of dividers in the body).
    #[test]
    fn parse_block_with_divider_in_replace_content_errors_loudly() {
        let engine = EditEngine::new();
        let input =
            "<<<<<<< SEARCH\nold\n=======\nnew with\n=======\nan extra divider\n>>>>>>> REPLACE";
        let err = engine.parse_blocks(input).unwrap_err();
        assert!(
            err.to_string().contains("turbovault-u9w"),
            "expected u9w ambiguity error, got: {err}"
        );
    }

    /// turbovault-u9w: a block with NO `=======` is incomplete and errors.
    #[test]
    fn parse_block_without_divider_errors() {
        let engine = EditEngine::new();
        let input = "<<<<<<< SEARCH\nold\nnew\n>>>>>>> REPLACE";
        let err = engine.parse_blocks(input).unwrap_err();
        assert!(
            err.to_string().contains("======="),
            "expected missing-divider error, got: {err}"
        );
    }

    /// turbovault-u9w: A `>>>>>>> REPLACE` outside any `<<<<<<< SEARCH` is
    /// a structural error.
    #[test]
    fn parse_block_with_dangling_replace_marker_errors() {
        let engine = EditEngine::new();
        let input = "no marker yet\n>>>>>>> REPLACE\nthen content";
        let err = engine.parse_blocks(input).unwrap_err();
        assert!(
            err.to_string().contains("outside"),
            "expected dangling-marker error, got: {err}"
        );
    }

    /// turbovault-u9w: nested `<<<<<<< SEARCH` (missing the prior `>>>>>>>
    /// REPLACE`) is also a structural error.
    #[test]
    fn parse_nested_search_marker_errors() {
        let engine = EditEngine::new();
        let input = "<<<<<<< SEARCH\nold\n<<<<<<< SEARCH\nmore\n=======\nnew\n>>>>>>> REPLACE";
        let err = engine.parse_blocks(input).unwrap_err();
        assert!(
            err.to_string().contains("Nested"),
            "expected nested-marker error, got: {err}"
        );
    }

    /// turbovault-74v (TV-006): a wider fence (8-wide markers) lets a block
    /// MATCH content containing a standard 7-char `=======` line — the 7-char
    /// lines are body content, the 8-char `========` is the divider.
    #[test]
    fn parse_wide_fence_allows_divider_in_content() {
        let engine = EditEngine::new();
        let input = "<<<<<<<< SEARCH\nabove\n=======\nbelow\n========\nABOVE\n=======\nBELOW\n>>>>>>>> REPLACE";
        let blocks = engine.parse_blocks(input).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].search, "above\n=======\nbelow");
        assert_eq!(blocks[0].replace, "ABOVE\n=======\nBELOW");
    }

    /// turbovault-74v (TV-006): end-to-end — edit a note whose body contains a
    /// `=======` horizontal-rule line via a wide fence. Feature-parity with a
    /// full overwrite; the prior refuse-loudly behavior could not touch such
    /// content.
    #[test]
    fn apply_wide_fence_edits_content_with_divider() {
        let engine = EditEngine::new();
        let content = "intro\n\nabove\n=======\nbelow\n\noutro";
        let input = "<<<<<<<< SEARCH\nabove\n=======\nbelow\n========\nABOVE\n=======\nBELOW\n>>>>>>>> REPLACE";
        let blocks = engine.parse_blocks(input).unwrap();
        let (res, new) = engine.apply_edits(content, &blocks, false).unwrap();
        assert!(res.success);
        assert_eq!(new, "intro\n\nABOVE\n=======\nBELOW\n\noutro");
    }

    /// turbovault-74v: the 7-wide ambiguity error now points at the wider-fence
    /// fix (keeps the u9w fail-loud guarantee for the default fence).
    #[test]
    fn divider_ambiguity_error_suggests_wider_fence() {
        let engine = EditEngine::new();
        let input = "<<<<<<< SEARCH\nbefore\n=======\nmid\n=======\nafter\n>>>>>>> REPLACE";
        let err = engine.parse_blocks(input).unwrap_err().to_string();
        assert!(
            err.contains("widen") && err.contains("turbovault-74v"),
            "expected wider-fence suggestion, got: {err}"
        );
    }

    #[test]
    fn test_exact_match() {
        let engine = EditEngine::new();
        let content = "Hello world\nThis is a test\nGoodbye world";
        let search = "This is a test";
        let replace = "This is modified";

        let (result, match_type) = engine.find_and_replace(content, search, replace).unwrap();
        assert_eq!(match_type, MatchType::Exact);
        assert!(result.contains("This is modified"));
    }

    #[test]
    fn test_indentation_match() {
        let engine = EditEngine::new();
        let content = "  indented line\n    more indented";
        let search = "indented line\nmore indented"; // No leading spaces

        let (_result, match_type) = engine
            .find_and_replace(content, search, "replaced")
            .unwrap();
        // Whitespace-insensitive strategy (2) fires before indentation (3) since both match
        assert!(
            match_type == MatchType::WhitespaceInsensitive
                || match_type == MatchType::IndentationPreserving
        );
    }

    #[test]
    fn test_whitespace_insensitive_match() {
        let engine = EditEngine::new();
        let content = "hello    world\n  foo   bar";
        let search = "hello world\nfoo bar"; // Normalized whitespace

        let (_result, match_type) = engine
            .find_and_replace(content, search, "replaced")
            .unwrap();
        assert_eq!(match_type, MatchType::WhitespaceInsensitive);
    }

    #[test]
    fn test_hash_computation() {
        let hash1 = compute_hash("test content");
        let hash2 = compute_hash("test content");
        let hash3 = compute_hash("different");

        assert_eq!(hash1, hash2);
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_unicode_normalization_in_hash() {
        // café as precomposed vs decomposed
        let precomposed = "caf\u{00E9}";
        let decomposed = "caf\u{0065}\u{0301}";

        let hash1 = compute_hash(precomposed);
        let hash2 = compute_hash(decomposed);

        // Should be same after NFC normalization
        assert_eq!(hash1, hash2);
    }

    // -------------------------------------------------------------------------
    // New comprehensive tests
    // -------------------------------------------------------------------------

    /// Levenshtein path must be skipped (not hang) when
    /// content.len() * search.len() > 10_000_000 budget.
    ///
    /// With content of 100_000 chars and search of 200 chars the product is
    /// 20_000_000 which exceeds the MAX_FUZZY_BUDGET guard (10_000_000).
    #[test]
    fn test_levenshtein_size_cap_skips_large_input() {
        let engine = EditEngine::new();

        // Build content and search strings that will exceed the budget guard.
        // Content: 100_000 'a' chars.  Search: 200 chars that will never match
        // exactly or via whitespace/indent (different characters).
        let content = "a".repeat(100_000);
        let search = "z".repeat(200); // product = 100_000 * 200 = 20_000_000 > 10_000_000
        let replace = "REPLACED";

        // No strategy should find a match (none of the 'z' patterns exist in
        // the all-'a' content), and crucially the Levenshtein path must be
        // skipped rather than running — so this must return quickly (no hang).
        let result = engine.find_and_replace(&content, &search, replace);
        assert!(
            result.is_err(),
            "Expected an error when no match is found after strategies are exhausted"
        );
    }

    /// A slight character transposition must be caught by the Levenshtein
    /// fuzzy strategy.
    ///
    /// We use a 20-char string where a single-character transposition yields a
    /// Levenshtein distance of 2, which is within the 15% budget for strings
    /// of that length (2/20 = 10% < 15%).
    #[test]
    fn test_levenshtein_fuzzy_match() {
        let engine = EditEngine::with_config(EditConfig {
            // Disable the earlier strategies so we exercise only the Levenshtein path.
            allow_whitespace_flex: false,
            allow_indent_flex: false,
            allow_fuzzy_match: true,
            max_fuzzy_distance: 0.85, // threshold = floor(20 * 0.15) = 3
            ..EditConfig::default()
        });

        // "the quikc brown foxes" — "quick" is misspelled as "quikc"
        // Distance from "the quick brown foxes" is 2 (swap i↔k), which is ≤ 3.
        let content = "the quikc brown foxes";
        let search = "the quick brown foxes";
        let replace = "the quick brown foxes";

        let result = engine.find_and_replace(content, search, replace);
        assert!(
            result.is_ok(),
            "Levenshtein strategy should find the near-match: {:?}",
            result.err()
        );

        let (_, match_type) = result.unwrap();
        assert_eq!(
            match_type,
            MatchType::FuzzyLevenshtein,
            "Expected FuzzyLevenshtein match type"
        );
    }

    /// Tab-delimited content should be matched by the whitespace-insensitive
    /// strategy when searching with plain spaces.
    #[test]
    fn test_whitespace_match_tabs_vs_spaces() {
        let engine = EditEngine::new();

        // Content uses tabs; search uses spaces.
        let content = "\thello\t\tworld";
        let search = "hello world";
        let replace = "replaced";

        let result = engine.find_and_replace(content, search, replace);
        assert!(
            result.is_ok(),
            "Whitespace-insensitive strategy should match tabs vs spaces: {:?}",
            result.err()
        );

        let (_, match_type) = result.unwrap();
        assert_eq!(match_type, MatchType::WhitespaceInsensitive);
    }

    /// Multi-line content with inconsistent leading whitespace must be found
    /// by the whitespace-insensitive strategy.
    #[test]
    fn test_whitespace_match_multiline() {
        let engine = EditEngine::new();

        let content = "  line one\n    line two";
        // Search text has no leading whitespace — should still match via
        // whitespace normalization (each line's tokens are identical).
        let search = "line one\nline two";
        let replace = "replaced";

        let result = engine.find_and_replace(content, search, replace);
        assert!(
            result.is_ok(),
            "WhitespaceInsensitive strategy should match multi-line with different indent: {:?}",
            result.err()
        );

        let (_, match_type) = result.unwrap();
        assert_eq!(match_type, MatchType::WhitespaceInsensitive);
    }

    /// When two sequential SEARCH/REPLACE blocks are applied the second block
    /// must operate on the output of the first (chained substitution).
    #[test]
    fn test_multiple_blocks_sequential() {
        let engine = EditEngine::new();

        // Block 1: foo → bar
        // Block 2: bar → baz  (only valid after block 1 has run)
        let blocks = vec![
            SearchReplaceBlock {
                search: "foo".to_string(),
                replace: "bar".to_string(),
            },
            SearchReplaceBlock {
                search: "bar".to_string(),
                replace: "baz".to_string(),
            },
        ];

        let content = "foo";
        let (result, warnings) = engine.apply_blocks(content, &blocks).unwrap();

        assert_eq!(result, "baz", "Chained replacement should produce 'baz'");
        // Both blocks used exact matching, so no warnings expected.
        assert!(
            warnings.is_empty(),
            "No fuzzy warnings expected: {:?}",
            warnings
        );
    }

    /// A block that has the SEARCH/REPLACE delimiters but is missing the `=======`
    /// separator should return a parse error rather than panicking.
    #[test]
    fn test_malformed_block_missing_separator() {
        let engine = EditEngine::new();

        // No `=======` line — the parser will reach end-of-input while still
        // in the InSearch state and must report an error.
        let malformed = "<<<<<<< SEARCH\nsome content\n>>>>>>> REPLACE";

        let result = engine.parse_blocks(malformed);
        assert!(
            result.is_err(),
            "Missing separator should produce a parse error, not a panic"
        );
    }

    /// `parse_blocks` alone (without `apply_blocks`) must never modify content
    /// — confirming the dry-run / parse-only path is side-effect-free.
    #[test]
    fn test_dry_run_does_not_modify() {
        let engine = EditEngine::new();

        let original = "original content";

        let input = "<<<<<<< SEARCH\noriginal content\n=======\nnew content\n>>>>>>> REPLACE";
        let blocks = engine.parse_blocks(input).unwrap();

        // apply_edits with dry_run = true
        let (edit_result, _new_content) = engine.apply_edits(original, &blocks, true).unwrap();

        // dry_run should produce a diff preview but NOT change the content on disk.
        // We verify it by reading original again — it should be unchanged.
        // Also check that the result metadata is consistent.
        assert!(edit_result.success);
        assert!(
            edit_result.diff_preview.is_some(),
            "dry_run should populate diff_preview"
        );

        // The old_hash must equal the hash of the unmodified original.
        let expected_old_hash = compute_hash(original);
        assert_eq!(edit_result.old_hash, expected_old_hash);

        // new_hash must differ (the edit would change the content).
        assert_ne!(
            edit_result.old_hash, edit_result.new_hash,
            "dry_run new_hash should reflect what the edit would produce"
        );
    }

    /// Non-matching SEARCH blocks must return an error promptly, not hang.
    /// Regression test for https://github.com/Epistates/turbovault/issues/10
    ///
    /// Uses a 200KB note (large vault MOC-sized) with a 500-char search block
    /// to simulate a realistic worst case. The old O(n·m³) sliding window
    /// would hang for minutes; the O(n·m) semi-global DP finishes in <100ms.
    #[test]
    fn test_no_match_returns_error_promptly() {
        let engine = EditEngine::new();

        // 200KB note — representative of a large vault's biggest files.
        // ~4,400 lines of mixed markdown content.
        let content = "## Section heading\n\nThe quick brown fox jumps over the lazy dog. \
            This is a paragraph with enough variation to make fuzzy matching work hard.\n\n\
            - bullet point with `inline code` and *emphasis*\n\
            - another item [[with a wikilink]] and #tag\n\n"
            .repeat(1_000);

        // 500-char search block that doesn't appear anywhere — long enough
        // to hit the Levenshtein strategy on medium files while the budget
        // guard correctly bails on this large one.
        let search = "fn completely_nonexistent_function() {\n    \
            let result = some_api::call_that_never_existed(param1, param2);\n    \
            match result {\n        \
            Ok(value) => println!(\"success: {value}\"),\n        \
            Err(e) => eprintln!(\"error: {e}\"),\n    \
            }\n    \
            // This block of code is entirely fabricated and will never match\n    \
            // any content in the vault. It exercises all four matching strategies.\n\
            }";
        let replace = "replacement";

        let start = std::time::Instant::now();
        let result = engine.find_and_replace(&content, search, replace);
        let elapsed = start.elapsed();

        assert!(
            result.is_err(),
            "Expected an error when SEARCH text has no match"
        );
        assert!(
            elapsed.as_millis() < 100,
            "Non-matching SEARCH on 200KB content should return within 100ms, took {:?}",
            elapsed
        );
    }
}
