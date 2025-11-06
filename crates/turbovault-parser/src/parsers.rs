//! OFM parser implementation using pulldown-cmark + custom regex layers

use std::path::{Path, PathBuf};
use turbovault_core::{FileMetadata, Frontmatter, Result, SourcePosition, VaultFile};

mod callouts;
mod embeds;
mod frontmatter_parser;
mod headings;
mod tags;
mod tasks;
mod wikilinks;

pub use self::frontmatter_parser::extract_frontmatter;

/// Main parser for OFM files
#[allow(dead_code)]
pub struct Parser {
    vault_root: PathBuf,
}

impl Parser {
    /// Create a new parser for the given vault root
    pub fn new(vault_root: PathBuf) -> Self {
        Self { vault_root }
    }

    /// Parse a file from path and content
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

    /// Parse all content elements from file
    fn parse_content(&self, vault_file: &mut VaultFile) -> Result<()> {
        let content = &vault_file.content;

        // Step 1: Extract frontmatter
        if let Ok((fm_str, content_without_fm)) = extract_frontmatter(content) {
            if let Some(fm_str) = fm_str {
                vault_file.frontmatter = self.parse_frontmatter(&fm_str)?;
            }
            vault_file.content = content_without_fm;
        }

        let content = &vault_file.content;

        // Step 2: Parse wikilinks and embeds
        vault_file
            .links
            .extend(wikilinks::parse_wikilinks(content, &vault_file.path));
        vault_file
            .links
            .extend(embeds::parse_embeds(content, &vault_file.path));

        // Step 3: Parse tags
        vault_file.tags.extend(tags::parse_tags(content));

        // Step 4: Parse tasks
        vault_file.tasks.extend(tasks::parse_tasks(content));

        // Step 5: Parse callouts
        vault_file
            .callouts
            .extend(callouts::parse_callouts(content));

        // Step 6: Parse headings
        vault_file
            .headings
            .extend(headings::parse_headings(content));

        Ok(())
    }

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
}
