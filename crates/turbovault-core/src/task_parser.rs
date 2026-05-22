//! Winnow parsers for Obsidian task metadata.
//!
//! Supports the Obsidian Tasks emoji format and Dataview inline field format.
//! Metadata is parsed only when it appears in the trailing metadata section of
//! the task, matching the Tasks plugin behavior described in its task-format
//! docs.
//!
//! This module intentionally exposes two entry points:
//!
//! - [`parse_task_content`] parses the text after the checkbox marker. This is
//!   the path used by the pulldown-cmark parser, because pulldown-cmark already
//!   recognizes Markdown task list markers and reports the task body separately.
//! - [`parse_task_line`] parses a raw Markdown task line, including its checkbox.
//!   It is useful for standalone line parsing, parser tests, and future
//!   task-focused APIs that do not need to run the full Markdown parser first.
//!
//! Keeping both entry points in one module ensures every caller shares the same
//! metadata parser instead of reimplementing task metadata extraction.

use std::collections::HashMap;

use chrono::NaiveDate;
use winnow::{
    ModalResult, Parser,
    ascii::space0,
    combinator::alt,
    error::{ContextError, ErrMode},
    token::{literal, one_of, take_while},
};

const DUE_EMOJI: &str = "📅";
const SCHEDULED_EMOJI: &str = "⏳";
const START_EMOJI: &str = "🛫";
const DONE_EMOJI: &str = "✅";
const CANCELLED_EMOJI: &str = "❌";
const CREATED_EMOJI: &str = "➕";
const RECURRENCE_EMOJI: &str = "🔁";
const ON_COMPLETION_EMOJI: &str = "🏁";
const ID_EMOJI: &str = "🆔";
const DEPENDS_ON_EMOJI: &str = "⛔";
const PRIORITY_HIGHEST_EMOJI: &str = "🔺";
const PRIORITY_HIGH_EMOJI: &str = "⏫";
const PRIORITY_MEDIUM_EMOJI: &str = "🔼";
const PRIORITY_LOW_EMOJI: &str = "🔽";
const PRIORITY_LOWEST_EMOJI: &str = "⏬";

/// Parsed content and trailing metadata for an Obsidian task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedTaskMetadata {
    pub description: String,
    pub due: Option<String>,
    pub scheduled: Option<String>,
    pub start: Option<String>,
    pub done: Option<String>,
    pub cancelled: Option<String>,
    pub created: Option<String>,
    pub priority: Option<char>,
    pub recurrence: Option<String>,
    pub on_completion: Option<String>,
    pub id: Option<String>,
    pub depends_on: Vec<String>,
    pub tags: Vec<String>,
    pub block_ref: Option<String>,
    pub metadata: HashMap<String, String>,
}

/// A parsed Obsidian task line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// Status character inside the checkbox.
    pub status: char,
    /// Human-readable description with trailing metadata removed.
    pub description: String,
    pub due: Option<String>,
    pub scheduled: Option<String>,
    pub start: Option<String>,
    pub done: Option<String>,
    pub cancelled: Option<String>,
    pub created: Option<String>,
    pub priority: Option<char>,
    pub recurrence: Option<String>,
    pub on_completion: Option<String>,
    pub id: Option<String>,
    pub depends_on: Vec<String>,
    pub tags: Vec<String>,
    pub block_ref: Option<String>,
    /// Dataview inline fields, including custom fields.
    pub metadata: HashMap<String, String>,
}

/// Parse a complete Obsidian task line, including the checkbox marker.
///
/// This is not used by the pulldown-cmark integration path, which receives task
/// content after Markdown has already identified the checkbox. It exists as a
/// small standalone API for callers that have a raw task line and want the same
/// metadata parsing behavior as the full document parser.
///
/// # Errors
///
/// Returns `Err` when the line does not start with a supported task checkbox.
pub fn parse_task_line(input: &str) -> Result<Task, String> {
    let mut input = input.trim();
    let status = parse_checkbox(&mut input).map_err(|e| format!("expected checkbox: {e}"))?;
    let parsed = parse_task_content(input);

    Ok(Task {
        status,
        description: parsed.description,
        due: parsed.due,
        scheduled: parsed.scheduled,
        start: parsed.start,
        done: parsed.done,
        cancelled: parsed.cancelled,
        created: parsed.created,
        priority: parsed.priority,
        recurrence: parsed.recurrence,
        on_completion: parsed.on_completion,
        id: parsed.id,
        depends_on: parsed.depends_on,
        tags: parsed.tags,
        block_ref: parsed.block_ref,
        metadata: parsed.metadata,
    })
}

