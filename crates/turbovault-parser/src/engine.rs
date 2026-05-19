//! Unified parsing engine - single source of truth for OFM parsing.
//!
//! This module provides a consolidated parsing engine that:
//! - Uses pulldown-cmark for CommonMark foundation (headings, links, tasks, code blocks)
//! - Tracks code block/inline code ranges to exclude from OFM regex parsing
//! - Uses regex only for Obsidian-specific syntax (wikilinks, embeds, tags, callouts)
//! - Builds LineIndex once and reuses for all position calculations
//!
//! All public parsing APIs delegate to this engine internally.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use regex::Regex;
use std::ops::Range;
use std::path::Path;
use std::sync::LazyLock;
use turbovault_core::{
    Callout, CalloutType, Frontmatter, Heading, LineIndex, Link, LinkType, SourcePosition,
    Tag as OFMTag, TaskBuilder, TaskItem,
};

use crate::ParseOptions;
use crate::blocks::slugify;
use crate::parsers::link_utils::{classify_url, classify_wikilink};

// ============================================================================
// Compiled regex patterns (LazyLock for Rust 1.80+ SOTA)
// Only for Obsidian-specific syntax not handled by pulldown-cmark
// ============================================================================

/// Wikilink: [[target]] or [[target|display]]
static WIKILINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[\[([^\]]+)\]\]").unwrap());

/// Embed: ![[target]] or ![[target|display]]
static EMBED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"!\[\[([^\]]+)\]\]").unwrap());

/// Tag: #tag or #parent/child (but not inside words or URLs)
static TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[\s\[(])#([a-zA-Z0-9_][a-zA-Z0-9_\-/]*)").unwrap());

/// Callout start: > [!TYPE] with optional fold marker and title
static CALLOUT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*>\s*\[!(\w+)\]([+-]?)\s*(.*?)$").unwrap());

/// Callout continuation: > content
static CALLOUT_CONT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*>\s*(.*)$").unwrap());

/// Non-CommonMark Obsidian task states not emitted by pulldown-cmark.
static EXTENDED_TASK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?P<indent>[ \t]*)(?P<bullet>[-*+])[ \t]+\[(?P<marker>[/\-])\][ \t]*(?P<content>.*)$",
    )
    .unwrap()
});

// ============================================================================
// Fast pre-filters (skip regex if pattern not present)
// ============================================================================

#[inline]
fn has_wikilink(content: &str) -> bool {
    content.contains("[[")
}

#[inline]
fn has_tag(content: &str) -> bool {
    content.contains('#')
}

#[inline]
fn has_callout(content: &str) -> bool {
    content.contains("[!")
}

#[inline]
fn has_extended_task(content: &str) -> bool {
    content.contains("[/]") || content.contains("[-]")
}

// ============================================================================
// Excluded ranges (code blocks, inline code, etc.)
// ============================================================================

/// Byte ranges to exclude from OFM regex parsing.
/// These are regions where markdown syntax should not be interpreted
/// (code blocks, inline code, HTML blocks, etc.)
#[derive(Debug, Default, Clone)]
struct ExcludedRanges {
    ranges: Vec<Range<usize>>,
}

impl ExcludedRanges {
    /// Check if a byte offset falls within any excluded range.
    #[inline]
    fn contains(&self, offset: usize) -> bool {
        // Optimized: Binary search for O(log N) lookup
        if self.ranges.is_empty() {
            return false;
        }

        // Find the first range that starts AFTER the offset.
        // The candidate range that might contain 'offset' is the one immediately preceding that.
        let idx = self.ranges.partition_point(|r| r.start <= offset);

        if idx == 0 {
            // All ranges start after offset, so none contain it
            return false;
        }

        // Check the range before the partition point
        // partition_point returns the index of the first element where the predicate is false
        // Predicate: r.start <= offset.
        // False means: r.start > offset.
        // So ranges[idx-1].start <= offset.
        let candidate = &self.ranges[idx - 1];
        offset < candidate.end
    }

    /// Add a range to exclude.
    fn add(&mut self, range: Range<usize>) {
        self.ranges.push(range);
    }

    /// Sort and merge overlapping ranges for efficient lookup.
    fn optimize(&mut self) {
        if self.ranges.is_empty() {
            return;
        }
        self.ranges.sort_by_key(|r| r.start);

        let mut merged = Vec::with_capacity(self.ranges.len());
        let mut current = self.ranges[0].clone();

        for range in self.ranges.iter().skip(1) {
            if range.start <= current.end {
                // Overlapping or adjacent, merge
                current.end = current.end.max(range.end);
            } else {
                merged.push(current);
                current = range.clone();
            }
        }
        merged.push(current);
        self.ranges = merged;
    }
}

// ============================================================================
// Parse result structure
// ============================================================================

/// Intermediate parse result used by the engine.
#[derive(Debug, Clone, Default)]
pub struct ParseResult {
    pub frontmatter: Option<Frontmatter>,
    pub frontmatter_end_offset: usize, // Offset where body starts (end of frontmatter)
    pub headings: Vec<Heading>,
    pub wikilinks: Vec<Link>,
    pub embeds: Vec<Link>,
    pub markdown_links: Vec<Link>,
    pub tags: Vec<OFMTag>,
    pub tasks: Vec<TaskItem>,
    pub callouts: Vec<Callout>,
}

impl ParseResult {
    /// Get all links combined.
    #[allow(dead_code)]
    pub fn all_links(&self) -> impl Iterator<Item = &Link> {
        self.wikilinks
            .iter()
            .chain(self.embeds.iter())
            .chain(self.markdown_links.iter())
    }
}

// ============================================================================
// Core parsing engine
// ============================================================================

/// Unified parsing engine that handles all OFM element extraction.
///
/// Uses a two-phase approach:
/// 1. pulldown-cmark pass: Extract CommonMark elements + code block ranges
/// 2. Regex pass: Extract OFM-specific elements, skipping excluded ranges
pub struct ParseEngine<'a> {
    content: &'a str,
    index: LineIndex,
    source_file: Option<&'a Path>,
}

impl<'a> ParseEngine<'a> {
    /// Create a new parse engine for the given content.
    pub fn new(content: &'a str) -> Self {
        Self {
            content,
            index: LineIndex::new(content),
            source_file: None,
        }
    }

    /// Create engine with source file context (for vault-aware parsing).
    pub fn with_source_file(content: &'a str, source_file: &'a Path) -> Self {
        Self {
            content,
            index: LineIndex::new(content),
            source_file: Some(source_file),
        }
    }

