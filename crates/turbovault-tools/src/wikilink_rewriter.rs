//! Wikilink rewriter for atomic move_note + delete_note (turbovault-lqr / oz6).
//!
//! Rewrites Obsidian-Flavored Markdown wikilinks targeting one vault path
//! to target another vault path. Handles every common form:
//!
//! - Bare basename: `[[old]]` -> `[[new]]`
//! - Path-prefix:   `[[wiki/old]]` -> `[[wiki/new]]`
//! - Alias:         `[[old|My Alias]]` -> `[[new|My Alias]]`
//! - Section:       `[[old#Header]]` -> `[[new#Header]]`
//! - Block anchor:  `[[old#^block-id]]` -> `[[new#^block-id]]`
//! - Embed:         `![[old]]` -> `![[new]]` (plus all the variants above)
//!
//! Markdown links to a note are rewritten too, in the form they were written
//! in, the way Obsidian updates them:
//!
//! - Name:          `[x](old.md)` -> `[x](new.md)`
//! - Vault path:    `[x](wiki/old.md)` -> `[x](archive/new.md)`
//! - Relative path: `[x](../wiki/old.md)` -> `[x](../archive/new.md)`, from
//!   the linking note's folder ([`rewrite_links_from`] only)
//! - Escaped:       `[x](My%20Old.md)` and `[x](<My Old.md>)` keep their
//!   escaping, and a fragment or title after the path is kept as written
//! - Reference:     `[x][ref]`, `[ref][]` and `[ref]` take their destination
//!   from a definition, `[ref]: old.md`, which is rewritten the same way
//!
//! Links are found by the parser, not by a pattern over the text, so a
//! link inside code is never touched and every edit lands on a span the
//! parser reported. Which links to edit is the caller's choice:
//! [`rewrite_wikilinks`] / [`wrap_wikilinks_as_stale`] pick them by name,
//! case-insensitively like Obsidian; the move and delete paths pass
//! [`links_in`] filtered through the link graph's own resolution, so they
//! rewrite exactly the links the graph reported as backlinks.

use std::collections::HashSet;
use std::ops::Range;

use turbovault_core::okf::normalize_link_target;
use turbovault_core::{Link, LinkType};
use turbovault_parser::parsers::link_utils::{
    link_destination_end, markdown_link_path, markdown_link_target,
};

/// Every wikilink, embed and internal Markdown link in `content`, with the
/// spans the parser reports. Links inside code are not links and are not
/// returned, and neither are external URLs or same-note anchors.
pub fn links_in(content: &str) -> Vec<Link> {
    let mut links = turbovault_parser::parse_wikilinks(content);
    links.extend(turbovault_parser::parse_embeds(content));
    links.extend(
        turbovault_parser::parse_markdown_links(content)
            .into_iter()
            .filter(|link| !matches!(link.type_, LinkType::ExternalLink | LinkType::Anchor)),
    );
    links
}

/// Rewrite every wikilink in `content` that targets `old_vault_path`
/// (vault-relative `.md` path, e.g. `wiki/old.md`) to target
/// `new_vault_path`. Bare-basename forms (`[[old]]`) and path forms
/// (`[[wiki/old]]`) are both rewritten, matched case-insensitively.
///
/// If a link's existing form is a path, it stays a path (re-targeted to the
/// new path-with-extension-stripped). If it's a bare basename, it stays bare
/// (re-targeted to the new basename). The caller doesn't need to know which
/// form the source used.
pub fn rewrite_wikilinks(content: &str, old_vault_path: &str, new_vault_path: &str) -> String {
    let links = links_named(content, old_vault_path);
    rewrite_links(content, &links, old_vault_path, new_vault_path)
}

