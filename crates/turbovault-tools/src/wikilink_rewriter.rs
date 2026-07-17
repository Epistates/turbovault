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
//! False-positive guard: the regex anchors on `[[` / `![[` on the left
//! and `|` / `#` / `]]` on the right, so `[[older]]` won't be touched when
//! rewriting target `old`.

use regex::Regex;

/// Rewrite every wikilink in `content` that targets `old_vault_path`
/// (vault-relative `.md` path, e.g. `wiki/old.md`) to target
/// `new_vault_path`. Bare-basename forms (`[[old]]`) and path-prefix
/// forms (`[[wiki/old]]`) are both rewritten.
///
/// If a link's existing form is path-prefix, it stays path-prefix
/// (re-targeted to the new path-with-extension-stripped). If it's bare
/// basename, it stays bare (re-targeted to the new basename). The
/// caller doesn't need to know which form the source used.
pub fn rewrite_wikilinks(content: &str, old_vault_path: &str, new_vault_path: &str) -> String {
    let old_path = strip_md(old_vault_path);
    let new_path = strip_md(new_vault_path);
    let old_base = basename(&old_path);
    let new_base = basename(&new_path);

    // Path-prefixed form FIRST (more specific). If the file is at the
    // vault root (path == basename) there's only one pass to do.
    let after_path = if old_path != old_base {
        rewrite_target_form(content, &old_path, &new_path)
    } else {
        content.to_string()
    };
    rewrite_target_form(&after_path, &old_base, &new_base)
}

/// Rewrite a SINGLE target form (either basename or path-prefix). Anchors
/// on `[[` / `![[` left and `|` / `#` / `]]` right. tlx.3: applies only
/// OUTSIDE fenced/inline code so wikilink-looking text in code examples is
/// left untouched.
fn rewrite_target_form(content: &str, old: &str, new: &str) -> String {
    let pattern = format!(r"(!?\[\[){}(\||#|\]\])", regex::escape(old));
    let re = Regex::new(&pattern).expect("wikilink rewrite regex compile");
    let apply = |text: &str| {
        re.replace_all(text, |caps: &regex::Captures| {
            format!("{}{}{}", &caps[1], new, &caps[2])
        })
        .into_owned()
    };
    map_outside_code(content, &apply)
}

/// turbovault-oz6: wrap every wikilink in `content` targeting
/// `deleted_vault_path` in `~~strikethrough~~` markdown, marking it as
/// a dead reference to a deleted page. Uses the same anchored regex
/// shape as [`rewrite_wikilinks`] so the same forms (basename,
/// path-prefix, alias, section, block, embed) are all wrapped without
/// false positives.
///
/// Returns the rewritten content. Idempotent: a link already wrapped
/// (`~~[[old]]~~`) won't be double-wrapped because the strikethrough
/// brackets sit outside the match window.
pub fn wrap_wikilinks_as_stale(content: &str, deleted_vault_path: &str) -> String {
    let old_path = strip_md(deleted_vault_path);
    let old_base = basename(&old_path);

    let after_path = if old_path != old_base {
        wrap_target_form(content, &old_path)
    } else {
        content.to_string()
    };
    wrap_target_form(&after_path, &old_base)
}

/// Wrap a SINGLE target form in `~~ ~~` strikethrough. Skips occurrences
/// already preceded by `~~` (idempotent for re-applied deletes). tlx.3:
/// applies only OUTSIDE fenced/inline code.
fn wrap_target_form(content: &str, target: &str) -> String {
    let link_pat = format!(r"!?\[\[{}(?:[|#][^\]]*)?\]\]", regex::escape(target));
    let re = Regex::new(&link_pat).expect("wrap regex compile");
    let wrap_one = |text: &str| -> String {
        let mut out = String::with_capacity(text.len());
        let mut cursor = 0;
        for m in re.find_iter(text) {
            // Skip already-wrapped: the 2 chars on each side are `~~`.
            let already_wrapped =
                text[..m.start()].ends_with("~~") && text[m.end()..].starts_with("~~");
            out.push_str(&text[cursor..m.start()]);
            if already_wrapped {
                out.push_str(m.as_str());
            } else {
                out.push_str("~~");
                out.push_str(m.as_str());
                out.push_str("~~");
            }
            cursor = m.end();
        }
        out.push_str(&text[cursor..]);
        out
    };
    map_outside_code(content, &wrap_one)
}

