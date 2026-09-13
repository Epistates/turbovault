use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use turbovault_plugin_api::{PluginError, PluginResult, PluginStore};

use super::CoreToolHandler;

/// Directory under a vault that holds TurboVault's own state.
///
/// Already unreachable through the note APIs (see
/// `turbovault_vault::PROTECTED_COMPONENTS`), which is exactly why plugin
/// state belongs here: an agent cannot stumble into a plugin's index through
/// `read_note`, and a plugin cannot publish its internals as notes.
const STATE_DIR: &str = ".turbovault";

/// Filesystem-backed [`PluginStore`], namespaced per plugin and per vault.
///
/// One instance per plugin. The plugin's namespace is baked in at
/// construction rather than passed per call, so there is no argument a plugin
/// could supply that would reach another plugin's data.
pub(super) struct FilePluginStore {
    core: CoreToolHandler,
    plugin_id: String,
}

impl FilePluginStore {
    pub(super) fn new(core: CoreToolHandler, plugin_id: impl Into<String>) -> Self {
        Self {
            core,
            plugin_id: plugin_id.into(),
        }
    }

    /// Resolve `<vault>/.turbovault/plugins/<plugin_id>/<key>`.
    ///
    /// `key` has already been validated by `PluginStorage`, so it cannot climb
    /// out of the namespace. The join is re-checked here anyway: this is the
    /// last point before touching the filesystem, and a store that trusts its
    /// caller for containment is one refactor away from not having any.
    async fn resolve(&self, vault: &str, key: &str) -> PluginResult<PathBuf> {
        let root = self.namespace_root(vault).await?;
        let resolved = root.join(key);
        if !resolved.starts_with(&root) {
            return Err(PluginError::invalid_input(format!(
                "storage key {key:?} escapes the plugin namespace"
            )));
        }
        Ok(resolved)
    }

    async fn namespace_root(&self, vault: &str) -> PluginResult<PathBuf> {
        let config = self
            .core
            .vault_config_by_name(vault)
            .await
            .map_err(|error| PluginError::not_found(error.to_string()))?;
        Ok(config
            .path
            .join(STATE_DIR)
            .join("plugins")
            .join(&self.plugin_id))
    }
}

fn io_error(error: std::io::Error) -> PluginError {
    PluginError::internal(error.to_string())
}

#[async_trait]
impl PluginStore for FilePluginStore {
    async fn get(&self, vault: &str, key: &str) -> PluginResult<Option<Vec<u8>>> {
        match tokio::fs::read(self.resolve(vault, key).await?).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io_error(error)),
        }
    }

    async fn put(&self, vault: &str, key: &str, value: &[u8]) -> PluginResult<()> {
        let path = self.resolve(vault, key).await?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(io_error)?;
        }

        // Write-then-rename, matching how the vault itself writes: a reader
        // concurrent with this call sees the old value or the new one, and an
        // interrupted write leaves a stray temp file rather than a truncated
        // index. The unique suffix keeps two writers of the same key from
        // sharing a temp path.
        let temp = path.with_extension(format!("tmp.{}", uuid::Uuid::new_v4()));
        tokio::fs::write(&temp, value).await.map_err(io_error)?;
        if let Err(error) = tokio::fs::rename(&temp, &path).await {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(io_error(error));
        }
        Ok(())
    }

    async fn delete(&self, vault: &str, key: &str) -> PluginResult<()> {
        match tokio::fs::remove_file(self.resolve(vault, key).await?).await {
            Ok(()) => Ok(()),
            // Deleting what is not there is the state the caller asked for.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_error(error)),
        }
    }

    async fn list(&self, vault: &str, prefix: &str) -> PluginResult<Vec<String>> {
        let root = self.namespace_root(vault).await?;
        let mut keys = Vec::new();
        collect_keys(&root, &root, &mut keys).await?;
        keys.retain(|key| key.starts_with(prefix));
        keys.sort();
        Ok(keys)
    }
}

/// Walk `dir`, pushing every file's path relative to `root` as a key.
///
/// A missing namespace directory is an empty namespace, not an error — a
/// plugin's first `list` happens before its first `put`.
fn collect_keys<'a>(
    root: &'a Path,
    dir: &'a Path,
    keys: &'a mut Vec<String>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = PluginResult<()>> + Send + 'a>> {
    Box::pin(async move {
        let mut entries = match tokio::fs::read_dir(dir).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(io_error(error)),
        };
        while let Some(entry) = entries.next_entry().await.map_err(io_error)? {
            let path = entry.path();
            let file_type = entry.file_type().await.map_err(io_error)?;
            if file_type.is_dir() {
                collect_keys(root, &path, keys).await?;
            } else if file_type.is_file() {
                // Temp files from an interrupted `put` are not keys.
                let is_temp = path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.starts_with("tmp."));
                if is_temp {
                    continue;
                }
                if let Ok(relative) = path.strip_prefix(root) {
                    keys.push(relative.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        Ok(())
    })
}

pub(super) fn plugin_store(core: CoreToolHandler, plugin_id: &str) -> Arc<dyn PluginStore> {
    Arc::new(FilePluginStore::new(core, plugin_id))
}
