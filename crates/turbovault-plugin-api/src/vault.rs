use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{PluginResult, WriteProvenance};

/// Public identity of the vault selected by the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultDescriptor {
    /// Host-configured vault name.
    pub name: String,
    /// Write substrate selected for this vault.
    pub write_backend: String,
}

/// A note plus the backend-native version token used for safe writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteSnapshot {
    /// Vault containing the note.
    pub vault: String,
    /// Vault-relative path using `/` separators.
    pub path: String,
    /// Complete markdown content.
    pub content: String,
    /// Opaque token accepted by [`WritePrecondition::Match`].
    pub version: String,
}

/// Required optimistic-concurrency condition for a plugin write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "version")]
pub enum WritePrecondition {
    /// Refuse if the target already exists.
    CreateOnly,
    /// Replace only the exact version previously returned by [`VaultApi::read_note`].
    Match(String),
}

/// Safe full-note write request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteNoteRequest {
    /// Vault-relative note path.
    pub path: String,
    /// Complete replacement content.
    pub content: String,
    /// Mandatory create-or-CAS condition; blind overwrites are not exposed.
    pub precondition: WritePrecondition,
    /// Optional Git commit subject. Hosts may require it by policy.
    pub commit_message: Option<String>,
    /// Optional best-effort writer identity for hook correlation.
    ///
    /// This metadata is advisory and is not an authorization boundary.
    pub provenance: Option<WriteProvenance>,
}

/// Result of a successful plugin write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteReceipt {
    /// Vault containing the note.
    pub vault: String,
    /// Vault-relative path that was written.
    pub path: String,
    /// New backend-native version token.
    pub version: String,
    /// Number of content bytes written.
    pub bytes: usize,
}

/// Host implementation behind [`VaultApi`].
///
/// The trait is public so plugin crates can provide small fakes in their own
/// tests. Production implementations remain owned by the TurboVault host.
#[async_trait]
pub trait VaultHost: Send + Sync {
    /// Return the active vault identity.
    async fn active_vault(&self) -> PluginResult<VaultDescriptor>;

    /// List markdown note paths in the active vault.
    async fn list_notes(&self) -> PluginResult<Vec<String>>;

    /// Read a complete note and its opaque version.
    async fn read_note(&self, path: &str) -> PluginResult<NoteSnapshot>;

    /// Create or compare-and-swap a complete note.
    async fn write_note(&self, request: WriteNoteRequest) -> PluginResult<WriteReceipt>;

    /// Read a read-only vault application-config file, e.g. an entry under
    /// `.obsidian/`, returning `None` when it does not exist.
    ///
    /// This is the only door onto the vault's non-note config space (the note
    /// APIs deliberately exclude dotfolders like `.obsidian`). It exists so a
    /// module can self-tune to the user's app settings — for example the
    /// Obsidian Tasks plugin's `data.json` — instead of requiring the settings
    /// to be duplicated into module config.
    ///
    /// Hosts enforce read scoping and path-traversal safety, and MAY decline the
    /// capability entirely; the default implementation returns `None` so that a
    /// host which does not support config reads degrades gracefully rather than
    /// erroring. The path is vault-relative and uses `/` separators.
    async fn read_config(&self, _relative_path: &str) -> PluginResult<Option<Vec<u8>>> {
        Ok(None)
    }
}

/// Cloneable, curated facade supplied to every plugin.
#[derive(Clone)]
pub struct VaultApi {
    host: Arc<dyn VaultHost>,
}

impl std::fmt::Debug for VaultApi {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("VaultApi").finish_non_exhaustive()
    }
}

impl VaultApi {
    /// Wrap a host implementation.
    pub fn new(host: Arc<dyn VaultHost>) -> Self {
        Self { host }
    }

    /// Return the active vault identity.
    pub async fn active_vault(&self) -> PluginResult<VaultDescriptor> {
        self.host.active_vault().await
    }

    /// List markdown note paths in the active vault.
    pub async fn list_notes(&self) -> PluginResult<Vec<String>> {
        self.host.list_notes().await
    }

    /// Read a complete note and its opaque version.
    pub async fn read_note(&self, path: &str) -> PluginResult<NoteSnapshot> {
        self.host.read_note(path).await
    }

    /// Create or compare-and-swap a complete note.
    pub async fn write_note(&self, request: WriteNoteRequest) -> PluginResult<WriteReceipt> {
        self.host.write_note(request).await
    }

    /// Read a read-only vault application-config file (e.g. under `.obsidian/`),
    /// returning `None` when it does not exist or the host declines the read.
    pub async fn read_config(&self, relative_path: &str) -> PluginResult<Option<Vec<u8>>> {
        self.host.read_config(relative_path).await
    }
}
