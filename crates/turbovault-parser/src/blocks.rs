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

/// Render an image back to its markdown spelling.
///
/// Used when rebuilding a blockquote's raw text, which is re-parsed rather than
/// carried through as structure, so anything not written here is lost.
///
/// A destination containing a space is wrapped in angle brackets, since bare
/// `![a](my file.png)` is not a link at all to a strict parser and would come
/// back as literal text. A title is re-quoted beside it.
fn format_image(alt: &str, src: &str, title: Option<&str>) -> String {
    let dest = if src.contains(char::is_whitespace) {
        format!("<{src}>")
    } else {
        src.to_string()
    };
    match title {
        // A title containing a double quote would terminate the title early,
        // so fall back to the destination alone rather than emit a broken one.
        Some(title) if !title.contains('"') => format!("![{alt}]({dest} \"{title}\")"),
        _ => format!("![{alt}]({dest})"),
    }
}

/// Split a link target into its destination and an optional CommonMark title.
///
/// A title is a quoted run at the end, separated from the destination by
/// whitespace: `x.png "Title"` is a destination plus a title, not a
/// destination containing a space. The returned title keeps its quotes, so a
/// caller can re-emit it verbatim.
///
/// Only `"` and `'` are recognised. CommonMark also allows a `(…)` title, but
/// [`LINK_WITH_SPACES_RE`] cannot capture one because its target class
/// excludes `)`.
fn split_link_title(target: &str) -> (&str, Option<&str>) {
    let trimmed = target.trim_end();
    let quote = match trimmed.chars().last() {
        Some(c @ ('"' | '\'')) => c,
        _ => return (target, None),
    };
    let body = &trimmed[..trimmed.len() - quote.len_utf8()];
    let Some(open) = body.rfind(quote) else {
        return (target, None);
    };
    // Without the separating whitespace this is one destination that happens
    // to contain quotes, not a destination and a title.
    if !body[..open].ends_with(char::is_whitespace) {
        return (target, None);
    }
    let url = body[..open].trim_end();
    if url.is_empty() {
        return (target, None);
    }
    (url, Some(&trimmed[open..]))
}

