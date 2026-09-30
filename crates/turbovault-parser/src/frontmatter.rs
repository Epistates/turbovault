//! Bounded YAML frontmatter parsing.
//!
//! Frontmatter is parsed on every write and every freshness pass, so its cost
//! is paid by the server whenever any note changes, and the YAML parser's cost
//! on nested flow collections (`[[[[...`) grows with the square of the depth:
//! 16k unclosed brackets take a quarter of a second to reject and 64k take
//! several. Nothing a person writes in frontmatter comes close to either
//! limit below, so a block past them is refused before the YAML parser sees
//! it. Alias expansion ("billion laughs") is already capped by the parser.

use serde::de::DeserializeOwned;

/// The largest frontmatter block that is parsed, in bytes.
pub const MAX_FRONTMATTER_BYTES: usize = 256 * 1024;

/// The deepest `[` / `{` nesting that is parsed.
pub const MAX_FRONTMATTER_DEPTH: usize = 64;

/// Parse a frontmatter block (the YAML between the `---` fences) into `T`,
/// refusing one that exceeds [`MAX_FRONTMATTER_BYTES`] or
/// [`MAX_FRONTMATTER_DEPTH`] before it reaches the YAML parser.
///
/// Every place that parses frontmatter should come through here.
pub fn parse_frontmatter_yaml<T: DeserializeOwned>(yaml: &str) -> Result<T, String> {
    if yaml.len() > MAX_FRONTMATTER_BYTES {
        return Err(format!(
            "frontmatter is {} bytes, over the {MAX_FRONTMATTER_BYTES}-byte limit",
            yaml.len()
        ));
    }
    let depth = flow_depth(yaml);
    if depth > MAX_FRONTMATTER_DEPTH {
        return Err(format!(
            "frontmatter nests {depth} levels deep, over the limit of {MAX_FRONTMATTER_DEPTH}"
        ));
    }
    yaml_serde::from_str(yaml).map_err(|e| e.to_string())
}

/// The deepest `[` / `{` nesting in `yaml`, counting brackets everywhere,
/// quoted text included. Tracking quotes would let an apostrophe in a plain
/// scalar hide real nesting from this scan; counting everything can only
/// over-estimate, and only for text with dozens of unbalanced brackets.
fn flow_depth(yaml: &str) -> usize {
    let mut depth = 0usize;
    let mut deepest = 0usize;
    for byte in yaml.bytes() {
        match byte {
            b'[' | b'{' => {
                depth += 1;
                deepest = deepest.max(depth);
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    deepest
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn ordinary_frontmatter_parses() {
        let fm: serde_json::Map<String, Value> =
            parse_frontmatter_yaml("title: Note\ntags: [a, b]\nnested: {x: [1, [2]]}\n").unwrap();
        assert_eq!(fm["title"], "Note");
    }

    /// #88: a bracket bomb used to cost seconds per parse, on every write
    /// and freshness pass. It is refused without reaching the YAML parser.
    #[test]
    fn deep_flow_nesting_is_refused_quickly() {
        let bomb = format!("k: {}", "[".repeat(64_000));
        let started = std::time::Instant::now();
        let err = parse_frontmatter_yaml::<Value>(&bomb).unwrap_err();
        assert!(err.contains("nests"), "{err}");
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
    }

    #[test]
    fn nesting_at_the_limit_still_parses() {
        let yaml = format!(
            "k: {}{}",
            "[".repeat(MAX_FRONTMATTER_DEPTH),
            "]".repeat(MAX_FRONTMATTER_DEPTH)
        );
        assert!(parse_frontmatter_yaml::<Value>(&yaml).is_ok());
    }

    #[test]
    fn oversized_frontmatter_is_refused() {
        let yaml = format!("k: \"{}\"", "x".repeat(MAX_FRONTMATTER_BYTES));
        let err = parse_frontmatter_yaml::<Value>(&yaml).unwrap_err();
        assert!(err.contains("bytes"), "{err}");
    }

    #[test]
    fn many_balanced_collections_are_not_deep() {
        let yaml: String = (0..5_000)
            .map(|i| format!("k{i}: [[a], {{b: c}}]\n"))
            .collect();
        assert!(parse_frontmatter_yaml::<Value>(&yaml).is_ok());
    }
}
