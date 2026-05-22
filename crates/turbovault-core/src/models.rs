//! Core data models representing Obsidian vault elements.
//!
//! These types are designed to be:
//! - **Serializable**: All types derive Serialize/Deserialize
//! - **Debuggable**: Derive Debug for easy inspection
//! - **Cloneable**: `Arc<T>` friendly for shared ownership
//! - **Type-Safe**: Enums replace magic strings
//!
//! The types roughly correspond to Python dataclasses in the reference implementation.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;

use crate::task_parser::ParsedTaskMetadata;

/// Position in source text (line, column, byte offset)
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
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

    /// Create position from byte offset by computing line and column.
    ///
    /// This is O(n) where n is the offset - suitable for single-use cases.
    /// For bulk operations, use `from_offset_indexed` with a pre-computed `LineIndex`.
    ///
    /// Line numbers start at 1, column numbers start at 1.
    pub fn from_offset(content: &str, offset: usize, length: usize) -> Self {
        let before = &content[..offset.min(content.len())];
        let line = before.matches('\n').count() + 1;
        let column = before
            .rfind('\n')
            .map(|pos| offset - pos)
            .unwrap_or(offset + 1);

        Self {
            line,
            column,
            offset,
            length,
        }
    }

    /// Create position from byte offset using a pre-computed line index.
    ///
    /// This is O(log n) - use for bulk parsing operations.
    pub fn from_offset_indexed(index: &LineIndex, offset: usize, length: usize) -> Self {
        let (line, column) = index.line_col(offset);
        Self {
            line,
            column,
            offset,
            length,
        }
    }
}

/// Pre-computed line starts for O(log n) line/column lookup.
///
/// Build once per document, then use for all position lookups.
/// This is essential for efficient parsing of documents with many OFM elements.
///
/// # Example
/// ```
/// use turbovault_core::{LineIndex, SourcePosition};
///
/// let content = "Line 1\nLine 2\nLine 3";
/// let index = LineIndex::new(content);
///
/// // O(log n) lookup instead of O(n)
/// let pos = SourcePosition::from_offset_indexed(&index, 7, 6);
/// assert_eq!(pos.line, 2);
/// assert_eq!(pos.column, 1);
/// ```
#[derive(Debug, Clone)]
pub struct LineIndex {
    /// Byte offsets where each line starts (line 1 = index 0)
    line_starts: Vec<usize>,
}

impl LineIndex {
    /// Build line index in O(n) - do once per document.
    pub fn new(content: &str) -> Self {
        let mut line_starts = vec![0];
        for (i, ch) in content.char_indices() {
            if ch == '\n' {
                line_starts.push(i + 1);
            }
        }
        Self { line_starts }
    }

    /// Get (line, column) for a byte offset in O(log n) via binary search.
    ///
    /// Line numbers start at 1, column numbers start at 1.
    pub fn line_col(&self, offset: usize) -> (usize, usize) {
        // Binary search to find which line contains this offset
        let line_idx = self.line_starts.partition_point(|&start| start <= offset);
        let line = line_idx.max(1); // Line numbers are 1-indexed
        let line_start = self
            .line_starts
            .get(line_idx.saturating_sub(1))
            .copied()
            .unwrap_or(0);
        let column = offset - line_start + 1; // Column numbers are 1-indexed
        (line, column)
    }

    /// Get the byte offset where a line starts.
    pub fn line_start(&self, line: usize) -> Option<usize> {
        if line == 0 {
            return None;
        }
        self.line_starts.get(line - 1).copied()
    }