    /// Parse content with the given options.
    pub fn parse(&self, options: &ParseOptions) -> ParseResult {
        let mut result = ParseResult::default();

        // Phase 1: pulldown-cmark pass
        // - Extract frontmatter, headings, markdown links, tasks
        // - Build excluded ranges (code blocks, inline code)
        let (excluded, body_start) = self.pulldown_pass(options, &mut result);

        // Store the body start offset (which is the end of frontmatter)
        result.frontmatter_end_offset = body_start;

        // Phase 1.5: Extract wikilinks and embeds from frontmatter YAML values.
        // Obsidian vaults commonly store wikilinks in frontmatter fields like
        // Area: "[[Hub]]" and Links: ["[[Doc A]]", "[[Doc B]]"].
        // These are invisible to the body-only regex pass, so we scan them here.
        if options.parse_wikilinks && body_start > 0 {
            let frontmatter_text = &self.content[..body_start];
            self.parse_frontmatter_wikilinks(frontmatter_text, &mut result);
        }

        // Phase 2: OFM-specific regex pass (respecting excluded ranges)
        let body = if body_start > 0 {
            &self.content[body_start..]
        } else {
            self.content
        };

        if options.parse_tasks {
            self.parse_extended_tasks(body, body_start, &excluded, &mut result);
        }

        if options.parse_wikilinks {
            self.parse_wikilinks(body, body_start, &excluded, &mut result);
            self.parse_embeds(body, body_start, &excluded, &mut result);
        }

        if options.parse_tags {
            self.parse_tags(body, body_start, &excluded, &mut result);
        }

        // Callouts are line-based and need special handling
        if options.parse_callouts {
            self.parse_callouts(body, body_start, &excluded, options, &mut result);
        }

        result
    }