/// turbovault-oz6: wrap every wikilink in `content` targeting
/// `deleted_vault_path` in `~~strikethrough~~` markdown, marking it as a dead
/// reference to a deleted page. Matches the same forms as
/// [`rewrite_wikilinks`].
///
/// Idempotent: a link already wrapped (`~~[[old]]~~`) is not wrapped again.
pub fn wrap_wikilinks_as_stale(content: &str, deleted_vault_path: &str) -> String {
    let links = links_named(content, deleted_vault_path);
    wrap_links_as_stale(content, &links)
}

/// Rewrite exactly `links`, which must be spans of `content` as the parser
/// reported them, from `old_vault_path` to `new_vault_path`.
///
/// A link whose target names the old note by basename or by path is
/// re-targeted in the same form. A link that reached the note some other way
/// (an alias in its frontmatter) is left alone: the alias moves with the
/// note, so the link still resolves after the move.
///
/// Without the linking note's path a relative Markdown link cannot be
/// re-pointed, so it is left alone too; [`rewrite_links_from`] rewrites it.
pub fn rewrite_links(
    content: &str,
    links: &[Link],
    old_vault_path: &str,
    new_vault_path: &str,
) -> String {
    rewrite(None, content, links, old_vault_path, new_vault_path)
}

/// [`rewrite_links`] for `content` read from `source_vault_path`, so that a
/// Markdown link written relative to that note's folder (`../wiki/old.md`)
/// is re-pointed from the same folder.
pub fn rewrite_links_from(
    source_vault_path: &str,
    content: &str,
    links: &[Link],
    old_vault_path: &str,
    new_vault_path: &str,
) -> String {
    rewrite(
        Some(source_vault_path),
        content,
        links,
        old_vault_path,
        new_vault_path,
    )
}

fn rewrite(
    source_vault_path: Option<&str>,
    content: &str,
    links: &[Link],
    old_vault_path: &str,
    new_vault_path: &str,
) -> String {
    let old_parts = parts_of(old_vault_path);
    let new_path = strip_md(new_vault_path);
    let new_base = basename(&new_path).to_string();

    // A reference link, `[x][ref]`, holds no destination of its own; the
    // definition it goes through, `[ref]: old.md`, does. Rewriting that
    // definition re-points every link through it at once.
    let mut spans = link_spans(links);
    let referenced: HashSet<&str> = links
        .iter()
        .filter(|link| is_reference_link(content, link))
        .map(|link| link.target.as_str())
        .collect();
    if !referenced.is_empty() {
        spans.extend(
            turbovault_parser::parse_reference_definitions(content)
                .into_iter()
                .filter(|definition| referenced.contains(definition.target.as_str()))
                .map(|definition| (definition.destination, Span::Destination)),
        );
    }

    splice(content, spans, |span, kind| {
        if kind == Span::Destination {
            return rewrite_destination(span, source_vault_path, &old_parts, &new_path);
        }
        let Some((open, inner)) = open_bracket(span) else {
            return rewrite_markdown_link(span, source_vault_path, &old_parts, &new_path);
        };
        let inner = inner.strip_suffix("]]")?;
        let path_end = path_end(inner);
        let path = &inner[..path_end];
        let parts = normalize_link_target(path)?;

        let replacement = if parts.len() == 1 && Some(&parts[0]) == old_parts.last() {
            new_base.clone()
        } else if parts.len() > 1 && old_parts.ends_with(&parts) {
            let root = if path.starts_with('/') { "/" } else { "" };
            format!("{root}{new_path}")
        } else {
            return None;
        };
        let extension = if has_md_extension(path.trim_end()) {
            ".md"
        } else {
            ""
        };
        Some(format!(
            "{open}{replacement}{extension}{}]]",
            &inner[path_end..]
        ))
    })
}

