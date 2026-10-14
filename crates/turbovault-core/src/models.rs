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
    /// 1-based line number (0 in [`SourcePosition::start`]).
    pub line: usize,
    /// 1-based column, in bytes from the line start.
    pub column: usize,
    /// Byte offset from the start of the content.
    pub offset: usize,
    /// Length in bytes of the element.
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
#[non_exhaustive]
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
    /// What form the link takes.
    pub type_: LinkType,
    /// The note the link is in.
    pub source_file: PathBuf,
    /// The target as written, with Markdown escapes decoded: `Note`,
    /// `folder/Note#Heading`, `My Note.md`.
    pub target: String,
    /// The alias or link text, if the link has one.
    pub display_text: Option<String>,
    /// Where the whole link sits in the source.
    pub position: SourcePosition,
    /// The note the target resolves to, once resolved.
    pub resolved_target: Option<PathBuf>,
    /// False once resolution finds no note for the target.
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
    /// The heading's text as it renders, without the `#` markers.
    pub text: String,
    /// Heading level, 1 to 6.
    pub level: u8,
    /// Where the heading sits in the source.
    pub position: SourcePosition,
    /// The slug a `[[Note#Heading]]` link uses, if the heading has text.
    pub anchor: Option<String>,
}

/// A tag in vault content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    /// The tag without its `#`, such as `project/alpha`.
    pub name: String,
    /// Where the tag sits in the source.
    pub position: SourcePosition,
    /// Whether the tag has a `/` level, like `#parent/child`.
    pub is_nested: bool,
}

/// A task item in vault content
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TaskItem {
    /// Task description with trailing Obsidian Tasks metadata removed.
    pub content: String,
    /// Whether the checkbox is checked (`x` or `X`).
    pub is_completed: bool,
    /// The character in the task's checkbox: ` ` to do, `x` done, `/` in
    /// progress, `-` cancelled, or a custom status a theme or plugin uses.
    #[serde(default = "TaskItem::unchecked")]
    pub status: char,
    /// Where the task's line sits in the source.
    pub position: SourcePosition,
    /// Created date (`➕`).
    pub created_date: Option<NaiveDate>,
    /// Scheduled date (`⏳`).
    pub scheduled_date: Option<NaiveDate>,
    /// Start date (`🛫`).
    pub start_date: Option<NaiveDate>,
    /// Due date (`📅`).
    pub due_date: Option<NaiveDate>,
    /// Done date (`✅`).
    pub done_date: Option<NaiveDate>,
    /// Cancelled date (`❌`).
    pub cancelled_date: Option<NaiveDate>,
    /// Priority from its emoji; `Normal` when there's none.
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
}

impl TaskItem {
    fn unchecked() -> char {
        ' '
    }

    /// Build a [`TaskItem`] from parsed Obsidian Tasks metadata. Its
    /// `status` is `x` or ` ` from `is_completed`; a builder that saw the
    /// checkbox sets the character it held.
    #[must_use]
    pub fn from_parsed_metadata(
        parsed: crate::task_parser::ParsedTaskMetadata,
        is_completed: bool,
        position: SourcePosition,
    ) -> Self {
        Self {
            content: parsed.description,
            is_completed,
            status: if is_completed { 'x' } else { ' ' },
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
        }
    }
}

/// A task's priority, as the Obsidian Tasks plugin writes it with an emoji.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskPriority {
    /// `⏬`
    Lowest,
    /// `🔽`
    Low,
    /// No priority emoji.
    #[default]
    Normal,
    /// `🔼`
    Medium,
    /// `⏫`
    High,
    /// `🔺`
    Highest,
}

impl TaskPriority {
    /// The emoji that marks this priority; empty for `Normal`.
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

    /// The priority a priority emoji marks; the empty string is `Normal`.
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

    /// [`TaskPriority::from_emoji`] for a single character.
    pub fn from_char(c: char) -> Option<Self> {
        Self::from_emoji(c.encode_utf8(&mut [0; 4]))
    }

    /// Whether `s` is a priority emoji (or empty, for `Normal`).
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

/// Collects a task's parts while the parser walks its list item, then
/// builds the [`TaskItem`].
#[derive(Debug, Clone, Default)]
pub struct TaskBuilder {
    /// The task's text, metadata included; parsed when built.
    pub content: String,
    /// Whether the checkbox is checked.
    pub is_completed: bool,
    /// The checkbox character, when the parser read it.
    pub status: Option<char>,
    /// Where the task's line sits in the source.
    pub position: SourcePosition,
}

impl TaskBuilder {
    /// Parse the collected text into a [`TaskItem`] and reset the builder
    /// for the next task.
    pub fn build(&mut self) -> TaskItem {
        let parsed: ParsedTaskMetadata = crate::task_parser::parse_task_content(&self.content);
        let mut task_item = TaskItem::from_parsed_metadata(
            parsed,
            std::mem::take(&mut self.is_completed),
            std::mem::take(&mut self.position),
        );
        if let Some(status) = self.status.take() {
            task_item.status = status;
        }
        self.content.clear();
        task_item
    }
}

fn parse_date_opt(value: Option<&str>) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value?, "%Y-%m-%d").ok()
}