/// Parse task text after the checkbox marker.
///
/// The returned description is the original content before the trailing metadata
/// section. Metadata-like text in the middle of the description is preserved.
#[must_use]
pub fn parse_task_content(content: &str) -> ParsedTaskMetadata {
    let meta_start = find_metadata_start(content);
    let mut parsed = parse_metadata_section(content[meta_start..].trim_start());
    parsed.tags = extract_tags(content);
    parsed.description = build_description(content, meta_start);
    parsed
}

fn parse_checkbox(input: &mut &str) -> ModalResult<char> {
    let _ = space0.parse_next(input)?;
    let _ = one_of(['-', '*', '+']).parse_next(input)?;
    let _ = space0.parse_next(input)?;
    let _ = literal("[").parse_next(input)?;
    let status = one_of([' ', 'x', 'X', '-', '>', '<', '/']).parse_next(input)?;
    let _ = literal("]").parse_next(input)?;
    let _ = space0.parse_next(input)?;
    Ok(status)
}

fn find_metadata_start(content: &str) -> usize {
    let mut in_word = false;
    for (i, c) in content.char_indices() {
        if c.is_whitespace() {
            in_word = false;
        } else if !in_word {
            in_word = true;
            let rest = &content[i..];
            if starts_metadata_token(rest)
                && !previous_token_is_priority(&content[..i])
                && is_pure_metadata(rest)
            {
                return i;
            }
        }
    }
    content.len()
}

fn previous_token_is_priority(prefix: &str) -> bool {
    prefix
        .split_whitespace()
        .next_back()
        .is_some_and(is_priority_emoji)
}

fn is_pure_metadata(s: &str) -> bool {
    let mut input = s;
    let mut seen_priority = false;
    let mut seen_item = false;
    loop {
        if input.is_empty() {
            return seen_item;
        }

        if consume_metadata_separator(&mut input).is_err() {
            return false;
        }

        match parse_one_metadata_item(&mut input) {
            Ok(item) => {
                seen_item = true;
                if item.priority.is_some() {
                    if seen_priority {
                        return false;
                    }
                    seen_priority = true;
                }
            }
            Err(_) => return false,
        }
    }
}

fn parse_metadata_section(meta_str: &str) -> ParsedTaskMetadata {
    let mut result = ParsedTaskMetadata::default();
    let mut input = meta_str;

    loop {
        if input.is_empty() {
            break;
        }

        if consume_metadata_separator(&mut input).is_err() {
            break;
        }

        match parse_one_metadata_item(&mut input) {
            Ok(item) => merge_metadata(&mut result, item),
            Err(_) => break,
        }
    }

    result
}

fn build_description(content: &str, meta_start: usize) -> String {
    let mut description = content[..meta_start].trim_end().to_string();

    for tag in extract_tag_literals(&content[meta_start..]) {
        if !description.is_empty() {
            description.push(' ');
        }
        description.push_str(tag);
    }

    description
}

fn extract_tags(content: &str) -> Vec<String> {
    extract_tag_literals(content)
        .into_iter()
        .map(|tag| tag.trim_start_matches('#').to_string())
        .collect()
}

fn extract_tag_literals(content: &str) -> Vec<&str> {
    let mut tags = Vec::new();

    for (i, c) in content.char_indices() {
        if c == '#'
            && is_tag_boundary(content, i)
            && let Some((_, end)) = parse_tag_literal(&content[i..])
        {
            tags.push(&content[i..i + end]);
        }
    }

    tags
}

fn is_tag_boundary(content: &str, index: usize) -> bool {
    content[..index]
        .chars()
        .next_back()
        .is_none_or(|c| c.is_whitespace())
}

fn parse_tag_literal(s: &str) -> Option<(&str, usize)> {
    let rest = s.strip_prefix('#')?;
    let mut end = 1usize;
    let mut has_alpha = false;
    let mut has_name = false;

    for c in rest.chars() {
        if c.is_alphanumeric() || c == '-' || c == '_' || c == '/' {
            has_name = true;
            has_alpha |= c.is_alphabetic();
            end += c.len_utf8();
        } else {
            break;
        }
    }

    if has_name && has_alpha {
        Some((&s[1..end], end))
    } else {
        None
    }
}

