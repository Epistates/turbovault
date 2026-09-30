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
//! Links are found by the parser, not by a pattern over the text, so a
//! wikilink inside code is never touched and every edit lands on a span the
//! parser reported. Which links to edit is the caller's choice:
//! [`rewrite_wikilinks`] / [`wrap_wikilinks_as_stale`] pick them by name,
//! case-insensitively like Obsidian; the move and delete paths pass
//! [`links_in`] filtered through the link graph's own resolution, so they
//! rewrite exactly the links the graph reported as backlinks.

use turbovault_core::Link;
use turbovault_core::okf::normalize_link_target;

/// Every wikilink and embed in `content`, with the spans the parser reports.
/// Links inside code are not links and are not returned.
pub fn links_in(content: &str) -> Vec<Link> {
    let mut links = turbovault_parser::parse_wikilinks(content);
    links.extend(turbovault_parser::parse_embeds(content));
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
pub fn rewrite_links(
    content: &str,
    links: &[Link],
    old_vault_path: &str,
    new_vault_path: &str,
) -> String {
    let old_parts = parts_of(old_vault_path);
    let new_path = strip_md(new_vault_path);
    let new_base = basename(&new_path).to_string();

    splice(content, links, |span| {
        let (open, inner) = open_bracket(span)?;
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

/// Wrap exactly `links`, which must be spans of `content` as the parser
/// reported them, in `~~ ~~`. A span already wrapped is left as it is.
pub fn wrap_links_as_stale(content: &str, links: &[Link]) -> String {
    let mut spans: Vec<&Link> = links.iter().collect();
    spans.retain(|link| {
        let (start, end) = (
            link.position.offset,
            link.position.offset + link.position.length,
        );
        !(content[..start].ends_with("~~") && content[end..].starts_with("~~"))
    });
    splice(
        content,
        &spans.into_iter().cloned().collect::<Vec<_>>(),
        |span| {
            open_bracket(span)?;
            Some(format!("~~{span}~~"))
        },
    )
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

/// Replace each link's span with `edit(span)`, where it returns one. Spans
/// are applied back to front so earlier offsets stay valid; a span that is
/// not a whole wikilink in `content`, or that overlaps one already edited,
/// is skipped rather than trusted.
fn splice(content: &str, links: &[Link], edit: impl Fn(&str) -> Option<String>) -> String {
    let mut spans: Vec<(usize, usize)> = links
        .iter()
        .map(|link| {
            (
                link.position.offset,
                link.position.offset + link.position.length,
            )
        })
        .filter(|&(start, end)| content.get(start..end).is_some())
        .collect();
    spans.sort_unstable();
    spans.dedup();

    let mut out = content.to_string();
    let mut floor = usize::MAX;
    for &(start, end) in spans.iter().rev() {
        if end > floor {
            continue;
        }
        if let Some(replacement) = edit(&content[start..end]) {
            out.replace_range(start..end, &replacement);
            floor = start;
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
}