    /// Phase 1: pulldown-cmark pass for CommonMark elements and excluded ranges.
    fn pulldown_pass(
        &self,
        options: &ParseOptions,
        result: &mut ParseResult,
    ) -> (ExcludedRanges, usize) {
        let mut excluded = ExcludedRanges::default();
        let mut body_start: usize = 0;

        // Configure pulldown-cmark options
        let mut opts = Options::empty();
        opts.insert(Options::ENABLE_TASKLISTS);
        opts.insert(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
        opts.insert(Options::ENABLE_HEADING_ATTRIBUTES);
        opts.insert(Options::ENABLE_STRIKETHROUGH);
        opts.insert(Options::ENABLE_TABLES);

        let parser = Parser::new_ext(self.content, opts);

        // State tracking
        let mut in_code_block = false;
        let mut code_block_start: usize = 0;
        let mut in_metadata = false;
        let mut metadata_content = String::new();
        let mut current_heading: Option<(HeadingLevel, Option<String>)> = None;
        let mut heading_text = String::new();
        let mut heading_start: usize = 0;

        let mut in_task_item: bool = false;
        let mut task_builder: TaskBuilder = TaskBuilder::default();

        let mut current_link: Option<(String, String)> = None; // (url, title)
        let mut link_text = String::new();
        let mut link_start: usize = 0;
        let mut markdown_link_range_start: Option<usize> = None;
        let mut markdown_image_range_start: Option<usize> = None;

        for (event, range) in parser.into_offset_iter() {
            match event {
                // === Code blocks (fenced and indented) ===
                Event::Start(Tag::CodeBlock(_)) => {
                    in_code_block = true;
                    code_block_start = range.start;
                }
                Event::End(TagEnd::CodeBlock) => {
                    in_code_block = false;
                    excluded.add(code_block_start..range.end);
                }

                // === Inline code ===
                Event::Code(text) if current_heading.is_some() => {
                    heading_text.push_str(&text);
                    excluded.add(range.clone());
                }
                Event::Code(_) => {
                    excluded.add(range.clone());
                }

                // === HTML blocks ===
                Event::Html(_) => {
                    excluded.add(range.clone());
                }

                // === Metadata/Frontmatter ===
                Event::Start(Tag::MetadataBlock(_)) => {
                    in_metadata = true;
                    metadata_content.clear();
                }
                Event::End(TagEnd::MetadataBlock(_)) => {
                    in_metadata = false;
                    body_start = range.end;

                    if options.parse_frontmatter && !metadata_content.is_empty() {
                        // Parse YAML frontmatter
                        if let Ok(serde_json::Value::Object(map)) =
                            yaml_serde::from_str(&metadata_content)
                        {
                            result.frontmatter = Some(Frontmatter {
                                data: map.into_iter().collect(),
                                position: SourcePosition::start(),
                            });
                        }
                    }
                }
                Event::Text(text) if in_metadata => {
                    metadata_content.push_str(&text);
                }

                // === Headings ===
                Event::Start(Tag::Heading { level, id, .. }) if options.parse_headings => {
                    current_heading = Some((level, id.map(|s| s.to_string())));
                    heading_text.clear();
                    heading_start = range.start;
                }
                Event::End(TagEnd::Heading(_)) => {
                    if let Some((level, id)) = current_heading.take() {
                        let level_num = match level {
                            HeadingLevel::H1 => 1,
                            HeadingLevel::H2 => 2,
                            HeadingLevel::H3 => 3,
                            HeadingLevel::H4 => 4,
                            HeadingLevel::H5 => 5,
                            HeadingLevel::H6 => 6,
                        };

                        // Generate anchor if not provided
                        let anchor = id.or_else(|| Some(slugify(&heading_text)));

                        result.headings.push(Heading {
                            text: heading_text.trim().to_string(),
                            level: level_num,
                            position: SourcePosition::from_offset_indexed(
                                &self.index,
                                heading_start,
                                range.end - heading_start,
                            ),
                            anchor,
                        });
                    }
                }
                Event::Text(text) if current_heading.is_some() => {
                    heading_text.push_str(&text);
                }

                // === Tasks ===
                Event::TaskListMarker(_checked) if options.parse_tasks => {
                    in_task_item = true;
                    // Read raw marker byte to detect extended states [/] [-]
                    task_builder.is_completed = {
                        let raw_marker = self
                            .content
                            .as_bytes()
                            .get(range.start + 1)
                            .copied()
                            .unwrap_or(b' ') as char;
                        crate::models::TaskStatus::from_marker(raw_marker).is_completed()
                    };
                    task_builder.position = SourcePosition::from_offset_indexed(
                        &self.index,
                        task_line_start_for_marker(self.content, &self.index, range.start),
                        task_line_length_for_marker(self.content, &self.index, range.start),
                    );
                }

                Event::End(TagEnd::Item) if in_task_item => {
                    in_task_item = false;
                    if !task_builder.content.is_empty() {
                        result.tasks.push(task_builder.build());
                    }
                }

                Event::Text(text) if in_task_item => {
                    task_builder.content.push_str(&text);
                }

                // === Markdown Links ===
                Event::Start(Tag::Link {
                    dest_url, title, ..
                }) if !in_code_block => {
                    markdown_link_range_start = Some(range.start);
                    if options.parse_markdown_links {
                        current_link = Some((dest_url.to_string(), title.to_string()));
                        link_text.clear();
                        link_start = range.start;
                    }
                }
                Event::End(TagEnd::Link) => {
                    if let Some(start) = markdown_link_range_start.take() {
                        excluded.add(start..range.end);
                    }
                    if let Some((url, _title)) = current_link.take() {
                        let link_type = classify_url(&url);

                        result.markdown_links.push(Link {
                            type_: link_type,
                            source_file: self
                                .source_file
                                .map(|p| p.to_path_buf())
                                .unwrap_or_default(),
                            target: url,
                            display_text: Some(link_text.trim().to_string()),
                            position: SourcePosition::from_offset_indexed(
                                &self.index,
                                link_start,
                                range.end - link_start,
                            ),
                            resolved_target: None,
                            is_valid: true,
                        });
                    }
                    link_text.clear();
                }
                Event::Text(text) if current_link.is_some() => {
                    link_text.push_str(&text);
                }

                // Markdown image alt text and destinations should not leak OFM tags.
                Event::Start(Tag::Image { .. }) if !in_code_block => {
                    markdown_image_range_start = Some(range.start);
                }
                Event::End(TagEnd::Image) => {
                    if let Some(start) = markdown_image_range_start.take() {
                        excluded.add(start..range.end);
                    }
                }

                _ => {}
            }
        }

        excluded.optimize();
        (excluded, body_start)
    }

    /// Parse wikilinks, respecting excluded ranges.
    fn parse_wikilinks(
        &self,
        body: &str,
        body_offset: usize,
        excluded: &ExcludedRanges,
        result: &mut ParseResult,
    ) {
        if !has_wikilink(body) {
            return;
        }

        let source = self
            .source_file
            .map(|p| p.to_path_buf())
            .unwrap_or_default();

        for caps in WIKILINK.captures_iter(body) {
            let full_match = caps.get(0).unwrap();
            let local_start = full_match.start();
            let global_start = body_offset + local_start;

            // Skip if in excluded range (code block, inline code, etc.)
            if excluded.contains(global_start) {
                continue;
            }

            // Skip if preceded by ! (it's an embed)
            if local_start > 0 && body.as_bytes().get(local_start - 1) == Some(&b'!') {
                continue;
            }

            let raw_target = caps.get(1).unwrap().as_str();
            let (target, display_text) = parse_link_target(raw_target);
            let link_type = classify_wikilink(&target);

            result.wikilinks.push(Link {
                type_: link_type,
                source_file: source.clone(),
                target,
                display_text,
                position: SourcePosition::from_offset_indexed(
                    &self.index,
                    global_start,
                    full_match.len(),
                ),
                resolved_target: None,
                is_valid: true,
            });
        }
    }

    /// Parse embeds, respecting excluded ranges.
    fn parse_embeds(
        &self,
        body: &str,
        body_offset: usize,
        excluded: &ExcludedRanges,
        result: &mut ParseResult,
    ) {
        if !has_wikilink(body) {
            return;
        }

        let source = self
            .source_file
            .map(|p| p.to_path_buf())
            .unwrap_or_default();

        for caps in EMBED.captures_iter(body) {
            let full_match = caps.get(0).unwrap();
            let local_start = full_match.start();
            let global_start = body_offset + local_start;

            // Skip if in excluded range
            if excluded.contains(global_start) {
                continue;
            }

            let raw_target = caps.get(1).unwrap().as_str();
            let (target, display_text) = parse_link_target(raw_target);

            result.embeds.push(Link {
                type_: LinkType::Embed,
                source_file: source.clone(),
                target,
                display_text,
                position: SourcePosition::from_offset_indexed(
                    &self.index,
                    global_start,
                    full_match.len(),
                ),
                resolved_target: None,
                is_valid: true,
            });
        }
    }

    /// Extract wikilinks and embeds from YAML frontmatter string values.
    ///
    /// Obsidian vaults commonly store wikilinks in frontmatter fields:
    /// ```yaml
    /// Area: "[[My Project Hub]]"
    /// Links:
    ///   - "[[Doc A]]"
    ///   - "[[Doc B]]"
    /// ```
    ///
    /// The body parser skips frontmatter entirely, so these links would be
    /// invisible to the link graph without this dedicated extraction pass.
    fn parse_frontmatter_wikilinks(&self, frontmatter_text: &str, result: &mut ParseResult) {
        if !has_wikilink(frontmatter_text) {
            return;
        }

        let source = self
            .source_file
            .map(|p| p.to_path_buf())
            .unwrap_or_default();

        // Extract wikilinks: [[target]] or [[target|display]]
        for caps in WIKILINK.captures_iter(frontmatter_text) {
            let full_match = caps.get(0).unwrap();
            let global_start = full_match.start();

            // Skip if preceded by ! (it's an embed, handled below)
            if global_start > 0 && frontmatter_text.as_bytes().get(global_start - 1) == Some(&b'!')
            {
                continue;
            }

            let raw_target = caps.get(1).unwrap().as_str();
            let (target, display_text) = parse_link_target(raw_target);
            let link_type = classify_wikilink(&target);

            result.wikilinks.push(Link {
                type_: link_type,
                source_file: source.clone(),
                target,
                display_text,
                position: SourcePosition::from_offset_indexed(
                    &self.index,
                    global_start,
                    full_match.len(),
                ),
                resolved_target: None,
                is_valid: true,
            });
        }

        // Extract embeds: ![[target]]
        for caps in EMBED.captures_iter(frontmatter_text) {
            let full_match = caps.get(0).unwrap();
            let global_start = full_match.start();

            let raw_target = caps.get(1).unwrap().as_str();
            let (target, display_text) = parse_link_target(raw_target);

            result.embeds.push(Link {
                type_: LinkType::Embed,
                source_file: source.clone(),
                target,
                display_text,
                position: SourcePosition::from_offset_indexed(
                    &self.index,
                    global_start,
                    full_match.len(),
                ),
                resolved_target: None,
                is_valid: true,
            });
        }
    }

    /// Parse tags, respecting excluded ranges.
    fn parse_tags(
        &self,
        body: &str,
        body_offset: usize,
        excluded: &ExcludedRanges,
        result: &mut ParseResult,
    ) {
        if !has_tag(body) {
            return;
        }

        let mut excluded = excluded.clone();
        add_wikilink_exclusions(body, body_offset, &mut excluded);
        excluded.optimize();

        for caps in TAG.captures_iter(body) {
            // The actual tag starts at the # character
            let tag_name = caps.get(1).unwrap();
            let local_start = tag_name.start() - 1; // -1 for the # prefix
            let global_start = body_offset + local_start;

            // Skip if in excluded range
            if excluded.contains(global_start) {
                continue;
            }

            let name = tag_name.as_str();

            result.tags.push(OFMTag {
                name: name.to_string(),
                position: SourcePosition::from_offset_indexed(
                    &self.index,
                    global_start,
                    name.len() + 1, // +1 for #
                ),
                is_nested: name.contains('/'),
            });
        }
    }

    /// Parse Obsidian task states that pulldown-cmark's GFM task parser ignores.
    fn parse_extended_tasks(
        &self,
        body: &str,
        body_offset: usize,
        excluded: &ExcludedRanges,
        result: &mut ParseResult,
    ) {
        if !has_extended_task(body) {
            return;
        }

        let mut offset = 0;

        for line in body.lines() {
            let line_start = offset;
            let global_line_start = body_offset + line_start;
            let line_end_size = line_ending_size_at(body, line_start + line.len());
            offset += line.len() + line_end_size;

            if excluded.contains(global_line_start) {
                continue;
            }

            let Some(caps) = EXTENDED_TASK.captures(line) else {
                continue;
            };

            let content = caps.name("content").map(|m| m.as_str()).unwrap_or("");
            if content.is_empty() {
                continue;
            }

            let indent = caps.name("indent").map(|m| m.as_str()).unwrap_or("");
            let marker = caps
                .name("marker")
                .and_then(|m| m.as_str().chars().next())
                .unwrap_or(' ');
            let task_start = global_line_start + indent.len();
            let mut task_builder = TaskBuilder {
                content: content.to_string(),
                is_completed: crate::models::TaskStatus::from_marker(marker).is_completed(),
                position: SourcePosition::from_offset_indexed(
                    &self.index,
                    task_start,
                    line.len() - indent.len(),
                ),
            };

            result.tasks.push(task_builder.build());
        }
    }

    /// Parse callouts (line-based, with excluded range awareness).
    fn parse_callouts(
        &self,
        body: &str,
        body_offset: usize,
        excluded: &ExcludedRanges,
        options: &ParseOptions,
        result: &mut ParseResult,
    ) {
        if !has_callout(body) {
            return;
        }

        let lines: Vec<&str> = body.lines().collect();
        let mut offset = 0;
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i];
            let line_start = offset;
            let global_line_start = body_offset + line_start;
            let line_end_size = line_ending_size_at(body, offset + line.len());
            offset += line.len() + line_end_size;

            // Skip if this line is in an excluded range
            if excluded.contains(global_line_start) {
                i += 1;
                continue;
            }

            if let Some(caps) = CALLOUT.captures(line) {
                let callout = if options.full_callouts {
                    self.parse_callout_full(
                        &lines,
                        body,
                        &mut i,
                        global_line_start,
                        &caps,
                        excluded,
                        body_offset,
                        &mut offset,
                    )
                } else {
                    i += 1;
                    self.parse_callout_simple(line, global_line_start, &caps)
                };
                result.callouts.push(callout);
            } else {
                i += 1;
            }
        }
    }

    /// Parse simple callout (header only).
    fn parse_callout_simple(
        &self,
        line: &str,
        global_offset: usize,
        caps: &regex::Captures,
    ) -> Callout {
        let type_str = caps.get(1).unwrap().as_str();
        let fold_marker = caps.get(2).unwrap().as_str();
        let title_text = caps.get(3).unwrap().as_str();

        Callout {
            type_: parse_callout_type(type_str),
            title: if title_text.is_empty() {
                None
            } else {
                Some(title_text.to_string())
            },
            content: String::new(),
            position: SourcePosition::from_offset_indexed(&self.index, global_offset, line.len()),
            is_foldable: !fold_marker.is_empty(),
        }
    }

    /// Parse callout with full multi-line content.
    #[allow(clippy::too_many_arguments)]
    fn parse_callout_full(
        &self,
        lines: &[&str],
        body: &str,
        i: &mut usize,
        global_line_start: usize,
        caps: &regex::Captures,
        excluded: &ExcludedRanges,
        body_offset: usize,
        offset: &mut usize,
    ) -> Callout {
        let start_line_idx = *i;
        let first_line = lines[start_line_idx];
        let type_str = caps.get(1).unwrap().as_str();
        let fold_marker = caps.get(2).unwrap().as_str();
        let title_text = caps.get(3).unwrap().as_str();

        // Collect continuation lines
        let mut callout_content = String::new();
        *i += 1;

        while *i < lines.len() {
            let line = lines[*i];
            let line_global_start = body_offset + *offset;

            // Skip excluded ranges
            if excluded.contains(line_global_start) {
                *offset += line.len() + line_ending_size_at(body, *offset + line.len());
                *i += 1;
                continue;
            }

            // Check if this is a new callout (stop)
            if CALLOUT.is_match(line) {
                break;
            }

            // Check if continuation line
            if let Some(cont_caps) = CALLOUT_CONT.captures(line) {
                let content_part = cont_caps.get(1).unwrap().as_str();
                if !callout_content.is_empty() {
                    callout_content.push('\n');
                }
                callout_content.push_str(content_part);
                *offset += line.len() + line_ending_size_at(body, *offset + line.len());
                *i += 1;
            } else {
                break;
            }
        }

        Callout {
            type_: parse_callout_type(type_str),
            title: if title_text.is_empty() {
                None
            } else {
                Some(title_text.to_string())
            },
            content: callout_content,
            position: SourcePosition::from_offset_indexed(
                &self.index,
                global_line_start,
                first_line.len(),
            ),
            is_foldable: !fold_marker.is_empty(),
        }
    }
}