/// Re-point a Markdown link span, `[text](destination "title")`, from the
/// note `old_parts` names to `new_path` (a vault path without `.md`), keeping
/// the form it was written in. `None` leaves the span as it is.
fn rewrite_markdown_link(
    span: &str,
    source_vault_path: Option<&str>,
    old_parts: &[String],
    new_path: &str,
) -> Option<String> {
    let (start, end) = markdown_destination(span)?;
    let destination =
        rewrite_destination(&span[start..end], source_vault_path, old_parts, new_path)?;
    Some(format!("{}{destination}{}", &span[..start], &span[end..]))
}

/// Re-point a Markdown link destination as written, `old.md`,
/// `My%20Old.md#Part` or `<My Old.md>`, the way [`rewrite_markdown_link`]
/// does. `None` leaves it as it is.
fn rewrite_destination(
    destination: &str,
    source_vault_path: Option<&str>,
    old_parts: &[String],
    new_path: &str,
) -> Option<String> {
    let (angled, written) = match destination.strip_prefix('<') {
        Some(inner) => (true, inner.strip_suffix('>')?),
        None => (false, destination),
    };
    let path_len = written.find('#').unwrap_or(written.len());
    let (written_path, fragment) = written.split_at(path_len);
    let path = if angled {
        written_path.to_string()
    } else {
        markdown_link_target(written_path)
    };

    let parts = normalize_link_target(&path)?;
    let names_old = if parts.len() == 1 {
        old_parts.last() == parts.first()
    } else {
        old_parts.ends_with(&parts)
    };
    if !names_old {
        return None;
    }

    let extension = if has_md_extension(path.trim_end()) {
        ".md"
    } else {
        ""
    };
    let source_dir: Option<Vec<&str>> = source_vault_path.map(|source| {
        let mut dir: Vec<&str> = source.split('/').filter(|s| !s.is_empty()).collect();
        dir.pop();
        dir
    });
    let dotted = path.starts_with("./") || path.starts_with("../");
    let from_source = source_dir
        .as_deref()
        .is_some_and(|dir| resolve_from(dir, &path) == old_parts);

    let new_destination = if path.starts_with('/') {
        format!("/{new_path}{extension}")
    } else if dotted || (parts.len() > 1 && from_source) {
        let relative = relative_path(source_dir.as_deref()?, &format!("{new_path}{extension}"));
        if path.starts_with("./") && !relative.starts_with("../") {
            format!("./{relative}")
        } else {
            relative
        }
    } else if parts.len() == 1 {
        format!("{}{extension}", basename(new_path))
    } else {
        format!("{new_path}{extension}")
    };

    Some(if angled {
        format!("<{new_destination}{fragment}>")
    } else {
        format!("{}{fragment}", markdown_link_path(&new_destination))
    })
}

/// Whether `link` is a reference link, `[x][ref]`, `[ref][]` or `[ref]`,
/// whose destination is in a definition rather than in its span.
fn is_reference_link(content: &str, link: &Link) -> bool {
    let end = link.position.offset + link.position.length;
    content.get(link.position.offset..end).is_some_and(|span| {
        span.starts_with('[')
            && open_bracket(span).is_none()
            && markdown_destination(span).is_none()
    })
}

/// Byte range of the destination in a Markdown link span, angle brackets
/// included: the `x.md` of `[text](x.md "title")`. `None` for a span with no
/// inline destination, such as a reference link `[text][ref]`.
fn markdown_destination(span: &str) -> Option<(usize, usize)> {
    let bytes = span.as_bytes();
    if bytes.first() != Some(&b'[') {
        return None;
    }
    // The link text's closing bracket, past any nested or escaped ones.
    let mut depth = 0usize;
    let mut i = 0;
    let text_end = loop {
        match bytes.get(i)? {
            b'\\' => i += 1,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    break i;
                }
            }
            _ => {}
        }
        i += 1;
    };
    if bytes.get(text_end + 1) != Some(&b'(') {
        return None;
    }

    let mut start = text_end + 2;
    while matches!(bytes.get(start), Some(b' ' | b'\t' | b'\n')) {
        start += 1;
    }
    Some((start, link_destination_end(span, start)?))
}