    /// Get total number of lines.
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
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
    /// Heading reference: `[[Note#Heading]]` or `file.md#section`
    HeadingRef,
    /// Same-document anchor: `#section` (no file reference)
    Anchor,
    /// Markdown link: `[text](url)` to relative file
    MarkdownLink,
    /// External URL: `http://...`, `https://...`, `mailto:...`
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
    /// Task description with trailing Obsidian Tasks metadata removed.
    pub content: String,
    pub is_completed: bool,
    pub position: SourcePosition,
    pub created_date: Option<NaiveDate>,
    pub scheduled_date: Option<NaiveDate>,
    pub start_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub done_date: Option<NaiveDate>,
    pub cancelled_date: Option<NaiveDate>,
    #[serde(default)]
    pub priority: TaskPriority,
    /// Recurrence rule text, for example `every weekday`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<String>,
    /// On-completion action, for example `keep` or `delete`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_completion: Option<String>,
    /// Tasks plugin dependency ID without the leading `🆔` marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Tasks plugin dependency IDs from `⛔` or `[dependsOn:: ...]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Inline task tags without the leading `#`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Obsidian block reference without the leading `^`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_ref: Option<String>,
    /// Dataview inline fields parsed from trailing task metadata.
    ///
    /// Standard fields are also projected into the typed fields above; custom
    /// fields remain available here.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
    /// Whether this task's metadata was written in emoji or dataview notation.
    /// Detected at parse time; used to round-trip writes in the original format.
    #[serde(default)]
    pub metadata_format: TaskMetadataFormat,
}

impl TaskItem {
    /// Build a [`TaskItem`] from parsed Obsidian Tasks metadata.
    #[must_use]
    pub fn from_parsed_metadata(
        parsed: crate::task_parser::ParsedTaskMetadata,
        is_completed: bool,
        position: SourcePosition,
    ) -> Self {
        let metadata_format = TaskMetadataFormat::detect(&parsed.metadata);
        Self {
            content: parsed.description,
            is_completed,
            position,
            created_date: parse_date_opt(parsed.created.as_deref()),
            scheduled_date: parse_date_opt(parsed.scheduled.as_deref()),
            start_date: parse_date_opt(parsed.start.as_deref()),
            due_date: parse_date_opt(parsed.due.as_deref()),
            done_date: parse_date_opt(parsed.done.as_deref()),
            cancelled_date: parse_date_opt(parsed.cancelled.as_deref()),
            priority: parsed
                .priority
                .and_then(TaskPriority::from_char)
                .unwrap_or_default(),
            recurrence: parsed.recurrence,
            on_completion: parsed.on_completion,
            id: parsed.id,
            depends_on: parsed.depends_on,
            tags: parsed.tags,
            block_ref: parsed.block_ref,
            metadata: parsed.metadata,
            metadata_format,
        }
    }

    /// Serialize this task back to an Obsidian Tasks markdown line.
    ///
    /// Dispatches to emoji or dataview format based on the format detected at parse time.
    /// Tags are emitted as part of `content` (where the parser left them).
    /// Canonical field order: content priority 🔁 🛫 ⏳ 📅 ✅ ❌
    pub fn to_markdown_line(&self) -> String {
        match self.metadata_format {
            TaskMetadataFormat::Emoji => self.to_markdown_line_emoji(),
            TaskMetadataFormat::Dataview => self.to_markdown_line_dataview(),
        }
    }

    fn to_markdown_line_emoji(&self) -> String {
        let checkbox = if self.is_completed { "[x]" } else { "[ ]" };
        let content = self.content.trim();
        let mut meta: Vec<String> = Vec::new();

        let priority_emoji = self.priority.emoji();
        if !priority_emoji.is_empty() {
            meta.push(priority_emoji.to_string());
        }
        if let Some(ref rec) = self.recurrence {
            meta.push(format!("🔁 {}", rec));
        }
        if let Some(d) = self.start_date {
            meta.push(format!("🛫 {}", d));
        }
        if let Some(d) = self.scheduled_date {
            meta.push(format!("⏳ {}", d));
        }
        if let Some(d) = self.due_date {
            meta.push(format!("📅 {}", d));
        }
        if let Some(d) = self.done_date {
            meta.push(format!("✅ {}", d));
        }
        if let Some(d) = self.cancelled_date {
            meta.push(format!("❌ {}", d));
        }

        if meta.is_empty() {
            format!("- {} {}", checkbox, content)
        } else {
            format!("- {} {} {}", checkbox, content, meta.join(" "))
        }
    }

