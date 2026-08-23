//! File operation tools for the Obsidian MCP server

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::UNIX_EPOCH;
use tokio::io::AsyncReadExt;
use turbovault_core::Precondition;
use turbovault_core::prelude::*;
use turbovault_vault::VaultManager;

/// Write mode for write_file operations
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WriteMode {
    /// Overwrite the entire file (default)
    #[default]
    Overwrite,
    /// Append content to end of file
    Append,
    /// Prepend content to beginning of file (after frontmatter if present)
    Prepend,
}

impl WriteMode {
    /// Parse from string (case-insensitive)
    pub fn from_str_opt(s: Option<&str>) -> Result<Self> {
        match s {
            None | Some("overwrite") => Ok(Self::Overwrite),
            Some("append") => Ok(Self::Append),
            Some("prepend") => Ok(Self::Prepend),
            Some(other) => Err(Error::config_error(format!(
                "Invalid write mode '{}'. Must be 'overwrite', 'append', or 'prepend'",
                other
            ))),
        }
    }
}

/// Lightweight note metadata (no content read)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteInfo {
    pub path: String,
    pub exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_frontmatter: Option<bool>,
}

/// Which part of a note a partial read should return.
///
/// A default (all-`None`) spec selects nothing, and [`slice_content`] reports
/// that by returning `Ok(None)` so the caller can read the whole file instead.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SliceSpec {
    /// Return only the first N lines.
    pub head_lines: Option<usize>,
    /// Return only the last N lines.
    pub tail_lines: Option<usize>,
}

impl SliceSpec {
    /// Is any selector set?
    pub fn selects_anything(&self) -> bool {
        self.head_lines.is_some() || self.tail_lines.is_some()
    }
}

/// The outcome of a partial read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SliceResult {
    /// The selected content — a verbatim substring of the original.
    pub content: String,
    /// Whether anything was omitted.
    pub truncated: bool,
    /// Line count of the complete file.
    pub total_lines: usize,
    /// Returned line span, 1-indexed and inclusive. `None` for an empty file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub returned_lines: Option<(usize, usize)>,
}

/// Byte offset at which each line of `content` starts.
///
/// A trailing newline ends the last line rather than beginning a new one, so
/// `"a\nb\n"` yields `[0, 2]` — two lines, not three. An empty string yields
/// `[0]`, which callers must treat as zero lines.
fn line_start_offsets(content: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (idx, byte) in content.bytes().enumerate() {
        if byte == b'\n' && idx + 1 < content.len() {
            starts.push(idx + 1);
        }
    }
    starts
}

/// Apply a partial-read selector to `content`.
///
/// Returns `Ok(None)` when `spec` selects nothing, so the caller can return the
/// whole file unchanged. Contradictory specs are an error rather than a guess.
///
/// Slicing happens on line boundaries by byte offset, so the result is always a
/// verbatim substring: line endings and any trailing newline survive intact.
pub fn slice_content(content: &str, spec: &SliceSpec) -> Result<Option<SliceResult>> {
    let starts = line_start_offsets(content);
    let total_lines = if content.is_empty() { 0 } else { starts.len() };

    // Matching the two options as a pair makes the four cases exhaustive, so no
    // arm needs to reason about which selector "must" be set.
    let (start, end, first_line, last_line) = match (spec.head_lines, spec.tail_lines) {
        (Some(_), Some(_)) => {
            return Err(Error::validation_error(
                "Cannot combine head_lines with tail_lines",
            ));
        }
        // No selector: let the caller read the whole file instead.
        (None, None) => return Ok(None),
        (Some(n), None) => {
            let kept = n.min(total_lines);
            // Keeping every line means slicing to the end, trailing newline
            // included; `starts[total_lines]` would be one past the last line.
            let end = if kept >= total_lines {
                content.len()
            } else {
                starts[kept]
            };
            (0, end, 1, kept)
        }
        (None, Some(n)) => {
            let skipped = total_lines.saturating_sub(n);
            // `skipped == total_lines` (n == 0) has no line start to point at.
            let start = starts.get(skipped).copied().unwrap_or(content.len());
            (start, content.len(), skipped + 1, total_lines)
        }
    };

    Ok(Some(SliceResult {
        content: content[start..end].to_string(),
        truncated: start > 0 || end < content.len(),
        total_lines,
        // An empty selection has no meaningful span: `last_line` sits below
        // `first_line` (head_lines: 0 gives 1..0, tail_lines: 0 gives 5..4).
        returned_lines: (first_line <= last_line).then_some((first_line, last_line)),
    }))
}

