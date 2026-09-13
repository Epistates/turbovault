use std::sync::Arc;

use async_trait::async_trait;

use crate::{PluginError, PluginResult};

/// Host implementation behind [`PluginStorage`].
///
/// The trait is public so plugin crates can provide in-memory fakes in their
/// own tests. Keys reaching an implementation have already been validated by
/// [`PluginStorage`], and namespacing is the host's responsibility.
#[async_trait]
pub trait PluginStore: Send + Sync {
    /// Read a value, or `None` when the key was never written.
    async fn get(&self, vault: &str, key: &str) -> PluginResult<Option<Vec<u8>>>;

    /// Write a value, replacing any previous one.
    async fn put(&self, vault: &str, key: &str, value: &[u8]) -> PluginResult<()>;

    /// Remove a value. Removing a key that does not exist succeeds.
    async fn delete(&self, vault: &str, key: &str) -> PluginResult<()>;

    /// List keys beginning with `prefix`, sorted. An empty prefix lists all.
    async fn list(&self, vault: &str, prefix: &str) -> PluginResult<Vec<String>>;
}

/// Durable, plugin-private, per-vault key/value storage.
///
/// A plugin that maintains derived state — an embedding index, a cached
/// parse, a reconciliation watermark — needs somewhere to put it that is not
/// the vault's notes. Writing derived state as notes would pollute the user's
/// vault, the link graph, and the search corpus.
///
/// Isolation is structural rather than declared: the host namespaces every key
/// under the calling plugin's own space, so a plugin cannot name another
/// plugin's data, and there is no capability to grant. Storage lives beside
/// the vault it describes, so it travels and is discarded with that vault.
///
/// Values are opaque bytes. Writes are atomic at the individual key: a
/// concurrent reader sees either the previous value or the new one, never a
/// partial write. There is no transaction across keys.
#[derive(Clone)]
pub struct PluginStorage {
    store: Arc<dyn PluginStore>,
}

impl std::fmt::Debug for PluginStorage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginStorage")
            .finish_non_exhaustive()
    }
}

impl PluginStorage {
    /// Wrap a host implementation.
    pub fn new(store: Arc<dyn PluginStore>) -> Self {
        Self { store }
    }

    /// Read a value, or `None` when the key was never written.
    pub async fn get(&self, vault: &str, key: &str) -> PluginResult<Option<Vec<u8>>> {
        self.store.get(vault, &validate_key(key)?).await
    }

    /// Write a value, replacing any previous one.
    pub async fn put(&self, vault: &str, key: &str, value: &[u8]) -> PluginResult<()> {
        self.store.put(vault, &validate_key(key)?, value).await
    }

    /// Remove a value. Removing a key that does not exist succeeds.
    pub async fn delete(&self, vault: &str, key: &str) -> PluginResult<()> {
        self.store.delete(vault, &validate_key(key)?).await
    }

    /// List keys beginning with `prefix`, sorted. An empty prefix lists all.
    pub async fn list(&self, vault: &str, prefix: &str) -> PluginResult<Vec<String>> {
        // A prefix is a filter, not a key, so it may be empty and need not
        // name an existing entry — but it must not be able to walk out of the
        // plugin's namespace either.
        let prefix = if prefix.is_empty() {
            String::new()
        } else {
            validate_key(prefix)?
        };
        self.store.list(vault, &prefix).await
    }
}

/// Reject a key that could escape the plugin's namespace.
///
/// Keys map onto a directory tree, so `/` is allowed as a grouping separator
/// while anything that could climb out of, or reach outside, the namespace is
/// not.
pub(crate) fn validate_key(key: &str) -> PluginResult<String> {
    if key.is_empty() {
        return Err(PluginError::invalid_input("storage key must not be empty"));
    }
    if key.len() > MAX_KEY_LEN {
        return Err(PluginError::invalid_input(format!(
            "storage key is {} bytes; the maximum is {MAX_KEY_LEN}",
            key.len()
        )));
    }
    if key.starts_with('/') || key.starts_with('\\') {
        return Err(PluginError::invalid_input(format!(
            "storage key {key:?} must be relative"
        )));
    }
    for segment in key.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(PluginError::invalid_input(format!(
                "storage key {key:?} must not contain empty or relative segments"
            )));
        }
        if segment
            .chars()
            .any(|character| character == '\\' || character == '\0' || character.is_control())
        {
            return Err(PluginError::invalid_input(format!(
                "storage key {key:?} contains an unsupported character"
            )));
        }
    }
    Ok(key.to_string())
}

/// Longest accepted storage key, chosen to stay well inside the path-length
/// limits of every supported platform once the namespace prefix is added.
const MAX_KEY_LEN: usize = 200;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_may_group_with_slashes() {
        for valid in ["index", "embeddings/v2/shard-0.bin", "watermark.json"] {
            assert_eq!(validate_key(valid).expect("valid key"), valid);
        }
    }

    #[test]
    fn keys_cannot_escape_the_namespace() {
        for rejected in [
            "",
            "/absolute",
            "..",
            "../sibling/data",
            "nested/../../escape",
            "trailing/",
            "back\\slash",
        ] {
            let error = validate_key(rejected).expect_err("must reject");
            assert_eq!(error.code, crate::PluginErrorCode::InvalidInput);
        }
        assert!(validate_key(&"k".repeat(MAX_KEY_LEN + 1)).is_err());
    }
}
