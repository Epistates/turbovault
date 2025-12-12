//! Block-level content parsing for markdown documents.
//!
//! This module provides full block-level parsing using pulldown-cmark,
//! producing a structured representation of markdown content including:
//! - Paragraphs, headings, code blocks
//! - Lists (ordered, unordered, task lists)
//! - Tables, blockquotes, images
//! - HTML details blocks
//!
//! The parser handles inline formatting within blocks, producing
//! `InlineElement` vectors for text content.

use pulldown_cmark::{
    Alignment as CmarkAlignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd,
};
use regex::Regex;
use std::sync::LazyLock;
use turbovault_core::{ContentBlock, InlineElement, ListItem, TableAlignment};

// ============================================================================
// Wikilink preprocessing (converts [[x]] to [x](wikilink:x) for pulldown-cmark)
// ============================================================================

/// Regex for wikilinks: [[target]] or [[target|alias]]
static WIKILINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\]|]+)(?:\|([^\]]+))?\]\]").unwrap());

/// Preprocess wikilinks to standard markdown links with wikilink: prefix.
/// This allows pulldown-cmark to parse them as regular links.
fn preprocess_wikilinks(markdown: &str) -> String {
    WIKILINK_RE
        .replace_all(markdown, |caps: &regex::Captures| {
            let target = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
            let alias = caps.get(2).map(|m| m.as_str().trim());
            let display_text = alias.unwrap_or(target);
            format!("[{}](wikilink:{})", display_text, target)
        })
        .to_string()
}

/// Regex for links with spaces in URL (not valid CommonMark but common in wikis)
static LINK_WITH_SPACES_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\(([^)<>]+\s[^)<>]*)\)").unwrap());

/// Preprocess links with spaces to angle bracket syntax.
fn preprocess_links_with_spaces(markdown: &str) -> String {
    LINK_WITH_SPACES_RE
        .replace_all(markdown, |caps: &regex::Captures| {
            let text = &caps[1];
            let url = &caps[2];
            if url.contains(' ') {
                format!("[{}](<{}>)", text, url)
            } else {
                caps[0].to_string()
            }
        })
        .to_string()
}

// ============================================================================
// Details block extraction (HTML <details><summary>)
// ============================================================================

/// Extract HTML <details> blocks and replace with placeholders.
fn extract_details_blocks(markdown: &str) -> (String, Vec<ContentBlock>) {
    let mut details_blocks = Vec::new();
    let mut result = String::new();
    let mut current_pos = 0;

    while current_pos < markdown.len() {
        if markdown[current_pos..].starts_with("<details")
            && let Some(tag_end) = markdown[current_pos..].find('>')
            && let details_start = current_pos + tag_end + 1
            && let Some(details_end_pos) = markdown[details_start..].find("</details>")
        {
            let details_end = details_start + details_end_pos;
            let details_content = &markdown[details_start..details_end];

            // Extract summary
            let summary = extract_summary(details_content);

            // Extract content after </summary>
            let content_start = if let Some(summary_end_pos) = details_content.find("</summary>") {
                let summary_tag_end = summary_end_pos + "</summary>".len();
                &details_content[summary_tag_end..]
            } else {
                details_content
            };

            let content_trimmed = content_start.trim();

            // Parse nested content
            let nested_blocks = if !content_trimmed.is_empty() {
                parse_blocks(content_trimmed)
            } else {
                Vec::new()
            };

            details_blocks.push(ContentBlock::Details {
                summary,
                content: content_trimmed.to_string(),
                blocks: nested_blocks,
            });

            result.push_str(&format!("\n[DETAILS_BLOCK_{}]\n", details_blocks.len() - 1));
            current_pos = details_end + "</details>".len();
            continue;
        }

        if let Some(ch) = markdown[current_pos..].chars().next() {
            result.push(ch);
            current_pos += ch.len_utf8();
        } else {
            break;
        }
    }

    (result, details_blocks)
}

/// Extract summary text from details content.
fn extract_summary(details_content: &str) -> String {
    if let Some(summary_start_pos) = details_content.find("<summary")
        && let Some(summary_tag_end) = details_content[summary_start_pos..].find('>')
        && let summary_content_start = summary_start_pos + summary_tag_end + 1
        && let Some(summary_end_pos) = details_content[summary_content_start..].find("</summary>")
    {
        let summary_end = summary_content_start + summary_end_pos;
        return details_content[summary_content_start..summary_end]
            .trim()
            .to_string();
    }
    String::new()
}

// ============================================================================
// Parser state machine
// ============================================================================

struct BlockParserState {
    current_line: usize,
    paragraph_buffer: String,
    inline_buffer: Vec<InlineElement>,
    list_items: Vec<ListItem>,
    list_ordered: bool,
    list_depth: usize,
    item_depth: usize,
    task_list_marker: Option<bool>,
    saved_task_markers: Vec<Option<bool>>,
    item_blocks: Vec<ContentBlock>,
    code_buffer: String,
    code_language: Option<String>,
    code_start_line: usize,
    blockquote_buffer: String,
    table_headers: Vec<String>,
    table_alignments: Vec<TableAlignment>,
    table_rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    heading_level: Option<usize>,
    heading_buffer: String,
    heading_inline: Vec<InlineElement>,
    in_paragraph: bool,
    in_list: bool,
    in_code: bool,
    in_blockquote: bool,
    in_table: bool,
    in_heading: bool,
    in_strong: bool,
    in_emphasis: bool,
    in_strikethrough: bool,
    in_code_inline: bool,
    in_link: bool,
    link_url: String,
    link_text: String,
    image_in_link: bool,
    in_image: bool,
    saved_link_url: String,
}