fn consume_metadata_separator(input: &mut &str) -> ModalResult<()> {
    let _ = space0.parse_next(input)?;

    if input.starts_with(',') {
        let _ = literal(",").parse_next(input)?;
        let _ = space0.parse_next(input)?;
    }

    Ok(())
}

fn parse_one_metadata_item(input: &mut &str) -> ModalResult<ParsedTaskMetadata> {
    alt((
        parse_emoji_date_field,
        parse_recurrence_field,
        parse_on_completion_field,
        parse_id_field,
        parse_depends_on_field,
        parse_dataview_field,
        parse_priority_emoji,
        parse_tag_field,
        parse_block_ref_field,
    ))
    .parse_next(input)
}

fn parse_emoji_date_field(input: &mut &str) -> ModalResult<ParsedTaskMetadata> {
    let key = alt((
        literal(DUE_EMOJI).value("due"),
        literal(SCHEDULED_EMOJI).value("scheduled"),
        literal(START_EMOJI).value("start"),
        literal(DONE_EMOJI).value("done"),
        literal(CANCELLED_EMOJI).value("cancelled"),
        literal(CREATED_EMOJI).value("created"),
    ))
    .parse_next(input)?;
    let _ = space0.parse_next(input)?;
    let value: &str = take_while(1.., |c: char| !c.is_whitespace()).parse_next(input)?;
    let value = value.trim();

    if !is_valid_task_date(value) {
        return Err(ErrMode::Backtrack(ContextError::new()));
    }

    let mut meta = ParsedTaskMetadata::default();
    set_standard_field(&mut meta, key, value);
    Ok(meta)
}

fn parse_recurrence_field(input: &mut &str) -> ModalResult<ParsedTaskMetadata> {
    let _ = literal(RECURRENCE_EMOJI).parse_next(input)?;
    let _ = space0.parse_next(input)?;

    let mut words = Vec::new();
    loop {
        let trimmed = input.trim_start();
        if trimmed.is_empty() || starts_metadata_token(trimmed) {
            break;
        }
        let word: &str = take_while(1.., |c: char| !c.is_whitespace()).parse_next(input)?;
        words.push(word);
        let _ = space0.parse_next(input)?;
    }

    if words.is_empty() {
        return Err(ErrMode::Backtrack(ContextError::new()));
    }

    Ok(ParsedTaskMetadata {
        recurrence: Some(words.join(" ")),
        ..Default::default()
    })
}

fn parse_on_completion_field(input: &mut &str) -> ModalResult<ParsedTaskMetadata> {
    let _ = literal(ON_COMPLETION_EMOJI).parse_next(input)?;
    let _ = space0.parse_next(input)?;
    let value: &str = take_while(1.., |c: char| !c.is_whitespace()).parse_next(input)?;

    Ok(ParsedTaskMetadata {
        on_completion: Some(value.trim().to_ascii_lowercase()),
        ..Default::default()
    })
}

fn parse_id_field(input: &mut &str) -> ModalResult<ParsedTaskMetadata> {
    let _ = literal(ID_EMOJI).parse_next(input)?;
    let _ = space0.parse_next(input)?;
    let id = parse_task_id(input)?;

    Ok(ParsedTaskMetadata {
        id: Some(id.to_string()),
        ..Default::default()
    })
}

fn parse_depends_on_field(input: &mut &str) -> ModalResult<ParsedTaskMetadata> {
    let _ = literal(DEPENDS_ON_EMOJI).parse_next(input)?;
    let _ = space0.parse_next(input)?;
    let value: &str = take_while(1.., |c: char| !c.is_whitespace()).parse_next(input)?;

    Ok(ParsedTaskMetadata {
        depends_on: split_dependency_ids(value),
        ..Default::default()
    })
}

fn parse_task_id<'i>(input: &mut &'i str) -> ModalResult<&'i str> {
    take_while(1.., |c: char| c.is_alphanumeric() || c == '-' || c == '_').parse_next(input)
}

