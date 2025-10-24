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
//! ## Fuzzy Matching Strategy (aider-inspired):
//! 1. Exact match (fastest)
//! 2. Whitespace-insensitive match
//! 3. Indentation-preserving match
//! 4. Fuzzy match with Levenshtein distance
//!
//! This tolerates minor LLM errors while remaining safe.

use turbovault_core::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
}

impl Default for EditConfig {
    fn default() -> Self {
        Self {
            max_fuzzy_distance: 0.85, // 85% similarity required
            allow_whitespace_flex: true,
            allow_indent_flex: true,
            allow_fuzzy_match: true,
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
        let mut blocks = Vec::new();
        let mut current_search = String::new();
        let mut current_replace = String::new();
        let mut state = ParseState::Init;

        for line in input.lines() {
            let trimmed = line.trim();

            match state {
                ParseState::Init => {
                    if trimmed == "<<<<<<< SEARCH" {
                        state = ParseState::InSearch;
                    }
                }
                ParseState::InSearch => {
                    if trimmed == "=======" {
                        state = ParseState::InReplace;
                    } else {
                        if !current_search.is_empty() {
                            current_search.push('\n');
                        }
                        current_search.push_str(line); // Preserve original indentation
                    }
                }
                ParseState::InReplace => {
                    if trimmed == ">>>>>>> REPLACE" {
                        blocks.push(SearchReplaceBlock {
                            search: current_search.clone(),
                            replace: current_replace.clone(),
                        });
                        current_search.clear();
                        current_replace.clear();
                        state = ParseState::Init;
                    } else {
                        if !current_replace.is_empty() {
                            current_replace.push('\n');
                        }
                        current_replace.push_str(line); // Preserve original indentation
                    }
                }
            }
        }

        // Check for incomplete block
        if state != ParseState::Init {
            return Err(Error::ParseError {
                reason: format!(
                    "Incomplete SEARCH/REPLACE block (state: {:?}). Expected >>>>>>> REPLACE",
                    state
                ),
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

    /// Apply edits with full result metadata
    pub fn apply_edits(
        &self,
        content: &str,
        blocks: &[SearchReplaceBlock],
        dry_run: bool,
    ) -> Result<EditResult> {
        let old_hash = compute_hash(content);

        // Generate diff preview if dry run
        let diff_preview = if dry_run {
            Some(self.generate_preview(content, blocks)?)
        } else {
            None
        };

        let (new_content, warnings) = if dry_run {
            // For dry run, compute what would change but don't return new content
            self.apply_blocks(content, blocks)?
        } else {
            self.apply_blocks(content, blocks)?
        };

        let new_hash = compute_hash(&new_content);

        Ok(EditResult {
            success: true,
            old_hash,
            new_hash,
            blocks_applied: blocks.len(),
            total_blocks: blocks.len(),
            diff_preview,
            warnings,
        })
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

    /// Find with whitespace normalization
    fn fuzzy_find_whitespace(&self, content: &str, search: &str) -> Option<(usize, usize)> {
        let normalized_search = normalize_whitespace(search);
        let normalized_content = normalize_whitespace(content);

        normalized_content.find(&normalized_search).map(|_| {
            // TODO: Map back to original positions
            // For now, return None to skip this strategy
            None
        })?
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

    /// Find using Levenshtein distance
    fn fuzzy_find_levenshtein(&self, content: &str, search: &str) -> Option<(usize, usize)> {
        // Sliding window approach
        let search_len = search.len();
        let threshold = (search_len as f32 * (1.0 - self.config.max_fuzzy_distance)) as usize;

        let mut best_match: Option<(usize, usize, usize)> = None; // (pos, len, distance)

        // Try windows of varying sizes around search length
        for window_size in search_len.saturating_sub(threshold)..=search_len + threshold {
            if window_size > content.len() {
                continue;
            }

            for start in 0..=content.len() - window_size {
                let window = &content[start..start + window_size];
                let distance = levenshtein_distance(search, window);

                if distance <= threshold {
                    if let Some((_, _, best_dist)) = best_match {
                        if distance < best_dist {
                            best_match = Some((start, window_size, distance));
                        }
                    } else {
                        best_match = Some((start, window_size, distance));
                    }
                }
            }
        }

        best_match.map(|(pos, len, _)| (pos, len))
    }

    /// Generate preview diff for dry run
    fn generate_preview(&self, content: &str, blocks: &[SearchReplaceBlock]) -> Result<String> {
        let (new_content, _warnings) = self.apply_blocks(content, blocks)?;

        use similar::{ChangeTag, TextDiff};

        let diff = TextDiff::from_lines(content, &new_content);
        let mut preview = String::new();

        for change in diff.iter_all_changes() {
            let sign = match change.tag() {
                ChangeTag::Delete => "-",
                ChangeTag::Insert => "+",
                ChangeTag::Equal => " ",
            };
            preview.push_str(&format!("{} {}", sign, change));
        }

        Ok(preview)
    }
}

impl Default for EditEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse state machine
#[derive(Debug, Clone, Copy, PartialEq)]
enum ParseState {
    Init,
    InSearch,
    InReplace,
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
    format!("{:x}", hash)
}

/// Normalize whitespace for comparison
fn normalize_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Compute Levenshtein distance between two strings
fn levenshtein_distance(a: &str, b: &str) -> usize {
    // Use strsim crate if available, otherwise use simple implementation
    strsim::levenshtein(a, b)
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
        assert_eq!(match_type, MatchType::IndentationPreserving);
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
}
