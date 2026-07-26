use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{PluginCapabilities, PluginError, PluginResult, WriteProvenance, validate_plugin_id};

/// Public identity of a vault the host has registered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct VaultDescriptor {
    /// Host-configured vault name. Pass this to every [`VaultApi`] operation.
    pub name: String,
    /// Write substrate selected for this vault.
    pub write_backend: String,
}

impl VaultDescriptor {
    /// Construct a descriptor.
    pub fn new(name: impl Into<String>, write_backend: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            write_backend: write_backend.into(),
        }
    }
}

/// A note plus the backend-native version token used for safe writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
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

impl NoteSnapshot {
    /// Construct a snapshot.
    pub fn new(
        vault: impl Into<String>,
        path: impl Into<String>,
        content: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self {
            vault: vault.into(),
            path: path.into(),
            content: content.into(),
            version: version.into(),
        }
    }
}

/// Required optimistic-concurrency condition for a plugin write.
///
/// # Guarantees per backend
///
/// On a `git` vault both conditions are enforced inside the substrate's commit
/// section against the tree being committed, so they are genuinely atomic — a
/// racing write loses the ref CAS and the whole changeset aborts.
///
/// On a `direct` vault [`Self::CreateOnly`] is enforced by an exclusive file
/// creation, so it is also atomic. [`Self::Match`] is a
/// read-compare-then-write against the working tree: it reliably catches a
/// change that happened before the comparison, but two writers that interleave
/// inside that window can still both proceed. Callers needing a hard guarantee
/// should use a git-backed vault.
// Deliberately NOT `#[non_exhaustive]`, unlike the rest of this crate's public
// types. This is a closed vocabulary that the host must implement completely:
// a new precondition is a change to what "a safe write" means, and both sides
// should be forced to acknowledge it rather than fall into a catch-all arm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "version")]
pub enum WritePrecondition {
    /// Refuse if the target already exists.
    CreateOnly,
    /// Replace only the exact version previously returned by [`VaultApi::read_note`].
    Match(String),
}

/// Safe full-note write request.
///
/// Construct with [`Self::new`]; the optional fields have builder setters. The
/// type is `#[non_exhaustive]` so the contract can gain fields without breaking
/// plugins that build requests through the constructor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WriteNoteRequest {
    /// Vault to write to.
    ///
    /// Required, and checked by the host. TurboVault serves one active vault at
    /// a time and that selection can change between a plugin's read and its
    /// write; naming the vault turns that race into a refusal instead of a
    /// write landing in the wrong vault.
    pub vault: String,
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
    /// This metadata is advisory and is not an authorization boundary. The
    /// host separately stamps the calling plugin's namespace onto the event it
    /// publishes, and that stamp is the trustworthy one.
    pub provenance: Option<WriteProvenance>,
}

impl WriteNoteRequest {
    /// Construct a write request for `vault`.
    pub fn new(
        vault: impl Into<String>,
        path: impl Into<String>,
        content: impl Into<String>,
        precondition: WritePrecondition,
    ) -> Self {
        Self {
            vault: vault.into(),
            path: path.into(),
            content: content.into(),
            precondition,
            commit_message: None,
            provenance: None,
        }
    }

    /// Set the Git commit subject.
    pub fn with_commit_message(mut self, message: impl Into<String>) -> Self {
        self.commit_message = Some(message.into());
        self
    }

    /// Set advisory writer provenance.
    pub fn with_provenance(mut self, provenance: WriteProvenance) -> Self {
        self.provenance = Some(provenance);
        self
    }
}

/// Result of a successful plugin write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WriteReceipt {
    /// Vault containing the note.
    pub vault: String,
    /// Vault-relative path that was written.
    pub path: String,
    /// New backend-native version token, read back from the stored note.
    pub version: String,
    /// Number of content bytes stored.
    pub bytes: usize,
}

impl WriteReceipt {
    /// Construct a receipt.
    pub fn new(
        vault: impl Into<String>,
        path: impl Into<String>,
        version: impl Into<String>,
        bytes: usize,
    ) -> Self {
        Self {
            vault: vault.into(),
            path: path.into(),
            version: version.into(),
            bytes,
        }
    }
}

/// A note's identity and change-detection metadata, without its content.
///
/// This is what makes reconciliation affordable. A consumer maintaining
/// derived state stores `(size_bytes, modified_ms)` alongside each entry and
/// re-reads only the notes whose values moved — one stat per note instead of
/// one full read per note, which is the difference between a few milliseconds
/// and a full corpus scan on a large vault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct NoteListing {
    /// Vault-relative path using `/` separators.
    pub path: String,
    /// Size in bytes when the vault was scanned.
    pub size_bytes: u64,
    /// Last-modified time as Unix epoch milliseconds, when the platform
    /// reports one.
    ///
    /// Treat this as a change hint, not a clock: filesystems vary in
    /// resolution and a restored file can carry an older timestamp than the
    /// content it replaced. Pair it with `size_bytes`, and fall back to a
    /// version comparison when correctness matters more than the extra read.
    pub modified_ms: Option<u64>,
}

