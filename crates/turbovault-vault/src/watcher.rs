//! File system watcher for vault changes.
//!
//! Provides real-time notification of file system events (create, modify, delete)
//! for markdown files in the vault. Built on notify crate with async event streaming.

use notify::{
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use turbovault_core::{Error, Result};

/// File system event types relevant to vault operations
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultEvent {
    /// A file was created
    FileCreated(PathBuf),
    /// A file was modified
    FileModified(PathBuf),
    /// A file was deleted
    FileDeleted(PathBuf),
    /// A file was renamed (from, to)
    FileRenamed(PathBuf, PathBuf),
}

impl VaultEvent {
    /// Get the primary path affected by this event
    pub fn path(&self) -> &Path {
        match self {
            Self::FileCreated(p)
            | Self::FileModified(p)
            | Self::FileDeleted(p)
            | Self::FileRenamed(_, p) => p,
        }
    }

    /// Check if event is for a markdown file
    pub fn is_markdown(&self) -> bool {
        self.path()
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("md"))
            .unwrap_or(false)
    }
}

/// Configuration for the file watcher
#[derive(Debug, Clone)]
pub struct WatcherConfig {
    /// Watch recursively
    pub recursive: bool,
    /// Only report events for markdown files
    pub markdown_only: bool,
    /// Ignore hidden files (starting with .)
    pub ignore_hidden: bool,
    /// Debounce duration in milliseconds (0 = no debounce)
    pub debounce_ms: u64,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            recursive: true,
            markdown_only: true,
            ignore_hidden: true,
            debounce_ms: 100,
        }
    }
}

/// Watches a vault directory for file system changes
pub struct VaultWatcher {
    config: WatcherConfig,
    watch_path: PathBuf,
    watcher: Arc<RwLock<Option<RecommendedWatcher>>>,
    event_tx: UnboundedSender<VaultEvent>,
}

impl VaultWatcher {
    /// Create a new vault watcher
    ///
    /// # Arguments
    /// * `path` - Directory to watch
    /// * `config` - Watcher configuration
    ///
    /// # Returns
    /// Tuple of (VaultWatcher, event receiver)
    pub fn new(
        path: PathBuf,
        config: WatcherConfig,
    ) -> Result<(Self, UnboundedReceiver<VaultEvent>)> {
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let watcher = Self {
            config,
            watch_path: path,
            watcher: Arc::new(RwLock::new(None)),
            event_tx,
        };

        Ok((watcher, event_rx))
    }

    /// Start watching the vault directory
    pub async fn start(&mut self) -> Result<()> {
        if self.watcher.read().await.is_some() {
            return Err(Error::invalid_path("Watcher already started".to_string()));
        }

        let event_tx = self.event_tx.clone();
        let config = self.config.clone();

        // Create notify watcher with event handler
        let mut notify_watcher = RecommendedWatcher::new(
            move |res: notify::Result<Event>| {
                if let Ok(event) = res
                    && let Some(vault_events) = Self::convert_event(event, &config)
                {
                    for vault_event in vault_events {
                        // Filter events based on config
                        if !Self::should_emit_event(&vault_event, &config) {
                            continue;
                        }
                        // Send event, ignore errors (receiver might be dropped)
                        let _ = event_tx.send(vault_event);
                    }
                }
            },
            Config::default(),
        )
        .map_err(|e| Error::io(std::io::Error::other(e)))?;

        // Start watching
        let mode = if self.config.recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };

        notify_watcher
            .watch(&self.watch_path, mode)
            .map_err(|e| Error::io(std::io::Error::other(e)))?;

        // Store watcher
        *self.watcher.write().await = Some(notify_watcher);

        Ok(())
    }

    /// Stop watching
    pub async fn stop(&mut self) -> Result<()> {
        let mut watcher = self.watcher.write().await;
        if let Some(w) = watcher.take() {
            drop(w); // Dropping the watcher stops it
        }
        Ok(())
    }

    /// Check if watcher is running
    pub async fn is_running(&self) -> bool {
        self.watcher.read().await.is_some()
    }

    /// Convert notify event to vault events
    fn convert_event(event: Event, _config: &WatcherConfig) -> Option<Vec<VaultEvent>> {
        let mut events = Vec::new();

        match event.kind {
            EventKind::Create(_) => {
                for path in event.paths {
                    events.push(VaultEvent::FileCreated(path));
                }
            }
            EventKind::Modify(_) => {
                for path in event.paths {
                    events.push(VaultEvent::FileModified(path));
                }
            }
            EventKind::Remove(_) => {
                for path in event.paths {
                    events.push(VaultEvent::FileDeleted(path));
                }
            }
            EventKind::Any => {
                // Generic event, treat as modification
                for path in event.paths {
                    events.push(VaultEvent::FileModified(path));
                }
            }
            _ => {
                // Ignore access and other events
                return None;
            }
        }

        if events.is_empty() {
            None
        } else {
            Some(events)
        }
    }

    /// Check if event should be emitted based on config
    fn should_emit_event(event: &VaultEvent, config: &WatcherConfig) -> bool {
        let path = event.path();

        // Check if hidden file
        if config.ignore_hidden
            && let Some(file_name) = path.file_name().and_then(|n| n.to_str())
            && file_name.starts_with('.')
        {
            return false;
        }

        // Check if markdown file
        if config.markdown_only && !event.is_markdown() {
            return false;
        }

        true
    }
}

