use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use turbovault_core::config::WriteBackend;
use turbovault_core::error::Error;
use turbovault_core::events::{VaultChange, WriteAttribution};
use turbovault_plugin_api::{
    NoteListing, NoteSnapshot, PluginError, PluginResult, VaultDescriptor, VaultHost,
    WriteNoteRequest, WritePrecondition, WriteReceipt,
};
use turbovault_tools::{FileTools, WriteMode};

use super::CoreToolHandler;

/// Bridges the plugin contract onto the host's tool internals.
///
/// Built PER PLUGIN. Carrying the plugin's validated namespace here is what
/// makes event attribution trustworthy: the namespace on a published event
/// comes from the mounted descriptor, never from data the plugin supplied.
pub(super) struct PluginVaultHost {
    core: CoreToolHandler,
    plugin_id: String,
}

impl PluginVaultHost {
    pub(super) fn new(core: CoreToolHandler, plugin_id: impl Into<String>) -> Self {
        Self {
            core,
            plugin_id: plugin_id.into(),
        }
    }

    /// Refuse when the caller's vault is not the one currently selected.
    ///
    /// Every host operation runs against the active vault, and the active
    /// vault can change between a plugin's read and its write. Naming the
    /// vault in the request turns that race into a refusal the plugin can
    /// retry instead of a write landing somewhere the plugin never read.
    async fn require_active_vault(&self, vault: &str) -> PluginResult<String> {
        let active = self
            .core
            .get_active_vault_name()
            .await
            .map_err(map_host_error)?;
        if active != vault {
            return Err(PluginError::conflict(format!(
                "vault {vault:?} is not the active vault (currently {active:?}); re-read and retry against the active vault"
            )));
        }
        Ok(active)
    }
}

fn map_core_error(error: Error) -> PluginError {
    match error {
        Error::FileNotFound { .. } | Error::NotFound { .. } => {
            PluginError::not_found(error.to_string())
        }
        Error::Io(ref io_error) if io_error.kind() == std::io::ErrorKind::NotFound => {
            PluginError::not_found(error.to_string())
        }
        // A protected path is a policy refusal, not a malformed request: the
        // path is well-formed and may well exist.
        Error::ProtectedPath { .. } => PluginError::permission_denied(error.to_string()),
        Error::InvalidPath { .. }
        | Error::PathTraversalAttempt { .. }
        | Error::FileTooLarge { .. }
        | Error::ParseError { .. }
        | Error::ValidationError { .. } => PluginError::invalid_input(error.to_string()),
        Error::ConcurrencyError { .. } => PluginError::conflict(error.to_string()),
        Error::ConfigError { .. } => PluginError::unavailable(error.to_string()),
        Error::Io(_) | Error::Other(_) | Error::Wrapped(_) => {
            PluginError::internal(error.to_string())
        }
    }
}

fn map_host_error(error: impl std::fmt::Display) -> PluginError {
    PluginError::unavailable(error.to_string())
}

#[async_trait]
impl VaultHost for PluginVaultHost {
    async fn active_vault(&self) -> PluginResult<VaultDescriptor> {
        let name = self
            .core
            .get_active_vault_name()
            .await
            .map_err(map_host_error)?;
        let config = self
            .core
            .multi_vault_mgr
            .get_active_vault_config()
            .await
            .map_err(map_core_error)?;
        let write_backend = match config.write_backend {
            WriteBackend::Direct => "direct",
            WriteBackend::Git => "git",
        };
        Ok(VaultDescriptor::new(name, write_backend))
    }

    async fn list_notes(&self, vault: &str) -> PluginResult<Vec<String>> {
        self.require_active_vault(vault).await?;
        let manager = self
            .core
            .get_active_vault_manager()
            .await
            .map_err(map_host_error)?;
        let mut notes = manager
            .scan_vault()
            .await
            .map_err(map_core_error)?
            .iter()
            .map(|path| manager.relative_path(path))
            .collect::<Vec<_>>();
        notes.sort();
        Ok(notes)
    }

    async fn list_notes_detailed(&self, vault: &str) -> PluginResult<Vec<NoteListing>> {
        self.require_active_vault(vault).await?;
        let manager = self
            .core
            .get_active_vault_manager()
            .await
            .map_err(map_host_error)?;
        // The scan already stats every candidate to apply the size limit, so
        // this metadata is free — which is the point: reconciling a large
        // vault must not cost a full read per note.
        let mut listings = manager
            .scan_vault_with_metadata()
            .map_err(map_core_error)?
            .into_iter()
            .map(|note| {
                NoteListing::new(
                    manager.relative_path(&note.path),
                    note.size_bytes,
                    note.modified_ms,
                )
            })
            .collect::<Vec<_>>();
        listings.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(listings)
    }

    async fn read_note(&self, vault: &str, path: &str) -> PluginResult<NoteSnapshot> {
        let vault = self.require_active_vault(vault).await?;
        let manager = self
            .core
            .get_active_vault_manager()
            .await
            .map_err(map_host_error)?;
        let content = FileTools::new(manager)
            .read_file(path)
            .await
            .map_err(map_core_error)?;
        let version = self
            .core
            .hash_for_active_backend(&content)
            .await
            .map_err(map_host_error)?;
        Ok(NoteSnapshot::new(vault, path, content, version))
    }