/// File tools context
#[derive(Clone)]
pub struct FileTools {
    pub manager: Arc<VaultManager>,
}

impl FileTools {
    /// Create new file tools
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self { manager }
    }

    /// Read a file from the vault
    pub async fn read_file(&self, path: &str) -> Result<String> {
        let file_path = PathBuf::from(path);
        self.manager.read_file(&file_path).await
    }

    /// Write a file to the vault with mode support (creates directories as needed)
    ///
    /// `message` is the commit subject the underlying `VaultManager` write
    /// carries: on a git-backed vault it becomes the commit subject; direct
    /// vaults ignore it. Threaded from the MCP layer's `resolve_commit_message`
    /// so a caller-supplied `commit_message` reaches the commit (write-
    /// substrate-layering M4d / R9) instead of a hardcoded auto-subject.
    pub async fn write_file_with_mode(
        &self,
        path: &str,
        content: &str,
        mode: WriteMode,
        expected_hash: Option<&str>,
        message: &str,
    ) -> Result<()> {
        let precondition = Precondition::for_replace(expected_hash, true);
        match mode {
            WriteMode::Overwrite => {
                let file_path = PathBuf::from(path);
                self.manager
                    .write_file(&file_path, content, precondition, message)
                    .await
            }
            WriteMode::Append => {
                let existing = self.read_file(path).await.unwrap_or_default();
                let combined = if existing.is_empty() {
                    content.to_string()
                } else {
                    format!("{}\n{}", existing, content)
                };
                let file_path = PathBuf::from(path);
                self.manager
                    .write_file(&file_path, &combined, precondition, message)
                    .await
            }
            WriteMode::Prepend => {
                let existing = self.read_file(path).await.unwrap_or_default();
                if existing.is_empty() {
                    let file_path = PathBuf::from(path);
                    return self
                        .manager
                        .write_file(&file_path, content, precondition, message)
                        .await;
                }

                // If existing file has frontmatter, insert new content after it
                let combined = if existing.starts_with("---\n") || existing.starts_with("---\r\n") {
                    // Find the closing --- delimiter
                    if let Some(end_idx) = find_frontmatter_end(&existing) {
                        let (frontmatter_block, body) = existing.split_at(end_idx);
                        format!(
                            "{}\n{}\n{}",
                            frontmatter_block.trim_end(),
                            content,
                            body.trim_start()
                        )
                    } else {
                        // Malformed frontmatter - prepend before everything
                        format!("{}\n{}", content, existing)
                    }
                } else {
                    format!("{}\n{}", content, existing)
                };
                let file_path = PathBuf::from(path);
                self.manager
                    .write_file(&file_path, &combined, precondition, message)
                    .await
            }
        }
    }

    /// Write a file to the vault (creates directories as needed) - backward compatible
    pub async fn write_file(&self, path: &str, content: &str) -> Result<()> {
        self.write_file_with_mode(
            path,
            content,
            WriteMode::Overwrite,
            None,
            &format!("write_file {path}"),
        )
        .await
    }

    /// Strict create: write `content` at `path` only when it is currently
    /// absent ([`Precondition::ExpectAbsent`]). Both substrates refuse to
    /// clobber an existing path with a `ConcurrencyError` — the TOCTOU-safe
    /// create the MCP `write_note` create-by-default and `create_from_template`
    /// route through (write-substrate-layering M4d). `message` is the git
    /// commit subject (ignored on direct).
    pub async fn create_file(&self, path: &str, content: &str, message: &str) -> Result<()> {
        self.manager
            .write_file(
                &PathBuf::from(path),
                content,
                Precondition::ExpectAbsent,
                message,
            )
            .await
    }

    /// Edit file using SEARCH/REPLACE blocks (LLM-optimized)
    ///
    /// Uses aider-inspired git merge conflict syntax that reduces LLM laziness by 3X.
    /// Provides fuzzy matching to tolerate minor formatting errors from LLMs.
    pub async fn edit_file(
        &self,
        path: &str,
        edits: &str,
        expected_hash: Option<&str>,
        dry_run: bool,
        message: &str,
    ) -> Result<turbovault_vault::EditResult> {
        let file_path = PathBuf::from(path);
        self.manager
            .edit_file(
                &file_path,
                edits,
                Precondition::for_in_place(expected_hash),
                dry_run,
                message,
            )
            .await
    }

    /// Delete a file from the vault (with audit trail and graph cleanup)
    pub async fn delete_file(&self, path: &str) -> Result<()> {
        self.manager
            .delete_file(
                &PathBuf::from(path),
                Precondition::for_in_place(None),
                &format!("delete_file {path}"),
            )
            .await
    }

    /// Delete a file with optional optimistic concurrency hash check
    pub async fn delete_file_with_hash(
        &self,
        path: &str,
        expected_hash: Option<&str>,
        message: &str,
    ) -> Result<()> {
        self.manager
            .delete_file(
                &PathBuf::from(path),
                Precondition::for_in_place(expected_hash),
                message,
            )
            .await
    }

    /// Move a file within the vault (with audit trail and graph update)
    pub async fn move_file(&self, from: &str, to: &str) -> Result<()> {
        self.manager
            .move_file(
                &PathBuf::from(from),
                &PathBuf::from(to),
                Precondition::for_in_place(None),
                Precondition::Blind,
                &format!("move_file {from} -> {to}"),
            )
            .await
    }

    /// Move a file with optional optimistic concurrency hash check
    pub async fn move_file_with_hash(
        &self,
        from: &str,
        to: &str,
        expected_hash: Option<&str>,
        message: &str,
    ) -> Result<()> {
        self.manager
            .move_file(
                &PathBuf::from(from),
                &PathBuf::from(to),
                Precondition::for_in_place(expected_hash),
                Precondition::Blind,
                message,
            )
            .await
    }

    /// Copy a file within the vault
    pub async fn copy_file(&self, from: &str, to: &str) -> Result<()> {
        let from_path = self.manager.resolve_path(&PathBuf::from(from))?;
        let to_path = self.manager.resolve_path(&PathBuf::from(to))?;

        // Create parent directory if needed
        if let Some(parent) = to_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(Error::io)?;
        }

        // Perform copy
        tokio::fs::copy(&from_path, &to_path)
            .await
            .map_err(Error::io)?;

        Ok(())
    }

    /// Get lightweight metadata for multiple files without reading full content
    pub async fn get_notes_info(&self, paths: &[String]) -> Result<Vec<NoteInfo>> {
        let mut results = Vec::with_capacity(paths.len());

        for path in paths {
            // Use resolve_path for proper path traversal protection
            let full_path = match self.manager.resolve_path(&PathBuf::from(path.as_str())) {
                Ok(p) => p,
                Err(_) => {
                    results.push(NoteInfo {
                        path: path.clone(),
                        exists: false,
                        size_bytes: None,
                        modified_at: None,
                        has_frontmatter: None,
                    });
                    continue;
                }
            };

            match tokio::fs::metadata(&full_path).await {
                Ok(meta) => {
                    let size = meta.len();
                    let modified = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| {
                            chrono::DateTime::from_timestamp(d.as_secs() as i64, d.subsec_nanos())
                                .map(|dt| dt.to_rfc3339())
                                .unwrap_or_default()
                        });

                    // Read only first 5 bytes to detect frontmatter marker "---\n" or "---\r\n"
                    let has_fm = if size >= 4 {
                        match tokio::fs::File::open(&full_path).await {
                            Ok(mut file) => {
                                let mut buf = [0u8; 5];
                                let n = file.read(&mut buf).await.unwrap_or(0);
                                let slice = &buf[..n];
                                Some(slice.starts_with(b"---\n") || slice.starts_with(b"---\r\n"))
                            }
                            Err(_) => None,
                        }
                    } else {
                        Some(false)
                    };

                    results.push(NoteInfo {
                        path: path.clone(),
                        exists: true,
                        size_bytes: Some(size),
                        modified_at: modified,
                        has_frontmatter: has_fm,
                    });
                }
                Err(_) => {
                    results.push(NoteInfo {
                        path: path.clone(),
                        exists: false,
                        size_bytes: None,
                        modified_at: None,
                        has_frontmatter: None,
                    });
                }
            }
        }

        Ok(results)
    }
}