impl BlockParserState {
    fn new(start_line: usize) -> Self {
        Self {
            current_line: start_line,
            paragraph_buffer: String::new(),
            inline_buffer: Vec::new(),
            list_items: Vec::new(),
            list_ordered: false,
            list_depth: 0,
            item_depth: 0,
            task_list_marker: None,
            saved_task_markers: Vec::new(),
            item_blocks: Vec::new(),
            code_buffer: String::new(),
            code_language: None,
            code_start_line: 0,
            blockquote_buffer: String::new(),
            table_headers: Vec::new(),
            table_alignments: Vec::new(),
            table_rows: Vec::new(),
            current_row: Vec::new(),
            heading_level: None,
            heading_buffer: String::new(),
            heading_inline: Vec::new(),
            in_paragraph: false,
            in_list: false,
            in_code: false,
            in_blockquote: false,
            in_table: false,
            in_heading: false,
            in_strong: false,
            in_emphasis: false,
            in_strikethrough: false,
            in_code_inline: false,
            in_link: false,
            link_url: String::new(),
            link_text: String::new(),
            image_in_link: false,
            in_image: false,
            saved_link_url: String::new(),
        }
    }

    fn finalize(&mut self, blocks: &mut Vec<ContentBlock>) {
        self.flush_paragraph(blocks);
        self.flush_list(blocks);
        self.flush_code(blocks);
        self.flush_blockquote(blocks);
        self.flush_table(blocks);
    }

    fn flush_paragraph(&mut self, blocks: &mut Vec<ContentBlock>) {
        if self.in_paragraph && !self.paragraph_buffer.is_empty() {
            blocks.push(ContentBlock::Paragraph {
                content: self.paragraph_buffer.clone(),
                inline: self.inline_buffer.clone(),
            });
            self.paragraph_buffer.clear();
            self.inline_buffer.clear();
            self.in_paragraph = false;
        }
    }

    fn flush_list(&mut self, blocks: &mut Vec<ContentBlock>) {
        if self.in_list && !self.list_items.is_empty() {
            blocks.push(ContentBlock::List {
                ordered: self.list_ordered,
                items: self.list_items.clone(),
            });
            self.list_items.clear();
            self.in_list = false;
        }
    }

    fn flush_code(&mut self, blocks: &mut Vec<ContentBlock>) {
        if self.in_code && !self.code_buffer.is_empty() {
            blocks.push(ContentBlock::Code {
                language: self.code_language.clone(),
                content: self.code_buffer.trim_end().to_string(),
                start_line: self.code_start_line,
                end_line: self.current_line,
            });
            self.code_buffer.clear();
            self.code_language = None;
            self.in_code = false;
        }
    }

    fn flush_blockquote(&mut self, blocks: &mut Vec<ContentBlock>) {
        if self.in_blockquote && !self.blockquote_buffer.is_empty() {
            let nested_blocks = parse_blocks(&self.blockquote_buffer);
            blocks.push(ContentBlock::Blockquote {
                content: self.blockquote_buffer.clone(),
                blocks: nested_blocks,
            });
            self.blockquote_buffer.clear();
            self.in_blockquote = false;
        }
    }

    fn flush_table(&mut self, blocks: &mut Vec<ContentBlock>) {
        if self.in_table && !self.table_headers.is_empty() {
            blocks.push(ContentBlock::Table {
                headers: self.table_headers.clone(),
                alignments: self.table_alignments.clone(),
                rows: self.table_rows.clone(),
            });
            self.table_headers.clear();
            self.table_alignments.clear();
            self.table_rows.clear();
            self.current_row.clear();
            self.paragraph_buffer.clear();
            self.inline_buffer.clear();
            self.in_table = false;
        }
    }

    fn add_inline_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        let element = if self.in_code_inline {
            InlineElement::Code {
                value: text.to_string(),
            }
        } else if self.in_strong {
            InlineElement::Strong {
                value: text.to_string(),
            }
        } else if self.in_emphasis {
            InlineElement::Emphasis {
                value: text.to_string(),
            }
        } else if self.in_strikethrough {
            InlineElement::Strikethrough {
                value: text.to_string(),
            }
        } else {
            InlineElement::Text {
                value: text.to_string(),
            }
        };

        self.inline_buffer.push(element);
        self.paragraph_buffer.push_str(text);
    }
}

// ============================================================================
// Event processing
// ============================================================================

