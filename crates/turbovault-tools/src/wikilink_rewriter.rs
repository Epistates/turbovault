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
/// on `[[` / `![[` left and `|` / `#` / `]]` right.
fn rewrite_target_form(content: &str, old: &str, new: &str) -> String {
    let pattern = format!(r"(!?\[\[){}(\||#|\]\])", regex::escape(old));
    let re = Regex::new(&pattern).expect("wikilink rewrite regex compile");
    re.replace_all(content, |caps: &regex::Captures| {
        format!("{}{}{}", &caps[1], new, &caps[2])
    })
    .into_owned()
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
/// already preceded by `~~` (idempotent for re-applied deletes).
fn wrap_target_form(content: &str, target: &str) -> String {
    // Match the FULL link (including the optional `!` embed marker and
    // the alias/section/empty-closer tail). The trailing group captures
    // the link's full body so we can re-emit it untouched.
    let pattern = format!(
        r"(?P<lead>(?:^|[^~])\s*?)(?P<link>!?\[\[{}(?:[|#][^\]]*)?\]\])",
        regex::escape(target)
    );
    // Above is brittle because it requires a leading non-`~` char or
    // start-of-string before the link to avoid re-wrapping. Simpler:
    // just rewrite all matches but check for ~~ on either side.
    let _ = pattern; // unused — we use the simpler loop below.

    let link_pat = format!(r"!?\[\[{}(?:[|#][^\]]*)?\]\]", regex::escape(target));
    let re = Regex::new(&link_pat).expect("wrap regex compile");
    let mut out = String::with_capacity(content.len());
    let mut cursor = 0;
    for m in re.find_iter(content) {
        // Skip already-wrapped: previous 2 chars are `~~` AND following 2
        // chars are `~~`.
        let already_wrapped =
            content[..m.start()].ends_with("~~") && content[m.end()..].starts_with("~~");
        out.push_str(&content[cursor..m.start()]);
        if already_wrapped {
            out.push_str(m.as_str());
        } else {
            out.push_str("~~");
            out.push_str(m.as_str());
            out.push_str("~~");
        }
        cursor = m.end();
    }
    out.push_str(&content[cursor..]);
    out
}

fn strip_md(p: &str) -> String {
    p.strip_suffix(".md").unwrap_or(p).to_string()
}

fn basename(p: &str) -> String {
    p.rsplit('/').next().unwrap_or(p).to_string()
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
    fn rewrites_regex_special_chars_in_basename() {
        // A filename with regex metacharacters like `+` or `.` should be
        // escaped before being inserted into the rewrite regex.
        let out = rewrite_wikilinks("see [[c++]]", "c++.md", "rust.md");
        assert_eq!(out, "see [[rust]]");
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