    fn to_markdown_line_dataview(&self) -> String {
        let checkbox = if self.is_completed { "[x]" } else { "[ ]" };
        let content = self.content.trim();
        let mut meta: Vec<String> = Vec::new();

        if self.priority != TaskPriority::Normal {
            let prio_text = match self.priority {
                TaskPriority::Lowest => "lowest",
                TaskPriority::Low => "low",
                TaskPriority::Medium => "medium",
                TaskPriority::High => "high",
                TaskPriority::Highest => "highest",
                TaskPriority::Normal => unreachable!(),
            };
            meta.push(format!("[priority:: {}]", prio_text));
        }
        if let Some(ref rec) = self.recurrence {
            meta.push(format!("[recurrence:: {}]", rec));
        }
        if let Some(d) = self.start_date {
            meta.push(format!("[start:: {}]", d));
        }
        if let Some(d) = self.scheduled_date {
            meta.push(format!("[scheduled:: {}]", d));
        }
        if let Some(d) = self.due_date {
            meta.push(format!("[due:: {}]", d));
        }
        if let Some(d) = self.done_date {
            meta.push(format!("[done:: {}]", d));
        }
        if let Some(d) = self.cancelled_date {
            meta.push(format!("[cancelled:: {}]", d));
        }

        if meta.is_empty() {
            format!("- {} {}", checkbox, content)
        } else {
            format!("- {} {} {}", checkbox, content, meta.join(" "))
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskPriority {
    Lowest,
    Low,
    #[default]
    Normal,
    Medium,
    High,
    Highest,
}

impl TaskPriority {
    pub fn emoji(&self) -> &'static str {
        match self {
            TaskPriority::Lowest => "⏬",
            TaskPriority::Low => "🔽",
            TaskPriority::Normal => "",
            TaskPriority::Medium => "🔼",
            TaskPriority::High => "⏫",
            TaskPriority::Highest => "🔺",
        }
    }

    pub fn from_emoji(s: &str) -> Option<Self> {
        Some(match s {
            "⏬" => TaskPriority::Lowest,
            "🔽" => TaskPriority::Low,
            "" => TaskPriority::Normal,
            "🔼" => TaskPriority::Medium,
            "⏫" => TaskPriority::High,
            "🔺" => TaskPriority::Highest,
            _ => return None,
        })
    }

    pub fn from_char(c: char) -> Option<Self> {
        Self::from_emoji(c.encode_utf8(&mut [0; 4]))
    }

    pub fn is_valid_emoji(s: &str) -> bool {
        Self::from_emoji(s).is_some()
    }
}

impl fmt::Display for TaskPriority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            TaskPriority::Lowest => "⏬",
            TaskPriority::Low => "🔽",
            TaskPriority::Normal => "",
            TaskPriority::Medium => "🔼",
            TaskPriority::High => "⏫",
            TaskPriority::Highest => "🔺",
        };
        write!(f, "{}", s)
    }
}

/// Whether a task's metadata uses emoji notation (📅 ⏫) or Dataview notation ([due:: …]).
///
/// Detected at parse time by checking whether any standard field keys appear in the
/// raw `metadata` HashMap (dataview fields are stored there; emoji fields are not).
/// Used by `TaskItem::to_markdown_line()` to round-trip writes without converting format.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TaskMetadataFormat {
    #[default]
    Emoji,
    Dataview,
}

