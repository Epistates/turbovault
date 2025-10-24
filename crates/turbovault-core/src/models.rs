//! Core data models representing Obsidian vault elements.
//!
//! These types are designed to be:
//! - **Serializable**: All types derive Serialize/Deserialize
//! - **Debuggable**: Derive Debug for easy inspection
//! - **Cloneable**: `Arc<T>` friendly for shared ownership
//! - **Type-Safe**: Enums replace magic strings
//!
//! The types roughly correspond to Python dataclasses in the reference implementation.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Position in source text (line, column, byte offset)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SourcePosition {
    pub line: usize,
    pub column: usize,
    pub offset: usize,
    pub length: usize,
}

impl SourcePosition {
    /// Create a new source position
    pub fn new(line: usize, column: usize, offset: usize, length: usize) -> Self {
        Self {
            line,
            column,
            offset,
            length,
        }
    }

    /// Create position at start
    pub fn start() -> Self {
        Self {
            line: 0,
            column: 0,
            offset: 0,
            length: 0,
        }
    }
}

/// Type of link in Obsidian content
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LinkType {
    /// Wikilink: `[[Note]]`
    WikiLink,
    /// Embedded note: `![[Note]]`
    Embed,
    /// Block reference: `[[Note#^block]]`
    BlockRef,
    /// Heading reference: `[[Note#Heading]]`
    HeadingRef,
    /// Markdown link: `[text](url)`
    MarkdownLink,
    /// External URL: http://...
    ExternalLink,
}

/// A link in vault content
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Hash)]
pub struct Link {
    pub type_: LinkType,
    pub source_file: PathBuf,
    pub target: String,
    pub display_text: Option<String>,
    pub position: SourcePosition,
    pub resolved_target: Option<PathBuf>,
    pub is_valid: bool,
}

impl Link {
    /// Create a new link
    pub fn new(
        type_: LinkType,
        source_file: PathBuf,
        target: String,
        position: SourcePosition,
    ) -> Self {
        Self {
            type_,
            source_file,
            target,
            display_text: None,
            position,
            resolved_target: None,
            is_valid: true,
        }
    }
}

/// A heading in vault content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heading {
    pub text: String,
    pub level: u8, // 1-6
    pub position: SourcePosition,
    pub anchor: Option<String>,
}

/// A tag in vault content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub name: String,
    pub position: SourcePosition,
    pub is_nested: bool, // #parent/child
}

/// A task item in vault content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskItem {
    pub content: String,
    pub is_completed: bool,
    pub position: SourcePosition,
    pub due_date: Option<String>,
}

/// Type of callout block
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CalloutType {
    Note,
    Tip,
    Info,
    Todo,
    Important,
    Success,
    Question,
    Warning,
    Failure,
    Danger,
    Bug,
    Example,
    Quote,
}

/// A callout block in vault content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Callout {
    pub type_: CalloutType,
    pub title: Option<String>,
    pub content: String,
    pub position: SourcePosition,
    pub is_foldable: bool,
}

/// A block in vault content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub content: String,
    pub block_id: Option<String>,
    pub position: SourcePosition,
    pub type_: String, // paragraph, heading, list_item, etc.
}

/// YAML frontmatter
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frontmatter {
    pub data: HashMap<String, serde_json::Value>,
    pub position: SourcePosition,
}

impl Frontmatter {
    /// Extract tags from frontmatter
    pub fn tags(&self) -> Vec<String> {
        match self.data.get("tags") {
            Some(serde_json::Value::String(s)) => vec![s.clone()],
            Some(serde_json::Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            _ => vec![],
        }
    }

    /// Extract aliases from frontmatter
    pub fn aliases(&self) -> Vec<String> {
        match self.data.get("aliases") {
            Some(serde_json::Value::String(s)) => vec![s.clone()],
            Some(serde_json::Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            _ => vec![],
        }
    }
}

/// File metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetadata {
    pub path: PathBuf,
    pub size: u64,
    pub created_at: f64,
    pub modified_at: f64,
    pub checksum: String,
    pub is_attachment: bool,
}

/// A complete vault file with parsed content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultFile {
    pub path: PathBuf,
    pub content: String,
    pub metadata: FileMetadata,

    // Parsed elements
    pub frontmatter: Option<Frontmatter>,
    pub headings: Vec<Heading>,
    pub links: Vec<Link>,
    pub backlinks: HashSet<Link>,
    pub blocks: Vec<Block>,
    pub tags: Vec<Tag>,
    pub callouts: Vec<Callout>,
    pub tasks: Vec<TaskItem>,

    // Cache status
    pub is_parsed: bool,
    pub parse_error: Option<String>,
    pub last_parsed: Option<f64>,
}

impl VaultFile {
    /// Create a new vault file
    pub fn new(path: PathBuf, content: String, metadata: FileMetadata) -> Self {
        Self {
            path,
            content,
            metadata,
            frontmatter: None,
            headings: vec![],
            links: vec![],
            backlinks: HashSet::new(),
            blocks: vec![],
            tags: vec![],
            callouts: vec![],
            tasks: vec![],
            is_parsed: false,
            parse_error: None,
            last_parsed: None,
        }
    }

    /// Get outgoing links
    pub fn outgoing_links(&self) -> HashSet<&str> {
        self.links
            .iter()
            .filter(|link| matches!(link.type_, LinkType::WikiLink | LinkType::Embed))
            .map(|link| link.target.as_str())
            .collect()
    }

    /// Get headings indexed by text
    pub fn headings_by_text(&self) -> HashMap<&str, &Heading> {
        self.headings.iter().map(|h| (h.text.as_str(), h)).collect()
    }

    /// Get blocks with IDs
    pub fn blocks_with_ids(&self) -> HashMap<&str, &Block> {
        self.blocks
            .iter()
            .filter_map(|b| b.block_id.as_deref().map(|id| (id, b)))
            .collect()
    }

    /// Check if file contains a tag
    pub fn has_tag(&self, tag: &str) -> bool {
        if let Some(fm) = &self.frontmatter
            && fm.tags().contains(&tag.to_string())
        {
            return true;
        }

        self.tags.iter().any(|t| t.name == tag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_position() {
        let pos = SourcePosition::new(5, 10, 100, 20);
        assert_eq!(pos.line, 5);
        assert_eq!(pos.column, 10);
        assert_eq!(pos.offset, 100);
        assert_eq!(pos.length, 20);
    }

    #[test]
    fn test_frontmatter_tags() {
        let mut data = HashMap::new();
        data.insert(
            "tags".to_string(),
            serde_json::Value::Array(vec![
                serde_json::Value::String("rust".to_string()),
                serde_json::Value::String("mcp".to_string()),
            ]),
        );

        let fm = Frontmatter {
            data,
            position: SourcePosition::start(),
        };

        let tags = fm.tags();
        assert_eq!(tags.len(), 2);
        assert!(tags.contains(&"rust".to_string()));
    }
}