impl Drop for VaultWatcher {
    fn drop(&mut self) {
        // Note: Can't await in Drop, but dropping watcher stops it
        // The watcher will be dropped when the Arc count reaches 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;
    use tokio::time::{Duration, sleep};

    async fn create_test_watcher() -> (VaultWatcher, UnboundedReceiver<VaultEvent>, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let config = WatcherConfig::default();
        let (watcher, rx) = VaultWatcher::new(temp_dir.path().to_path_buf(), config).unwrap();
        (watcher, rx, temp_dir)
    }

    #[tokio::test]
    async fn test_watcher_creation() {
        let temp_dir = TempDir::new().unwrap();
        let config = WatcherConfig::default();
        let result = VaultWatcher::new(temp_dir.path().to_path_buf(), config);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_watcher_start_stop() {
        let (mut watcher, _rx, _temp_dir) = create_test_watcher().await;

        assert!(!watcher.is_running().await);

        watcher.start().await.unwrap();
        assert!(watcher.is_running().await);

        watcher.stop().await.unwrap();
        assert!(!watcher.is_running().await);
    }

    #[tokio::test]
    async fn test_cannot_start_twice() {
        let (mut watcher, _rx, _temp_dir) = create_test_watcher().await;

        watcher.start().await.unwrap();
        let result = watcher.start().await;
        assert!(result.is_err());

        watcher.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_file_created_event() {
        let (mut watcher, mut rx, temp_dir) = create_test_watcher().await;

        watcher.start().await.unwrap();

        // Give watcher time to initialize
        sleep(Duration::from_millis(200)).await;

        // Create a markdown file
        let file_path = temp_dir.path().join("test.md");
        fs::write(&file_path, "# Test").unwrap();

        // Wait for event
        sleep(Duration::from_millis(500)).await;

        // Should receive create event (might get multiple events on some platforms)
        let mut found_create = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, VaultEvent::FileCreated(_)) {
                // Canonicalize paths for comparison (macOS /var vs /private/var)
                let event_path = event.path().canonicalize().ok();
                let expected_path = file_path.canonicalize().ok();
                if event_path == expected_path {
                    found_create = true;
                    break;
                }
            }
        }
        assert!(found_create, "Did not receive FileCreated event");

        watcher.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_file_modified_event() {
        let (mut watcher, mut rx, temp_dir) = create_test_watcher().await;

        // Create file first
        let file_path = temp_dir.path().join("test.md");
        fs::write(&file_path, "# Test").unwrap();

        watcher.start().await.unwrap();
        sleep(Duration::from_millis(200)).await;

        // Clear any create events
        while rx.try_recv().is_ok() {}

        // Modify file
        fs::write(&file_path, "# Modified").unwrap();

        // Wait for event
        sleep(Duration::from_millis(500)).await;

        // Should receive modify event (might get multiple events)
        let mut found_modify = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(
                event,
                VaultEvent::FileModified(_) | VaultEvent::FileCreated(_)
            ) {
                // Some platforms may emit Create instead of Modify on write
                found_modify = true;
                break;
            }
        }
        assert!(found_modify, "Did not receive modification event");

        watcher.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_file_deleted_event() {
        let (mut watcher, mut rx, temp_dir) = create_test_watcher().await;

        // Create file first
        let file_path = temp_dir.path().join("test.md");
        fs::write(&file_path, "# Test").unwrap();

        watcher.start().await.unwrap();
        sleep(Duration::from_millis(200)).await;

        // Clear any create events
        while rx.try_recv().is_ok() {}

        // Delete file
        fs::remove_file(&file_path).unwrap();

        // Wait for event
        sleep(Duration::from_millis(500)).await;

        // Should receive delete event
        let mut found_delete = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, VaultEvent::FileDeleted(_)) {
                found_delete = true;
                break;
            }
        }
        assert!(found_delete, "Did not receive FileDeleted event");

        watcher.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_ignores_hidden_files() {
        let (mut watcher, mut rx, temp_dir) = create_test_watcher().await;

        watcher.start().await.unwrap();
        sleep(Duration::from_millis(100)).await;

        // Create hidden file
        let file_path = temp_dir.path().join(".hidden.md");
        fs::write(&file_path, "# Hidden").unwrap();

        // Wait
        sleep(Duration::from_millis(200)).await;

        // Should NOT receive event
        let event = rx.try_recv();
        assert!(event.is_err());

        watcher.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_ignores_non_markdown_files() {
        let (mut watcher, mut rx, temp_dir) = create_test_watcher().await;

        watcher.start().await.unwrap();
        sleep(Duration::from_millis(100)).await;

        // Create non-markdown file
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Test").unwrap();

        // Wait
        sleep(Duration::from_millis(200)).await;

        // Should NOT receive event (markdown_only is true by default)
        let event = rx.try_recv();
        assert!(event.is_err());

        watcher.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_vault_event_is_markdown() {
        let event = VaultEvent::FileCreated(PathBuf::from("test.md"));
        assert!(event.is_markdown());

        let event = VaultEvent::FileCreated(PathBuf::from("test.MD"));
        assert!(event.is_markdown());

        let event = VaultEvent::FileCreated(PathBuf::from("test.txt"));
        assert!(!event.is_markdown());
    }

    #[tokio::test]
    async fn test_vault_event_path() {
        let path = PathBuf::from("test.md");
        let event = VaultEvent::FileCreated(path.clone());
        assert_eq!(event.path(), &path);

        let event = VaultEvent::FileModified(path.clone());
        assert_eq!(event.path(), &path);

        let event = VaultEvent::FileDeleted(path.clone());
        assert_eq!(event.path(), &path);
    }

    #[test]
    fn test_watcher_config_defaults() {
        let config = WatcherConfig::default();
        assert!(config.recursive);
        assert!(config.markdown_only);
        assert!(config.ignore_hidden);
        assert_eq!(config.debounce_ms, 100);
    }
}
