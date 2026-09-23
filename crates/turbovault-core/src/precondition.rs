//! The op-level write precondition (turbovault-nbl.6 / GWS design §3).
//!
//! A single first-class precondition carried by every mutating op, borrowed from
//! HTTP conditional requests (If-Match / If-None-Match / ETag). It replaces the
//! `(force: Option<bool>, expected_hash: Option<String>)` pair the mutating ops
//! used to take.
//!
//! The version token is a git blob-oid **hex string** — keeping this type
//! git2-independent so it can live in `core`; the tool layer parses it to an oid
//! at the substrate boundary.
//!
//! **Cutover scope (nbl.6):** this is the signature carrier only. It is threaded
//! to the substrate's *existing* per-path primitives — which check against the
//! base/HEAD tree — so behavior is unchanged. Evaluating it against the WORKING
//! TREE (and the standalone dirty gate) is the later burndown (nbl.8).

/// A write-safety precondition on a single target path.
///
/// | Variant | HTTP analogue | Meaning |
/// |---|---|---|
/// | [`ExpectBlob`](Self::ExpectBlob) | `If-Match: "<etag>"` | path must currently hold exactly this blob (the token the caller read) |
/// | [`ExpectAbsent`](Self::ExpectAbsent) | `If-None-Match: *` | path must not exist (create-only; no clobber) |
/// | [`ExpectExists`](Self::ExpectExists) | `If-Match: *` | path must exist, any content (in-place ops' default) |
/// | [`Blind`](Self::Blind) | (no header) | no precondition; last-writer-wins |
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precondition {
    /// The path must currently hold exactly this blob (hex git blob oid).
    ExpectBlob(String),
    /// The path must not exist — a create-only write that refuses to clobber.
    ExpectAbsent,
    /// The path must exist, any content — the in-place default.
    ExpectExists,
    /// No precondition; last-writer-wins.
    Blind,
}

impl Precondition {
    /// Build the precondition an old `(force, expected_hash)` caller implied for
    /// a **wholesale-replace / create** op (write_note overwrite,
    /// create_from_template):
    /// - `expected_hash: Some(oid)` → [`ExpectBlob`](Self::ExpectBlob)
    /// - `force: true` → [`Blind`](Self::Blind)
    /// - neither → [`ExpectAbsent`](Self::ExpectAbsent) (the 947 no-clobber default)
    ///
    /// Precedence matches the pre-cutover tools: an explicit token wins over
    /// `force`, `force` wins over the create default.
    pub fn for_replace(expected_hash: Option<&str>, force: bool) -> Self {
        match (expected_hash, force) {
            (Some(oid), _) => Precondition::ExpectBlob(oid.to_string()),
            (None, true) => Precondition::Blind,
            (None, false) => Precondition::ExpectAbsent,
        }
    }

    /// Build the precondition an old `expected_hash` caller implied for an
    /// **in-place** op (edit_note, delete_note, update_frontmatter,
    /// manage_tags, move source): the in-place default is
    /// [`ExpectExists`](Self::ExpectExists) when the caller omits the param.
    pub fn for_in_place(expected_hash: Option<&str>) -> Self {
        Self::from_wire(expected_hash, Precondition::ExpectExists)
    }

    /// Decode the MCP wire `expected_hash` value — a **sentinel-or-oid string**
    /// (turbovault-qae.6.4) — into a precondition, falling back to `default` when
    /// the caller omits the param (preserving each op's pre-cutover behavior):
    /// - `None` → `default`
    /// - `"absent"` → [`ExpectAbsent`](Self::ExpectAbsent)
    /// - `"exists"` → [`ExpectExists`](Self::ExpectExists)
    /// - `"blind"` → [`Blind`](Self::Blind)
    /// - anything else → [`ExpectBlob`](Self::ExpectBlob) (a version-token oid,
    ///   carried verbatim; the substrate validates its shape at apply time)
    pub fn from_wire(expected_hash: Option<&str>, default: Precondition) -> Self {
        match expected_hash {
            None => default,
            Some("absent") => Precondition::ExpectAbsent,
            Some("exists") => Precondition::ExpectExists,
            Some("blind") => Precondition::Blind,
            Some(oid) => Precondition::ExpectBlob(oid.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_replace_token_wins_over_force() {
        assert_eq!(
            Precondition::for_replace(Some("deadbeef"), true),
            Precondition::ExpectBlob("deadbeef".to_string())
        );
    }

    #[test]
    fn for_replace_force_wins_over_create_default() {
        assert_eq!(Precondition::for_replace(None, true), Precondition::Blind);
    }

    #[test]
    fn for_replace_defaults_to_expect_absent() {
        assert_eq!(
            Precondition::for_replace(None, false),
            Precondition::ExpectAbsent
        );
    }

    #[test]
    fn for_in_place_with_token_is_expect_blob() {
        assert_eq!(
            Precondition::for_in_place(Some("cafebabe")),
            Precondition::ExpectBlob("cafebabe".to_string())
        );
    }

    #[test]
    fn from_wire_decodes_sentinels_and_oids() {
        let d = Precondition::ExpectAbsent;
        assert_eq!(Precondition::from_wire(None, d.clone()), d);
        assert_eq!(
            Precondition::from_wire(Some("absent"), Precondition::Blind),
            Precondition::ExpectAbsent
        );
        assert_eq!(
            Precondition::from_wire(Some("exists"), Precondition::Blind),
            Precondition::ExpectExists
        );
        assert_eq!(
            Precondition::from_wire(Some("blind"), Precondition::ExpectAbsent),
            Precondition::Blind
        );
        assert_eq!(
            Precondition::from_wire(Some("cafebabe"), Precondition::Blind),
            Precondition::ExpectBlob("cafebabe".to_string())
        );
    }

    #[test]
    fn for_in_place_without_token_is_expect_exists() {
        assert_eq!(Precondition::for_in_place(None), Precondition::ExpectExists);
    }
}