/// Preprocess links with spaces to angle bracket syntax.
///
/// The title is split off first. Wrapping `x.png "Title"` whole would make the
/// title part of the destination, which is how `![a](x.png "Title")` used to
/// parse to `src: "x.png \"Title\""` with no title at all.
fn preprocess_links_with_spaces(markdown: &str) -> String {
    LINK_WITH_SPACES_RE
        .replace_all(markdown, |caps: &regex::Captures| {
            let text = &caps[1];
            let (url, title) = split_link_title(&caps[2]);
            // Only a destination that genuinely contains a space needs the
            // angle brackets. Once the title is off, most do not, and leaving
            // them alone lets pulldown-cmark parse them natively.
            if !url.contains(' ') {
                return caps[0].to_string();
            }
            match title {
                Some(title) => format!("[{text}](<{url}> {title})"),
                None => format!("[{text}](<{url}>)"),
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

            let consumed_end = details_end + "</details>".len();
            let placeholder = format!("\n[DETAILS_BLOCK_{}]\n", details_blocks.len() - 1);
            // Pad back to the height the block occupied. The placeholder is
            // shorter than what it replaces, so without this every line after a
            // `<details>` block reports a number from higher up the document.
            let consumed_lines = markdown[current_pos..consumed_end].matches('\n').count();
            let placeholder_lines = placeholder.matches('\n').count();
            result.push_str(&placeholder);
            for _ in 0..consumed_lines.saturating_sub(placeholder_lines) {
                result.push('\n');
            }
            current_pos = consumed_end;
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

/// One list level open inside a blockquote that is still buffering.
///
/// A quote is rebuilt by re-parsing its raw text, so a list inside one has to
/// be written back out as markdown rather than flushed to `blocks`.
struct QuotedList {
    /// Next ordinal for an ordered list, `None` for a bullet list.
    next_number: Option<u64>,
    /// Column this list's markers start at, which is the content column of
    /// whichever item encloses it.
    indent: String,
}

/// One list open in the main document tree, at whatever depth.
///
/// A nested list used to have no home of its own: the parser tracked how deep
/// it was, but every item still landed in one shared top-level buffer. A
/// stack gives each open list its own items, so a list nested inside an item
/// closes into *that* item's blocks instead of merging into whichever list
/// happens to be flushed next.
struct ListFrame {
    /// Whether this specific list is ordered. A nested list's marker kind is
    /// independent of its parent's, so it cannot be read off any outer state.
    ordered: bool,
    items: Vec<ListItem>,
}

/// One list item open in the main document tree, at whatever depth.
///
/// Its own text accumulates in `state.paragraph_buffer`/`inline_buffer` while
/// it is the innermost open item, the same fields a top-level item already
/// used. `saved_paragraph_buffer` and friends hold what the enclosing item
/// (or the top-level document, if there is none) had pending there, so
/// opening a nested item doesn't overwrite a tight parent item's own
/// text-so-far, and closing the nested item hands that text back instead of
/// losing it.
struct ItemFrame {
    checked: Option<bool>,
    blocks: Vec<ContentBlock>,
    /// Real source line the item's marker starts on. Used to give a nested
    /// inline element's `line_offset` a real line number, the same source of
    /// truth `current_line` gives every other block, rather than a count of
    /// characters written into some reconstructed buffer.
    start_line: usize,
    saved_paragraph_buffer: String,
    saved_inline_buffer: Vec<InlineElement>,
    saved_in_paragraph: bool,
}

struct BlockParserState {
    /// Document line the event being processed starts on, 1-based.
    current_line: usize,
    /// Document line the event being processed ends on. For a fenced block
    /// that is the closing fence, since the event's span covers the whole
    /// block.
    current_end_line: usize,
    /// Document line the buffering blockquote started on, so the re-parse of
    /// its raw text can number its blocks from there instead of from zero.
    blockquote_start_line: usize,
    paragraph_buffer: String,
    inline_buffer: Vec<InlineElement>,
    /// Lists open in the main document tree, outermost first.
    list_stack: Vec<ListFrame>,
    /// List items open in the main document tree, outermost first.
    item_stack: Vec<ItemFrame>,
    code_buffer: String,
    code_language: Option<String>,
    code_start_line: usize,
    /// The current image's title, held here rather than borrowing
    /// `paragraph_buffer`. Parking it there overwrote whatever the paragraph
    /// had accumulated, so `- item ![a](a.png)` lost its "item " prefix.
    image_title: String,
    blockquote_buffer: String,
    /// Lists open inside the buffering blockquote, outermost first.
    quoted_lists: Vec<QuotedList>,
    /// Content column of each open item in `quoted_lists`, so a nested list
    /// indents under its parent's marker instead of a fixed two spaces.
    quoted_item_indents: Vec<String>,
    table_headers: Vec<String>,
    table_alignments: Vec<TableAlignment>,
    table_rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    heading_level: Option<usize>,
    heading_buffer: String,
    heading_inline: Vec<InlineElement>,
    in_paragraph: bool,
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
            current_end_line: start_line,
            blockquote_start_line: start_line,
            paragraph_buffer: String::new(),
            inline_buffer: Vec::new(),
            list_stack: Vec::new(),
            item_stack: Vec::new(),
            code_buffer: String::new(),
            code_language: None,
            code_start_line: 0,
            image_title: String::new(),
            blockquote_buffer: String::new(),
            quoted_lists: Vec::new(),
            quoted_item_indents: Vec::new(),
            table_headers: Vec::new(),
            table_alignments: Vec::new(),
            table_rows: Vec::new(),
            current_row: Vec::new(),
            heading_level: None,
            heading_buffer: String::new(),
            heading_inline: Vec::new(),
            in_paragraph: false,
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

    /// Pops the innermost open list, if any, and attaches it to whatever
    /// encloses it: the current item's own blocks when the list was nested
    /// inside one, the top-level `blocks` otherwise.
    fn close_list(&mut self, blocks: &mut Vec<ContentBlock>) {
        let Some(frame) = self.list_stack.pop() else {
            return;
        };
        if frame.items.is_empty() {
            return;
        }
        let list_block = ContentBlock::List {
            ordered: frame.ordered,
            items: frame.items,
        };
        match self.item_stack.last_mut() {
            Some(item) => item.blocks.push(list_block),
            None => blocks.push(list_block),
        }
    }

    fn flush_list(&mut self, blocks: &mut Vec<ContentBlock>) {
        while !self.list_stack.is_empty() {
            self.close_list(blocks);
        }
    }

    fn flush_code(&mut self, blocks: &mut Vec<ContentBlock>) {
        if self.in_code && !self.code_buffer.is_empty() {
            blocks.push(ContentBlock::Code {
                language: self.code_language.clone(),
                content: self.code_buffer.trim_end().to_string(),
                start_line: self.code_start_line,
                end_line: self.current_end_line,
            });
            self.code_buffer.clear();
            self.code_language = None;
            self.in_code = false;
        }
    }

    fn flush_blockquote(&mut self, blocks: &mut Vec<ContentBlock>) {
        if self.in_blockquote && !self.blockquote_buffer.is_empty() {
            // Paragraph ends inside the quote append a blank-line separator,
            // which leaves a trailing one on the last paragraph. It carries no
            // meaning and would show up in every consumer's `content`.
            let content = self.blockquote_buffer.trim_end().to_string();
            // Numbered from the quote's own line, not from zero. The re-parse
            // rebuilds the quote line for line, so a block inside it lands on
            // the document line it was written on. Parsing the fragment cold
            // reported every quoted fence as `start_line: 0`, which put it
            // before the start of the document for any consumer filtering on
            // position.
            let nested_blocks = parse_blocks_from_line(&content, self.blockquote_start_line);
            blocks.push(ContentBlock::Blockquote {
                content,
                blocks: nested_blocks,
            });
            self.blockquote_buffer.clear();
            self.in_blockquote = false;
        }
    }

    /// Document line the next character written to the quote buffer lands on.
    fn next_quoted_line(&self) -> usize {
        self.blockquote_start_line + self.blockquote_buffer.matches('\n').count()
    }

    /// Pads the quote buffer with newlines until its next write lands on
    /// `source_line`, so the re-parse numbers each block at the line it was
    /// actually written on and the separators match the source instead of
    /// being invented.
    ///
    /// A fixed `\n\n` between blocks was both: it ran a fence onto the last
    /// list item's line when the source had a blank line, and it inserted a
    /// blank line when the source had none, which pushed every block below it
    /// in the quote one line down.
    fn sync_quoted_line(&mut self, source_line: usize) {
        // A block always starts on a fresh line even where the source somehow
        // reports the same one, so the buffer can never run two together.
        if !self.blockquote_buffer.is_empty() && !self.blockquote_buffer.ends_with('\n') {
            self.blockquote_buffer.push('\n');
        }
        while self.next_quoted_line() < source_line {
            self.blockquote_buffer.push('\n');
        }
    }

    /// Column the open quoted list item's own blocks start at, empty when no
    /// item is open.
    fn quoted_item_indent(&self) -> String {
        self.quoted_item_indents.last().cloned().unwrap_or_default()
    }

    /// Opens a block in the quote buffer at `source_line`, indented to the open
    /// list item's content column.
    ///
    /// Written at column zero a block inside an item is not a continuation at
    /// all: it ends the list and comes back as the list's sibling. That is how
    /// a fence inside a quoted item landed beside the list, and with no
    /// separator at all a continuation paragraph was joined onto the item's own
    /// text, so `> - step` `>` `>   text` came back as one item reading
    /// `steptext`.
    ///
    /// The item's *first* block is the exception: it sits on the marker's own
    /// line, the marker has already been written there, and the column it
    /// starts at is the content column. Breaking the line or indenting again
    /// would split the item from its own text.
    fn open_quoted_block(&mut self, source_line: usize) {
        if !self.quoted_lists.is_empty() && source_line <= self.next_quoted_line() {
            return;
        }
        self.sync_quoted_line(source_line);
        if !self.quoted_lists.is_empty() {
            let indent = self.quoted_item_indent();
            self.blockquote_buffer.push_str(&indent);
        }
    }

    fn open_quoted_list(&mut self, start_number: Option<u64>, source_line: usize) {
        let indent = self.quoted_item_indents.last().cloned().unwrap_or_default();
        if self.quoted_lists.is_empty() {
            self.sync_quoted_line(source_line);
        } else if !self.blockquote_buffer.ends_with('\n') {
            // A nested list opens on the line below its parent's marker, and a
            // blank line here would make the enclosing list loose.
            self.blockquote_buffer.push('\n');
        }
        self.quoted_lists.push(QuotedList {
            next_number: start_number,
            indent,
        });
    }

    fn close_quoted_list(&mut self) {
        self.quoted_lists.pop();
    }

    fn open_quoted_item(&mut self, source_line: usize) {
        if self.quoted_lists.is_empty() {
            return;
        }
        self.sync_quoted_line(source_line);
        let Some(list) = self.quoted_lists.last_mut() else {
            return;
        };
        let indent = list.indent.clone();
        let marker = match &mut list.next_number {
            Some(number) => {
                let marker = format!("{number}. ");
                *number += 1;
                marker
            }
            None => "- ".to_string(),
        };
        self.quoted_item_indents
            .push(" ".repeat(indent.len() + marker.len()));
        self.blockquote_buffer.push_str(&indent);
        self.blockquote_buffer.push_str(&marker);
    }

    fn close_quoted_item(&mut self) {
        self.quoted_item_indents.pop();
        if !self.blockquote_buffer.ends_with('\n') {
            self.blockquote_buffer.push('\n');
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

    /// A nested inline element's line, relative to the real source line the
    /// outermost enclosing list item's own marker starts on. `None` outside a
    /// list.
    ///
    /// Anchored on the outermost item rather than the innermost one still
    /// open: a link three levels deep and a link one level deep report on the
    /// same scale, which is what lets a consumer that only sees the outer
    /// item's aggregated `inline` (see `collect_inline_elements`) tell how far
    /// into the source a given element actually is.
    fn item_line_offset(&self) -> Option<usize> {
        self.item_stack
            .first()
            .map(|frame| self.current_line.saturating_sub(frame.start_line))
    }

    /// The inline list the enclosing container collects into. A heading keeps
    /// its own, and routing a heading's image or link to the paragraph list is
    /// what hoisted them out of the heading.
    fn inline_elements(&mut self) -> &mut Vec<InlineElement> {
        if self.in_heading {
            &mut self.heading_inline
        } else {
            &mut self.inline_buffer
        }
    }

    /// Records an inline element against the enclosing container, along with
    /// the text it contributes to that container's own `content`.
    ///
    /// The two differ. A paragraph's `content` is re-serialized by its
    /// consumers, so an image contributes its source spelling. A heading's is a
    /// label: it is what an outline prints and what its anchor is cut from, so
    /// an element contributes what it renders as, which for an image is
    /// nothing. Giving a heading the source spelling put the whole
    /// `![alt](src)` in its text and `title-aapng` in its slug.
    fn push_inline(&mut self, element: InlineElement, source: &str, rendered: &str) {
        if self.in_heading {
            self.heading_inline.push(element);
            self.heading_buffer.push_str(rendered);
        } else {
            self.inline_buffer.push(element);
            self.paragraph_buffer.push_str(source);
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
            if state.in_blockquote {
                // Put the paragraph on the source line it came from, and at the
                // content column of whichever item encloses it.
                state.open_quoted_block(state.current_line);
            }
            state.in_paragraph = true;
        }
        Event::End(TagEnd::Paragraph) => {
            if state.in_blockquote {
                // The quote's text already went to `blockquote_buffer`, and the
                // next block syncs itself to its own source line, so there is
                // no separator to invent here.
                state.in_paragraph = false;
            } else if let Some(item) = state.item_stack.last_mut()
                && state.in_paragraph
                && !state.paragraph_buffer.is_empty()
            {
                item.blocks.push(ContentBlock::Paragraph {
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
            if state.in_blockquote && state.in_code {
                // Emitting the code block here would push it onto the
                // top-level `blocks`, where it lands *ahead of* the blockquote
                // that is still buffering, so a fenced block inside a callout
                // rendered above the callout header. Re-fence it into the
                // buffer instead and let the nested parse rebuild it in place.
                // The end event's span covers the whole block, so its start is
                // the opening fence's own line.
                state.open_quoted_block(state.current_line);
                // Every line of the block carries the item's indent, not just
                // the opening fence: an indented fence whose body sits back at
                // column zero closes the item at its first body line.
                let indent = state.quoted_item_indent();
                match &state.code_language {
                    Some(lang) => state.blockquote_buffer.push_str(&format!("```{lang}\n")),
                    None => state.blockquote_buffer.push_str("```\n"),
                }
                let body = std::mem::take(&mut state.code_buffer);
                for line in body.trim_end().lines() {
                    state.blockquote_buffer.push_str(&indent);
                    state.blockquote_buffer.push_str(line);
                    state.blockquote_buffer.push('\n');
                }
                state.blockquote_buffer.push_str(&indent);
                state.blockquote_buffer.push_str("```\n");
                state.code_language = None;
                state.in_code = false;
            } else if let Some(item) = state.item_stack.last_mut()
                && state.in_code
                && !state.code_buffer.is_empty()
            {
                item.blocks.push(ContentBlock::Code {
                    language: state.code_language.clone(),
                    content: state.code_buffer.trim_end().to_string(),
                    start_line: state.code_start_line,
                    end_line: state.current_end_line,
                });
                state.code_buffer.clear();
                state.code_language = None;
                state.in_code = false;
            } else {
                state.flush_code(blocks);
            }
        }
        // A list inside a blockquote is written back into the buffer as
        // markdown, the same as the fenced blocks above. Flushing it to
        // `blocks` instead put an item-less list ahead of the quote and left
        // the items' text bare in the quote's content, with no markers and no
        // line breaks, so `> - one` `> - two` came back as "onetwo".
        Event::Start(Tag::List(start_number)) if state.in_blockquote => {
            state.open_quoted_list(start_number, state.current_line);
        }
        Event::End(TagEnd::List(_)) if state.in_blockquote => {
            state.close_quoted_list();
        }
        Event::Start(Tag::Item) if state.in_blockquote => {
            state.open_quoted_item(state.current_line);
        }
        Event::End(TagEnd::Item) if state.in_blockquote => {
            state.close_quoted_item();
        }
        Event::TaskListMarker(checked) if state.in_blockquote => {
            state
                .blockquote_buffer
                .push_str(if checked { "[x] " } else { "[ ] " });
        }
        Event::Start(Tag::List(start_number)) => {
            state.list_stack.push(ListFrame {
                ordered: start_number.is_some(),
                items: Vec::new(),
            });
        }
        Event::End(TagEnd::List(_)) => {
            state.close_list(blocks);
        }
        Event::Start(Tag::Item) => {
            // Whatever the enclosing item (or the top-level document) had
            // pending has to survive this item's own accumulation: a tight
            // outer item's text is sitting in `paragraph_buffer` right now,
            // and starting this item fresh must not overwrite it.
            state.item_stack.push(ItemFrame {
                checked: None,
                blocks: Vec::new(),
                start_line: state.current_line,
                saved_paragraph_buffer: std::mem::take(&mut state.paragraph_buffer),
                saved_inline_buffer: std::mem::take(&mut state.inline_buffer),
                saved_in_paragraph: state.in_paragraph,
            });
            state.in_paragraph = false;
        }
        Event::End(TagEnd::Item) => {
            let Some(frame) = state.item_stack.pop() else {
                return;
            };
            let mut item_blocks = frame.blocks;
            let (content, mut inline, remaining_blocks) = if !state.paragraph_buffer.is_empty() {
                (
                    std::mem::take(&mut state.paragraph_buffer),
                    std::mem::take(&mut state.inline_buffer),
                    item_blocks,
                )
            } else if let Some(ContentBlock::Paragraph { content, inline }) =
                item_blocks.first().cloned()
            {
                let remaining: Vec<ContentBlock> = item_blocks.drain(1..).collect();
                (content, inline, remaining)
            } else {
                (String::new(), Vec::new(), std::mem::take(&mut item_blocks))
            };

            // Collect inline elements from all nested blocks (paragraphs, lists, etc.)
            collect_inline_elements(&remaining_blocks, &mut inline);

            let item = ListItem {
                checked: frame.checked,
                content,
                inline,
                blocks: remaining_blocks,
            };
            if let Some(list) = state.list_stack.last_mut() {
                list.items.push(item);
            }

            // Hand the enclosing item's own pending text back so it can keep
            // accumulating, the same as it would have if this item had never
            // opened.
            state.paragraph_buffer = frame.saved_paragraph_buffer;
            state.inline_buffer = frame.saved_inline_buffer;
            state.in_paragraph = frame.saved_in_paragraph;
        }
        Event::TaskListMarker(checked) => {
            if let Some(item) = state.item_stack.last_mut() {
                item.checked = Some(checked);
            }
        }
        Event::Start(Tag::BlockQuote(_)) => {
            // Anchor on the outermost quote only. A nested quote accumulates
            // into the same buffer, so re-anchoring here would measure the
            // buffer's height from the inner quote's line and leave every sync
            // below it a no-op, which ran the nested quote's paragraph onto the
            // outer quote's line.
            if !state.in_blockquote {
                state.blockquote_start_line = state.current_line;
            }
            state.in_blockquote = true;
        }
        Event::End(TagEnd::BlockQuote(_)) => {
            state.flush_blockquote(blocks);
            // `flush_blockquote` only resets the flag when it had something to
            // emit, so a quote that produced no text (`>` on a line by itself)
            // left it set and every block after it was swallowed into a quote
            // that had already closed.
            state.in_blockquote = false;
            state.quoted_lists.clear();
            state.quoted_item_indents.clear();
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
            if state.in_heading {
                state.heading_buffer.push_str(&text);
                state.heading_inline.push(InlineElement::Code {
                    value: text.to_string(),
                });
            } else if state.in_blockquote {
                // Re-emit with delimiters so the buffer is re-parseable as inline code
                state.blockquote_buffer.push('`');
                state.blockquote_buffer.push_str(&text);
                state.blockquote_buffer.push('`');
            } else if state.in_table {
                // Re-emit with delimiters so table cell strings carry inline code markers
                state.paragraph_buffer.push('`');
                state.paragraph_buffer.push_str(&text);
                state.paragraph_buffer.push('`');
            } else {
                state.in_code_inline = true;
                state.add_inline_text(&text);
                state.in_code_inline = false;
            }
        }
        Event::Start(Tag::Link { dest_url, .. }) => {
            state.in_link = true;
            state.link_url = dest_url.to_string();
            state.link_text.clear();
        }
        Event::End(TagEnd::Link) => {
            state.in_link = false;

            // Same as images: inside a quote only the buffer reaches the
            // re-parse, so the destination has to be written back out. A link
            // wrapping an image re-emits the image as its label, which is the
            // one case where `link_text` is not the whole story.
            if state.in_blockquote {
                let label = if state.image_in_link {
                    format_image(&state.link_text, &state.link_url, None)
                } else {
                    state.link_text.clone()
                };
                let url = if state.image_in_link {
                    state.saved_link_url.clone()
                } else {
                    state.link_url.clone()
                };
                state
                    .blockquote_buffer
                    .push_str(&format!("[{label}]({url})"));
                state.link_text.clear();
                state.link_url.clear();
                state.saved_link_url.clear();
                state.image_in_link = false;
                return;
            }

            let line_offset = state.item_line_offset();

            // A linked image carries the image's own destination in `link_url`
            // by this point, so the wrapper reads its href from `saved_link_url`.
            let url = if state.image_in_link {
                state.saved_link_url.clone()
            } else {
                state.link_url.clone()
            };
            let element = InlineElement::Link {
                text: state.link_text.clone(),
                url: url.clone(),
                title: None,
                line_offset,
            };
            let source = format!("[{}]({})", state.link_text, url);
            // A link renders as its label, except when its label is an image,
            // which renders as nothing.
            let rendered = if state.image_in_link {
                String::new()
            } else {
                state.link_text.clone()
            };
            state.push_inline(element, &source, &rendered);

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
            // NOT `paragraph_buffer`: text already collected for the enclosing
            // paragraph has to survive an image appearing partway through it.
            state.image_title = title.to_string();
        }
        Event::End(TagEnd::Image) => {
            state.in_image = false;

            let title = if state.image_title.is_empty() {
                None
            } else {
                Some(std::mem::take(&mut state.image_title))
            };

            let line_offset = state.item_line_offset();

            // Inside a quote the buffer is the only thing that survives to the
            // re-parse, so re-emit the whole element rather than the alt alone.
            // A linked image writes nothing here; `TagEnd::Link` emits the
            // wrapper with this image nested inside it.
            if state.in_blockquote && !state.image_in_link {
                state.blockquote_buffer.push_str(&format_image(
                    &state.link_text,
                    &state.link_url,
                    title.as_deref(),
                ));
                state.link_text.clear();
                state.link_url.clear();
                return;
            }

            if state.image_in_link {
                // A linked image (`[![alt](img)](href)`) used to emit nothing
                // at all, so a README badge row reported zero images. The
                // enclosing link is still emitted when it ends; the two are
                // siblings because `InlineElement::Link` carries no children.
                // `link_url` currently holds the image destination and
                // `saved_link_url` the link's own, which `TagEnd::Link` uses.
                let element = InlineElement::Image {
                    alt: state.link_text.clone(),
                    src: state.link_url.clone(),
                    title,
                    line_offset,
                };
                state.inline_elements().push(element);
                // Deliberately keep `link_text` and `link_url`: the enclosing
                // link still needs the text, and clearing the url would strand
                // `TagEnd::Link`'s restore.
                return;
            }

            // A tight list item produces no Paragraph events, so this used to
            // fall through to the block arm and push the image onto the
            // top-level blocks, hoisted clean out of the list, while
            // `flush_paragraph` discarded the item's own text. An image in an
            // item belongs to the item whether or not the list is loose.
            if state.in_heading || state.in_paragraph || !state.item_stack.is_empty() {
                let element = InlineElement::Image {
                    alt: state.link_text.clone(),
                    src: state.link_url.clone(),
                    title,
                    line_offset,
                };
                // Append the image's source spelling to whatever the container
                // has so far, rather than replacing it. An image renders as
                // itself and contributes no text, so a heading gets nothing.
                let source = format!("![{}]({})", state.link_text, state.link_url);
                state.push_inline(element, &source, "");
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
        Event::Text(text) => {
            if state.in_code {
                state.code_buffer.push_str(&text);
            } else if state.in_blockquote && (state.in_image || state.in_link) {
                // An image's alt or a link's label, which is only half of the
                // element. Held here so the end tag can re-emit the whole
                // `![alt](src)` / `[text](url)` into the buffer. Appending it
                // straight to `blockquote_buffer` is what dropped every
                // destination inside a quote: the re-parse saw bare text.
                state.link_text.push_str(&text);
            } else if state.in_blockquote {
                state.blockquote_buffer.push_str(&text);
            } else if state.in_heading && (state.in_image || state.in_link) {
                // An image's alt or a link's label inside a heading. Held for
                // the end tag like everywhere else, because letting it fall
                // into `heading_buffer` is what made `# Title ![h](h.png)`
                // read as "Title h" with the image reporting an empty alt.
                state.link_text.push_str(&text);
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
                state.add_inline_text(&text);
            }
        }
        // A break inside a blockquote separates two source lines, and the
        // blockquote is reconstructed from raw text, so the break has to reach
        // that buffer. Sending it to `paragraph_buffer` instead is what joined
        // `> a` and `> b` into "ab" and left a stray whitespace-only paragraph
        // beside the blockquote. This arm must precede the `in_paragraph` ones,
        // since pulldown-cmark opens a paragraph inside the quote too.
        Event::SoftBreak | Event::HardBreak if state.in_blockquote => {
            state.blockquote_buffer.push('\n');
        }
        Event::SoftBreak if state.in_paragraph => {
            state.paragraph_buffer.push(' ');
            state.inline_buffer.push(InlineElement::Text {
                value: " ".to_string(),
            });
        }
        Event::HardBreak if state.in_paragraph => {
            state.paragraph_buffer.push('\n');
            state.inline_buffer.push(InlineElement::Text {
                value: "\n".to_string(),
            });
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
            // A banner heading is all image and no text. Requiring text was
            // safe only while an image's markup counted as text; now that it
            // does not, that rule would delete the block outright.
            if state.in_heading
                && !(state.heading_buffer.is_empty() && state.heading_inline.is_empty())
                && let Some(level) = state.heading_level
            {
                let content = normalize_heading_text(&state.heading_buffer);
                let slug = slugify(&content);
                blocks.push(ContentBlock::Heading {
                    level,
                    content,
                    inline: state.heading_inline.clone(),
                    // A heading with no text has no slug to offer, and one cut
                    // from its markup is worse than none.
                    anchor: (!slug.is_empty()).then_some(slug),
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
/// A heading's text as a label: no leading or trailing space, and no doubled
/// run of whitespace where an element that renders as nothing was dropped out
/// from between two words.
fn normalize_heading_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

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
// Helper functions
// ============================================================================

/// Recursively collect inline elements from content blocks.
///
/// This traverses nested structures (paragraphs, lists, blockquotes) to gather
/// all inline elements, enabling consumers to find links and other inline
/// content from nested list items.
fn collect_inline_elements(blocks: &[ContentBlock], output: &mut Vec<InlineElement>) {
    for block in blocks {
        match block {
            ContentBlock::Paragraph { inline, .. } => {
                output.extend(inline.iter().cloned());
            }
            // A list item's own `inline` is already this recursive collection
            // applied to it, gathered when the item itself was closed (see the
            // `Event::End(TagEnd::Item)` handler). Walking `item.blocks` here
            // too would gather a nested item's links twice.
            ContentBlock::List { items, .. } => {
                for item in items {
                    output.extend(item.inline.iter().cloned());
                }
            }
            ContentBlock::Blockquote { blocks, .. } => {
                collect_inline_elements(blocks, output);
            }
            ContentBlock::Details { blocks, .. } => {
                collect_inline_elements(blocks, output);
            }
            // Headings, Code, HorizontalRule, Table, Image don't have nested inline elements
            // that we need to collect (or they store them differently)
            _ => {}
        }
    }
}

// ============================================================================
// Public API
// ============================================================================

/// Parse markdown content into structured blocks.
///
/// This is the main entry point for block-level parsing. It handles:
/// - Wikilink preprocessing (converts `[[x]]` to markdown links)
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
    parse_blocks_from_line(markdown, 1)
}

/// Byte offset at which each line of `text` begins.
fn line_start_offsets(text: &str) -> Vec<usize> {
    let mut starts = Vec::with_capacity(text.len() / 32 + 1);
    starts.push(0);
    starts.extend(text.match_indices('\n').map(|(index, _)| index + 1));
    starts
}

/// Index of the line containing `offset`, counting the first line as 0.
///
/// `starts` is sorted, so the search either lands on a line start or reports
/// the insertion point, in which case the offset falls inside the line before.
fn line_index_of(starts: &[usize], offset: usize) -> usize {
    match starts.binary_search(&offset) {
        Ok(index) => index,
        Err(index) => index.saturating_sub(1),
    }
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

    // `into_offset_iter` carries each event's source span, which is the only
    // way a block learns where it sits. `current_line` was previously set once
    // at construction and never advanced, so every `ContentBlock::Code` in
    // every document reported the same line the parse started from, which for
    // `parse_blocks` meant a hard-coded 0.
    let line_starts = line_start_offsets(&processed_markdown);
    let parser = Parser::new_ext(&processed_markdown, options).into_offset_iter();
    let mut blocks = Vec::new();
    let mut state = BlockParserState::new(start_line);

    for (event, span) in parser {
        state.current_line = start_line + line_index_of(&line_starts, span.start);
        // The span runs to just past the node's last byte, so step back one to
        // land inside the closing line rather than on the one after it.
        state.current_end_line =
            start_line + line_index_of(&line_starts, span.end.saturating_sub(1));
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

    #[test]
    fn test_nested_list_item_inline_elements() {
        // Test that inline elements from nested list items are collected
        // into the parent item's inline field
        let markdown = r#"- [Features](#features)
  - [Interactive TUI](#interactive-tui)
  - [CLI Mode](#cli-mode)"#;

        let blocks = parse_blocks(markdown);
        assert_eq!(blocks.len(), 1);

        if let ContentBlock::List { items, .. } = &blocks[0] {
            assert_eq!(items.len(), 1, "Should have 1 top-level item");

            let item = &items[0];
            // The inline field should contain ALL links, including from nested items
            let links: Vec<_> = item
                .inline
                .iter()
                .filter_map(|e| {
                    if let InlineElement::Link { text, url, .. } = e {
                        Some((text.as_str(), url.as_str()))
                    } else {
                        None
                    }
                })
                .collect();

            assert_eq!(links.len(), 3, "Should have 3 links total");
            assert!(
                links.iter().any(|(text, _)| *text == "Features"),
                "Should have Features link"
            );
            assert!(
                links.iter().any(|(text, _)| *text == "Interactive TUI"),
                "Should have Interactive TUI link"
            );
            assert!(
                links.iter().any(|(text, _)| *text == "CLI Mode"),
                "Should have CLI Mode link"
            );
        } else {
            panic!("Expected List block");
        }
    }

    #[test]
    fn test_deeply_nested_list_inline_elements() {
        // Test deeply nested list items
        let markdown = r#"- Level 1 [link1](url1)
  - Level 2 [link2](url2)
    - Level 3 [link3](url3)"#;

        let blocks = parse_blocks(markdown);

        if let ContentBlock::List { items, .. } = &blocks[0] {
            let item = &items[0];
            let links: Vec<_> = item
                .inline
                .iter()
                .filter(|e| matches!(e, InlineElement::Link { .. }))
                .collect();

            assert_eq!(links.len(), 3, "Should collect all 3 nested links");
        } else {
            panic!("Expected List block");
        }
    }

    #[test]
    fn test_inline_element_line_offset() {
        // Test that line_offset is correctly tracked for nested list items
        let markdown = r#"- [Features](#features)
  - [Interactive TUI](#interactive-tui)
  - [CLI Mode](#cli-mode)"#;

        let blocks = parse_blocks(markdown);

        if let ContentBlock::List { items, .. } = &blocks[0] {
            let item = &items[0];
            let links: Vec<_> = item
                .inline
                .iter()
                .filter_map(|e| {
                    if let InlineElement::Link {
                        text, line_offset, ..
                    } = e
                    {
                        Some((text.as_str(), *line_offset))
                    } else {
                        None
                    }
                })
                .collect();

            assert_eq!(links.len(), 3);

            // Features is on line 0 (first line of the item)
            let features = links.iter().find(|(t, _)| *t == "Features").unwrap();
            assert_eq!(features.1, Some(0), "Features should be on line 0");

            // Interactive TUI is on line 1 (after first newline)
            let tui = links.iter().find(|(t, _)| *t == "Interactive TUI").unwrap();
            assert_eq!(tui.1, Some(1), "Interactive TUI should be on line 1");

            // CLI Mode is on line 2 (after second newline)
            let cli = links.iter().find(|(t, _)| *t == "CLI Mode").unwrap();
            assert_eq!(cli.1, Some(2), "CLI Mode should be on line 2");
        } else {
            panic!("Expected List block");
        }
    }

    #[test]
    fn test_line_offset_not_set_outside_lists() {
        // line_offset should be None for links outside of list items
        let markdown = "See [example](url) for more.";
        let blocks = parse_blocks(markdown);

        if let ContentBlock::Paragraph { inline, .. } = &blocks[0] {
            let link = inline
                .iter()
                .find(|e| matches!(e, InlineElement::Link { .. }));
            if let Some(InlineElement::Link { line_offset, .. }) = link {
                assert_eq!(
                    *line_offset, None,
                    "line_offset should be None outside lists"
                );
            }
        } else {
            panic!("Expected Paragraph block");
        }
    }

    // Regression tests for PR #15: inline code in headings/blockquotes/tables
    // previously leaked into the following paragraph's buffers.

    #[test]
    fn test_inline_code_in_heading_does_not_leak() {
        let markdown = "# Use `foo()` carefully\n\nThis is the body.";
        let blocks = parse_blocks(markdown);

        assert_eq!(blocks.len(), 2);

        let ContentBlock::Heading {
            content, inline, ..
        } = &blocks[0]
        else {
            panic!("Expected Heading block, got {:?}", blocks[0]);
        };
        assert_eq!(content, "Use foo() carefully");
        assert!(
            inline
                .iter()
                .any(|e| matches!(e, InlineElement::Code { value } if value == "foo()")),
            "heading inline elements should include the Code element"
        );

        let ContentBlock::Paragraph { content, .. } = &blocks[1] else {
            panic!("Expected Paragraph block, got {:?}", blocks[1]);
        };
        assert_eq!(
            content, "This is the body.",
            "inline code from heading must not leak into the following paragraph"
        );
    }

    #[test]
    fn test_inline_code_in_blockquote_preserved() {
        let markdown = "> Run `cargo test` before committing";
        let blocks = parse_blocks(markdown);

        assert_eq!(blocks.len(), 1);
        let ContentBlock::Blockquote { content, .. } = &blocks[0] else {
            panic!("Expected Blockquote block, got {:?}", blocks[0]);
        };
        assert!(
            content.contains("`cargo test`"),
            "blockquote content should preserve inline code with backticks, got: {content:?}"
        );
    }

    #[test]
    fn test_inline_code_in_table_cell_preserved() {
        let markdown = "| Command | Effect |\n|---|---|\n| `ls` | list files |";
        let blocks = parse_blocks(markdown);

        assert_eq!(blocks.len(), 1);
        let ContentBlock::Table { rows, .. } = &blocks[0] else {
            panic!("Expected Table block, got {:?}", blocks[0]);
        };
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0][0].contains("`ls`"),
            "table cell should preserve inline code with backticks, got: {:?}",
            rows[0][0]
        );
        assert_eq!(rows[0][1], "list files");
    }
}