fn parse_dataview_field(input: &mut &str) -> ModalResult<ParsedTaskMetadata> {
    let closing = parse_dataview_open(input)?;
    let key: &str = take_while(1.., is_dataview_key_char).parse_next(input)?;
    let _ = literal("::").parse_next(input)?;
    let _ = space0.parse_next(input)?;
    let value = parse_balanced_inline_value(input, closing)?;
    let _ = one_of([closing]).parse_next(input)?;

    let key = key.trim().to_ascii_lowercase();
    let value = value.trim().to_string();
    let mut meta = ParsedTaskMetadata::default();
    set_standard_field(&mut meta, &key, &value);
    meta.metadata.insert(key, value);
    Ok(meta)
}

fn parse_dataview_open(input: &mut &str) -> ModalResult<char> {
    let opening = one_of(['[', '(']).parse_next(input)?;
    Ok(match opening {
        '[' => ']',
        '(' => ')',
        _ => unreachable!(),
    })
}

fn is_dataview_key_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

fn parse_balanced_inline_value<'i>(input: &mut &'i str, closing: char) -> ModalResult<&'i str> {
    let start = *input;
    let mut square_depth = 0usize;
    let mut paren_depth = 0usize;
    let mut consumed = 0usize;
    let mut found_close = false;

    for c in start.chars() {
        match c {
            '[' => {
                square_depth += 1;
                consumed += c.len_utf8();
            }
            ']' if closing == ']' && square_depth == 0 && paren_depth == 0 => {
                found_close = true;
                break;
            }
            ']' => {
                square_depth = square_depth.saturating_sub(1);
                consumed += c.len_utf8();
            }
            '(' => {
                paren_depth += 1;
                consumed += c.len_utf8();
            }
            ')' if closing == ')' && square_depth == 0 && paren_depth == 0 => {
                found_close = true;
                break;
            }
            ')' => {
                paren_depth = paren_depth.saturating_sub(1);
                consumed += c.len_utf8();
            }
            _ => consumed += c.len_utf8(),
        }
    }

    if !found_close {
        return Err(ErrMode::Backtrack(ContextError::new()));
    }

    let value = &start[..consumed];
    *input = &start[consumed..];
    Ok(value)
}

fn parse_priority_emoji(input: &mut &str) -> ModalResult<ParsedTaskMetadata> {
    let priority = alt((
        literal(PRIORITY_HIGH_EMOJI).value('⏫'),
        literal(PRIORITY_HIGHEST_EMOJI).value('🔺'),
        literal(PRIORITY_MEDIUM_EMOJI).value('🔼'),
        literal(PRIORITY_LOW_EMOJI).value('🔽'),
        literal(PRIORITY_LOWEST_EMOJI).value('⏬'),
    ))
    .parse_next(input)?;

    Ok(ParsedTaskMetadata {
        priority: Some(priority),
        ..Default::default()
    })
}

fn parse_tag_field(input: &mut &str) -> ModalResult<ParsedTaskMetadata> {
    let _ = literal("#").parse_next(input)?;
    let name: &str = take_while(1.., |c: char| {
        c.is_alphanumeric() || c == '-' || c == '_' || c == '/'
    })
    .parse_next(input)?;

    if !name.chars().any(|c| c.is_alphabetic()) {
        return Err(ErrMode::Backtrack(ContextError::new()));
    }

    let mut meta = ParsedTaskMetadata::default();
    meta.tags.push(name.to_string());
    Ok(meta)
}

fn parse_block_ref_field(input: &mut &str) -> ModalResult<ParsedTaskMetadata> {
    let _ = literal("^").parse_next(input)?;
    let id: &str =
        take_while(1.., |c: char| c.is_alphanumeric() || c == '-' || c == '_').parse_next(input)?;

    Ok(ParsedTaskMetadata {
        block_ref: Some(id.to_string()),
        ..Default::default()
    })
}

fn starts_metadata_token(s: &str) -> bool {
    s.starts_with(DUE_EMOJI)
        || s.starts_with(SCHEDULED_EMOJI)
        || s.starts_with(START_EMOJI)
        || s.starts_with(DONE_EMOJI)
        || s.starts_with(CANCELLED_EMOJI)
        || s.starts_with(CREATED_EMOJI)
        || s.starts_with(RECURRENCE_EMOJI)
        || s.starts_with(ON_COMPLETION_EMOJI)
        || s.starts_with(ID_EMOJI)
        || s.starts_with(DEPENDS_ON_EMOJI)
        || is_priority_metadata_start(s)
        || s.starts_with('#')
        || s.starts_with('^')
        || ((s.starts_with('[') || s.starts_with('(')) && s.contains("::"))
}