/// The note `path` names when read from the folder `dir`, as the lowercased,
/// `.md`-stripped components [`parts_of`] gives a vault path.
fn resolve_from(dir: &[&str], path: &str) -> Vec<String> {
    let mut resolved: Vec<&str> = dir.to_vec();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                resolved.pop();
            }
            segment => resolved.push(segment),
        }
    }
    parts_of(&resolved.join("/"))
}

/// `target` (a vault path) as a path relative to the folder `dir`.
fn relative_path(dir: &[&str], target: &str) -> String {
    let target: Vec<&str> = target.split('/').filter(|s| !s.is_empty()).collect();
    let common = dir.iter().zip(&target).take_while(|(a, b)| a == b).count();
    let mut parts = vec![".."; dir.len() - common];
    parts.extend(&target[common..]);
    parts.join("/")
}

/// Wrap exactly `links`, which must be spans of `content` as the parser
/// reported them, in `~~ ~~`. A span already wrapped is left as it is.
pub fn wrap_links_as_stale(content: &str, links: &[Link]) -> String {
    let mut spans = link_spans(links);
    spans.retain(|(span, _)| {
        content.get(span.clone()).is_some()
            && !(content[..span.start].ends_with("~~") && content[span.end..].starts_with("~~"))
    });
    splice(content, spans, |span, _| {
        // A wikilink, embed or Markdown link of any form starts with a
        // bracket; anything else is not a span the parser reported as a link.
        if !span.starts_with('[') && open_bracket(span).is_none() {
            return None;
        }
        Some(format!("~~{span}~~"))
    })
}

/// The links in `content` that name `vault_path` by basename or by path,
/// case-insensitively. Without the link graph there is no way to tell which
/// of two same-named notes a bare `[[Note]]` means, so this takes it to mean
/// this one; the move and delete paths resolve through the graph instead.
fn links_named(content: &str, vault_path: &str) -> Vec<Link> {
    let target = parts_of(vault_path);
    links_in(content)
        .into_iter()
        .filter(|link| {
            normalize_link_target(&link.target).is_some_and(|parts| {
                (parts.len() == 1 && target.last() == parts.first()) || target.ends_with(&parts)
            })
        })
        .collect()
}

/// What a span handed to [`splice`] holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Span {
    /// A whole link, as the parser reported it.
    Link,
    /// The destination of a reference definition, `old.md` in `[ref]: old.md`.
    Destination,
}

fn link_spans(links: &[Link]) -> Vec<(Range<usize>, Span)> {
    links
        .iter()
        .map(|link| {
            let start = link.position.offset;
            (start..start + link.position.length, Span::Link)
        })
        .collect()
}

/// Replace each span with `edit(text, kind)`, where it returns one. Spans
/// are applied back to front so earlier offsets stay valid; a span that is
/// not text of `content`, or that overlaps one already edited, is skipped
/// rather than trusted.
fn splice(
    content: &str,
    mut spans: Vec<(Range<usize>, Span)>,
    edit: impl Fn(&str, Span) -> Option<String>,
) -> String {
    spans.retain(|(span, _)| content.get(span.clone()).is_some());
    spans.sort_unstable_by_key(|(span, kind)| (span.start, span.end, *kind));
    spans.dedup();

    let mut out = content.to_string();
    let mut floor = usize::MAX;
    for (span, kind) in spans.into_iter().rev() {
        if span.end > floor {
            continue;
        }
        if let Some(replacement) = edit(&content[span.clone()], kind) {
            floor = span.start;
            out.replace_range(span, &replacement);
        }
    }
    out
}

/// Split a wikilink span into its opening (`[[` or `![[`) and the rest.
fn open_bracket(span: &str) -> Option<(&'static str, &str)> {
    if let Some(rest) = span.strip_prefix("![[") {
        Some(("![[", rest))
    } else {
        span.strip_prefix("[[").map(|rest| ("[[", rest))
    }
}