// ============================================================================
// Helper functions
// ============================================================================

/// Parse wikilink/embed target, extracting display text if present.
fn parse_link_target(raw: &str) -> (String, Option<String>) {
    if let Some(pipe_idx) = raw.find('|') {
        let target = raw[..pipe_idx].to_string();
        let display = raw[pipe_idx + 1..].to_string();
        (target, Some(display))
    } else {
        (raw.to_string(), None)
    }
}

/// Exclude Obsidian link spans from tag parsing.
///
/// Same-document anchors such as `[[#Heading]]` and embedded anchors such as
/// `![[#Preview]]` contain `#`, but they are links, not tags.
fn add_wikilink_exclusions(body: &str, body_offset: usize, excluded: &mut ExcludedRanges) {
    if !has_wikilink(body) {
        return;
    }

    for mat in WIKILINK.find_iter(body) {
        excluded.add(body_offset + mat.start()..body_offset + mat.end());
    }

    for mat in EMBED.find_iter(body) {
        excluded.add(body_offset + mat.start()..body_offset + mat.end());
    }
}

fn line_ending_size_at(content: &str, offset: usize) -> usize {
    let Some(remaining) = content.get(offset..) else {
        return 0;
    };

    if remaining.starts_with("\r\n") {
        2
    } else if remaining.starts_with('\n') {
        1
    } else {
        0
    }
}