fn strip_md(p: &str) -> String {
    // tlx.10/[17]: case-insensitive — a path ending in `.MD`/`.Md` must still
    // strip to the bare stem, else moving `Foo.MD` looks for `[[Foo.MD]]` and
    // leaves backlinks unrewritten. `get(..)` keeps the slice on a char
    // boundary; a matched ascii `.md` suffix guarantees `len - 3` is one too.
    match p.get(p.len().saturating_sub(3)..) {
        Some(suffix) if suffix.eq_ignore_ascii_case(".md") => p[..p.len() - 3].to_string(),
        _ => p.to_string(),
    }
}

fn basename(p: &str) -> String {
    p.rsplit('/').next().unwrap_or(p).to_string()
}

// ---- tlx.3: code-aware masking ----
//
// The rewrite/wrap regexes must not touch wikilink-looking text inside code.
// We split `content` into code vs non-code and apply the transform only to the
// non-code parts. Not a full CommonMark parser: fenced blocks (``` / ~~~) and
// inline backtick spans are handled (the common cases). 4-space indented code
// blocks and exotic nested-backtick forms are not — those are rare and the
// failure is cosmetic, not corrupting.

/// Apply `f` to every region of `content` that is NOT inside a fenced code
/// block or an inline code span; emit code regions verbatim.
fn map_outside_code(content: &str, f: &dyn Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(content.len());
    let mut fence: Option<(char, usize)> = None;
    for line in content.split_inclusive('\n') {
        let marker = fence_marker(line);
        match fence {
            Some((fc, flen)) => {
                out.push_str(line); // inside a fence: verbatim
                // A matching, long-enough run closes the fence.
                if let Some((mc, mlen)) = marker
                    && mc == fc
                    && mlen >= flen
                {
                    fence = None;
                }
            }
            None => match marker {
                Some((mc, mlen)) => {
                    out.push_str(line); // opening fence: verbatim
                    fence = Some((mc, mlen));
                }
                None => out.push_str(&map_outside_inline_code(line, f)),
            },
        }
    }
    out
}

/// If `line` (ignoring leading whitespace) is a code fence, return its marker
/// char and run length (a run of >= 3 backticks or tildes).
fn fence_marker(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    let first = trimmed.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }
    let run = trimmed.chars().take_while(|&c| c == first).count();
    (run >= 3).then_some((first, run))
}

/// Apply `f` to the parts of `line` outside inline backtick code spans.
fn map_outside_inline_code(line: &str, f: &dyn Fn(&str) -> String) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    let mut plain_start = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let run_start = i;
            let mut n = 0;
            while i < bytes.len() && bytes[i] == b'`' {
                n += 1;
                i += 1;
            }
            // A code span closes on the next run of EXACTLY n backticks.
            if let Some(close_start) = find_backtick_run(bytes, i, n) {
                out.push_str(&f(&line[plain_start..run_start]));
                let code_end = close_start + n;
                out.push_str(&line[run_start..code_end]); // span verbatim
                i = code_end;
                plain_start = code_end;
            }
            // No closing run: the backticks are literal text; keep scanning.
        } else {
            i += 1;
        }
    }
    out.push_str(&f(&line[plain_start..]));
    out
}

/// Byte index of the next run of EXACTLY `n` backticks at or after `from`.
fn find_backtick_run(bytes: &[u8], from: usize, n: usize) -> Option<usize> {
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let start = i;
            let mut run = 0;
            while i < bytes.len() && bytes[i] == b'`' {
                run += 1;
                i += 1;
            }
            if run == n {
                return Some(start);
            }
        } else {
            i += 1;
        }
    }
    None
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
