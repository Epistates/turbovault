//! OFM parser implementation using unified ParseEngine.

use std::path::{Path, PathBuf};
use turbovault_core::{FileMetadata, Frontmatter, Result, SourcePosition, VaultFile};

use crate::ParseOptions;
use crate::engine::ParseEngine;

// Individual parser modules are still available for backwards compatibility
// and granular use cases, but the main Parser uses the unified engine.
pub mod callouts;
pub mod embeds;
pub mod frontmatter_parser;
pub mod headings;
pub mod link_utils;
pub mod markdown_links;
pub mod tags;
pub mod tasks;
pub mod wikilinks;

pub use self::frontmatter_parser::extract_frontmatter;

/// Main parser for OFM files.
///
/// Uses the unified ParseEngine internally for efficient, single-source parsing.
pub struct Parser {
    vault_root: PathBuf,
}

impl Parser {
    /// Create a new parser for the given vault root.
    pub fn new(vault_root: PathBuf) -> Self {
        Self { vault_root }
    }

    /// Get the vault root path.
    pub fn vault_root(&self) -> &Path {
        &self.vault_root
    }

    /// Parse a file from path and content.
    pub fn parse_file(&self, path: &Path, content: &str) -> Result<VaultFile> {
        let metadata = self.extract_metadata(path, content)?;
        let mut vault_file = VaultFile::new(path.to_path_buf(), content.to_string(), metadata);

        // Parse content if markdown
        if path.extension().is_some_and(|ext| ext == "md") {
            self.parse_content(&mut vault_file)?;
            vault_file.is_parsed = true;
            vault_file.last_parsed = Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64(),
            );
        }

        Ok(vault_file)
    }

    fn extract_metadata(&self, path: &Path, content: &str) -> Result<FileMetadata> {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let size = content.len() as u64;
        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        let checksum = format!("{:x}", hasher.finish());

        Ok(FileMetadata {
            path: path.to_path_buf(),
            size,
            created_at: 0.0,
            modified_at: 0.0,
            checksum,
            is_attachment: !matches!(
                path.extension().map(|e| e.to_str()),
                Some(Some("md" | "txt"))
            ),
        })
    }

    /// Parse all content elements from file using unified engine.
    fn parse_content(&self, vault_file: &mut VaultFile) -> Result<()> {
        let content = &vault_file.content;

        // Use ParseEngine with source file for vault-aware parsing
        let engine = ParseEngine::with_source_file(content, &vault_file.path);
        let result = engine.parse(&ParseOptions::all());

        // Transfer results to VaultFile
        vault_file.frontmatter = result.frontmatter;

        // Update content without frontmatter (if present)
        // Note: The engine doesn't return stripped content, but for VaultFile
        // we may want to keep original content. For now, preserve behavior.
        if vault_file.frontmatter.is_some()
            && let Ok((_, stripped)) = extract_frontmatter(content)
        {
            vault_file.content = stripped;
        }

        // Links (wikilinks, embeds, markdown links)
        vault_file.links.extend(result.wikilinks);
        vault_file.links.extend(result.embeds);
        vault_file.links.extend(result.markdown_links);

        // Other elements
        vault_file.tags.extend(result.tags);
        vault_file.tasks.extend(result.tasks);
        vault_file.callouts.extend(result.callouts);
        vault_file.headings.extend(result.headings);

        Ok(())
    }

    /// Parse frontmatter from YAML string.
    #[allow(dead_code)]
    fn parse_frontmatter(&self, fm_str: &str) -> Result<Option<Frontmatter>> {
        match serde_yaml::from_str::<serde_json::Value>(fm_str) {
            Ok(serde_json::Value::Object(map)) => {
                let data = map.into_iter().collect();
                Ok(Some(Frontmatter {
                    data,
                    position: SourcePosition::start(),
                }))
            }
            Ok(_) => Ok(None),
            Err(_) => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parser_creation() {
        let parser = Parser::new(PathBuf::from("/vault"));
        assert_eq!(parser.vault_root, PathBuf::from("/vault"));
    }

    #[test]
    fn test_parse_file_complete() {
        let parser = Parser::new(PathBuf::from("/vault"));
        let content = r#"---
title: Test
---

# Heading

[[Link]] and [md](url) with #tag

- [ ] Task

> [!NOTE] Callout
"#;
        let result = parser
            .parse_file(&PathBuf::from("test.md"), content)
            .unwrap();

        assert!(result.frontmatter.is_some());
        assert_eq!(result.headings.len(), 1);
        assert!(result.links.len() >= 2); // wikilink + markdown link
        assert_eq!(result.tags.len(), 1);
        assert_eq!(result.tasks.len(), 1);
        assert_eq!(result.callouts.len(), 1);
    }

    #[test]
    fn test_parse_file_non_markdown() {
        let parser = Parser::new(PathBuf::from("/vault"));
        let content = "[[Link]] #tag";
        let result = parser
            .parse_file(&PathBuf::from("test.txt"), content)
            .unwrap();

        // .txt files are not parsed for OFM elements
        assert!(!result.is_parsed);
    }
}
