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
}
