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
            WriteBackend::Legacy => "legacy",
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
        let vault = self
            .core
            .get_active_vault_name()
            .await
            .map_err(map_host_error)?;
        let manager = self
            .core
            .get_active_vault_manager()
            .await
            .map_err(map_host_error)?;
        let tools = self
            .core
            .get_active_write_tools()
            .await
            .map_err(map_host_error)?;
        let resolved_path = manager
            .resolve_path(Path::new(&request.path))
            .map_err(map_core_error)?;
        let message = self
            .core
            .resolve_commit_message(request.commit_message.clone(), || {
                format!("plugin write {}", request.path)
            })
            .await
            .map_err(map_host_error)?;

        match &request.precondition {
            WritePrecondition::CreateOnly => {
                if !tools.is_git() && tokio::fs::try_exists(&resolved_path).await.unwrap_or(false) {
                    return Err(PluginError::conflict(format!(
                        "create refused: {:?} already exists",
                        request.path
                    )));
                }
                tools
                    .create_file_with_message(&request.path, &request.content, &message)
                    .await
                    .map_err(map_core_error)?;
            }
            WritePrecondition::Match(version) => {
                tools
                    .write_file_with_mode_and_message(
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

        self.core.invalidate_similarity_cache().await;
        self.core.invalidate_search_cache().await;
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