/// Generate an Obsidian URI for a note
///
/// Format: obsidian://open?vault=VaultName&file=path/to/note
pub fn obsidian_uri(vault_name: &str, file_path: &str) -> String {
    // Strip .md extension for Obsidian URI convention
    let file = file_path.strip_suffix(".md").unwrap_or(file_path);
    format!(
        "obsidian://open?vault={}&file={}",
        urlencoding::encode(vault_name),
        urlencoding::encode(file)
    )
}

/// Find the end position of YAML frontmatter block (including closing ---)
/// Returns the byte offset just after the closing "---\n" (or "---\r\n")
fn find_frontmatter_end(content: &str) -> Option<usize> {
    // Skip the opening "---\n" or "---\r\n"
    let start = if content.starts_with("---\r\n") {
        5
    } else if content.starts_with("---\n") {
        4
    } else {
        return None;
    };

    let bytes = content.as_bytes();

    // Helper: check if position `pos` starts a closing delimiter line "---" followed by \n, \r\n, or EOF
    let check_closing = |pos: usize| -> Option<usize> {
        if !bytes[pos..].starts_with(b"---") {
            return None;
        }
        let after_dashes = pos + 3;
        if after_dashes >= bytes.len() {
            return Some(after_dashes); // "---" at EOF
        }
        match bytes[after_dashes] {
            b'\n' => Some(after_dashes + 1),
            b'\r' if after_dashes + 1 < bytes.len() && bytes[after_dashes + 1] == b'\n' => {
                Some(after_dashes + 2)
            }
            _ => None, // "---" followed by other chars, not a delimiter
        }
    };

    // Check if closing delimiter is immediately after opening (empty frontmatter)
    if let Some(end) = check_closing(start) {
        return Some(end);
    }

    // Search for closing delimiter at start of each subsequent line
    let mut i = start;
    while i < bytes.len() {
        let nl_pos = match memchr_newline(bytes, i) {
            Some(pos) => pos,
            None => break,
        };

        // Advance past the newline
        let line_start =
            if bytes[nl_pos] == b'\r' && nl_pos + 1 < bytes.len() && bytes[nl_pos + 1] == b'\n' {
                nl_pos + 2
            } else {
                nl_pos + 1
            };

        if line_start >= bytes.len() {
            break;
        }

        if let Some(end) = check_closing(line_start) {
            return Some(end);
        }

        i = line_start;
    }
    None
}

