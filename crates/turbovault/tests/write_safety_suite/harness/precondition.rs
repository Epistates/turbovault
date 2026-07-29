//! The precondition axis (design doc §3).
//!
//! `Precondition` is the *aspirational* API: at the Phase-5 cutover it moves to
//! `turbovault-core` and the mutating ops take it directly, replacing
//! `force` + `expected_hash`. Here it lives test-local so the harness compiles
//! and is validated before the production surface changes.
//!
//! A [`PreconditionKind`] is what a matrix cell selects; [`resolve`] turns it —
//! against a state's resolved [`Oids`] — into a concrete [`Precondition`], or
//! `None` when the token is undefined for that state (the matrix's N/A).

use super::state::Oids;

// nbl.6 cutover: the precondition now lives in `turbovault-core` and the real
// mutating ops take it directly. The harness re-exports it so every adapter can
// keep referring to `harness::precondition::Precondition`, and `resolve` hands
// back the production type — the matrix drives the real op surface.
pub use turbovault_core::Precondition;

/// A well-formed, non-null blob oid that matches no real content — the matrix's
/// `WRONG_OID`. Parses as a valid `git2::Oid` but never equals a stored blob.
pub const WRONG_OID: &str = "0000000000000000000000000000000000000001";

/// Encode a [`Precondition`] as the sentinel-or-oid string the wire + batch ops
/// carry: `<oid> | "absent" | "exists" | "blind"` (the ratified `expected_hash`
/// overloading). Every variant maps to `Some(_)`; the op's default precondition
/// applies only when the caller OMITS the param — which the WSS invokers never do.
pub fn sentinel(pc: &Precondition) -> Option<String> {
    Some(match pc {
        Precondition::ExpectBlob(oid) => return Some(oid.clone()),
        Precondition::ExpectAbsent => "absent".to_string(),
        Precondition::ExpectExists => "exists".to_string(),
        Precondition::Blind => "blind".to_string(),
    })
}

/// The precondition-axis selector a matrix cell carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreconditionKind {
    /// `ExpectBlob(HEAD_OID)` — defined iff committed.
    Head,
    /// `ExpectBlob(INDEX_OID)` — defined iff staged.
    Index,
    /// `ExpectBlob(WORKDIR_OID)` — defined iff exists.
    Workdir,
    /// `ExpectBlob(WRONG_OID)` — always defined.
    Wrong,
    /// `ExpectAbsent` — always defined.
    Absent,
    /// `ExpectExists` — always defined (in-place ops' no-token default).
    Exists,
    /// `Blind` — always defined.
    Blind,
}

impl PreconditionKind {
    /// Every kind, in matrix row order — lets the harness's own setup probe sweep
    /// the FULL precondition axis (`probe.rs`) rather than a hand-kept subset.
    pub const ALL: [PreconditionKind; 7] = [
        Self::Blind,
        Self::Absent,
        Self::Exists,
        Self::Head,
        Self::Index,
        Self::Workdir,
        Self::Wrong,
    ];

    /// Resolve against a state's tokens. `None` == the matrix's N/A (the token
    /// this kind names is undefined for the state, so no test is constructible).
    pub fn resolve(self, oids: &Oids) -> Option<Precondition> {
        match self {
            PreconditionKind::Head => oids.head.clone().map(Precondition::ExpectBlob),
            PreconditionKind::Index => oids.index.clone().map(Precondition::ExpectBlob),
            PreconditionKind::Workdir => oids.workdir.clone().map(Precondition::ExpectBlob),
            PreconditionKind::Wrong => Some(Precondition::ExpectBlob(WRONG_OID.to_string())),
            PreconditionKind::Absent => Some(Precondition::ExpectAbsent),
            PreconditionKind::Exists => Some(Precondition::ExpectExists),
            PreconditionKind::Blind => Some(Precondition::Blind),
        }
    }

    /// Short label for self-describing test names.
    pub fn code(self) -> &'static str {
        match self {
            PreconditionKind::Head => "HEAD",
            PreconditionKind::Index => "INDEX",
            PreconditionKind::Workdir => "WORKDIR",
            PreconditionKind::Wrong => "WRONG",
            PreconditionKind::Absent => "ABSENT",
            PreconditionKind::Exists => "EXISTS",
            PreconditionKind::Blind => "BLIND",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oids(head: Option<&str>, index: Option<&str>, workdir: Option<&str>) -> Oids {
        Oids {
            head: head.map(str::to_string),
            index: index.map(str::to_string),
            workdir: workdir.map(str::to_string),
        }
    }

    #[test]
    fn blob_kinds_resolve_to_their_token_or_na() {
        // etcsu-like: all three tokens present and distinct.
        let o = oids(Some("aaa"), Some("bbb"), Some("ccc"));
        assert_eq!(
            PreconditionKind::Head.resolve(&o),
            Some(Precondition::ExpectBlob("aaa".into()))
        );
        assert_eq!(
            PreconditionKind::Index.resolve(&o),
            Some(Precondition::ExpectBlob("bbb".into()))
        );
        assert_eq!(
            PreconditionKind::Workdir.resolve(&o),
            Some(Precondition::ExpectBlob("ccc".into()))
        );

        // Absent state: every blob token undefined => N/A for Head/Index/Workdir.
        let empty = oids(None, None, None);
        assert_eq!(PreconditionKind::Head.resolve(&empty), None);
        assert_eq!(PreconditionKind::Index.resolve(&empty), None);
        assert_eq!(PreconditionKind::Workdir.resolve(&empty), None);
    }

    #[test]
    fn non_token_kinds_are_always_defined() {
        let empty = oids(None, None, None);
        assert_eq!(
            PreconditionKind::Wrong.resolve(&empty),
            Some(Precondition::ExpectBlob(WRONG_OID.into()))
        );
        assert_eq!(
            PreconditionKind::Absent.resolve(&empty),
            Some(Precondition::ExpectAbsent)
        );
        assert_eq!(
            PreconditionKind::Exists.resolve(&empty),
            Some(Precondition::ExpectExists)
        );
        assert_eq!(
            PreconditionKind::Blind.resolve(&empty),
            Some(Precondition::Blind)
        );
    }

    #[test]
    fn wrong_oid_is_valid_and_never_a_real_token() {
        // Parses as a git oid (well-formed) ...
        assert!(git2::Oid::from_str(WRONG_OID).is_ok());
        // ... and is not the null oid.
        assert_ne!(WRONG_OID, "0000000000000000000000000000000000000000");
    }
}