#[allow(clippy::too_many_lines)]
fn process_event(event: Event, state: &mut BlockParserState, blocks: &mut Vec<ContentBlock>) {
    match event {
        Event::Start(Tag::Paragraph) => {
            state.in_paragraph = true;
        }
        Event::End(TagEnd::Paragraph) => {
            if state.item_depth >= 1 && state.in_paragraph && !state.paragraph_buffer.is_empty() {
                state.item_blocks.push(ContentBlock::Paragraph {
                    content: state.paragraph_buffer.clone(),
                    inline: state.inline_buffer.clone(),
                });
                state.paragraph_buffer.clear();
                state.inline_buffer.clear();
                state.in_paragraph = false;
            } else {
                state.flush_paragraph(blocks);
            }
        }
        Event::Start(Tag::CodeBlock(kind)) => {
            state.in_code = true;
            state.code_start_line = state.current_line;
            state.code_language = match kind {
                CodeBlockKind::Fenced(lang) => {
                    if lang.is_empty() {
                        None
                    } else {
                        Some(lang.to_string())
                    }
                }
                CodeBlockKind::Indented => None,
            };
        }
        Event::End(TagEnd::CodeBlock) => {
            if state.item_depth >= 1 && state.in_code && !state.code_buffer.is_empty() {
                state.item_blocks.push(ContentBlock::Code {
                    language: state.code_language.clone(),
                    content: state.code_buffer.trim_end().to_string(),
                    start_line: state.code_start_line,
                    end_line: state.current_line,
                });
                state.code_buffer.clear();
                state.code_language = None;
                state.in_code = false;
            } else {
                state.flush_code(blocks);
            }
        }
        Event::Start(Tag::List(start_number)) => {
            state.list_depth += 1;
            if state.list_depth == 1 {
                state.in_list = true;
                state.list_ordered = start_number.is_some();
            }
        }
        Event::End(TagEnd::List(_)) => {
            state.list_depth = state.list_depth.saturating_sub(1);
            if state.list_depth == 0 {
                state.flush_list(blocks);
            }
        }
        Event::Start(Tag::Item) => {
            state.item_depth += 1;
            if state.item_depth > 1 {
                state.saved_task_markers.push(state.task_list_marker);
                state.task_list_marker = None;
            }
            if state.item_depth == 1 {
                state.paragraph_buffer.clear();
                state.inline_buffer.clear();
                state.item_blocks.clear();
            }
        }
        Event::End(TagEnd::Item) => {
            if state.item_depth > 1
                && let Some(saved) = state.saved_task_markers.pop()
            {
                state.task_list_marker = saved;
            }
            if state.item_depth == 1 {
                let (content, inline, remaining_blocks) = if !state.paragraph_buffer.is_empty() {
                    let all_blocks: Vec<ContentBlock> = state.item_blocks.drain(..).collect();
                    (
                        state.paragraph_buffer.clone(),
                        state.inline_buffer.clone(),
                        all_blocks,
                    )
                } else if let Some(ContentBlock::Paragraph { content, inline }) =
                    state.item_blocks.first().cloned()
                {
                    let remaining: Vec<ContentBlock> = state.item_blocks.drain(1..).collect();
                    (content, inline, remaining)
                } else {
                    let all_blocks: Vec<ContentBlock> = state.item_blocks.drain(..).collect();
                    (String::new(), Vec::new(), all_blocks)
                };

                state.list_items.push(ListItem {
                    checked: state.task_list_marker,
                    content,
                    inline,
                    blocks: remaining_blocks,
                });
                state.paragraph_buffer.clear();
                state.inline_buffer.clear();
                state.item_blocks.clear();
                state.task_list_marker = None;
            }
            state.item_depth = state.item_depth.saturating_sub(1);
        }
        Event::TaskListMarker(checked) => {
            state.task_list_marker = Some(checked);
        }
        Event::Start(Tag::BlockQuote(_)) => {
            state.in_blockquote = true;
        }
        Event::End(TagEnd::BlockQuote(_)) => {
            state.flush_blockquote(blocks);
        }
        Event::Start(Tag::Table(alignments)) => {
            state.in_table = true;
            state.table_alignments = alignments
                .iter()
                .map(|a| match a {
                    CmarkAlignment::Left => TableAlignment::Left,
                    CmarkAlignment::Center => TableAlignment::Center,
                    CmarkAlignment::Right => TableAlignment::Right,
                    CmarkAlignment::None => TableAlignment::None,
                })
                .collect();
        }
        Event::End(TagEnd::Table) => {
            state.flush_table(blocks);
        }
        Event::Start(Tag::TableHead) => {}
        Event::End(TagEnd::TableHead) => {
            state.table_headers = state.current_row.clone();
            state.current_row.clear();
        }
        Event::Start(Tag::TableRow) => {}
        Event::End(TagEnd::TableRow) => {
            state.table_rows.push(state.current_row.clone());
            state.current_row.clear();
        }
        Event::Start(Tag::TableCell) => {
            state.paragraph_buffer.clear();
            state.inline_buffer.clear();
        }
        Event::End(TagEnd::TableCell) => {
            state.current_row.push(state.paragraph_buffer.clone());
            state.paragraph_buffer.clear();
            state.inline_buffer.clear();
        }
        Event::Start(Tag::Strong) => {
            state.in_strong = true;
        }
        Event::End(TagEnd::Strong) => {
            state.in_strong = false;
        }
        Event::Start(Tag::Emphasis) => {
            state.in_emphasis = true;
        }
        Event::End(TagEnd::Emphasis) => {
            state.in_emphasis = false;
        }
        Event::Start(Tag::Strikethrough) => {
            state.in_strikethrough = true;
        }
        Event::End(TagEnd::Strikethrough) => {
            state.in_strikethrough = false;
        }
        Event::Code(text) => {
            state.in_code_inline = true;
            state.add_inline_text(&text);
            state.in_code_inline = false;
        }
        Event::Start(Tag::Link { dest_url, .. }) => {
            state.in_link = true;
            state.link_url = dest_url.to_string();
            state.link_text.clear();
        }
        Event::End(TagEnd::Link) => {
            state.in_link = false;

            if state.image_in_link {
                state.inline_buffer.push(InlineElement::Link {
                    text: state.link_text.clone(),
                    url: state.saved_link_url.clone(),
                    title: None,
                });
                state
                    .paragraph_buffer
                    .push_str(&format!("[{}]({})", state.link_text, state.saved_link_url));
            } else {
                state.inline_buffer.push(InlineElement::Link {
                    text: state.link_text.clone(),
                    url: state.link_url.clone(),
                    title: None,
                });
                state
                    .paragraph_buffer
                    .push_str(&format!("[{}]({})", state.link_text, state.link_url));
            }

            state.link_text.clear();
            state.link_url.clear();
            state.saved_link_url.clear();
            state.image_in_link = false;
        }
        Event::Start(Tag::Image {
            dest_url, title, ..
        }) => {
            if state.in_link {
                state.image_in_link = true;
                state.saved_link_url = state.link_url.clone();
            }
            state.in_image = true;
            state.link_url = dest_url.to_string();
            state.link_text.clear();
            state.paragraph_buffer = title.to_string();
        }
        Event::End(TagEnd::Image) => {
            state.in_image = false;

            if !state.image_in_link {
                // Capture title before we modify paragraph_buffer
                let title = if state.paragraph_buffer.is_empty() {
                    None
                } else {
                    Some(state.paragraph_buffer.clone())
                };

                if state.in_paragraph {
                    // Reset paragraph_buffer for image representation
                    state.paragraph_buffer.clear();
                    state.inline_buffer.push(InlineElement::Image {
                        alt: state.link_text.clone(),
                        src: state.link_url.clone(),
                        title,
                    });
                    // Add image placeholder to paragraph content
                    state
                        .paragraph_buffer
                        .push_str(&format!("![{}]({})", state.link_text, state.link_url));
                } else {
                    state.flush_paragraph(blocks);
                    blocks.push(ContentBlock::Image {
                        alt: state.link_text.clone(),
                        src: state.link_url.clone(),
                        title,
                    });
                    state.paragraph_buffer.clear();
                }

                state.link_text.clear();
                state.link_url.clear();
            }
        }
        Event::Text(text) => {
            if state.in_code {
                state.code_buffer.push_str(&text);
            } else if state.in_blockquote {
                state.blockquote_buffer.push_str(&text);
            } else if state.in_heading {
                state.heading_buffer.push_str(&text);
                let element = if state.in_code_inline {
                    InlineElement::Code {
                        value: text.to_string(),
                    }
                } else if state.in_strong {
                    InlineElement::Strong {
                        value: text.to_string(),
                    }
                } else if state.in_emphasis {
                    InlineElement::Emphasis {
                        value: text.to_string(),
                    }
                } else {
                    InlineElement::Text {
                        value: text.to_string(),
                    }
                };
                state.heading_inline.push(element);
            } else if state.in_link || state.in_image {
                state.link_text.push_str(&text);
            } else {
                if state.in_list && state.item_depth > 1 {
                    if !state.paragraph_buffer.is_empty() && !state.paragraph_buffer.ends_with('\n')
                    {
                        state.paragraph_buffer.push('\n');
                    }
                    let indent = "  ".repeat(state.item_depth - 1);
                    state.paragraph_buffer.push_str(&indent);

                    if let Some(checked) = state.task_list_marker {
                        let marker = if checked { "[x] " } else { "[ ] " };
                        state.paragraph_buffer.push_str(marker);
                        state.task_list_marker = None;
                    }
                }
                state.add_inline_text(&text);
            }
        }
        Event::SoftBreak => {
            if state.in_paragraph {
                state.paragraph_buffer.push(' ');
                state.inline_buffer.push(InlineElement::Text {
                    value: " ".to_string(),
                });
            }
        }
        Event::HardBreak => {
            if state.in_paragraph {
                state.paragraph_buffer.push('\n');
                state.inline_buffer.push(InlineElement::Text {
                    value: "\n".to_string(),
                });
            }
        }
        Event::Rule => {
            state.flush_paragraph(blocks);
            blocks.push(ContentBlock::HorizontalRule);
        }
        Event::Start(Tag::Heading { level, .. }) => {
            state.flush_paragraph(blocks);
            state.in_heading = true;
            state.heading_level = Some(level as usize);
            state.heading_buffer.clear();
            state.heading_inline.clear();
        }
        Event::End(TagEnd::Heading(_)) => {
            if state.in_heading
                && !state.heading_buffer.is_empty()
                && let Some(level) = state.heading_level
            {
                let anchor = Some(slugify(&state.heading_buffer));
                blocks.push(ContentBlock::Heading {
                    level,
                    content: state.heading_buffer.clone(),
                    inline: state.heading_inline.clone(),
                    anchor,
                });
            }
            state.in_heading = false;
            state.heading_level = None;
            state.heading_buffer.clear();
            state.heading_inline.clear();
        }
        _ => {}
    }
}

