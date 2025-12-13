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
    Tag as OFMTag, TaskItem,
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
    LazyLock::new(|| Regex::new(r"(?:^|[\s\[(])#([a-zA-Z][a-zA-Z0-9_\-/]*)").unwrap());

/// Callout start: > [!TYPE] with optional fold marker and title
static CALLOUT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*>\s*\[!(\w+)\]([+-]?)\s*(.*?)$").unwrap());

/// Callout continuation: > content
static CALLOUT_CONT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*>\s*(.*)$").unwrap());

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

        // Phase 2: OFM-specific regex pass (respecting excluded ranges)
        let body = if body_start > 0 {
            &self.content[body_start..]
        } else {
            self.content
        };

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
        let mut in_task_item = false;
        let mut task_checked = false;
        let mut task_content = String::new();
        let mut task_start: usize = 0;
        let mut current_link: Option<(String, String)> = None; // (url, title)
        let mut link_text = String::new();
        let mut link_start: usize = 0;

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
                            serde_yaml::from_str(&metadata_content)
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
                Event::Start(Tag::Heading { level, id, .. }) => {
                    if options.parse_headings {
                        current_heading = Some((level, id.map(|s| s.to_string())));
                        heading_text.clear();
                        heading_start = range.start;
                    }
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
                Event::TaskListMarker(checked) => {
                    if options.parse_tasks {
                        in_task_item = true;
                        task_checked = checked;
                        task_content.clear();
                        task_start = range.start;
                    }
                }
                Event::End(TagEnd::Item) if in_task_item => {
                    in_task_item = false;
                    if !task_content.is_empty() {
                        result.tasks.push(TaskItem {
                            content: task_content.trim().to_string(),
                            is_completed: task_checked,
                            position: SourcePosition::from_offset_indexed(
                                &self.index,
                                task_start,
                                range.end - task_start,
                            ),
                            due_date: None,
                        });
                    }
                    task_content.clear();
                }
                Event::Text(text) if in_task_item => {
                    task_content.push_str(&text);
                }

                // === Markdown Links ===
                Event::Start(Tag::Link {
                    dest_url, title, ..
                }) => {
                    if options.parse_markdown_links && !in_code_block {
                        current_link = Some((dest_url.to_string(), title.to_string()));
                        link_text.clear();
                        link_start = range.start;
                    }
                }
                Event::End(TagEnd::Link) => {
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
            offset += line.len() + 1; // +1 for newline

            // Skip if this line is in an excluded range
            if excluded.contains(global_line_start) {
                i += 1;
                continue;
            }

            if let Some(caps) = CALLOUT.captures(line) {
                let callout = if options.full_callouts {
                    self.parse_callout_full(
                        &lines,
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
                *offset += line.len() + 1;
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
                *offset += line.len() + 1;
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
}