    async fn read_notes(&self, vault: &str, paths: &[String]) -> PluginResult<Vec<NoteSnapshot>> {
        let vault = self.require_active_vault(vault).await?;
        // Resolve the vault and its backend once for the whole batch instead
        // of per note — that per-call resolution is what makes a loop over
        // `read_note` expensive at reconciliation scale.
        let manager = self
            .core
            .get_active_vault_manager()
            .await
            .map_err(map_host_error)?;
        let backend = self.core.active_backend().await.map_err(map_host_error)?;
        let files = FileTools::new(manager);

        let mut snapshots = Vec::with_capacity(paths.len());
        for path in paths {
            // A note deleted between listing and reading is expected during
            // reconciliation; omitting it beats failing the whole pass.
            let Ok(content) = files.read_file(path).await else {
                continue;
            };
            let version =
                CoreToolHandler::hash_for_backend(backend, &content).map_err(map_host_error)?;
            snapshots.push(NoteSnapshot::new(&vault, path, content, version));
        }
        Ok(snapshots)
    }

    async fn write_note(&self, request: WriteNoteRequest) -> PluginResult<WriteReceipt> {
        self.require_active_vault(&request.vault).await?;
        let prepared = self
            .core
            .prepare_complete_note_write(
                &request.path,
                request.commit_message.clone(),
                "plugin write",
            )
            .await
            .map_err(map_host_error)?;
        let vault = prepared.vault_name;
        let manager = prepared.manager;
        let message = prepared.message;
        let files = FileTools::new(manager.clone());

        match &request.precondition {
            // CreateOnly → ExpectAbsent (create_file). The filesystem pre-check
            // keeps the friendly conflict message on the direct path; ExpectAbsent
            // is the TOCTOU-safe backstop on both substrates.
            WritePrecondition::CreateOnly => {
                // `create_file` carries an `ExpectAbsent` precondition, and
                // both substrates now check it atomically with the write: the
                // direct one under its write lock, the git one through the
                // commit's compare-and-swap. Two concurrent create-only writers
                // cannot both observe the path as free, so no pre-check here.
                files
                    .create_file(&request.path, &request.content, &message)
                    .await
                    .map_err(map_core_error)?;
            }
            // Match(version) → ExpectBlob (write_file_with_mode carries the token).
            WritePrecondition::Match(version) => {
                files
                    .write_file_with_mode(
                        &request.path,
                        &request.content,
                        WriteMode::Overwrite,
                        Some(version),
                        &message,
                    )
                    .await
                    .map_err(map_core_error)?;
            }
        }

        // Read the version back from what was actually stored rather than
        // hashing the request. The two agree today, but a receipt that assumes
        // the backend stored the bytes verbatim would hand back a CAS token
        // that silently stops matching the moment any normalization appears.
        let stored = FileTools::new(Arc::clone(&manager))
            .read_file(&request.path)
            .await
            .map_err(map_core_error)?;
        let version = self
            .core
            .hash_for_active_backend(&stored)
            .await
            .map_err(map_host_error)?;

        let change = match request.precondition {
            WritePrecondition::CreateOnly => VaultChange::Created {
                path: request.path.clone(),
            },
            WritePrecondition::Match(_) => VaultChange::Modified {
                path: request.path.clone(),
            },
        };
        let mut attribution = WriteAttribution::default();
        // Host-stamped from the mounted descriptor. Everything below it is
        // caller-supplied and advisory.
        attribution.plugin_id = Some(self.plugin_id.clone());
        if let Some(provenance) = request.provenance {
            attribution.source = Some(provenance.source);
            attribution.correlation_id = provenance.correlation_id;
            attribution.note = provenance.note;
        }
        self.core.after_write_one(&vault, change, attribution).await;

        Ok(WriteReceipt::new(
            vault,
            request.path,
            version,
            stored.len(),
        ))
    }

    async fn read_config(&self, vault: &str, relative_path: &str) -> PluginResult<Option<Vec<u8>>> {
        self.require_active_vault(vault).await?;
        let manager = self
            .core
            .get_active_vault_manager()
            .await
            .map_err(map_host_error)?;

        // `VaultApi` has already checked this path against the calling
        // plugin's declaration and normalized it. What is left is the vault
        // boundary: `resolve_path_bypassing_policy` skips the note-API
        // protected-directory rule (that rule is what this capability exists
        // to make an exception to) while keeping traversal protection.
        let resolved = manager
            .resolve_path_bypassing_policy(Path::new(relative_path))
            .map_err(map_core_error)?;

        // Resolve symlinks too: a link inside `.obsidian/` pointing at
        // `~/.ssh/id_rsa` is inside the vault by path and outside it by
        // content, and the path check alone cannot tell the difference.
        match tokio::fs::canonicalize(&resolved).await {
            Ok(real) => {
                let vault_root = tokio::fs::canonicalize(manager.vault_path())
                    .await
                    .map_err(|error| PluginError::internal(error.to_string()))?;
                if !real.starts_with(&vault_root) {
                    return Err(PluginError::permission_denied(format!(
                        "config path {relative_path:?} resolves outside the vault"
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(PluginError::internal(error.to_string())),
        }

        match tokio::fs::read(&resolved).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(PluginError::internal(error.to_string())),
        }
    }
}

pub(super) fn vault_host(core: CoreToolHandler, plugin_id: &str) -> Arc<dyn VaultHost> {
    Arc::new(PluginVaultHost::new(core, plugin_id))
}