// ============================================================================
// Slug generation
// ============================================================================

/// Generate URL-friendly slug from heading text.
pub fn slugify(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c
            } else if c.is_whitespace() || c == '-' {
                '-'
            } else {
                '\0'
            }
        })
        .filter(|&c| c != '\0')
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

// ============================================================================
// Public API
// ============================================================================

/// Parse markdown content into structured blocks.
///
/// This is the main entry point for block-level parsing. It handles:
/// - Wikilink preprocessing (converts [[x]] to markdown links)
/// - HTML details block extraction
/// - Full pulldown-cmark parsing with GFM extensions
///
/// # Example
///
/// ```
/// use turbovault_parser::parse_blocks;
/// use turbovault_core::ContentBlock;
///
/// let markdown = "# Hello World\n\nThis is a **paragraph** with *inline* formatting.";
///
/// let blocks = parse_blocks(markdown);
/// assert!(matches!(blocks[0], ContentBlock::Heading { level: 1, .. }));
/// ```
pub fn parse_blocks(markdown: &str) -> Vec<ContentBlock> {
    parse_blocks_from_line(markdown, 0)
}

/// Parse markdown content into structured blocks, starting from a specific line.
///
/// Use this when you need accurate line numbers for nested content.
pub fn parse_blocks_from_line(markdown: &str, start_line: usize) -> Vec<ContentBlock> {
    // Pre-process wikilinks
    let preprocessed = preprocess_wikilinks(markdown);

    // Pre-process links with spaces
    let preprocessed = preprocess_links_with_spaces(&preprocessed);

    // Extract details blocks
    let (processed_markdown, details_blocks) = extract_details_blocks(&preprocessed);

    // Enable GFM extensions
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);

    let parser = Parser::new_ext(&processed_markdown, options);
    let mut blocks = Vec::new();
    let mut state = BlockParserState::new(start_line);

    for event in parser {
        process_event(event, &mut state, &mut blocks);
    }

    state.finalize(&mut blocks);

    // Replace placeholders with actual Details blocks
    let mut final_blocks = Vec::new();
    for block in blocks {
        let replaced = if let ContentBlock::Paragraph { content, .. } = &block {
            let trimmed = content.trim();
            trimmed
                .strip_prefix("[DETAILS_BLOCK_")
                .and_then(|s| s.strip_suffix(']'))
                .and_then(|s| s.parse::<usize>().ok())
                .and_then(|idx| details_blocks.get(idx).cloned())
        } else {
            None
        };

        final_blocks.push(replaced.unwrap_or(block));
    }

    final_blocks
}

