//! Backend-neutral vault change notifications.
//!
//! TurboVault mutates a vault through two substrates (the direct working-tree
//! writer and the git substrate) and observes a third source of change
//! (out-of-band commits picked up by the HEAD-ref listener). Anything that
//! wants to react to vault changes — today the plugin hook bus, tomorrow an
//! index maintainer — needs one vocabulary covering all three.
//!
//! This module deliberately holds no delivery machinery. It is the shape of a
//! change plus the sink trait; the host decides what a sink does with it. That
//! keeps `turbovault-core` free of any dependency on the plugin contract while
//! still letting the write paths report what they did.

/// A single observed vault mutation.
///
/// Exhaustive on purpose: every consumer inside the workspace must map each
/// kind of change deliberately, so adding one is a change everybody is forced
/// to look at rather than absorb into a catch-all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultChange {
    /// A note came into existence at `path`.
    Created {
        /// Vault-relative path using `/` separators.
        path: String,
    },
    /// An existing note's content changed.
    Modified {
        /// Vault-relative path using `/` separators.
        path: String,
    },
    /// A note was removed.
    Deleted {
        /// Vault-relative path using `/` separators.
        path: String,
    },
    /// A note moved between two paths.
    Renamed {
        /// Original vault-relative path.
        from: String,
        /// New vault-relative path.
        to: String,
    },
    /// Observation continuity was lost; consumers must re-read authoritative
    /// state rather than assume they saw every intervening change.
    ResyncRequired {
        /// Human-readable reason.
        reason: String,
    },
}

impl VaultChange {
    /// Construct a created/modified change from a "did it exist before" flag.
    ///
    /// Write paths generally know whether they replaced something; this keeps
    /// them from each re-deriving the same two-armed match.
    pub fn written(path: impl Into<String>, existed_before: bool) -> Self {
        let path = path.into();
        if existed_before {
            Self::Modified { path }
        } else {
            Self::Created { path }
        }
    }
}

/// Who the host believes performed a write.
///
/// `plugin_id` is stamped by the host from the mounted plugin's validated
/// descriptor and is therefore trustworthy. Every other field is copied
/// verbatim from the writer and is advisory only — useful for correlating
/// related operations, never an authorization or authenticity signal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct WriteAttribution {
    /// Mounted plugin namespace, when the write came through a plugin.
    pub plugin_id: Option<String>,
    /// Caller-selected source label.
    pub source: Option<String>,
    /// Caller-supplied identifier linking related operations.
    pub correlation_id: Option<String>,
    /// Caller-supplied human-readable reason.
    pub note: Option<String>,
}

impl WriteAttribution {
    /// Attribution for a change TurboVault observed but cannot attribute to a
    /// write it performed — an external editor, a `git pull`, another process.
    pub fn external() -> Self {
        Self::default()
    }

    /// Attribution for a write performed by the host's own MCP tools.
    pub fn host(source: impl Into<String>) -> Self {
        Self {
            source: Some(source.into()),
            ..Self::default()
        }
    }

    /// Whether anything is known about the writer.
    pub fn is_known(&self) -> bool {
        self.plugin_id.is_some() || self.source.is_some()
    }
}

/// Receives vault changes as the write and observation paths report them.
///
/// Implementations must not block: sinks are invoked on the write path, and a
/// slow consumer would become a slow write. Delivery is best-effort by
/// contract — a sink that cannot keep up is expected to tell its own consumers
/// to resynchronize rather than apply backpressure here.
pub trait VaultEventSink: Send + Sync {
    /// Report one change in `vault`.
    ///
    /// `content_hash` is the post-change content identity when the caller
    /// already had it; callers must not compute one just to fill this in.
    fn publish(
        &self,
        vault: &str,
        change: VaultChange,
        content_hash: Option<String>,
        attribution: WriteAttribution,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_distinguishes_creation_from_replacement() {
        assert_eq!(
            VaultChange::written("note.md", false),
            VaultChange::Created {
                path: "note.md".to_string()
            }
        );
        assert_eq!(
            VaultChange::written("note.md", true),
            VaultChange::Modified {
                path: "note.md".to_string()
            }
        );
    }

    #[test]
    fn external_attribution_claims_nothing() {
        let external = WriteAttribution::external();
        assert!(!external.is_known());
        assert!(external.plugin_id.is_none());
        assert!(WriteAttribution::host("write_note").is_known());
    }
}