fn is_priority_metadata_start(s: &str) -> bool {
    s.starts_with(PRIORITY_HIGH_EMOJI)
        || s.starts_with(PRIORITY_HIGHEST_EMOJI)
        || s.starts_with(PRIORITY_MEDIUM_EMOJI)
        || s.starts_with(PRIORITY_LOW_EMOJI)
        || s.starts_with(PRIORITY_LOWEST_EMOJI)
}

fn is_priority_emoji(s: &str) -> bool {
    matches!(
        s,
        PRIORITY_HIGH_EMOJI
            | PRIORITY_HIGHEST_EMOJI
            | PRIORITY_MEDIUM_EMOJI
            | PRIORITY_LOW_EMOJI
            | PRIORITY_LOWEST_EMOJI
    )
}

fn set_standard_field(meta: &mut ParsedTaskMetadata, key: &str, value: &str) {
    match key {
        "due" if is_valid_task_date(value) => meta.due = Some(value.to_string()),
        "scheduled" if is_valid_task_date(value) => meta.scheduled = Some(value.to_string()),
        "start" if is_valid_task_date(value) => meta.start = Some(value.to_string()),
        "done" | "completion" if is_valid_task_date(value) => meta.done = Some(value.to_string()),
        "cancelled" | "canceled" if is_valid_task_date(value) => {
            meta.cancelled = Some(value.to_string());
        }
        "created" if is_valid_task_date(value) => meta.created = Some(value.to_string()),
        "recurrence" | "repeat" => meta.recurrence = Some(value.to_string()),
        "oncompletion" => meta.on_completion = Some(value.to_ascii_lowercase()),
        "id" => meta.id = Some(value.to_string()),
        "dependson" => meta.depends_on = split_dependency_ids(value),
        "priority" => meta.priority = priority_from_dataview_value(value),
        _ => {}
    }
}

fn is_valid_task_date(value: &str) -> bool {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
}