/// Find next newline byte (\n or \r) starting from `start`
fn memchr_newline(bytes: &[u8], start: usize) -> Option<usize> {
    bytes[start..]
        .iter()
        .position(|&b| b == b'\n' || b == b'\r')
        .map(|p| start + p)
}

/// Split content into frontmatter YAML string and body content
pub fn split_frontmatter(content: &str) -> (Option<String>, String) {
    if !content.starts_with("---\n") && !content.starts_with("---\r\n") {
        return (None, content.to_string());
    }

    let start = if content.starts_with("---\r\n") { 5 } else { 4 };

    if let Some(end) = find_frontmatter_end(content) {
        let body = &content[end..];

        // The closing delimiter starts at end minus the delimiter line length.
        // Find where the closing "---" line begins by searching backwards from `end`
        // for the start of the "---" line.
        let closing_start = if end >= 5
            && content.as_bytes()[end - 1] == b'\n'
            && content.as_bytes()[end - 2] == b'\r'
        {
            // "---\r\n" closing
            end - 5
        } else if end >= 4 && content.as_bytes()[end - 1] == b'\n' {
            // "---\n" closing
            end - 4
        } else {
            // "---" at EOF (no trailing newline)
            end - 3
        };

        let yaml = content[start..closing_start].trim().to_string();

        (
            if yaml.is_empty() { None } else { Some(yaml) },
            body.to_string(),
        )
    } else {
        (None, content.to_string())
    }
}