/// Type of callout block
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CalloutType {
    /// `[!note]`, and any type TurboVault doesn't know.
    Note,
    /// `[!tip]`
    Tip,
    /// `[!info]`
    Info,
    /// `[!todo]`
    Todo,
    /// `[!important]`
    Important,
    /// `[!success]`
    Success,
    /// `[!question]`
    Question,
    /// `[!warning]`
    Warning,
    /// `[!failure]`, also `fail` and `missing`.
    Failure,
    /// `[!danger]`, also `error`.
    Danger,
    /// `[!bug]`
    Bug,
    /// `[!example]`
    Example,
    /// `[!quote]`, also `cite`.
    Quote,
}

/// A callout block in vault content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Callout {
    /// The callout's type, from `[!type]`.
    pub type_: CalloutType,
    /// The text after `[!type]` on the first line, if any.
    pub title: Option<String>,
    /// The callout's body, without the `>` markers.
    pub content: String,
    /// Where the callout sits in the source.
    pub position: SourcePosition,
    /// Whether it has a `+` or `-` fold marker.
    pub is_foldable: bool,
}

/// A block in vault content (Obsidian block reference with ^id)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    /// The block's text.
    pub content: String,
    /// The `^id` a `[[Note#^id]]` link uses, without the `^`.
    pub block_id: Option<String>,
    /// Where the block sits in the source.
    pub position: SourcePosition,
    /// The kind of block: `paragraph`, `heading`, `list_item` and so on.
    pub type_: String,
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
        /// Heading level, 1 to 6.
        level: usize,
        /// The heading's markdown, without the `#` markers.
        content: String,
        /// Its inline elements, parsed.
        inline: Vec<InlineElement>,
        /// The slug a link to the heading uses, if it has text.
        anchor: Option<String>,
    },
    /// A paragraph of text
    Paragraph {
        /// The paragraph's markdown.
        content: String,
        /// Its inline elements, parsed.
        inline: Vec<InlineElement>,
    },
    /// A fenced or indented code block
    Code {
        /// The fence's language, if it names one.
        language: Option<String>,
        /// The code, without the fences.
        content: String,
        /// Line the block starts on.
        start_line: usize,
        /// Line the block ends on.
        end_line: usize,
    },
    /// An ordered or unordered list
    List {
        /// Whether it's numbered.
        ordered: bool,
        /// Its items, in order.
        items: Vec<ListItem>,
    },
    /// A blockquote (> text)
    Blockquote {
        /// The quote's markdown, without the `>` markers.
        content: String,
        /// The blocks inside the quote.
        blocks: Vec<ContentBlock>,
    },
    /// A table with headers and rows
    Table {
        /// The header cells.
        headers: Vec<String>,
        /// Each column's alignment.
        alignments: Vec<TableAlignment>,
        /// The body rows, each a list of cells.
        rows: Vec<Vec<String>>,
    },
    /// An image (standalone, not inline)
    Image {
        /// The image's alt text.
        alt: String,
        /// Its source path or URL.
        src: String,
        /// Its title, if it has one.
        title: Option<String>,
    },
    /// A horizontal rule (---, ***, ___)
    HorizontalRule,
    /// HTML `<details><summary>` block
    Details {
        /// The `<summary>` text.
        summary: String,
        /// The markdown inside the block.
        content: String,
        /// The blocks inside it, parsed.
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
    Text {
        /// The text.
        value: String,
    },
    /// Bold text (**text** or __text__)
    Strong {
        /// The text inside the markers.
        value: String,
    },
    /// Italic text (*text* or _text_)
    Emphasis {
        /// The text inside the markers.
        value: String,
    },
    /// Inline code (`code`)
    Code {
        /// The code inside the backticks.
        value: String,
    },
    /// A link such as `[text](url)`
    Link {
        /// The link's text.
        text: String,
        /// Its destination as written.
        url: String,
        /// Its title, if it has one.
        title: Option<String>,
        /// Relative line offset within parent block (for nested list items)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line_offset: Option<usize>,
    },
    /// An inline image ![alt](src)
    Image {
        /// The image's alt text.
        alt: String,
        /// Its source path or URL.
        src: String,
        /// Its title, if it has one.
        title: Option<String>,
        /// Relative line offset within parent block (for nested list items)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line_offset: Option<usize>,
    },
    /// Strikethrough text (~~text~~)
    Strikethrough {
        /// The text inside the markers.
        value: String,
    },
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
    /// `:---`
    Left,
    /// `:---:`
    Center,
    /// `---:`
    Right,
    /// `---`, no alignment given.
    None,
}