fn split_dependency_ids(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn priority_from_dataview_value(value: &str) -> Option<char> {
    match value.trim().to_ascii_lowercase().as_str() {
        "highest" => Some('🔺'),
        "high" => Some('⏫'),
        "medium" => Some('🔼'),
        "low" => Some('🔽'),
        "lowest" => Some('⏬'),
        "normal" | "none" => None,
        _ => None,
    }
}

fn merge_metadata(dst: &mut ParsedTaskMetadata, src: ParsedTaskMetadata) {
    if src.due.is_some() {
        dst.due = src.due;
    }
    if src.scheduled.is_some() {
        dst.scheduled = src.scheduled;
    }
    if src.start.is_some() {
        dst.start = src.start;
    }
    if src.done.is_some() {
        dst.done = src.done;
    }
    if src.cancelled.is_some() {
        dst.cancelled = src.cancelled;
    }
    if src.created.is_some() {
        dst.created = src.created;
    }
    if src.priority.is_some() {
        dst.priority = src.priority;
    }
    if src.recurrence.is_some() {
        dst.recurrence = src.recurrence;
    }
    if src.on_completion.is_some() {
        dst.on_completion = src.on_completion;
    }
    if src.id.is_some() {
        dst.id = src.id;
    }
    if !src.depends_on.is_empty() {
        dst.depends_on = src.depends_on;
    }
    if src.block_ref.is_some() {
        dst.block_ref = src.block_ref;
    }
    dst.tags.extend(src.tags);
    dst.metadata.extend(src.metadata);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_emoji_metadata() {
        let task = parse_task_line("- [ ] Buy milk 📅 2025-05-15").unwrap();
        assert_eq!(task.description, "Buy milk");
        assert_eq!(task.due.as_deref(), Some("2025-05-15"));
    }

    #[test]
    fn non_iso_date_metadata_is_parsed_but_not_converted_to_task_date() {
        // This test ensures that the parser behaves like the Obsidian Tasks plugin does
        let parsed = parse_task_content("Buy milk 📅 05-01-2026 ⏳ 2026-05-02");
        assert_eq!(parsed.description, "Buy milk 📅 05-01-2026");
        assert_eq!(parsed.due.as_deref(), None);
        assert_eq!(parsed.scheduled.as_deref(), Some("2026-05-02"));

        let task = crate::models::TaskItem::from_parsed_metadata(
            parsed,
            false,
            crate::models::SourcePosition::start(),
        );
        assert!(task.due_date.is_none());
        assert_eq!(
            task.scheduled_date.map(|date| date.to_string()).as_deref(),
            Some("2026-05-02")
        );
    }

    #[test]
    fn parses_dataview_metadata() {
        let task = parse_task_line("- [ ] Finish report [due:: 2025-06-01]").unwrap();
        assert_eq!(task.description, "Finish report");
        assert_eq!(task.due.as_deref(), Some("2025-06-01"));
        assert_eq!(
            task.metadata.get("due").map(String::as_str),
            Some("2025-06-01")
        );
    }

    #[test]
    fn parses_mixed_emoji_and_dataview() {
        let task = parse_task_line(
            "- [x] Do the thing 📅 2025-05-01 [scheduled:: 2025-05-03] 🔁 every weekday",
        )
        .unwrap();
        assert_eq!(task.description, "Do the thing");
        assert_eq!(task.due.as_deref(), Some("2025-05-01"));
        assert_eq!(task.scheduled.as_deref(), Some("2025-05-03"));
        assert_eq!(task.recurrence.as_deref(), Some("every weekday"));
        assert_eq!(task.status, 'x');
    }

    #[test]
    fn parses_comma_separated_dataview_fields() {
        let task = parse_task_line(
            "- [ ] This is a task [priority:: high], [start:: 2023-04-24], [due:: 2023-05-01]",
        )
        .unwrap();

        assert_eq!(task.description, "This is a task");
        assert_eq!(task.priority, Some('⏫'));
        assert_eq!(task.start.as_deref(), Some("2023-04-24"));
        assert_eq!(task.due.as_deref(), Some("2023-05-01"));
    }

    #[test]
    fn parses_dataview_completion_repeat_and_priority() {
        let task = parse_task_line(
            "- [ ] Ship it [completion:: 2025-07-01] [repeat:: every month] [priority:: high]",
        )
        .unwrap();
        assert_eq!(task.description, "Ship it");
        assert_eq!(task.done.as_deref(), Some("2025-07-01"));
        assert_eq!(task.recurrence.as_deref(), Some("every month"));
        assert_eq!(task.priority, Some('⏫'));
    }

    #[test]
    fn parses_dataview_on_completion_and_dependencies() {
        let task = parse_task_line(
            "- [ ] Blocked work [repeat:: every day], [onCompletion:: delete], [id:: dcf64c], [dependsOn:: abc123,def456]",
        )
        .unwrap();

        assert_eq!(task.description, "Blocked work");
        assert_eq!(task.recurrence.as_deref(), Some("every day"));
        assert_eq!(task.on_completion.as_deref(), Some("delete"));
        assert_eq!(task.id.as_deref(), Some("dcf64c"));
        assert_eq!(
            task.depends_on,
            vec!["abc123".to_string(), "def456".to_string()]
        );
    }

    #[test]
    fn parses_emoji_on_completion_and_dependencies() {
        let task =
            parse_task_line("- [ ] Blocked work 🏁 keep 🆔 dcf64c ⛔ abc123,def456").unwrap();

        assert_eq!(task.description, "Blocked work");
        assert_eq!(task.on_completion.as_deref(), Some("keep"));
        assert_eq!(task.id.as_deref(), Some("dcf64c"));
        assert_eq!(
            task.depends_on,
            vec!["abc123".to_string(), "def456".to_string()]
        );
    }

    #[test]
    fn parses_tags_and_block_ref_after_metadata() {
        let task = parse_task_line("- [ ] Task description 📅 2025-05-10 #urgent #work ^task-123")
            .unwrap();
        assert_eq!(task.description, "Task description #urgent #work");
        assert_eq!(task.due.as_deref(), Some("2025-05-10"));
        assert_eq!(task.tags, vec!["urgent".to_string(), "work".to_string()]);
        assert_eq!(task.block_ref.as_deref(), Some("task-123"));
    }

    #[test]
    fn parses_tag_at_start_of_task_content() {
        let task =
            parse_task_line("- [ ] #task This is the task content 📅 2026-05-01 🔺").unwrap();

        assert_eq!(task.description, "#task This is the task content");
        assert_eq!(task.due.as_deref(), Some("2026-05-01"));
        assert_eq!(task.priority, Some('🔺'));
        assert_eq!(task.tags, vec!["task".to_string()]);
    }

    #[test]
    fn parses_two_tags_at_start_of_task_content() {
        let task = parse_task_line("- [ ] #task #urgent This is the task content 📅 2026-05-01 🔺")
            .unwrap();

        assert_eq!(task.description, "#task #urgent This is the task content");
        assert_eq!(task.due.as_deref(), Some("2026-05-01"));
        assert_eq!(task.priority, Some('🔺'));
        assert_eq!(task.tags, vec!["task".to_string(), "urgent".to_string()]);
    }

    #[test]
    fn indexes_tags_without_metadata_and_keeps_them_in_description() {
        let task = parse_task_line("- [ ] Read #book chapter #research").unwrap();

        assert_eq!(task.description, "Read #book chapter #research");
        assert_eq!(task.tags, vec!["book".to_string(), "research".to_string()]);
    }

    #[test]
    fn stops_tag_at_dot_and_keeps_full_text_in_description() {
        // The Obsidian Tasks plugin treats #task.foo as #task tag with .foo as part of the description. This test demonstrates mirroring that behavior.
        let task = parse_task_line("- [ ] Update #task.foo before standup").unwrap();

        assert_eq!(task.description, "Update #task.foo before standup");
        assert_eq!(task.tags, vec!["task".to_string()]);
    }

    #[test]
    fn ignores_false_positive_in_description() {
        let task =
            parse_task_line("- [ ] Review the [due date] section and 📅 2025-04-30").unwrap();
        assert_eq!(task.description, "Review the [due date] section and");
        assert_eq!(task.due.as_deref(), Some("2025-04-30"));
    }

    #[test]
    fn duplicate_fields_rightmost_wins() {
        let task = parse_task_line("- [ ] Task 📅 2025-05-01 📅 2025-06-15").unwrap();
        assert_eq!(task.due.as_deref(), Some("2025-06-15"));
    }

    #[test]
    fn parses_priority() {
        let task = parse_task_line("- [ ] High priority task ⏫").unwrap();
        assert_eq!(task.priority, Some('⏫'));
        assert_eq!(task.description, "High priority task");
    }

    #[test]
    fn keeps_middle_metadata_in_description() {
        let task = parse_task_line(
            "- [ ] 📅 2025-05-01 This date in middle should be ignored ⏳ 2025-05-10",
        )
        .unwrap();
        assert_eq!(
            task.description,
            "📅 2025-05-01 This date in middle should be ignored"
        );
        assert_eq!(task.scheduled.as_deref(), Some("2025-05-10"));
        assert!(task.due.is_none());
    }

    #[test]
    fn parses_dataview_value_with_wikilink() {
        // #123 is not a valid tag, but should be preserved in the description.
        let task = parse_task_line(
            "- [ ] Review PR #123 [project:: [[Team Work]]] 📅 2025-05-20 🔼 #review ^pr-123",
        )
        .unwrap();
        assert_eq!(task.description, "Review PR #123 #review");
        assert_eq!(task.due.as_deref(), Some("2025-05-20"));
        assert_eq!(task.priority, Some('🔼'));
        assert_eq!(task.tags, vec!["review".to_string()]);
        assert_eq!(task.block_ref.as_deref(), Some("pr-123"));
        assert_eq!(
            task.metadata.get("project").map(String::as_str),
            Some("[[Team Work]]")
        );
    }

    #[test]
    fn supports_parenthesized_dataview_fields() {
        let task = parse_task_line("- [ ] Call Alex (due:: 2025-06-01)").unwrap();
        assert_eq!(task.description, "Call Alex");
        assert_eq!(task.due.as_deref(), Some("2025-06-01"));
    }

    #[test]
    fn keeps_bare_recurrence_marker_in_description() {
        let task = parse_task_line("- [ ] Decide recurrence 🔁").unwrap();
        assert_eq!(task.description, "Decide recurrence 🔁");
        assert!(task.recurrence.is_none());
    }
}