impl TaskMetadataFormat {
    fn detect(metadata: &HashMap<String, String>) -> Self {
        const DATAVIEW_KEYS: &[&str] = &[
            "due",
            "scheduled",
            "start",
            "done",
            "completion",
            "cancelled",
            "canceled",
            "created",
            "recurrence",
            "repeat",
            "priority",
        ];
        if DATAVIEW_KEYS.iter().any(|k| metadata.contains_key(*k)) {
            Self::Dataview
        } else {
            Self::Emoji
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TaskBuilder {
    pub content: String,
    pub is_completed: bool,
    pub position: SourcePosition,
}

impl TaskBuilder {
    pub fn build(&mut self) -> TaskItem {
        let parsed: ParsedTaskMetadata = crate::task_parser::parse_task_content(&self.content);
        let task_item = TaskItem::from_parsed_metadata(
            parsed,
            std::mem::take(&mut self.is_completed),
            std::mem::take(&mut self.position),
        );
        self.content.clear();
        task_item
    }
}

fn parse_date_opt(value: Option<&str>) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value?, "%Y-%m-%d").ok()
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

/// A block in vault content (Obsidian block reference with ^id)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub content: String,
    pub block_id: Option<String>,
    pub position: SourcePosition,
    pub type_: String, // paragraph, heading, list_item, etc.
}

// ============================================================================
// Content Block Types (for full markdown parsing)
// ============================================================================

/// A parsed content block in a markdown document.
///
/// These represent the block-level structure of markdown content,
/// similar to an AST but optimized for consumption by tools like treemd.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ContentBlock {
    /// A heading (# H1, ## H2, etc.)
    Heading {
        level: usize,
        content: String,
        inline: Vec<InlineElement>,
        anchor: Option<String>,
    },
    /// A paragraph of text
    Paragraph {
        content: String,
        inline: Vec<InlineElement>,
    },
    /// A fenced or indented code block
    Code {
        language: Option<String>,
        content: String,
        start_line: usize,
        end_line: usize,
    },
    /// An ordered or unordered list
    List { ordered: bool, items: Vec<ListItem> },
    /// A blockquote (> text)
    Blockquote {
        content: String,
        blocks: Vec<ContentBlock>,
    },
    /// A table with headers and rows
    Table {
        headers: Vec<String>,
        alignments: Vec<TableAlignment>,
        rows: Vec<Vec<String>>,
    },
    /// An image (standalone, not inline)
    Image {
        alt: String,
        src: String,
        title: Option<String>,
    },
    /// A horizontal rule (---, ***, ___)
    HorizontalRule,
    /// HTML <details><summary> block
    Details {
        summary: String,
        content: String,
        blocks: Vec<ContentBlock>,
    },
}

impl ContentBlock {
    /// Extract plain text from this content block.
    ///
    /// Returns only the visible text content, stripping markdown syntax.
    /// This is useful for search indexing, accessibility, and accurate word counts.
    ///
    /// # Example
    /// ```
    /// use turbovault_core::{ContentBlock, InlineElement};
    ///
    /// let block = ContentBlock::Paragraph {
    ///     content: "[Overview](#overview) and **bold**".to_string(),
    ///     inline: vec![
    ///         InlineElement::Link {
    ///             text: "Overview".to_string(),
    ///             url: "#overview".to_string(),
    ///             title: None,
    ///             line_offset: None,
    ///         },
    ///         InlineElement::Text { value: " and ".to_string() },
    ///         InlineElement::Strong { value: "bold".to_string() },
    ///     ],
    /// };
    /// assert_eq!(block.to_plain_text(), "Overview and bold");
    /// ```
    #[must_use]
    pub fn to_plain_text(&self) -> String {
        match self {
            Self::Heading { inline, .. } | Self::Paragraph { inline, .. } => {
                inline.iter().map(InlineElement::to_plain_text).collect()
            }
            Self::Code { content, .. } => content.clone(),
            Self::List { items, .. } => items
                .iter()
                .map(ListItem::to_plain_text)
                .collect::<Vec<_>>()
                .join("\n"),
            Self::Blockquote { blocks, .. } => blocks
                .iter()
                .map(Self::to_plain_text)
                .collect::<Vec<_>>()
                .join("\n"),
            Self::Table { headers, rows, .. } => {
                let header_text = headers.join("\t");
                let row_texts: Vec<String> = rows.iter().map(|row| row.join("\t")).collect();
                if row_texts.is_empty() {
                    header_text
                } else {
                    format!("{}\n{}", header_text, row_texts.join("\n"))
                }
            }
            Self::Image { alt, .. } => alt.clone(),
            Self::HorizontalRule => String::new(),
            Self::Details {
                summary, blocks, ..
            } => {
                let blocks_text: String = blocks
                    .iter()
                    .map(Self::to_plain_text)
                    .collect::<Vec<_>>()
                    .join("\n");
                if blocks_text.is_empty() {
                    summary.clone()
                } else {
                    format!("{}\n{}", summary, blocks_text)
                }
            }
        }
    }
}