/// Extract plain text from markdown content.
///
/// Strips all markdown syntax, returning only text that would be
/// visible when rendered. This is useful for:
/// - **Search indexing**: Index only searchable text
/// - **Accessibility**: Screen reader text extraction
/// - **Word counts**: Accurate content word counts
/// - **Diffs**: Compare semantic content, not syntax
///
/// # Elements stripped
///
/// | Markdown | Plain Text |
/// |----------|------------|
/// | `[text](url)` | `text` |
/// | `![alt](url)` | `alt` |
/// | `[[Page]]` | `Page` |
/// | `[[Page\|Display]]` | `Display` |
/// | `**bold**` | `bold` |
/// | `*italic*` | `italic` |
/// | `` `code` `` | `code` |
/// | `~~strike~~` | `strike` |
/// | `# Heading` | `Heading` |
/// | `> quote` | (quote content) |
/// | Code fences | (content preserved) |
///
/// # Example
///
/// ```
/// use turbovault_parser::to_plain_text;
///
/// let plain = to_plain_text("[Overview](#overview) and **bold**");
/// assert_eq!(plain, "Overview and bold");
///
/// // Wikilinks are handled properly
/// let plain = to_plain_text("See [[Note]] and [[Other|display]]");
/// assert_eq!(plain, "See Note and display");
/// ```
pub fn to_plain_text(markdown: &str) -> String {
    let blocks = parse_blocks(markdown);
    blocks
        .iter()
        .map(ContentBlock::to_plain_text)
        .collect::<Vec<_>>()
        .join("\n")
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_paragraph() {
        let markdown = "This is a simple paragraph.";
        let blocks = parse_blocks(markdown);

        assert_eq!(blocks.len(), 1);
        assert!(matches!(blocks[0], ContentBlock::Paragraph { .. }));
        if let ContentBlock::Paragraph { content, .. } = &blocks[0] {
            assert_eq!(content, "This is a simple paragraph.");
        }
    }

    #[test]
    fn test_parse_heading() {
        let markdown = "# Hello World";
        let blocks = parse_blocks(markdown);

        assert_eq!(blocks.len(), 1);
        if let ContentBlock::Heading {
            level,
            content,
            anchor,
            ..
        } = &blocks[0]
        {
            assert_eq!(*level, 1);
            assert_eq!(content, "Hello World");
            assert_eq!(anchor.as_deref(), Some("hello-world"));
        } else {
            panic!("Expected Heading block");
        }
    }

    #[test]
    fn test_parse_code_block() {
        let markdown = "```rust\nfn main() {}\n```";
        let blocks = parse_blocks(markdown);

        assert_eq!(blocks.len(), 1);
        if let ContentBlock::Code {
            language, content, ..
        } = &blocks[0]
        {
            assert_eq!(language.as_deref(), Some("rust"));
            assert_eq!(content, "fn main() {}");
        } else {
            panic!("Expected Code block");
        }
    }

    #[test]
    fn test_parse_unordered_list() {
        let markdown = "- Item 1\n- Item 2\n- Item 3";
        let blocks = parse_blocks(markdown);

        assert_eq!(blocks.len(), 1);
        if let ContentBlock::List { ordered, items } = &blocks[0] {
            assert!(!ordered);
            assert_eq!(items.len(), 3);
            assert_eq!(items[0].content, "Item 1");
            assert_eq!(items[1].content, "Item 2");
            assert_eq!(items[2].content, "Item 3");
        } else {
            panic!("Expected List block");
        }
    }

    #[test]
    fn test_parse_ordered_list() {
        let markdown = "1. First\n2. Second\n3. Third";
        let blocks = parse_blocks(markdown);

        assert_eq!(blocks.len(), 1);
        if let ContentBlock::List { ordered, items } = &blocks[0] {
            assert!(ordered);
            assert_eq!(items.len(), 3);
        } else {
            panic!("Expected List block");
        }
    }

    #[test]
    fn test_parse_task_list() {
        let markdown = "- [ ] Todo\n- [x] Done";
        let blocks = parse_blocks(markdown);

        assert_eq!(blocks.len(), 1);
        if let ContentBlock::List { items, .. } = &blocks[0] {
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].checked, Some(false));
            assert_eq!(items[0].content, "Todo");
            assert_eq!(items[1].checked, Some(true));
            assert_eq!(items[1].content, "Done");
        } else {
            panic!("Expected List block");
        }
    }

    #[test]
    fn test_parse_table() {
        let markdown = "| A | B |\n|---|---|\n| 1 | 2 |";
        let blocks = parse_blocks(markdown);

        assert_eq!(blocks.len(), 1);
        if let ContentBlock::Table { headers, rows, .. } = &blocks[0] {
            assert_eq!(headers.len(), 2);
            assert_eq!(headers[0], "A");
            assert_eq!(headers[1], "B");
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0][0], "1");
            assert_eq!(rows[0][1], "2");
        } else {
            panic!("Expected Table block");
        }
    }

    #[test]
    fn test_parse_blockquote() {
        let markdown = "> This is a quote";
        let blocks = parse_blocks(markdown);

        assert_eq!(blocks.len(), 1);
        if let ContentBlock::Blockquote { content, .. } = &blocks[0] {
            assert!(content.contains("This is a quote"));
        } else {
            panic!("Expected Blockquote block");
        }
    }

    #[test]
    fn test_parse_horizontal_rule() {
        let markdown = "Before\n\n---\n\nAfter";
        let blocks = parse_blocks(markdown);

        assert_eq!(blocks.len(), 3);
        assert!(matches!(blocks[1], ContentBlock::HorizontalRule));
    }

    #[test]
    fn test_parse_inline_formatting() {
        let markdown = "This has **bold** and *italic* and `code`.";
        let blocks = parse_blocks(markdown);

        assert_eq!(blocks.len(), 1);
        if let ContentBlock::Paragraph { inline, .. } = &blocks[0] {
            assert!(
                inline
                    .iter()
                    .any(|e| matches!(e, InlineElement::Strong { .. }))
            );
            assert!(
                inline
                    .iter()
                    .any(|e| matches!(e, InlineElement::Emphasis { .. }))
            );
            assert!(
                inline
                    .iter()
                    .any(|e| matches!(e, InlineElement::Code { .. }))
            );
        } else {
            panic!("Expected Paragraph block");
        }
    }

    #[test]
    fn test_parse_link() {
        let markdown = "See [example](https://example.com) for more.";
        let blocks = parse_blocks(markdown);

        assert_eq!(blocks.len(), 1);
        if let ContentBlock::Paragraph { inline, .. } = &blocks[0] {
            let link = inline
                .iter()
                .find(|e| matches!(e, InlineElement::Link { .. }));
            assert!(link.is_some());
            if let Some(InlineElement::Link { text, url, .. }) = link {
                assert_eq!(text, "example");
                assert_eq!(url, "https://example.com");
            }
        } else {
            panic!("Expected Paragraph block");
        }
    }

    #[test]
    fn test_wikilink_preprocessing() {
        let markdown = "See [[Note]] and [[Other|display]] for info.";
        let blocks = parse_blocks(markdown);

        assert_eq!(blocks.len(), 1);
        if let ContentBlock::Paragraph { inline, .. } = &blocks[0] {
            let links: Vec<_> = inline
                .iter()
                .filter(|e| matches!(e, InlineElement::Link { .. }))
                .collect();
            assert_eq!(links.len(), 2);

            if let InlineElement::Link { text, url, .. } = &links[0] {
                assert_eq!(text, "Note");
                assert_eq!(url, "wikilink:Note");
            }
            if let InlineElement::Link { text, url, .. } = &links[1] {
                assert_eq!(text, "display");
                assert_eq!(url, "wikilink:Other");
            }
        } else {
            panic!("Expected Paragraph block");
        }
    }

    #[test]
    fn test_list_with_nested_code() {
        let markdown = r#"1. First item
   ```rust
   code here
   ```

2. Second item"#;

        let blocks = parse_blocks(markdown);

        assert_eq!(blocks.len(), 1);
        if let ContentBlock::List { items, .. } = &blocks[0] {
            assert_eq!(items.len(), 2);
            assert!(!items[0].blocks.is_empty());
            assert!(matches!(items[0].blocks[0], ContentBlock::Code { .. }));
        } else {
            panic!("Expected List block");
        }
    }

    #[test]
    fn test_parse_image() {
        // Standalone image is wrapped in paragraph by pulldown-cmark
        let markdown = "![Alt text](image.png)";
        let blocks = parse_blocks(markdown);

        // pulldown-cmark wraps standalone images in paragraphs
        assert_eq!(blocks.len(), 1);
        if let ContentBlock::Paragraph { inline, .. } = &blocks[0] {
            let img = inline
                .iter()
                .find(|e| matches!(e, InlineElement::Image { .. }));
            assert!(img.is_some(), "Should have inline image");
        } else {
            panic!("Expected Paragraph block with inline image");
        }
    }

    #[test]
    fn test_parse_block_image() {
        // Image following other content becomes a block image
        let markdown = "Some text\n\n![Alt](image.png)";
        let blocks = parse_blocks(markdown);

        // First paragraph, then image (inline or block)
        assert!(blocks.len() >= 2);
    }

    #[test]
    fn test_parse_details_block() {
        let markdown = r#"<details>
<summary>Click to expand</summary>

Inner content here.

</details>"#;

        let blocks = parse_blocks(markdown);

        assert_eq!(blocks.len(), 1);
        if let ContentBlock::Details {
            summary,
            blocks: inner,
            ..
        } = &blocks[0]
        {
            assert_eq!(summary, "Click to expand");
            assert!(!inner.is_empty());
        } else {
            panic!("Expected Details block");
        }
    }

    #[test]
    fn test_slugify() {
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(slugify("API Reference"), "api-reference");
        assert_eq!(slugify("1. Getting Started"), "1-getting-started");
        assert_eq!(slugify("What's New?"), "whats-new");
    }

    #[test]
    fn test_strikethrough() {
        let markdown = "This is ~~deleted~~ text.";
        let blocks = parse_blocks(markdown);

        assert_eq!(blocks.len(), 1);
        if let ContentBlock::Paragraph { inline, .. } = &blocks[0] {
            assert!(
                inline
                    .iter()
                    .any(|e| matches!(e, InlineElement::Strikethrough { .. }))
            );
        }
    }

    #[test]
    fn test_indented_code_blocks_in_list_items() {
        // Bug report: indented fenced code blocks in list items should be recognized
        // Per CommonMark spec, code blocks can be indented up to 3 spaces to be part of a list item
        let markdown = r#"## Installation

1. Install from crates.io:
   ```bash
   cargo install treemd
   ```

2. Or build from source:
   ```bash
   git clone https://github.com/example/repo
   cd repo
   cargo install --path .
   ```"#;

        let blocks = parse_blocks(markdown);

        // Should have: Heading, List
        assert_eq!(blocks.len(), 2, "Expected 2 blocks (heading + list)");
        assert!(
            matches!(blocks[0], ContentBlock::Heading { level: 2, .. }),
            "First block should be H2"
        );

        if let ContentBlock::List { ordered, items } = &blocks[1] {
            assert!(ordered, "Should be an ordered list");
            assert_eq!(items.len(), 2, "Should have 2 list items");

            // First item should have code block in its nested blocks
            assert!(
                !items[0].blocks.is_empty(),
                "First item should have nested blocks"
            );
            assert!(
                matches!(items[0].blocks[0], ContentBlock::Code { .. }),
                "First item's nested block should be Code"
            );
            if let ContentBlock::Code {
                language, content, ..
            } = &items[0].blocks[0]
            {
                assert_eq!(language.as_deref(), Some("bash"));
                assert!(content.contains("cargo install treemd"));
            }

            // Second item should also have code block in its nested blocks
            assert!(
                !items[1].blocks.is_empty(),
                "Second item should have nested blocks"
            );
            assert!(
                matches!(items[1].blocks[0], ContentBlock::Code { .. }),
                "Second item's nested block should be Code"
            );
            if let ContentBlock::Code {
                language, content, ..
            } = &items[1].blocks[0]
            {
                assert_eq!(language.as_deref(), Some("bash"));
                assert!(content.contains("git clone"));
            }
        } else {
            panic!("Expected List block");
        }
    }

    // ========================================================================
    // to_plain_text tests
    // ========================================================================

    #[test]
    fn test_to_plain_text_simple_paragraph() {
        let markdown = "This is a simple paragraph.";
        let plain = to_plain_text(markdown);
        assert_eq!(plain, "This is a simple paragraph.");
    }

    #[test]
    fn test_to_plain_text_with_link() {
        let markdown = "[Overview](#overview) and more text";
        let plain = to_plain_text(markdown);
        assert_eq!(plain, "Overview and more text");
    }

    #[test]
    fn test_to_plain_text_with_bold_and_italic() {
        let markdown = "This has **bold** and *italic* text.";
        let plain = to_plain_text(markdown);
        assert_eq!(plain, "This has bold and italic text.");
    }

    #[test]
    fn test_to_plain_text_with_inline_code() {
        let markdown = "Use the `println!` macro.";
        let plain = to_plain_text(markdown);
        assert_eq!(plain, "Use the println! macro.");
    }

    #[test]
    fn test_to_plain_text_with_strikethrough() {
        let markdown = "This is ~~deleted~~ text.";
        let plain = to_plain_text(markdown);
        assert_eq!(plain, "This is deleted text.");
    }

    #[test]
    fn test_to_plain_text_wikilinks() {
        let markdown = "See [[Note]] and [[Other|display]] for info.";
        let plain = to_plain_text(markdown);
        assert_eq!(plain, "See Note and display for info.");
    }

    #[test]
    fn test_to_plain_text_heading() {
        let markdown = "# Hello World";
        let plain = to_plain_text(markdown);
        assert_eq!(plain, "Hello World");
    }

    #[test]
    fn test_to_plain_text_code_block() {
        let markdown = "```rust\nfn main() {}\n```";
        let plain = to_plain_text(markdown);
        assert_eq!(plain, "fn main() {}");
    }

    #[test]
    fn test_to_plain_text_list() {
        let markdown = "- Item 1\n- Item 2\n- Item 3";
        let plain = to_plain_text(markdown);
        assert_eq!(plain, "Item 1\nItem 2\nItem 3");
    }

    #[test]
    fn test_to_plain_text_table() {
        let markdown = "| A | B |\n|---|---|\n| 1 | 2 |";
        let plain = to_plain_text(markdown);
        // Table headers and rows separated by tabs
        assert!(plain.contains("A\tB"));
        assert!(plain.contains("1\t2"));
    }

    #[test]
    fn test_to_plain_text_blockquote() {
        let markdown = "> This is a quote";
        let plain = to_plain_text(markdown);
        assert!(plain.contains("This is a quote"));
    }

    #[test]
    fn test_to_plain_text_image() {
        let markdown = "![Alt text](image.png)";
        let plain = to_plain_text(markdown);
        assert_eq!(plain, "Alt text");
    }

    #[test]
    fn test_to_plain_text_horizontal_rule() {
        let markdown = "Before\n\n---\n\nAfter";
        let plain = to_plain_text(markdown);
        // Horizontal rules produce empty strings, paragraphs separated by newlines
        assert!(plain.contains("Before"));
        assert!(plain.contains("After"));
    }

    #[test]
    fn test_to_plain_text_complex_document() {
        let markdown = r#"# Document Title

This is a paragraph with **bold** and *italic* text.

- [Link One](#one)
- [Link Two](#two)
- [Link Three](#three)

See [[WikiNote]] for more info."#;

        let plain = to_plain_text(markdown);

        // Should contain heading text
        assert!(plain.contains("Document Title"));
        // Should contain paragraph with formatting stripped
        assert!(plain.contains("bold"));
        assert!(plain.contains("italic"));
        // Should contain link text, not URLs
        assert!(plain.contains("Link One"));
        assert!(plain.contains("Link Two"));
        // Should contain wikilink display text
        assert!(plain.contains("WikiNote"));
        // Should NOT contain URLs
        assert!(!plain.contains("#one"));
        assert!(!plain.contains("#two"));
    }

    #[test]
    fn test_to_plain_text_treemd_use_case() {
        // This test validates the original treemd use case:
        // searching in "[Overview](#overview)" should only match visible text "Overview"
        // not the hidden anchor "#overview"
        let markdown = "[Overview](#overview)";
        let plain = to_plain_text(markdown);
        assert_eq!(plain, "Overview");

        // The visible text "Overview" has 1 'O', while raw markdown has 2 'o's total
        // (capital O in "Overview" + lowercase o in "#overview")
        // Plain text extraction should only show the visible part
        let o_count = plain.chars().filter(|c| *c == 'o' || *c == 'O').count();
        assert_eq!(
            o_count, 1,
            "Should only count 'o' in visible text, not hidden anchor"
        );

        // More explicitly: the anchor URL should not be in plain text
        assert!(!plain.contains("#overview"));
        assert!(!plain.contains("overview")); // lowercase version from anchor
    }

    #[test]
    fn test_to_plain_text_nested_formatting() {
        // Test nested structures
        let markdown = "**[bold link](url)** and *[italic link](url2)*";
        let plain = to_plain_text(markdown);
        // The link text should be extracted
        assert!(plain.contains("bold link"));
        assert!(plain.contains("italic link"));
        // URLs should not appear
        assert!(!plain.contains("url"));
    }
}