impl NoteListing {
    /// Construct a listing entry.
    pub fn new(path: impl Into<String>, size_bytes: u64, modified_ms: Option<u64>) -> Self {
        Self {
            path: path.into(),
            size_bytes,
            modified_ms,
        }
    }

    /// Whether this entry looks unchanged from a previously recorded one.
    ///
    /// A cheap pre-filter, not proof: equal size and timestamp are strong
    /// evidence nothing happened, but a same-size edit within one timestamp
    /// tick is invisible here. Consumers that cannot tolerate a missed change
    /// should combine this with the event feed, which reports the write
    /// regardless.
    pub fn looks_unchanged_from(&self, previous: &Self) -> bool {
        self.size_bytes == previous.size_bytes
            && self.modified_ms.is_some()
            && self.modified_ms == previous.modified_ms
    }
}

/// Host implementation behind [`VaultApi`].
///
/// The trait is public so plugin crates can provide small fakes in their own
/// tests. Production implementations remain owned by the TurboVault host.
///
/// Capability enforcement does NOT live here — [`VaultApi`] checks a plugin's
/// declared capabilities before delegating, so every host implementation gets
/// the same enforcement and no host can forget it.
#[async_trait]
pub trait VaultHost: Send + Sync {
    /// Return the currently selected vault's identity.
    async fn active_vault(&self) -> PluginResult<VaultDescriptor>;

    /// List markdown note paths in `vault`.
    async fn list_notes(&self, vault: &str) -> PluginResult<Vec<String>>;

    /// List notes in `vault` with change-detection metadata.
    async fn list_notes_detailed(&self, vault: &str) -> PluginResult<Vec<NoteListing>>;

    /// Read a complete note and its opaque version from `vault`.
    async fn read_note(&self, vault: &str, path: &str) -> PluginResult<NoteSnapshot>;

    /// Read several notes in one call.
    ///
    /// Implementations resolve the vault once for the whole batch. A path that
    /// cannot be read is omitted rather than failing the batch, so one deleted
    /// note does not sink a reconciliation pass over thousands.
    async fn read_notes(&self, vault: &str, paths: &[String]) -> PluginResult<Vec<NoteSnapshot>>;

    /// Create or compare-and-swap a complete note.
    async fn write_note(&self, request: WriteNoteRequest) -> PluginResult<WriteReceipt>;

    /// Read one application-config file, bypassing the note-API path policy.
    ///
    /// [`VaultApi::read_config`] has already checked the path against the
    /// calling plugin's declared capability, so implementations only enforce
    /// the vault boundary (including symlink escapes) and read the bytes.
    /// Return `Ok(None)` when the file does not exist.
    async fn read_config(&self, vault: &str, relative_path: &str) -> PluginResult<Option<Vec<u8>>>;
}

/// The calling plugin's namespace and what it declared it needs.
///
/// Built by the host from the mounted plugin's validated descriptor; a plugin
/// cannot fabricate one for itself.
#[derive(Debug, Clone)]
pub struct PluginIdentity {
    id: String,
    capabilities: PluginCapabilities,
}

impl PluginIdentity {
    /// Bind a validated plugin namespace to its declared capabilities.
    ///
    /// Returns an error if the namespace or any declared capability is
    /// invalid, so a malformed declaration stops the plugin from mounting
    /// rather than failing later at the first call.
    pub fn new(id: impl Into<String>, capabilities: PluginCapabilities) -> PluginResult<Self> {
        let id = id.into();
        validate_plugin_id(&id)?;
        capabilities.validate()?;
        Ok(Self { id, capabilities })
    }

    /// The plugin's MCP namespace.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// What the plugin declared it needs.
    pub fn capabilities(&self) -> &PluginCapabilities {
        &self.capabilities
    }
}

/// Cloneable, curated facade supplied to every plugin.
///
/// The host builds one of these PER PLUGIN, bound to that plugin's identity, so
/// a capability granted to one plugin is not reachable from another.
#[derive(Clone)]
pub struct VaultApi {
    host: Arc<dyn VaultHost>,
    identity: Arc<PluginIdentity>,
}

impl std::fmt::Debug for VaultApi {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VaultApi")
            .field("plugin", &self.identity.id)
            .finish_non_exhaustive()
    }
}

impl VaultApi {
    /// Wrap a host implementation for one specific plugin.
    pub fn new(host: Arc<dyn VaultHost>, identity: PluginIdentity) -> Self {
        Self {
            host,
            identity: Arc::new(identity),
        }
    }

    /// The calling plugin's identity, as the host validated it.
    pub fn identity(&self) -> &PluginIdentity {
        &self.identity
    }

    /// Return the currently selected vault's identity.
    ///
    /// The selection can change between calls. Capture the name once and pass
    /// it to subsequent operations rather than re-resolving per call.
    pub async fn active_vault(&self) -> PluginResult<VaultDescriptor> {
        self.host.active_vault().await
    }