/// An inline element within a block.
///
/// These represent inline formatting and links within text content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum InlineElement {
    /// Plain text
    Text { value: String },
    /// Bold text (**text** or __text__)
    Strong { value: String },
    /// Italic text (*text* or _text_)
    Emphasis { value: String },
    /// Inline code (`code`)
    Code { value: String },
    /// A link [text](url)
    Link {
        text: String,
        url: String,
        title: Option<String>,
        /// Relative line offset within parent block (for nested list items)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line_offset: Option<usize>,
    },
    /// An inline image ![alt](src)
    Image {
        alt: String,
        src: String,
        title: Option<String>,
        /// Relative line offset within parent block (for nested list items)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line_offset: Option<usize>,
    },
    /// Strikethrough text (~~text~~)
    Strikethrough { value: String },
}

impl InlineElement {
    /// Extract plain text from this inline element.
    ///
    /// Returns only the visible text content, stripping markdown syntax.
    /// For links, returns the link text (not the URL).
    /// For images, returns the alt text.
    ///
    /// # Example
    /// ```
    /// use turbovault_core::InlineElement;
    ///
    /// let link = InlineElement::Link {
    ///     text: "Overview".to_string(),
    ///     url: "#overview".to_string(),
    ///     title: None,
    ///     line_offset: None,
    /// };
    /// assert_eq!(link.to_plain_text(), "Overview");
    /// ```
    #[must_use]
    pub fn to_plain_text(&self) -> &str {
        match self {
            Self::Text { value }
            | Self::Strong { value }
            | Self::Emphasis { value }
            | Self::Code { value }
            | Self::Strikethrough { value } => value,
            Self::Link { text, .. } => text,
            Self::Image { alt, .. } => alt,
        }
    }
}

/// A list item with optional checkbox and nested content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListItem {
    /// For task lists: Some(true) = checked, Some(false) = unchecked, None = not a task
    pub checked: Option<bool>,
    /// Raw text content of the item
    pub content: String,
    /// Parsed inline elements
    pub inline: Vec<InlineElement>,
    /// Nested blocks (e.g., code blocks, sub-lists inside list items)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<ContentBlock>,
}

impl ListItem {
    /// Extract plain text from this list item.
    ///
    /// Returns the visible text content by joining inline elements.
    /// Includes nested block content recursively.
    ///
    /// # Example
    /// ```
    /// use turbovault_core::{ListItem, InlineElement};
    ///
    /// let item = ListItem {
    ///     checked: Some(false),
    ///     content: "Todo item".to_string(),
    ///     inline: vec![InlineElement::Text { value: "Todo item".to_string() }],
    ///     blocks: vec![],
    /// };
    /// assert_eq!(item.to_plain_text(), "Todo item");
    /// ```
    #[must_use]
    pub fn to_plain_text(&self) -> String {
        let mut result = String::new();

        // Extract text from inline elements
        for elem in &self.inline {
            result.push_str(elem.to_plain_text());
        }

        // Include nested blocks
        for block in &self.blocks {
            if !result.is_empty() && !result.ends_with('\n') {
                result.push('\n');
            }
            result.push_str(&block.to_plain_text());
        }

        result
    }
}