/// YAML frontmatter
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frontmatter {
    /// The properties, keyed by name, with YAML values as JSON.
    pub data: HashMap<String, serde_json::Value>,
    /// Where the frontmatter block sits in the source.
    pub position: SourcePosition,
}

impl Frontmatter {
    /// The note's `tags` property, as Obsidian reads it (see [`property_tags`]).
    pub fn tags(&self) -> Vec<String> {
        property_tags(self.data.get("tags"))
    }

    /// The note's `aliases` property, as Obsidian reads it (see [`property_list`]).
    pub fn aliases(&self) -> Vec<String> {
        property_list(self.data.get("aliases"))
    }
}

/// The values of a list property, read the way Obsidian 1.9 and later read
/// `tags`, `aliases` and `cssclasses`: a YAML list, of which the non-empty
/// strings count. Obsidian ignores any other value, a comma-separated string
/// included, and so does this. Writers that want to keep such a value should
/// convert it to a list rather than read it as one.
pub fn property_list(value: Option<&serde_json::Value>) -> Vec<String> {
    match value {
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str())
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// The values of a `tags` property: [`property_list`], without the `#` a tag
/// may be written with (`- "#project"` is the tag `project`).
pub fn property_tags(value: Option<&serde_json::Value>) -> Vec<String> {
    property_list(value)
        .into_iter()
        .map(|tag| tag.strip_prefix('#').unwrap_or(&tag).to_string())
        .filter(|tag| !tag.is_empty())
        .collect()
}

/// File metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetadata {
    /// The file's path.
    pub path: PathBuf,
    /// Size in bytes.
    pub size: u64,
    /// When the file was created, in seconds since the Unix epoch; its
    /// modification time where the filesystem keeps no creation time, and
    /// 0.0 when unknown.
    pub created_at: f64,
    /// When the file was last modified, in seconds since the Unix epoch;
    /// 0.0 when unknown.
    pub modified_at: f64,
    /// [`crate::compute_hash`] of the content: the same hash `read_note`
    /// reports and a Direct write's `expected_hash` is checked against.
    pub checksum: String,
    /// Whether the file is an attachment rather than a note.
    pub is_attachment: bool,
}

/// A complete vault file with parsed content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultFile {
    /// The file's path.
    pub path: PathBuf,
    /// Its full text.
    pub content: String,
    /// Size, times and content hash.
    pub metadata: FileMetadata,

    /// The YAML frontmatter, if there is any.
    pub frontmatter: Option<Frontmatter>,
    /// Headings, in document order.
    pub headings: Vec<Heading>,
    /// Outgoing links: wikilinks, embeds and Markdown links.
    pub links: Vec<Link>,
    /// Links from other notes to this one, when the caller filled them in.
    pub backlinks: HashSet<Link>,
    /// Blocks with a `^id`.
    pub blocks: Vec<Block>,
    /// Inline `#tags` in the body.
    pub tags: Vec<Tag>,
    /// Callout blocks.
    pub callouts: Vec<Callout>,
    /// Task list items.
    pub tasks: Vec<TaskItem>,

    /// Whether the parsed fields above are filled in.
    pub is_parsed: bool,
    /// Why parsing failed, if it did.
    pub parse_error: Option<String>,
    /// When it was last parsed, in seconds since the Unix epoch.
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

    /// Obsidian 1.9 and later read list properties only as lists.
    #[test]
    fn test_list_properties_read_as_obsidian_reads_them() {
        use serde_json::json;
        assert_eq!(
            property_tags(Some(&json!(["#work", " personal ", "", 3, "#"]))),
            ["work", "personal"]
        );
        assert_eq!(
            property_list(Some(&json!(["Home Base", "HB"]))),
            ["Home Base", "HB"]
        );
        // The pre-1.9 string form, and anything else, is not a list.
        assert!(property_tags(Some(&json!("work, personal"))).is_empty());
        assert!(property_list(Some(&json!("Home Base"))).is_empty());
        assert!(property_list(Some(&json!(null))).is_empty());
        assert!(property_list(None).is_empty());
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
}
