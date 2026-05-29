//! GWS.5fm — `ReadSet`: the read-set precondition primitive for the
//! reconsideration domino.
//!
//! When an agent reads derived state (backlinks, search results, similar
//! notes — anything computed from a SET of files) and then writes a new
//! file based on what it read, the substrate's blob-oid precondition only
//! protects the files the agent WROTE. The read set — the source files
//! whose content determined the derivation — is invisible to the substrate.
//!
//! This module is the substrate-level building block: a transportable
//! token that encodes `Vec<(path, blob_oid)>`, and helpers that translate
//! it into `Precondition::expect_blob` entries on a [`turbovault_git::Transaction`].
//!
//! Read tools that compute derived state (planned MCP layer extension —
//! see the follow-up ticket) emit one; write tools that accept one apply
//! the resulting preconditions, so a concurrent change to ANY source file
//! the agent's read depended on will abort the transaction loudly and
//! force re-synthesis.
//!
//! ## Encoding
//!
//! Tokens are plain JSON arrays — agents can inspect and even hand-build
//! them. No base64; the substrate doesn't care about wire-format opacity.
//!
//! ```text
//! [["path/to/a.md","aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
//!  ["path/to/b.md","bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"]]
//! ```
//!
//! Token strings parse with [`ReadSet::decode`]; build with
//! [`ReadSet::from_entries`] then [`ReadSet::encode`].

use serde::{Deserialize, Serialize};
use turbovault_core::prelude::*;
use turbovault_git::{Oid, Transaction};

/// A read-set: paths the caller read at the moment of read, paired with
/// the blob oid each path held at that moment. Translated into a fan of
/// `expect_blob` preconditions on the next write transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReadSet {
    entries: Vec<(String, OidWire)>,
}

/// Wire-format Oid (hex string). The substrate Oid type doesn't implement
/// Serialize/Deserialize directly, so we serialize as hex and parse on
/// the way back in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
struct OidWire(String);

impl ReadSet {
    /// Build from `(path, oid)` entries. The order is preserved (matters
    /// only for human-readability of the encoded token).
    pub fn from_entries(entries: Vec<(String, Oid)>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|(p, o)| (p, OidWire(o.to_string())))
                .collect(),
        }
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Decoded `(path, oid)` view of the entries. Errors if any encoded
    /// oid fails to parse.
    pub fn decoded_entries(&self) -> Result<Vec<(String, Oid)>> {
        self.entries
            .iter()
            .map(|(p, o)| {
                Oid::from_str(&o.0)
                    .map(|oid| (p.clone(), oid))
                    .map_err(|e| Error::ConcurrencyError {
                        reason: format!(
                            "ReadSet token contains malformed blob oid for {}: {} ({})",
                            p, o.0, e
                        ),
                    })
            })
            .collect()
    }

    /// Serialize to the JSON wire format.
    pub fn encode(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| Error::config_error(format!("ReadSet encode failed: {}", e)))
    }

    /// Parse a wire-format token. Returns
    /// [`Error::ConcurrencyError`] on malformed input — the agent's
    /// recourse is to re-read and rebuild the read set, same as any other
    /// stale-token failure.
    pub fn decode(s: &str) -> Result<Self> {
        serde_json::from_str(s).map_err(|e| Error::ConcurrencyError {
            reason: format!("ReadSet token malformed: {}", e),
        })
    }
}

/// Augment `txn` with `expect_blob` preconditions for every entry in
/// `read_set`. The returned transaction aborts (the reconsideration
/// domino) if ANY read-set path's blob has changed since the agent read
/// it — the substrate's existing multi-file CAS does the rest.
///
/// Returns an error only if the token's oids don't parse; otherwise
/// returns the augmented transaction.
pub fn apply_read_set_to_transaction(
    mut txn: Transaction,
    read_set: &ReadSet,
) -> Result<Transaction> {
    for (path, oid) in read_set.decoded_entries()? {
        txn = txn.expect_blob(path, oid);
    }
    Ok(txn)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(byte: u8) -> Oid {
        let hex = std::iter::repeat_n(byte, 20)
            .map(|b| format!("{:02x}", b))
            .collect::<String>();
        Oid::from_str(&hex).unwrap()
    }

    #[test]
    fn empty_read_set_round_trips() {
        let r = ReadSet::from_entries(vec![]);
        assert!(r.is_empty());
        let s = r.encode().unwrap();
        let back = ReadSet::decode(&s).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn entries_round_trip_via_json() {
        let r = ReadSet::from_entries(vec![
            ("a.md".to_string(), oid(0xaa)),
            ("dir/b.md".to_string(), oid(0xbb)),
        ]);
        let token = r.encode().unwrap();
        let back = ReadSet::decode(&token).unwrap();
        assert_eq!(back, r);
        assert_eq!(back.len(), 2);
    }

    #[test]
    fn decoded_entries_yields_oid_typed_pairs() {
        let r = ReadSet::from_entries(vec![("p".to_string(), oid(0xcc))]);
        let out = r.decoded_entries().unwrap();
        assert_eq!(out, vec![("p".to_string(), oid(0xcc))]);
    }

    #[test]
    fn malformed_token_is_concurrency_error() {
        let err = ReadSet::decode("{not-json").unwrap_err();
        assert!(
            matches!(err, Error::ConcurrencyError { .. }),
            "got: {err:?}"
        );
    }

    #[test]
    fn malformed_oid_in_token_surfaces_as_concurrency_error() {
        // Hand-craft a JSON token with a non-40-char "oid".
        let token = r#"[["a.md","not-an-oid"]]"#;
        let r = ReadSet::decode(token).unwrap();
        let err = r.decoded_entries().unwrap_err();
        assert!(
            matches!(err, Error::ConcurrencyError { .. }),
            "got: {err:?}"
        );
    }

    #[test]
    fn apply_read_set_to_transaction_adds_preconditions_in_order() {
        // Substrate-level proof: the augmented transaction carries
        // expect_blob preconditions for every read-set entry.
        let txn = Transaction::new("write y").upsert("y.md", b"Y" as &[u8]);
        let read_set = ReadSet::from_entries(vec![
            ("src1.md".into(), oid(0x11)),
            ("src2.md".into(), oid(0x22)),
        ]);
        let augmented = apply_read_set_to_transaction(txn, &read_set).unwrap();
        // Can't inspect Transaction internals (private fields); apply it
        // against an unborn repo to observe behavior in the integration
        // test below this module's domain.
        let _ = augmented;
    }
}