/// Table column alignment.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TableAlignment {
    Left,
    Center,
    Right,
    None,
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

    #[test]
    fn test_line_index_single_line() {
        let content = "Hello, world!";
        let index = LineIndex::new(content);

        assert_eq!(index.line_count(), 1);
        assert_eq!(index.line_col(0), (1, 1)); // 'H'
        assert_eq!(index.line_col(7), (1, 8)); // 'w'
    }

    #[test]
    fn test_line_index_multiline() {
        let content = "Line 1\nLine 2\nLine 3";
        let index = LineIndex::new(content);

        assert_eq!(index.line_count(), 3);

        // Line 1
        assert_eq!(index.line_col(0), (1, 1)); // 'L' of Line 1
        assert_eq!(index.line_col(5), (1, 6)); // '1'

        // Line 2 (offset 7 = first char after newline)
        assert_eq!(index.line_col(7), (2, 1)); // 'L' of Line 2
        assert_eq!(index.line_col(13), (2, 7)); // '2'

        // Line 3 (offset 14 = first char after second newline)
        assert_eq!(index.line_col(14), (3, 1)); // 'L' of Line 3
    }

    #[test]
    fn test_line_index_line_start() {
        let content = "Line 1\nLine 2\nLine 3";
        let index = LineIndex::new(content);

        assert_eq!(index.line_start(1), Some(0));
        assert_eq!(index.line_start(2), Some(7));
        assert_eq!(index.line_start(3), Some(14));
        assert_eq!(index.line_start(0), None); // Invalid line
        assert_eq!(index.line_start(4), None); // Beyond content
    }

    #[test]
    fn test_source_position_from_offset() {
        let content = "Line 1\nLine 2 [[Link]] here\nLine 3";

        // Position of [[Link]] starts at offset 14
        let pos = SourcePosition::from_offset(content, 14, 8);
        assert_eq!(pos.line, 2);
        assert_eq!(pos.column, 8); // "Line 2 " = 7 chars, so column 8
        assert_eq!(pos.offset, 14);
        assert_eq!(pos.length, 8);
    }

    #[test]
    fn test_source_position_from_offset_indexed() {
        let content = "Line 1\nLine 2 [[Link]] here\nLine 3";
        let index = LineIndex::new(content);

        // Same test as above but using indexed lookup
        let pos = SourcePosition::from_offset_indexed(&index, 14, 8);
        assert_eq!(pos.line, 2);
        assert_eq!(pos.column, 8);
        assert_eq!(pos.offset, 14);
        assert_eq!(pos.length, 8);
    }

    #[test]
    fn test_source_position_first_line() {
        let content = "[[Link]] at start";

        let pos = SourcePosition::from_offset(content, 0, 8);
        assert_eq!(pos.line, 1);
        assert_eq!(pos.column, 1);
    }

    #[test]
    fn test_line_index_empty_content() {
        let content = "";
        let index = LineIndex::new(content);

        assert_eq!(index.line_count(), 1); // Even empty content has "line 1"
        assert_eq!(index.line_col(0), (1, 1));
    }

    #[test]
    fn test_line_index_trailing_newline() {
        let content = "Line 1\n";
        let index = LineIndex::new(content);

        assert_eq!(index.line_count(), 2); // Line 1 + empty line 2
        assert_eq!(index.line_col(6), (1, 7)); // The newline itself
        assert_eq!(index.line_col(7), (2, 1)); // After newline
    }

    #[test]
    fn test_task_item_from_parsed_metadata() {
        let mut metadata = HashMap::new();
        metadata.insert("project".to_string(), "[[Team Work]]".to_string());

        let task = TaskItem::from_parsed_metadata(
            crate::task_parser::ParsedTaskMetadata {
                description: "Review PR".to_string(),
                due: Some("2026-05-01".to_string()),
                scheduled: Some("2026-04-30".to_string()),
                start: Some("2026-04-29".to_string()),
                done: None,
                cancelled: None,
                created: Some("2026-04-28".to_string()),
                priority: Some('⏫'),
                recurrence: Some("every weekday".to_string()),
                on_completion: Some("delete".to_string()),
                id: Some("pr-123".to_string()),
                depends_on: vec!["abc123".to_string(), "def456".to_string()],
                tags: vec!["review".to_string()],
                block_ref: Some("pr-123".to_string()),
                metadata,
            },
            false,
            SourcePosition::start(),
        );

        assert_eq!(task.content, "Review PR");
        assert_eq!(
            task.due_date.map(|date| date.to_string()).as_deref(),
            Some("2026-05-01")
        );
        assert_eq!(
            task.scheduled_date.map(|date| date.to_string()).as_deref(),
            Some("2026-04-30")
        );
        assert_eq!(
            task.start_date.map(|date| date.to_string()).as_deref(),
            Some("2026-04-29")
        );
        assert_eq!(
            task.created_date.map(|date| date.to_string()).as_deref(),
            Some("2026-04-28")
        );
        assert_eq!(task.priority, TaskPriority::High);
        assert_eq!(task.recurrence.as_deref(), Some("every weekday"));
        assert_eq!(task.on_completion.as_deref(), Some("delete"));
        assert_eq!(task.id.as_deref(), Some("pr-123"));
        assert_eq!(
            task.depends_on,
            vec!["abc123".to_string(), "def456".to_string()]
        );
        assert_eq!(task.tags, vec!["review".to_string()]);
        assert_eq!(task.block_ref.as_deref(), Some("pr-123"));
        assert_eq!(
            task.metadata.get("project").map(String::as_str),
            Some("[[Team Work]]")
        );
    }

    #[test]
    fn test_task_item_deserializes_without_metadata_fields() {
        let task: TaskItem = serde_json::from_str(
            r#"{
                "content": "Legacy task",
                "is_completed": false,
                "position": {
                    "line": 1,
                    "column": 1,
                    "offset": 0,
                    "length": 13
                }
            }"#,
        )
        .unwrap();

        assert_eq!(task.content, "Legacy task");
        assert!(!task.is_completed);
        assert_eq!(task.priority, TaskPriority::Normal);
        assert!(task.due_date.is_none());
        assert!(task.metadata.is_empty());
    }

    #[test]
    fn test_task_item_deserializes_with_metadata_fields() {
        let task: TaskItem = serde_json::from_str(
            r#"{
                "content": "Modern task",
                "is_completed": true,
                "position": {
                    "line": 2,
                    "column": 1,
                    "offset": 14,
                    "length": 42
                },
                "created_date": "2026-04-28",
                "scheduled_date": "2026-04-29",
                "start_date": "2026-04-30",
                "due_date": "2026-05-01",
                "done_date": "2026-05-02",
                "cancelled_date": null,
                "priority": "HIGH",
                "recurrence": "every weekday",
                "on_completion": "delete",
                "id": "task-123",
                "depends_on": ["abc123", "def456"],
                "tags": ["review", "work"],
                "block_ref": "block-123",
                "metadata": {
                    "project": "[[Team Work]]"
                }
            }"#,
        )
        .unwrap();

        assert_eq!(task.content, "Modern task");
        assert!(task.is_completed);
        assert_eq!(
            task.created_date.map(|date| date.to_string()).as_deref(),
            Some("2026-04-28")
        );
        assert_eq!(
            task.scheduled_date.map(|date| date.to_string()).as_deref(),
            Some("2026-04-29")
        );
        assert_eq!(
            task.start_date.map(|date| date.to_string()).as_deref(),
            Some("2026-04-30")
        );
        assert_eq!(
            task.due_date.map(|date| date.to_string()).as_deref(),
            Some("2026-05-01")
        );
        assert_eq!(
            task.done_date.map(|date| date.to_string()).as_deref(),
            Some("2026-05-02")
        );
        assert!(task.cancelled_date.is_none());
        assert_eq!(task.priority, TaskPriority::High);
        assert_eq!(task.recurrence.as_deref(), Some("every weekday"));
        assert_eq!(task.on_completion.as_deref(), Some("delete"));
        assert_eq!(task.id.as_deref(), Some("task-123"));
        assert_eq!(
            task.depends_on,
            vec!["abc123".to_string(), "def456".to_string()]
        );
        assert_eq!(task.tags, vec!["review".to_string(), "work".to_string()]);
        assert_eq!(task.block_ref.as_deref(), Some("block-123"));
        assert_eq!(
            task.metadata.get("project").map(String::as_str),
            Some("[[Team Work]]")
        );
    }

    #[test]
    fn test_task_priority_serializes_as_stable_api_value() {
        assert_eq!(
            serde_json::to_value(TaskPriority::High).unwrap(),
            serde_json::Value::String("HIGH".to_string())
        );
    }

    // ==================== TaskItem::to_markdown_line() ====================

    fn make_task(content: &str, is_completed: bool) -> TaskItem {
        TaskItem {
            content: content.to_string(),
            is_completed,
            position: SourcePosition::default(),
            created_date: None,
            scheduled_date: None,
            start_date: None,
            due_date: None,
            done_date: None,
            cancelled_date: None,
            priority: TaskPriority::Normal,
            recurrence: None,
            on_completion: None,
            id: None,
            depends_on: Vec::new(),
            tags: Vec::new(),
            block_ref: None,
            metadata: HashMap::new(),
            metadata_format: TaskMetadataFormat::Emoji,
        }
    }

    #[test]
    fn test_to_markdown_line_simple_pending() {
        let task = make_task("Take out the trash", false);
        assert_eq!(task.to_markdown_line(), "- [ ] Take out the trash");
    }

    #[test]
    fn test_to_markdown_line_simple_completed() {
        let task = make_task("Submit expense report", true);
        assert_eq!(task.to_markdown_line(), "- [x] Submit expense report");
    }

    #[test]
    fn test_to_markdown_line_emoji_full() {
        use chrono::NaiveDate;
        let mut task = make_task("Take out the trash", false);
        task.priority = TaskPriority::Highest;
        task.recurrence = Some("every day".to_string());
        task.start_date = NaiveDate::from_ymd_opt(2026, 4, 30);
        task.scheduled_date = NaiveDate::from_ymd_opt(2026, 4, 30);
        task.due_date = NaiveDate::from_ymd_opt(2026, 4, 30);
        assert_eq!(
            task.to_markdown_line(),
            "- [ ] Take out the trash 🔺 🔁 every day 🛫 2026-04-30 ⏳ 2026-04-30 📅 2026-04-30"
        );
    }

    #[test]
    fn test_to_markdown_line_emoji_completed_with_dates() {
        use chrono::NaiveDate;
        let mut task = make_task("Submit expense report", true);
        task.due_date = NaiveDate::from_ymd_opt(2026, 4, 28);
        task.done_date = NaiveDate::from_ymd_opt(2026, 4, 29);
        assert_eq!(
            task.to_markdown_line(),
            "- [x] Submit expense report 📅 2026-04-28 ✅ 2026-04-29"
        );
    }

    #[test]
    fn test_to_markdown_line_emoji_with_tag_in_content() {
        use chrono::NaiveDate;
        let mut task = make_task("Clean the kitchen #task_type_1", false);
        task.tags = vec!["task_type_1".to_string()];
        task.recurrence = Some("every week".to_string());
        task.start_date = NaiveDate::from_ymd_opt(2026, 4, 30);
        task.scheduled_date = NaiveDate::from_ymd_opt(2026, 4, 30);
        task.due_date = NaiveDate::from_ymd_opt(2026, 4, 30);
        assert_eq!(
            task.to_markdown_line(),
            "- [ ] Clean the kitchen #task_type_1 🔁 every week 🛫 2026-04-30 ⏳ 2026-04-30 📅 2026-04-30"
        );
    }

    #[test]
    fn test_to_markdown_line_dataview_format() {
        use chrono::NaiveDate;
        let mut task = make_task("Buy groceries #errands", false);
        task.metadata_format = TaskMetadataFormat::Dataview;
        task.priority = TaskPriority::Medium;
        task.due_date = NaiveDate::from_ymd_opt(2026, 5, 1);
        task.tags = vec!["errands".to_string()];
        assert_eq!(
            task.to_markdown_line(),
            "- [ ] Buy groceries #errands [priority:: medium] [due:: 2026-05-01]"
        );
    }

    #[test]
    fn test_to_markdown_line_dataview_full() {
        use chrono::NaiveDate;
        let mut task = make_task("Plan sprint", false);
        task.metadata_format = TaskMetadataFormat::Dataview;
        task.priority = TaskPriority::High;
        task.start_date = NaiveDate::from_ymd_opt(2026, 5, 1);
        task.due_date = NaiveDate::from_ymd_opt(2026, 5, 7);
        assert_eq!(
            task.to_markdown_line(),
            "- [ ] Plan sprint [priority:: high] [start:: 2026-05-01] [due:: 2026-05-07]"
        );
    }

    #[test]
    fn test_metadata_format_detection_emoji() {
        let metadata = HashMap::new();
        assert_eq!(
            TaskMetadataFormat::detect(&metadata),
            TaskMetadataFormat::Emoji
        );
    }

    #[test]
    fn test_metadata_format_detection_dataview_due() {
        let mut metadata = HashMap::new();
        metadata.insert("due".to_string(), "2026-05-01".to_string());
        assert_eq!(
            TaskMetadataFormat::detect(&metadata),
            TaskMetadataFormat::Dataview
        );
    }

    #[test]
    fn test_metadata_format_detection_custom_key_not_dataview() {
        let mut metadata = HashMap::new();
        metadata.insert("project".to_string(), "myproject".to_string());
        assert_eq!(
            TaskMetadataFormat::detect(&metadata),
            TaskMetadataFormat::Emoji
        );
    }
}