/// Reconstruct file content from frontmatter and body
pub fn reconstruct_content(
    frontmatter: Option<&serde_json::Map<String, serde_json::Value>>,
    body: &str,
) -> String {
    match frontmatter {
        Some(fm) if !fm.is_empty() => {
            let yaml = yaml_serde::to_string(&fm).unwrap_or_default();
            format!("---\n{}---\n{}", yaml, body)
        }
        _ => body.to_string(),
    }
}

/// Deep merge two serde_json Values (overlay keys win, objects are recursively merged)
pub fn deep_merge(base: &mut serde_json::Value, overlay: serde_json::Value) {
    match (base, overlay) {
        (serde_json::Value::Object(base_map), serde_json::Value::Object(overlay_map)) => {
            for (key, value) in overlay_map {
                let entry = base_map.entry(key).or_insert(serde_json::Value::Null);
                if entry.is_object() && value.is_object() {
                    deep_merge(entry, value);
                } else {
                    *entry = value;
                }
            }
        }
        (base, overlay) => {
            *base = overlay;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_mode_from_str() {
        assert_eq!(WriteMode::from_str_opt(None).unwrap(), WriteMode::Overwrite);
        assert_eq!(
            WriteMode::from_str_opt(Some("overwrite")).unwrap(),
            WriteMode::Overwrite
        );
        assert_eq!(
            WriteMode::from_str_opt(Some("append")).unwrap(),
            WriteMode::Append
        );
        assert_eq!(
            WriteMode::from_str_opt(Some("prepend")).unwrap(),
            WriteMode::Prepend
        );
        assert!(WriteMode::from_str_opt(Some("invalid")).is_err());
    }

    #[test]
    fn test_split_frontmatter_with_fm() {
        let content = "---\ntitle: Test\ntags: [a, b]\n---\n# Body\nHello";
        let (fm, body) = split_frontmatter(content);
        assert!(fm.is_some());
        assert!(fm.unwrap().contains("title: Test"));
        assert!(body.contains("# Body"));
    }

    #[test]
    fn test_split_frontmatter_without_fm() {
        let content = "# No Frontmatter\nJust content";
        let (fm, body) = split_frontmatter(content);
        assert!(fm.is_none());
        assert_eq!(body, content);
    }

    #[test]
    fn test_obsidian_uri() {
        let uri = obsidian_uri("My Vault", "daily/2024-01-15.md");
        assert_eq!(
            uri,
            "obsidian://open?vault=My%20Vault&file=daily%2F2024-01-15"
        );
    }

    #[test]
    fn test_obsidian_uri_no_extension() {
        let uri = obsidian_uri("vault", "folder/note");
        assert_eq!(uri, "obsidian://open?vault=vault&file=folder%2Fnote");
    }

    #[test]
    fn test_deep_merge() {
        let mut base = serde_json::json!({"a": 1, "b": {"c": 2, "d": 3}});
        let overlay = serde_json::json!({"b": {"c": 99, "e": 4}, "f": 5});
        deep_merge(&mut base, overlay);
        assert_eq!(base["a"], 1);
        assert_eq!(base["b"]["c"], 99);
        assert_eq!(base["b"]["d"], 3);
        assert_eq!(base["b"]["e"], 4);
        assert_eq!(base["f"], 5);
    }

    #[test]
    fn test_reconstruct_content() {
        let mut fm = serde_json::Map::new();
        fm.insert("title".to_string(), serde_json::json!("Test"));
        let result = reconstruct_content(Some(&fm), "# Body\nContent\n");
        assert!(result.starts_with("---\n"));
        assert!(result.contains("title: Test"));
        assert!(result.contains("---\n# Body"));
    }

    #[test]
    fn test_reconstruct_content_no_fm() {
        let result = reconstruct_content(None, "# Just body\n");
        assert_eq!(result, "# Just body\n");
    }

    #[test]
    fn test_split_frontmatter_with_dashes_in_yaml_value() {
        // YAML value containing "---" should NOT be treated as closing delimiter
        let content = "---\ntitle: foo---bar\nstatus: draft\n---\n# Body";
        let (fm, body) = split_frontmatter(content);
        assert!(fm.is_some());
        let yaml = fm.unwrap();
        assert!(yaml.contains("foo---bar"));
        assert!(yaml.contains("status: draft"));
        assert_eq!(body, "# Body");
    }

    #[test]
    fn test_split_frontmatter_empty_frontmatter() {
        let content = "---\n---\n# Body";
        let (fm, body) = split_frontmatter(content);
        assert!(fm.is_none());
        assert_eq!(body, "# Body");
    }

    #[test]
    fn test_find_frontmatter_end_at_eof() {
        let content = "---\ntitle: Test\n---";
        let end = find_frontmatter_end(content);
        assert_eq!(end, Some(content.len()));
    }

    // ==================== find_frontmatter_end edge cases ====================

    #[test]
    fn test_find_frontmatter_end_crlf_opening_and_closing() {
        let content = "---\r\ntitle: Test\r\n---\r\n# Body";
        let end = find_frontmatter_end(content);
        // "---\r\n" (5) + "title: Test\r\n" (13) + "---\r\n" (5) = 23
        assert_eq!(end, Some(23));
        assert_eq!(&content[end.unwrap()..], "# Body");
    }

    #[test]
    fn test_find_frontmatter_end_crlf_opening_lf_closing() {
        let content = "---\r\ntitle: Test\n---\n# Body";
        let end = find_frontmatter_end(content);
        assert!(end.is_some());
        assert_eq!(&content[end.unwrap()..], "# Body");
    }

    #[test]
    fn test_find_frontmatter_end_no_closing_delimiter() {
        let content = "---\ntitle: Test\nno closing here\n";
        let end = find_frontmatter_end(content);
        assert_eq!(end, None);
    }

    #[test]
    fn test_find_frontmatter_end_not_frontmatter() {
        let content = "# Just a heading\nNot frontmatter";
        let end = find_frontmatter_end(content);
        assert_eq!(end, None);
    }

    #[test]
    fn test_find_frontmatter_end_dashes_mid_line_not_delimiter() {
        // "foo---bar" on its own line should NOT close frontmatter
        let content = "---\ntitle: Test\nfoo---bar\n---\n# Body";
        let end = find_frontmatter_end(content);
        assert!(end.is_some());
        assert_eq!(&content[end.unwrap()..], "# Body");
    }

    #[test]
    fn test_find_frontmatter_end_dashes_with_trailing_chars() {
        // "---x" should NOT match as closing delimiter
        let content = "---\ntitle: Test\n---x\n---\n# Body";
        let end = find_frontmatter_end(content);
        assert!(end.is_some());
        assert_eq!(&content[end.unwrap()..], "# Body");
    }

    #[test]
    fn test_find_frontmatter_end_immediate_close() {
        // Empty frontmatter: "---\n---\n"
        let content = "---\n---\n# Body";
        let end = find_frontmatter_end(content);
        assert_eq!(end, Some(8));
        assert_eq!(&content[8..], "# Body");
    }

    // ==================== memchr_newline edge cases ====================

    #[test]
    fn test_memchr_newline_finds_lf() {
        let bytes = b"hello\nworld";
        assert_eq!(memchr_newline(bytes, 0), Some(5));
    }

    #[test]
    fn test_memchr_newline_finds_cr() {
        let bytes = b"hello\rworld";
        assert_eq!(memchr_newline(bytes, 0), Some(5));
    }

    #[test]
    fn test_memchr_newline_no_newline() {
        let bytes = b"hello world";
        assert_eq!(memchr_newline(bytes, 0), None);
    }

    #[test]
    fn test_memchr_newline_start_offset() {
        let bytes = b"a\nb\nc";
        assert_eq!(memchr_newline(bytes, 2), Some(3)); // past first \n, finds second
    }

    #[test]
    fn test_memchr_newline_start_at_end() {
        let bytes = b"hello";
        assert_eq!(memchr_newline(bytes, 5), None); // empty slice
    }

    // ==================== split_frontmatter edge cases ====================

    #[test]
    fn test_split_frontmatter_crlf_only() {
        let content = "---\r\ntitle: Test\r\n---\r\n# Body";
        let (fm, body) = split_frontmatter(content);
        assert!(fm.is_some());
        assert!(fm.unwrap().contains("title: Test"));
        assert_eq!(body, "# Body");
    }

    #[test]
    fn test_split_frontmatter_whitespace_only_yaml() {
        let content = "---\n   \n---\n# Body";
        let (fm, body) = split_frontmatter(content);
        assert!(fm.is_none()); // whitespace-only yaml trims to empty
        assert_eq!(body, "# Body");
    }

    #[test]
    fn test_split_frontmatter_body_starts_with_dashes() {
        // Body content has "---" (horizontal rule) — should not be confused with frontmatter
        let content = "---\ntitle: Test\n---\n---\nMore content";
        let (fm, body) = split_frontmatter(content);
        assert!(fm.is_some());
        assert!(fm.unwrap().contains("title: Test"));
        assert_eq!(body, "---\nMore content");
    }

    #[test]
    fn test_split_frontmatter_no_closing_returns_none() {
        let content = "---\ntitle: Test\nno closing\n";
        let (fm, body) = split_frontmatter(content);
        assert!(fm.is_none());
        assert_eq!(body, content);
    }

    #[test]
    fn test_split_frontmatter_closing_at_eof_no_newline() {
        let content = "---\ntitle: Test\n---";
        let (fm, body) = split_frontmatter(content);
        assert!(fm.is_some());
        assert!(fm.unwrap().contains("title: Test"));
        assert_eq!(body, "");
    }

    // ==================== reconstruct_content edge cases ====================

    #[test]
    fn test_reconstruct_content_empty_map() {
        let fm = serde_json::Map::new();
        let result = reconstruct_content(Some(&fm), "# Body\n");
        // Empty map should fall through to just body
        assert_eq!(result, "# Body\n");
    }

    #[test]
    fn test_reconstruct_content_roundtrip() {
        // Write frontmatter, then split it and verify round-trip
        let mut fm = serde_json::Map::new();
        fm.insert("title".to_string(), serde_json::json!("Test"));
        fm.insert("tags".to_string(), serde_json::json!(["a", "b"]));
        let body = "# Body\nContent\n";

        let content = reconstruct_content(Some(&fm), body);
        let (parsed_fm, parsed_body) = split_frontmatter(&content);

        assert!(parsed_fm.is_some());
        let parsed: serde_json::Map<String, serde_json::Value> =
            yaml_serde::from_str(&parsed_fm.unwrap()).unwrap();
        assert_eq!(parsed["title"], "Test");
        assert_eq!(parsed_body.trim(), body.trim());
    }

    #[test]
    fn test_reconstruct_content_special_yaml_chars() {
        let mut fm = serde_json::Map::new();
        fm.insert("url".to_string(), serde_json::json!("http://example.com"));
        fm.insert(
            "desc".to_string(),
            serde_json::json!("has: colons [and] brackets"),
        );
        let result = reconstruct_content(Some(&fm), "body\n");
        assert!(result.starts_with("---\n"));
        // Should be parseable
        let (parsed_fm, _) = split_frontmatter(&result);
        assert!(parsed_fm.is_some());
        let parsed: serde_json::Map<String, serde_json::Value> =
            yaml_serde::from_str(&parsed_fm.unwrap()).unwrap();
        assert_eq!(parsed["url"], "http://example.com");
    }

    // ==================== deep_merge edge cases ====================

    #[test]
    fn test_deep_merge_scalar_overlay_replaces_object() {
        let mut base = serde_json::json!({"a": {"nested": 1}});
        let overlay = serde_json::json!({"a": 42});
        deep_merge(&mut base, overlay);
        assert_eq!(base["a"], 42);
    }

    #[test]
    fn test_deep_merge_object_overlay_replaces_scalar() {
        let mut base = serde_json::json!({"a": 42});
        let overlay = serde_json::json!({"a": {"nested": 1}});
        deep_merge(&mut base, overlay);
        assert_eq!(base["a"]["nested"], 1);
    }

    #[test]
    fn test_deep_merge_null_overlay() {
        let mut base = serde_json::json!({"a": 1});
        let overlay = serde_json::json!({"a": null});
        deep_merge(&mut base, overlay);
        assert!(base["a"].is_null());
    }

    #[test]
    fn test_deep_merge_array_replaced_not_appended() {
        let mut base = serde_json::json!({"tags": ["a", "b"]});
        let overlay = serde_json::json!({"tags": ["c"]});
        deep_merge(&mut base, overlay);
        assert_eq!(base["tags"], serde_json::json!(["c"]));
    }

    #[test]
    fn test_deep_merge_non_object_base() {
        let mut base = serde_json::json!(null);
        let overlay = serde_json::json!({"a": 1});
        deep_merge(&mut base, overlay);
        assert_eq!(base["a"], 1);
    }

    // ---- partial reads: positional selector ----

    /// Four lines, trailing newline. Line starts: 0, 2, 4, 6.
    const FOUR: &str = "a\nb\nc\nd\n";

    fn head(n: usize) -> SliceResult {
        slice_content(
            FOUR,
            &SliceSpec {
                head_lines: Some(n),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap()
    }

    fn tail(n: usize) -> SliceResult {
        slice_content(
            FOUR,
            &SliceSpec {
                tail_lines: Some(n),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap()
    }

    #[test]
    fn test_slice_no_selector_returns_none() {
        assert!(
            slice_content(FOUR, &SliceSpec::default())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_slice_head() {
        let r = head(2);
        assert_eq!(r.content, "a\nb\n");
        assert!(r.truncated);
        assert_eq!(r.total_lines, 4);
        assert_eq!(r.returned_lines, Some((1, 2)));
    }

    #[test]
    fn test_slice_tail() {
        let r = tail(2);
        assert_eq!(r.content, "c\nd\n");
        assert!(r.truncated);
        assert_eq!(r.returned_lines, Some((3, 4)));
    }

    #[test]
    fn test_slice_n_larger_than_file_is_not_truncated() {
        for r in [head(99), tail(99)] {
            assert_eq!(r.content, FOUR);
            assert!(!r.truncated);
            assert_eq!(r.returned_lines, Some((1, 4)));
        }
    }

    #[test]
    fn test_slice_exact_line_count_is_not_truncated() {
        for r in [head(4), tail(4)] {
            assert_eq!(r.content, FOUR);
            assert!(!r.truncated);
        }
    }

    #[test]
    fn test_slice_zero_lines() {
        for r in [head(0), tail(0)] {
            assert_eq!(r.content, "");
            assert!(r.truncated);
            assert_eq!(r.returned_lines, None);
        }
    }

    #[test]
    fn test_slice_empty_file() {
        let r = slice_content(
            "",
            &SliceSpec {
                tail_lines: Some(3),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.content, "");
        assert!(!r.truncated);
        assert_eq!(r.total_lines, 0);
        assert_eq!(r.returned_lines, None);
    }

    #[test]
    fn test_slice_no_trailing_newline() {
        let content = "a\nb\nc";
        let r = slice_content(
            content,
            &SliceSpec {
                tail_lines: Some(2),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.content, "b\nc");
        assert_eq!(r.total_lines, 3);
        assert_eq!(r.returned_lines, Some((2, 3)));
    }

    /// The slice must be a byte-for-byte substring: CRLF endings survive.
    #[test]
    fn test_slice_preserves_crlf() {
        let content = "a\r\nb\r\nc\r\n";
        let r = slice_content(
            content,
            &SliceSpec {
                head_lines: Some(2),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.content, "a\r\nb\r\n");
        assert_eq!(r.total_lines, 3);
    }

    /// Line boundaries never split a UTF-8 codepoint.
    #[test]
    fn test_slice_multibyte_content() {
        let content = "🎒 resources\nzweite Zeile\n";
        let r = slice_content(
            content,
            &SliceSpec {
                tail_lines: Some(1),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.content, "zweite Zeile\n");
    }

    #[test]
    fn test_slice_rejects_head_and_tail_together() {
        let spec = SliceSpec {
            head_lines: Some(1),
            tail_lines: Some(1),
        };
        let err = slice_content(FOUR, &spec).unwrap_err();
        assert!(err.to_string().contains("head_lines"), "got: {err}");
    }

    #[test]
    fn test_line_start_offsets() {
        assert_eq!(line_start_offsets("a\nb\n"), vec![0, 2]);
        assert_eq!(line_start_offsets("a\nb"), vec![0, 2]);
        assert_eq!(line_start_offsets(""), vec![0]);
        assert_eq!(line_start_offsets("\n"), vec![0]);
    }
}