/// Where the path ends inside a link's brackets: at a heading or block anchor
/// (`#`), or at the display text (`|`, escaped `\|` inside a table).
fn path_end(inner: &str) -> usize {
    let end = inner.find(['#', '|']).unwrap_or(inner.len());
    if inner[..end].ends_with('\\') {
        end - 1
    } else {
        end
    }
}

/// A vault path as the lowercased, `.md`-stripped components the link graph
/// resolves against.
fn parts_of(vault_path: &str) -> Vec<String> {
    normalize_link_target(&strip_md(vault_path)).unwrap_or_default()
}

fn has_md_extension(p: &str) -> bool {
    p.get(p.len().saturating_sub(3)..)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".md"))
}

fn strip_md(p: &str) -> String {
    // tlx.10/[17]: case-insensitive — a path ending in `.MD`/`.Md` must still
    // strip to the bare stem, else moving `Foo.MD` looks for `[[Foo.MD]]` and
    // leaves backlinks unrewritten. A matched ascii `.md` suffix guarantees
    // `len - 3` is a char boundary.
    if has_md_extension(p) {
        p[..p.len() - 3].to_string()
    } else {
        p.to_string()
    }
}

fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_bare_basename() {
        let out = rewrite_wikilinks("see [[old]] for more", "old.md", "new.md");
        assert_eq!(out, "see [[new]] for more");
    }

    #[test]
    fn rewrites_path_prefix() {
        let out = rewrite_wikilinks("see [[wiki/old]] for more", "wiki/old.md", "wiki/new.md");
        assert_eq!(out, "see [[wiki/new]] for more");
    }

    #[test]
    fn rewrites_alias_form() {
        let out = rewrite_wikilinks("see [[old|My Alias]]", "old.md", "new.md");
        assert_eq!(out, "see [[new|My Alias]]");
    }

    #[test]
    fn rewrites_section_anchor() {
        let out = rewrite_wikilinks("see [[old#Header]]", "old.md", "new.md");
        assert_eq!(out, "see [[new#Header]]");
    }

    #[test]
    fn rewrites_block_anchor() {
        let out = rewrite_wikilinks("see [[old#^block-id]]", "old.md", "new.md");
        assert_eq!(out, "see [[new#^block-id]]");
    }

    #[test]
    fn rewrites_embed_form() {
        let out = rewrite_wikilinks("![[old]]", "old.md", "new.md");
        assert_eq!(out, "![[new]]");
    }

    #[test]
    fn rewrites_embed_with_section() {
        let out = rewrite_wikilinks("![[old#Header]]", "old.md", "new.md");
        assert_eq!(out, "![[new#Header]]");
    }

    #[test]
    fn does_not_rewrite_partial_basename_match() {
        // `[[older]]` is a different target — must NOT be rewritten when
        // we rewrite `old` -> `new`.
        let out = rewrite_wikilinks("see [[older]] and [[old]]", "old.md", "new.md");
        assert_eq!(out, "see [[older]] and [[new]]");
    }

    #[test]
    fn does_not_rewrite_suffix_match() {
        // `[[my-old]]` is a different target.
        let out = rewrite_wikilinks("see [[my-old]] vs [[old]]", "old.md", "new.md");
        assert_eq!(out, "see [[my-old]] vs [[new]]");
    }

    #[test]
    fn rewrites_multiple_matches_in_one_file() {
        let out = rewrite_wikilinks(
            "first [[old]], second [[old|alias]], third ![[old#Sec]]",
            "old.md",
            "new.md",
        );
        assert_eq!(
            out,
            "first [[new]], second [[new|alias]], third ![[new#Sec]]"
        );
    }

    #[test]
    fn passthrough_when_no_matches() {
        let original = "no wikilinks here, just text";
        let out = rewrite_wikilinks(original, "old.md", "new.md");
        assert_eq!(out, original);
    }

    #[test]
    fn rewrites_path_form_when_source_uses_path_target_uses_basename() {
        // Source uses bare basename; the rewrite still applies because
        // basename-form rewrites also run.
        let out = rewrite_wikilinks("use [[old]] here", "wiki/old.md", "concepts/new.md");
        assert_eq!(out, "use [[new]] here");
    }

    #[test]
    fn rewrites_path_form_keeps_path_target_when_directory_changes() {
        // Source uses `wiki/old`; the rewrite produces `concepts/new`.
        let out = rewrite_wikilinks("use [[wiki/old]] here", "wiki/old.md", "concepts/new.md");
        assert_eq!(out, "use [[concepts/new]] here");
    }

    #[test]
    fn rewrites_uppercase_md_extension() {
        // tlx.10/[17]: moving `Foo.MD` must still target the bare `[[Foo]]`
        // link, not look for a non-existent `[[Foo.MD]]`.
        let out = rewrite_wikilinks("see [[Foo]] here", "Foo.MD", "Bar.md");
        assert_eq!(out, "see [[Bar]] here");
    }

    #[test]
    fn rewrites_regex_special_chars_in_basename() {
        // A filename with regex metacharacters like `+` or `.` should be
        // escaped before being inserted into the rewrite regex.
        let out = rewrite_wikilinks("see [[c++]]", "c++.md", "rust.md");
        assert_eq!(out, "see [[rust]]");
    }

    #[test]
    fn rewrites_regardless_of_case() {
        // #88: Obsidian (and the link graph) resolve targets case-insensitively.
        let out = rewrite_wikilinks(
            "see [[Old Note]] and [[OLD NOTE|x]]",
            "old note.md",
            "new note.md",
        );
        assert_eq!(out, "see [[new note]] and [[new note|x]]");
    }

    #[test]
    fn keeps_an_explicit_md_extension() {
        let out = rewrite_wikilinks(
            "see [[old.md]] and [[wiki/old.MD#H]]",
            "wiki/old.md",
            "wiki/new.md",
        );
        assert_eq!(out, "see [[new.md]] and [[wiki/new.md#H]]");
    }

    #[test]
    fn rewrites_the_target_of_a_table_escaped_alias() {
        let out = rewrite_wikilinks("| [[old\\|shown]] |", "old.md", "new.md");
        assert_eq!(out, "| [[new\\|shown]] |");
    }

    #[test]
    fn rewrites_a_path_suffix_to_the_full_new_path() {
        let out = rewrite_wikilinks("[[sub/old]]", "a/sub/old.md", "b/new.md");
        assert_eq!(out, "[[b/new]]");
    }

    // -------- tlx.3: code-aware masking --------

    #[test]
    fn does_not_rewrite_inside_fenced_code() {
        let input = "before [[old]]\n```\nexample [[old]] in code\n```\nafter [[old]]";
        let out = rewrite_wikilinks(input, "old.md", "new.md");
        assert_eq!(
            out,
            "before [[new]]\n```\nexample [[old]] in code\n```\nafter [[new]]"
        );
    }

    #[test]
    fn does_not_rewrite_inside_inline_code() {
        let out = rewrite_wikilinks("real [[old]] but `[[old]]` literal", "old.md", "new.md");
        assert_eq!(out, "real [[new]] but `[[old]]` literal");
    }

    #[test]
    fn does_not_rewrite_tilde_fenced_code() {
        let input = "~~~\n[[old]]\n~~~\nplain [[old]]";
        let out = rewrite_wikilinks(input, "old.md", "new.md");
        assert_eq!(out, "~~~\n[[old]]\n~~~\nplain [[new]]");
    }

    #[test]
    fn wrap_stale_skips_fenced_code() {
        let input = "see [[old]]\n```\ncode [[old]]\n```";
        let out = wrap_wikilinks_as_stale(input, "old.md");
        assert_eq!(out, "see ~~[[old]]~~\n```\ncode [[old]]\n```");
    }

    // -------- turbovault-oz6: stale-callout wrapper --------

    #[test]
    fn wrap_stale_bare_basename() {
        let out = wrap_wikilinks_as_stale("see [[old]] here", "old.md");
        assert_eq!(out, "see ~~[[old]]~~ here");
    }

    #[test]
    fn wrap_stale_with_alias() {
        let out = wrap_wikilinks_as_stale("see [[old|My Alias]] here", "old.md");
        assert_eq!(out, "see ~~[[old|My Alias]]~~ here");
    }

    #[test]
    fn wrap_stale_with_section() {
        let out = wrap_wikilinks_as_stale("see [[old#Header]] here", "old.md");
        assert_eq!(out, "see ~~[[old#Header]]~~ here");
    }

    #[test]
    fn wrap_stale_embed() {
        let out = wrap_wikilinks_as_stale("![[old]]", "old.md");
        assert_eq!(out, "~~![[old]]~~");
    }

    #[test]
    fn wrap_stale_path_prefix() {
        let out = wrap_wikilinks_as_stale("see [[wiki/old]]", "wiki/old.md");
        // Path-form wrapped; basename pass does NOT re-wrap because the
        // already-wrapped guard fires on the inner `old` match (its prefix
        // is now `/` plus our `~~`).
        assert!(out.contains("~~[[wiki/old]]~~"));
    }

    #[test]
    fn wrap_stale_idempotent() {
        let already = "see ~~[[old]]~~ here";
        let out = wrap_wikilinks_as_stale(already, "old.md");
        assert_eq!(out, already, "already-wrapped links must not double-wrap");
    }

    #[test]
    fn wrap_stale_skips_partial_basename_match() {
        let out = wrap_wikilinks_as_stale("see [[older]] and [[old]]", "old.md");
        assert_eq!(out, "see [[older]] and ~~[[old]]~~");
    }

    #[test]
    fn wrap_stale_multiple_links() {
        let out = wrap_wikilinks_as_stale(
            "first [[old]] then [[old|alias]] then ![[old#Sec]]",
            "old.md",
        );
        assert_eq!(
            out,
            "first ~~[[old]]~~ then ~~[[old|alias]]~~ then ~~![[old#Sec]]~~"
        );
    }

    #[test]
    fn wrap_stale_no_matches_passes_through() {
        let original = "no wikilinks here";
        let out = wrap_wikilinks_as_stale(original, "old.md");
        assert_eq!(out, original);
    }

    // ==================== Markdown links ====================

    /// `rewrite_links_from` over every link `links_in` finds, the way the move
    /// path calls it.
    fn move_from(source: &str, content: &str, old: &str, new: &str) -> String {
        rewrite_links_from(source, content, &links_in(content), old, new)
    }

    #[test]
    fn markdown_link_keeps_its_form() {
        let content = "[a](old.md) [b](wiki/old.md#Part) [c](/wiki/old.md) [d](old)";
        assert_eq!(
            move_from("index.md", content, "wiki/old.md", "archive/new.md"),
            "[a](new.md) [b](archive/new.md#Part) [c](/archive/new.md) [d](new)"
        );
    }

    #[test]
    fn relative_markdown_link_is_repointed_from_its_note() {
        let content = "[a](../wiki/old.md) and [b](./old.md) and [c](sub/old.md)";
        assert_eq!(
            move_from(
                "wiki/index.md",
                content,
                "wiki/old.md",
                "archive/deep/new.md"
            ),
            "[a](../archive/deep/new.md) and [b](../archive/deep/new.md) and [c](sub/old.md)"
        );
        assert_eq!(
            move_from(
                "notes/index.md",
                "[a](sub/old.md)",
                "notes/sub/old.md",
                "notes/new.md"
            ),
            "[a](new.md)",
            "a path that resolves from the note's folder is relative"
        );
        assert_eq!(
            move_from(
                "wiki/index.md",
                "[a](./old.md)",
                "wiki/old.md",
                "wiki/new.md"
            ),
            "[a](./new.md)"
        );
    }

    #[test]
    fn relative_markdown_link_needs_its_note() {
        let content = "[a](../wiki/old.md)";
        let links = links_in(content);
        assert_eq!(
            rewrite_links(content, &links, "wiki/old.md", "archive/new.md"),
            content
        );
    }

    #[test]
    fn markdown_link_keeps_its_escaping_and_title() {
        let content = "[a](My%20Old.md#Part%20Two) [b](<My Old.md> \"Title\")";
        assert_eq!(
            move_from("index.md", content, "My Old.md", "Done/My New.md"),
            "[a](My%20New.md#Part%20Two) [b](<My New.md> \"Title\")"
        );
    }

    #[test]
    fn markdown_link_in_a_property_is_rewritten() {
        let content = "---\nsource: \"[Paper](old.md)\"\n---\nBody [[old]]\n";
        assert_eq!(
            move_from("index.md", content, "old.md", "new.md"),
            "---\nsource: \"[Paper](new.md)\"\n---\nBody [[new]]\n"
        );
    }

    #[test]
    fn other_markdown_links_are_left_alone() {
        let content =
            "[web](https://example.com/old.md) [ref][r] [other](other.md)\n\n[r]: other.md\n";
        assert_eq!(move_from("index.md", content, "old.md", "new.md"), content);
    }

    #[test]
    fn reference_links_are_rewritten_through_their_definition() {
        let content = "[Full][r], [r][] and [R].\n\n[r]: wiki/old.md#Part \"Title\"\n";
        assert_eq!(
            move_from("index.md", content, "wiki/old.md", "archive/new.md"),
            "[Full][r], [r][] and [R].\n\n[r]: archive/new.md#Part \"Title\"\n"
        );
    }

    #[test]
    fn reference_definition_keeps_its_form() {
        let content = "[a][1] [b][2] [c][3]\n\n[1]: <My Old.md>\n[2]:\n    My%20Old.md\n> [3]: ../wiki/My%20Old.md\n";
        assert_eq!(
            move_from(
                "notes/index.md",
                content,
                "wiki/My Old.md",
                "Done/My New.md"
            ),
            "[a][1] [b][2] [c][3]\n\n[1]: <My New.md>\n[2]:\n    My%20New.md\n> [3]: ../Done/My%20New.md\n"
        );
    }

    #[test]
    fn reference_definition_moves_with_an_inline_link_beside_it() {
        let content = "[a](old.md) [b][r] [[old]]\n\n[r]: old.md\n[unused]: old.md\n";
        assert_eq!(
            move_from("index.md", content, "old.md", "new.md"),
            "[a](new.md) [b][r] [[new]]\n\n[r]: new.md\n[unused]: new.md\n"
        );
    }

    #[test]
    fn wikilink_beside_a_matching_definition_is_rewritten_once() {
        let content = "[[old]]\n\n[old]: https://example.com\n";
        assert_eq!(
            move_from("index.md", content, "old.md", "new.md"),
            "[[new]]\n\n[old]: https://example.com\n"
        );
    }

    #[test]
    fn wrap_stale_markdown_link() {
        let out = wrap_wikilinks_as_stale("see [x](old.md) and [[old]]", "old.md");
        assert_eq!(out, "see ~~[x](old.md)~~ and ~~[[old]]~~");
    }

    #[test]
    fn wrap_stale_reference_links() {
        let out = wrap_wikilinks_as_stale("[x][r] [r][] [r]\n\n[r]: old.md\n", "old.md");
        assert_eq!(out, "~~[x][r]~~ ~~[r][]~~ ~~[r]~~\n\n[r]: old.md\n");
    }
}
