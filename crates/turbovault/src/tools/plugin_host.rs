use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use turbovault_core::config::WriteBackend;
use turbovault_core::error::Error;
use turbovault_plugin_api::{
    EventAttribution, HookBus, HookEvent, NoteSnapshot, PluginError, PluginResult, VaultDescriptor,
    VaultHost, WriteNoteRequest, WritePrecondition, WriteReceipt,
};
use turbovault_tools::{FileTools, WriteMode};

use super::CoreToolHandler;

pub(super) struct PluginVaultHost {
    core: CoreToolHandler,
    hooks: HookBus,
}

impl PluginVaultHost {
    pub(super) fn new(core: CoreToolHandler, hooks: HookBus) -> Self {
        Self { core, hooks }
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
        Ok(VaultDescriptor {
            name,
            write_backend: write_backend.to_string(),
        })
    }

    async fn list_notes(&self) -> PluginResult<Vec<String>> {
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

    async fn read_note(&self, path: &str) -> PluginResult<NoteSnapshot> {
        let (vault, manager) = self.core.get_vault_pair().await.map_err(map_host_error)?;
        let content = FileTools::new(manager)
            .read_file(path)
            .await
            .map_err(map_core_error)?;
        let version = self
            .core
            .hash_for_active_backend(&content)
            .await
            .map_err(map_host_error)?;
        Ok(NoteSnapshot {
            vault,
            path: path.to_string(),
            content,
            version,
        })
    }

    async fn write_note(&self, request: WriteNoteRequest) -> PluginResult<WriteReceipt> {
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
        let resolved_path = manager
            .resolve_path(Path::new(&request.path))
            .map_err(map_core_error)?;
        let files = FileTools::new(manager.clone());

        match &request.precondition {
            // CreateOnly → ExpectAbsent (create_file). The filesystem pre-check
            // keeps the friendly conflict message on the direct path; ExpectAbsent
            // is the TOCTOU-safe backstop on both substrates.
            WritePrecondition::CreateOnly => {
                let is_git = self
                    .core
                    .active_vault_is_git()
                    .await
                    .map_err(map_host_error)?;
                if !is_git && tokio::fs::try_exists(&resolved_path).await.unwrap_or(false) {
                    return Err(PluginError::conflict(format!(
                        "create refused: {:?} already exists",
                        request.path
                    )));
                }
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
                        turbovault_core::Precondition::ExpectBlob(version.clone()),
                        &message,
                    )
                    .await
                    .map_err(map_core_error)?;
            }
        }

        self.core.finish_complete_note_write().await;
        let version = self
            .core
            .hash_for_active_backend(&request.content)
            .await
            .map_err(map_host_error)?;
        let event = match request.precondition {
            WritePrecondition::CreateOnly => HookEvent::FileCreated {
                path: request.path.clone(),
            },
            WritePrecondition::Match(_) => HookEvent::FileModified {
                path: request.path.clone(),
            },
        };
        let attribution = request
            .provenance
            .map(EventAttribution::Attributed)
            .unwrap_or(EventAttribution::ExternalOrUnknown);
        let _ = self
            .hooks
            .publish(&vault, event, Some(version.clone()), attribution);

        Ok(WriteReceipt {
            vault,
            path: request.path,
            version,
            bytes: request.content.len(),
        })
    }
}

pub(super) fn vault_host(core: CoreToolHandler, hooks: HookBus) -> Arc<dyn VaultHost> {
    Arc::new(PluginVaultHost::new(core, hooks))
}