fn task_line_start_for_marker(content: &str, index: &LineIndex, marker_offset: usize) -> usize {
    let (line, _) = index.line_col(marker_offset);
    let line_start = index.line_start(line).unwrap_or(marker_offset);
    let line_end = line_end_without_line_ending(content, line_start);
    let line_text = &content[line_start..line_end];
    line_start + leading_ascii_whitespace_len(line_text)
}

fn task_line_length_for_marker(content: &str, index: &LineIndex, marker_offset: usize) -> usize {
    let task_start = task_line_start_for_marker(content, index, marker_offset);
    line_end_without_line_ending(content, task_start) - task_start
}

fn line_end_without_line_ending(content: &str, line_start: usize) -> usize {
    let rest = content.get(line_start..).unwrap_or_default();
    let mut line_end = rest
        .find('\n')
        .map(|idx| line_start + idx)
        .unwrap_or(content.len());

    if line_end > line_start && content.as_bytes().get(line_end - 1) == Some(&b'\r') {
        line_end -= 1;
    }

    line_end
}

fn leading_ascii_whitespace_len(s: &str) -> usize {
    s.as_bytes()
        .iter()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count()
}

/// Parse callout type string into enum.
fn parse_callout_type(type_str: &str) -> CalloutType {
    match type_str.to_lowercase().as_str() {
        "note" => CalloutType::Note,
        "tip" => CalloutType::Tip,
        "info" => CalloutType::Info,
        "todo" => CalloutType::Todo,
        "important" => CalloutType::Important,
        "success" => CalloutType::Success,
        "question" => CalloutType::Question,
        "warning" => CalloutType::Warning,
        "failure" | "fail" | "missing" => CalloutType::Failure,
        "danger" | "error" => CalloutType::Danger,
        "bug" => CalloutType::Bug,
        "example" => CalloutType::Example,
        "quote" | "cite" => CalloutType::Quote,
        _ => CalloutType::Note,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_engine_wikilinks() {
        let content = "See [[Note]] and [[Other|display]]";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.wikilinks.len(), 2);
        assert_eq!(result.wikilinks[0].target, "Note");
        assert_eq!(result.wikilinks[1].target, "Other");
        assert_eq!(
            result.wikilinks[1].display_text,
            Some("display".to_string())
        );
    }

    #[test]
    fn test_engine_embeds_not_wikilinks() {
        let content = "[[Link]] and ![[Embed]]";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.wikilinks.len(), 1);
        assert_eq!(result.embeds.len(), 1);
        assert_eq!(result.wikilinks[0].target, "Link");
        assert_eq!(result.embeds[0].target, "Embed");
    }

    #[test]
    fn test_engine_markdown_links() {
        let content = "[text](url) and ![image](img.png)";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        // pulldown-cmark handles this - images are Tag::Image, not Tag::Link
        assert_eq!(result.markdown_links.len(), 1);
        assert_eq!(result.markdown_links[0].target, "url");
    }

    #[test]
    fn test_engine_tags() {
        let content = "Has #tag and #nested/tag";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.tags.len(), 2);
        assert!(!result.tags[0].is_nested);
        assert!(result.tags[1].is_nested);
    }

    #[test]
    fn test_engine_headings_via_pulldown() {
        let content = "# Heading 1\n\n## Heading 2\n\n### Heading 3";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.headings.len(), 3);
        assert_eq!(result.headings[0].level, 1);
        assert_eq!(result.headings[0].text, "Heading 1");
        assert_eq!(result.headings[1].level, 2);
        assert_eq!(result.headings[2].level, 3);
    }

    // Regression test for PR #15: inline code inside a heading must appear in
    // the extracted heading text rather than being dropped.
    #[test]
    fn test_engine_heading_with_inline_code() {
        let content = "# Use `foo()` carefully\n\nBody.";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.headings.len(), 1);
        assert_eq!(result.headings[0].text, "Use foo() carefully");
    }

    #[test]
    fn test_engine_tasks_via_pulldown() {
        let content = "- [ ] Todo task\n- [x] Done task";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.tasks.len(), 2);
        assert!(!result.tasks[0].is_completed);
        assert_eq!(result.tasks[0].content, "Todo task");
        assert!(result.tasks[1].is_completed);
        assert_eq!(result.tasks[1].content, "Done task");
    }

    #[test]
    fn test_engine_frontmatter() {
        let content = "---\ntitle: Test\nauthor: Alice\n---\n\n# Content";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert!(result.frontmatter.is_some());
        let fm = result.frontmatter.unwrap();
        assert_eq!(fm.data.get("title").and_then(|v| v.as_str()), Some("Test"));
        assert_eq!(
            fm.data.get("author").and_then(|v| v.as_str()),
            Some("Alice")
        );
    }

    #[test]
    fn test_engine_callout_simple() {
        let content = "> [!NOTE] This is a note\n> Content here";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.callouts.len(), 1);
        assert_eq!(result.callouts[0].type_, CalloutType::Note);
        assert_eq!(result.callouts[0].title, Some("This is a note".to_string()));
    }

    #[test]
    fn test_engine_callout_full() {
        let content = "> [!WARNING] Title\n> Line 1\n> Line 2";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all().with_full_callouts());

        assert_eq!(result.callouts.len(), 1);
        assert_eq!(result.callouts[0].content, "Line 1\nLine 2");
    }

    #[test]
    fn test_full_callout_crlf_offsets_next_callout() {
        let content = "> [!NOTE] First\r\n> Line 1\r\n> [!WARNING] Second\r\n> Line 2";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all().with_full_callouts());

        assert_eq!(result.callouts.len(), 2);
        assert_eq!(result.callouts[0].content, "Line 1");
        assert_eq!(result.callouts[1].title, Some("Second".to_string()));
        assert_eq!(result.callouts[1].content, "Line 2");
        assert_eq!(result.callouts[1].position.line, 3);
        assert_eq!(result.callouts[1].position.column, 1);
    }

    // =========================================================================
    // CODE BLOCK EXCLUSION TESTS - The main reason for pulldown-cmark
    // =========================================================================

    #[test]
    fn test_code_block_excludes_wikilinks() {
        let content = r#"
Normal [[Valid Link]] here.

```rust
// Code block
let link = "[[Fake Link Inside Code]]";
```

Also [[Another Valid]]
"#;
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        // Should only find the 2 valid links, NOT the one inside the code block
        assert_eq!(result.wikilinks.len(), 2);
        assert_eq!(result.wikilinks[0].target, "Valid Link");
        assert_eq!(result.wikilinks[1].target, "Another Valid");
    }

    #[test]
    fn test_code_block_excludes_embeds() {
        let content = r#"
![[Valid Embed]]

```
![[Fake Embed In Code]]
```
"#;
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.embeds.len(), 1);
        assert_eq!(result.embeds[0].target, "Valid Embed");
    }

    #[test]
    fn test_code_block_excludes_tags() {
        let content = r##"
Real #tag here.

```python
# This is a comment, not a tag
x = "#notag"
```

Another #valid-tag
"##;
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.tags.len(), 2);
        assert_eq!(result.tags[0].name, "tag");
        assert_eq!(result.tags[1].name, "valid-tag");
    }

    #[test]
    fn test_inline_code_excludes_patterns() {
        let content = "See [[Valid]] and `[[Not A Link]]` inline";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.wikilinks.len(), 1);
        assert_eq!(result.wikilinks[0].target, "Valid");
    }

    #[test]
    fn test_indented_code_block_excludes() {
        let content = r#"
Normal [[Link]]

    // Indented code block
    [[Not A Link]]

Back to normal [[Valid]]
"#;
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        // Indented code blocks are also excluded
        assert_eq!(result.wikilinks.len(), 2);
    }

    #[test]
    fn test_multiple_code_blocks() {
        let content = r#"
[[Link1]]

```
[[Fake1]]
```

[[Link2]]

```python
[[Fake2]]
```

[[Link3]]
"#;
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.wikilinks.len(), 3);
        assert_eq!(result.wikilinks[0].target, "Link1");
        assert_eq!(result.wikilinks[1].target, "Link2");
        assert_eq!(result.wikilinks[2].target, "Link3");
    }

    #[test]
    fn test_position_tracking() {
        let content = "Line 1\n[[Link]] on line 2";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.wikilinks[0].position.line, 2);
        assert_eq!(result.wikilinks[0].position.column, 1);
        assert_eq!(result.wikilinks[0].position.offset, 7);
    }

    #[test]
    fn test_selective_parsing() {
        let content = "# Heading\n[[Link]] #tag";
        let engine = ParseEngine::new(content);

        let opts = ParseOptions {
            parse_wikilinks: true,
            parse_headings: false,
            parse_tags: false,
            ..ParseOptions::none()
        };
        let result = engine.parse(&opts);

        assert_eq!(result.wikilinks.len(), 1);
        assert!(result.headings.is_empty());
        assert!(result.tags.is_empty());
    }

    #[test]
    fn test_fast_path_empty() {
        let content = "Plain text without any OFM elements";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert!(result.wikilinks.is_empty());
        assert!(result.embeds.is_empty());
        assert!(result.markdown_links.is_empty());
        assert!(result.tags.is_empty());
        assert!(result.headings.is_empty());
        assert!(result.tasks.is_empty());
        assert!(result.callouts.is_empty());
    }

    #[test]
    fn test_excluded_ranges_optimization() {
        let mut excluded = ExcludedRanges::default();
        excluded.add(0..10);
        excluded.add(5..15); // Overlapping
        excluded.add(20..30);
        excluded.add(25..35); // Overlapping

        excluded.optimize();

        // Should merge to [0..15, 20..35]
        assert_eq!(excluded.ranges.len(), 2);
        assert_eq!(excluded.ranges[0], 0..15);
        assert_eq!(excluded.ranges[1], 20..35);
    }

    // ============================================================================
    // Link type classification tests
    // ============================================================================

    #[test]
    fn test_wikilink_block_ref() {
        let content = "See [[Note#^blockid]] for reference";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.wikilinks.len(), 1);
        assert_eq!(result.wikilinks[0].target, "Note#^blockid");
        assert_eq!(result.wikilinks[0].type_, LinkType::BlockRef);
    }

    #[test]
    fn test_wikilink_heading_ref() {
        let content = "See [[Note#Heading]] for details";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.wikilinks.len(), 1);
        assert_eq!(result.wikilinks[0].target, "Note#Heading");
        assert_eq!(result.wikilinks[0].type_, LinkType::HeadingRef);
    }

    #[test]
    fn test_wikilink_same_doc_anchor() {
        let content = "See [[#Heading]] in this document";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.wikilinks.len(), 1);
        assert_eq!(result.wikilinks[0].target, "#Heading");
        assert_eq!(result.wikilinks[0].type_, LinkType::Anchor);
    }

    #[test]
    fn test_wikilink_same_doc_block_ref() {
        let content = "See [[#^blockid]] in this document";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.wikilinks.len(), 1);
        assert_eq!(result.wikilinks[0].target, "#^blockid");
        assert_eq!(result.wikilinks[0].type_, LinkType::BlockRef);
    }

    #[test]
    fn test_markdown_link_anchor() {
        let content = "Jump to [section](#installation)";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.markdown_links.len(), 1);
        assert_eq!(result.markdown_links[0].target, "#installation");
        assert_eq!(result.markdown_links[0].type_, LinkType::Anchor);
    }

    #[test]
    fn test_markdown_link_heading_ref() {
        let content = "See [API](docs/api.md#methods) reference";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.markdown_links.len(), 1);
        assert_eq!(result.markdown_links[0].target, "docs/api.md#methods");
        assert_eq!(result.markdown_links[0].type_, LinkType::HeadingRef);
    }

    #[test]
    fn test_markdown_link_block_ref() {
        let content = "See [block](note.md#^abc123) reference";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.markdown_links.len(), 1);
        assert_eq!(result.markdown_links[0].target, "note.md#^abc123");
        assert_eq!(result.markdown_links[0].type_, LinkType::BlockRef);
    }

    #[test]
    fn test_markdown_link_external() {
        let content = "Visit [site](https://example.com)";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.markdown_links.len(), 1);
        assert_eq!(result.markdown_links[0].type_, LinkType::ExternalLink);
    }

    #[test]
    fn test_markdown_link_relative() {
        let content = "See [docs](./docs/api.md) for more";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.markdown_links.len(), 1);
        assert_eq!(result.markdown_links[0].type_, LinkType::MarkdownLink);
    }

    #[test]
    fn test_heading_anchor_generation() {
        // Note: pulldown-cmark separates inline code, so `code` becomes separate event
        // The heading text captured is "BIG heading" + "code" + " ~+-!@#"
        let content = "# BIG heading?! with Special @chars";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.headings.len(), 1);
        // Should be lowercased, special chars removed, spaces to hyphens
        assert_eq!(
            result.headings[0].anchor,
            Some("big-heading-with-special-chars".to_string())
        );
    }

    #[test]
    fn test_heading_anchor_consecutive_spaces() {
        let content = "# Multiple   Spaces   Here";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.headings.len(), 1);
        // Consecutive spaces should collapse to single hyphen
        assert_eq!(
            result.headings[0].anchor,
            Some("multiple-spaces-here".to_string())
        );
    }

    // =========================================================================
    // FRONTMATTER WIKILINK EXTRACTION TESTS
    // =========================================================================

    #[test]
    fn test_frontmatter_wikilink_single() {
        let content = "---\nArea: \"[[My Hub]]\"\n---\n\n# Content";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.wikilinks.len(), 1);
        assert_eq!(result.wikilinks[0].target, "My Hub");
    }

    #[test]
    fn test_frontmatter_wikilink_list() {
        let content =
            "---\nLinks:\n  - \"[[Doc A]]\"\n  - \"[[Doc B]]\"\n  - \"[[Doc C]]\"\n---\n\nBody";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.wikilinks.len(), 3);
        assert_eq!(result.wikilinks[0].target, "Doc A");
        assert_eq!(result.wikilinks[1].target, "Doc B");
        assert_eq!(result.wikilinks[2].target, "Doc C");
    }

    #[test]
    fn test_frontmatter_wikilink_with_display_text() {
        let content = "---\nArea: \"[[Hub|My Display Text]]\"\n---\n\nBody";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.wikilinks.len(), 1);
        assert_eq!(result.wikilinks[0].target, "Hub");
        assert_eq!(
            result.wikilinks[0].display_text,
            Some("My Display Text".to_string())
        );
    }

    #[test]
    fn test_frontmatter_wikilink_with_heading_ref() {
        let content = "---\nLinks:\n  - \"[[Note#Section]]\"\n---\n\nBody";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.wikilinks.len(), 1);
        assert_eq!(result.wikilinks[0].target, "Note#Section");
        assert_eq!(result.wikilinks[0].type_, LinkType::HeadingRef);
    }

    #[test]
    fn test_frontmatter_embed() {
        let content = "---\nBanner: \"![[image.png]]\"\n---\n\nBody";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.embeds.len(), 1);
        assert_eq!(result.embeds[0].target, "image.png");
        assert_eq!(result.embeds[0].type_, LinkType::Embed);
        // The embed should NOT also appear as a wikilink
        assert!(result.wikilinks.is_empty());
    }

    #[test]
    fn test_frontmatter_and_body_wikilinks_combined() {
        let content = "---\nArea: \"[[Hub]]\"\n---\n\nSee [[Body Link]] here";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.wikilinks.len(), 2);
        assert_eq!(result.wikilinks[0].target, "Hub");
        assert_eq!(result.wikilinks[1].target, "Body Link");
    }

    #[test]
    fn test_frontmatter_wikilinks_not_extracted_when_disabled() {
        let content = "---\nArea: \"[[Hub]]\"\n---\n\nBody";
        let engine = ParseEngine::new(content);
        let opts = ParseOptions {
            parse_wikilinks: false,
            ..ParseOptions::none()
        };
        let result = engine.parse(&opts);

        assert!(result.wikilinks.is_empty());
    }

    #[test]
    fn test_frontmatter_wikilink_unicode() {
        let content = "---\nArea: \"[[My Project Hub]]\"\n---\n\nBody";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.wikilinks.len(), 1);
        assert_eq!(result.wikilinks[0].target, "My Project Hub");
    }

    #[test]
    fn test_frontmatter_no_wikilinks() {
        let content = "---\ntitle: Just a string\ntags:\n  - rust\n---\n\nBody";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert!(result.wikilinks.is_empty());
    }

    #[test]
    fn test_frontmatter_multiple_fields_with_wikilinks() {
        let content = "---\nArea: \"[[Hub]]\"\nLayer: \"[[Security Layer]]\"\nLinks:\n  - \"[[Doc A]]\"\n---\n\nBody";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.wikilinks.len(), 3);
        let targets: Vec<&str> = result.wikilinks.iter().map(|l| l.target.as_str()).collect();
        assert!(targets.contains(&"Hub"));
        assert!(targets.contains(&"Security Layer"));
        assert!(targets.contains(&"Doc A"));
    }

    #[test]
    fn test_no_frontmatter_no_crash() {
        let content = "No frontmatter here\n\n[[Link]]";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.wikilinks.len(), 1);
        assert_eq!(result.wikilinks[0].target, "Link");
    }

    // =========================================================================
    // Tag regex and TaskStatus integration tests
    // =========================================================================

    #[test]
    fn test_digit_first_tag() {
        // Tags that start with a digit should be extracted; the regex allows [a-zA-Z0-9_]
        // as the first character of the tag name.
        let content = "#2024 is a year";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        let names: Vec<&str> = result.tags.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"2024"),
            "expected tag '2024' in {:?}",
            names
        );
    }

    #[test]
    fn test_numeric_tag_with_subtag() {
        let content = "#2024/q1";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        let names: Vec<&str> = result.tags.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"2024/q1"),
            "expected tag '2024/q1' in {:?}",
            names
        );
    }

    #[test]
    fn test_tag_in_url_not_matched() {
        // The # in a URL fragment is preceded by a non-whitespace word character,
        // so the look-behind `(?:^|[\s\[(])` should prevent a match.
        let content = "See https://example.com#section for details";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        let names: Vec<&str> = result.tags.iter().map(|t| t.name.as_str()).collect();
        assert!(
            !names.contains(&"section"),
            "tag 'section' should NOT be extracted from a URL fragment, got {:?}",
            names
        );
    }

    #[test]
    fn test_tag_in_markdown_anchor_link_not_matched() {
        let content = "Jump to [section](#installation) and keep #real-tag";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.markdown_links.len(), 1);
        let names: Vec<&str> = result.tags.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["real-tag"]);
    }

    #[test]
    fn test_tag_only_parse_excludes_markdown_anchor_link() {
        let content = "Jump to [section](#installation) and keep #real-tag";
        let engine = ParseEngine::new(content);
        let opts = ParseOptions {
            parse_tags: true,
            ..ParseOptions::none()
        };
        let result = engine.parse(&opts);

        let names: Vec<&str> = result.tags.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["real-tag"]);
    }

    #[test]
    fn test_tag_in_same_doc_wikilink_not_matched() {
        let content = "See [[#Heading]] and ![[#Preview]] but keep #real";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.wikilinks.len(), 1);
        assert_eq!(result.embeds.len(), 1);
        let names: Vec<&str> = result.tags.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["real"]);
    }

    #[test]
    fn test_tag_only_parse_excludes_same_doc_wikilink() {
        let content = "See [[#Heading]] and ![[#Preview]] but keep #real";
        let engine = ParseEngine::new(content);
        let opts = ParseOptions {
            parse_tags: true,
            ..ParseOptions::none()
        };
        let result = engine.parse(&opts);

        let names: Vec<&str> = result.tags.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["real"]);
    }

    #[test]
    fn test_parenthesized_tag_still_matches() {
        let content = "This remains valid (#context)";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        let names: Vec<&str> = result.tags.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["context"]);
    }

    #[test]
    fn test_task_status_in_progress() {
        let content = "- [/] In progress task";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.tasks.len(), 1);
        assert_eq!(result.tasks[0].content, "In progress task");
        assert!(!result.tasks[0].is_completed);
        assert_eq!(result.tasks[0].position.line, 1);
        assert_eq!(result.tasks[0].position.column, 1);
    }

    #[test]
    fn test_task_status_cancelled() {
        let content = "- [-] Cancelled task";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.tasks.len(), 1);
        assert_eq!(result.tasks[0].content, "Cancelled task");
        assert!(!result.tasks[0].is_completed);
        assert_eq!(result.tasks[0].position.line, 1);
        assert_eq!(result.tasks[0].position.column, 1);
    }

    #[test]
    fn test_extended_tasks_exclude_code_blocks() {
        let content = "```md\n- [/] Not a task\n```\n\n- [-] Real task";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.tasks.len(), 1);
        assert_eq!(result.tasks[0].content, "Real task");
        assert_eq!(result.tasks[0].position.line, 5);
    }

    #[test]
    fn test_task_priority_parsing() {
        use turbovault_core::TaskPriority;

        let content = "\
# Some header

Some text here.
- [ ] A lowest priority task ⏬
- [ ] A low priority task 🔽
- [ ] A task with double priority ⏬ 🔺
- [ ] A normal priority task
- [ ] A medium priority task 🔼
- [ ] A high priority task ⏫
- [ ] A highest priority task 🔺
";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.tasks.len(), 7,);

        assert_eq!(
            result.tasks[0].priority,
            TaskPriority::Lowest,
            "Should parse ⏬ as lowest priority marker"
        );
        assert_eq!(result.tasks[0].content, "A lowest priority task");

        assert_eq!(
            result.tasks[1].priority,
            TaskPriority::Low,
            "Should parse 🔽 as low priority marker"
        );
        assert_eq!(result.tasks[1].content, "A low priority task");

        assert_eq!(
            result.tasks[2].priority,
            TaskPriority::Normal,
            "Should parse double marker as normal priority"
        );
        assert_eq!(result.tasks[2].content, "A task with double priority ⏬ 🔺");

        assert_eq!(
            result.tasks[3].priority,
            TaskPriority::Normal,
            "Should parse no marker as normal priority"
        );

        assert_eq!(
            result.tasks[4].priority,
            TaskPriority::Medium,
            "Should parse 🔼 marker as medium priority"
        );

        assert_eq!(
            result.tasks[5].priority,
            TaskPriority::High,
            "Should parse ⏫ marker as high priority"
        );

        assert_eq!(
            result.tasks[6].priority,
            TaskPriority::Highest,
            "Should parse 🔺 marker as highest priority"
        );
    }

    #[test]
    fn test_task_metadata_parsing() {
        let content = "- [ ] Review PR [due:: 2026-05-01], [project:: [[Team Work]]], [id:: pr-123], [dependsOn:: abc123,def456] 🔁 every weekday #review ^pr-123";
        let engine = ParseEngine::new(content);
        let result = engine.parse(&ParseOptions::all());

        assert_eq!(result.tasks.len(), 1);
        assert_eq!(result.tasks[0].content, "Review PR");
        assert_eq!(
            result.tasks[0]
                .due_date
                .map(|date| date.to_string())
                .as_deref(),
            Some("2026-05-01")
        );
        assert_eq!(result.tasks[0].recurrence.as_deref(), Some("every weekday"));
        assert_eq!(result.tasks[0].id.as_deref(), Some("pr-123"));
        assert_eq!(
            result.tasks[0].depends_on,
            vec!["abc123".to_string(), "def456".to_string()]
        );
        assert_eq!(result.tasks[0].tags, vec!["review".to_string()]);
        assert_eq!(result.tasks[0].block_ref.as_deref(), Some("pr-123"));
        assert_eq!(
            result.tasks[0].metadata.get("project").map(String::as_str),
            Some("[[Team Work]]")
        );
    }
}