    /// List markdown note paths in `vault`.
    pub async fn list_notes(&self, vault: &str) -> PluginResult<Vec<String>> {
        self.host.list_notes(vault).await
    }

    /// List notes in `vault` with change-detection metadata.
    ///
    /// The reconciliation half of the change contract: [`crate::HookBus`] is
    /// the fast path for changes while a plugin is running, and this is how a
    /// plugin catches up on everything the feed could not tell it about —
    /// changes made while the process was down, and changes lost to
    /// [`crate::HookRecvError::Lagged`]. Compare each entry against what you
    /// stored and re-read only what moved.
    pub async fn list_notes_detailed(&self, vault: &str) -> PluginResult<Vec<NoteListing>> {
        self.host.list_notes_detailed(vault).await
    }

    /// Read a complete note and its opaque version from `vault`.
    pub async fn read_note(&self, vault: &str, path: &str) -> PluginResult<NoteSnapshot> {
        self.host.read_note(vault, path).await
    }

    /// Read several notes in one call.
    ///
    /// Prefer this to a loop over [`Self::read_note`] when reconciling: the
    /// host resolves the vault once for the whole batch instead of once per
    /// note. Paths that cannot be read are omitted, so a note deleted between
    /// listing and reading does not fail the pass.
    pub async fn read_notes(
        &self,
        vault: &str,
        paths: &[String],
    ) -> PluginResult<Vec<NoteSnapshot>> {
        self.host.read_notes(vault, paths).await
    }

    /// Create or compare-and-swap a complete note.
    pub async fn write_note(&self, request: WriteNoteRequest) -> PluginResult<WriteReceipt> {
        self.host.write_note(request).await
    }

    /// Read one application-config file this plugin declared it needs.
    ///
    /// The note APIs cannot reach `.obsidian/` — that space holds code the
    /// application executes, so it is not part of the note surface. A plugin
    /// that genuinely needs to self-tune to app settings declares the exact
    /// files it reads via [`PluginCapabilities::config_reads`], and this method
    /// serves only those. Anything else is
    /// [`PluginErrorCode::PermissionDenied`](crate::PluginErrorCode::PermissionDenied).
    ///
    /// Returns `Ok(None)` when a permitted file does not exist.
    pub async fn read_config(&self, vault: &str, path: &str) -> PluginResult<Option<Vec<u8>>> {
        let normalized = normalize_config_path(path)?;
        if !self
            .identity
            .capabilities
            .config_reads
            .iter()
            .any(|declared| declared == &normalized)
        {
            return Err(PluginError::permission_denied(format!(
                "plugin {:?} did not declare {normalized:?} in config_reads",
                self.identity.id
            )));
        }
        self.host.read_config(vault, &normalized).await
    }
}

/// Normalize and reject a config path before it is matched against a
/// declaration.
///
/// Matching happens on the normalized form so that `.obsidian/./x.json` and
/// `.obsidian\x.json` cannot slip past a declaration of `.obsidian/x.json`, and
/// so a declaration can never be satisfied by a path that walks out of the
/// config space.
pub(crate) fn normalize_config_path(path: &str) -> PluginResult<String> {
    let unified = path.replace('\\', "/");
    if unified.is_empty() {
        return Err(PluginError::invalid_input("config path must not be empty"));
    }
    if unified.starts_with('/') {
        return Err(PluginError::invalid_input(format!(
            "config path {path:?} must be vault-relative"
        )));
    }
    let mut segments = Vec::new();
    for segment in unified.split('/') {
        match segment {
            "" | "." => continue,
            ".." => {
                return Err(PluginError::invalid_input(format!(
                    "config path {path:?} must not contain '..'"
                )));
            }
            other => segments.push(other),
        }
    }
    let normalized = segments.join("/");
    if !normalized.starts_with(".obsidian/") {
        return Err(PluginError::invalid_input(format!(
            "config path {path:?} must live under '.obsidian/'"
        )));
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_paths_normalize_to_a_single_comparable_form() {
        assert_eq!(
            normalize_config_path(".obsidian/plugins/tasks/data.json").expect("plain path"),
            ".obsidian/plugins/tasks/data.json"
        );
        assert_eq!(
            normalize_config_path(".obsidian\\plugins\\tasks\\data.json").expect("windows path"),
            ".obsidian/plugins/tasks/data.json"
        );
        assert_eq!(
            normalize_config_path(".obsidian/./plugins//tasks/data.json").expect("noisy path"),
            ".obsidian/plugins/tasks/data.json"
        );
    }

    #[test]
    fn config_paths_reject_escapes_and_foreign_space() {
        for rejected in [
            "",
            "/etc/passwd",
            ".obsidian/../.git/config",
            "..",
            "notes/note.md",
            ".turbovault/audit/operations.jsonl",
        ] {
            let error = normalize_config_path(rejected).expect_err("must reject");
            assert_eq!(error.code, crate::PluginErrorCode::InvalidInput);
        }
    }
}
