//! Configuration types for the Obsidian server.
//!
//! Follows a builder pattern for complex configuration with validation.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

/// Selects which write path serves a vault's mutations.
///
/// A **permanent per-vault** choice, not a cutover flag: `direct` and `git`
/// are two write mechanisms that coexist, one per vault. Per-vault by design —
/// the git substrate's working-tree-equals-HEAD invariant forbids mixing
/// within one vault (a direct write commits nothing, leaving the working tree
/// out of sync with the git tip), so a vault is one or the other end-to-end.
///
/// The default is `Direct`. The old variant name `legacy` is kept as a serde
/// alias so existing configs (`write_backend: legacy`) deserialize unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WriteBackend {
    /// The non-git write path: `VaultManager` mutators write directly to the
    /// filesystem. The default. Accepts `legacy` as a serde alias.
    #[default]
    #[serde(alias = "legacy")]
    Direct,
    /// The git-native write substrate (`turbovault-git`). Requires the vault
    /// path to be a git repository.
    Git,
}

impl WriteBackend {
    /// Parse a caller-supplied backend name.
    ///
    /// `direct` is accepted as a synonym for `legacy` so callers can already
    /// use the clearer name — "legacy" describes the implementation's history
    /// rather than what it does, which is write straight to the working tree.
    pub fn parse(value: &str) -> std::result::Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "legacy" | "direct" => Ok(Self::Direct),
            "git" => Ok(Self::Git),
            other => Err(format!(
                "unknown write_backend {other:?}; expected \"git\" or \"direct\" (alias \"legacy\")"
            )),
        }
    }
}

/// How the git substrate merges a fan-out's wip branch back into main
/// (mirrors `turbovault_git::MergeStrategy` as a serializable config type so
/// `turbovault-core` doesn't pick up a git2/libgit2 dependency). The consumer
/// converts at the substrate boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GitMergeStrategy {
    /// `git merge --no-ff` — preserves the wip branch's per-transaction
    /// commits with a merge commit on main. The default.
    #[default]
    MergeCommit,
    /// Advance main directly to the wip tip — errors if main advanced
    /// concurrently (caller falls back to `MergeCommit`).
    FastForward,
}

/// Commit identity for git-backed writes. Optional in the config — when
/// absent, the substrate falls back to the repo's `user.name`/`user.email`
/// and then to a built-in TurboVault default.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitAuthor {
    pub name: String,
    pub email: String,
}

/// Per-vault git substrate configuration. Only meaningful when
/// [`VaultConfig::write_backend`] is `Git`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultGitConfig {
    /// Target branch for commits. `None` = use the repo's current HEAD branch.
    #[serde(default)]
    pub branch: Option<String>,
    /// Commit author identity. `None` = repo's git config -> TurboVault default.
    #[serde(default)]
    pub author: Option<GitAuthor>,
    /// Default merge strategy for fan-out merge-back (`commit_transaction`).
    #[serde(default)]
    pub merge_strategy: GitMergeStrategy,
    /// turbovault-lri: when `false`, every git-backend mutation pre-checks
    /// each touched path against the worktree's `.gitignore` matcher and
    /// refuses the transaction (typed config error) if any path would be
    /// excluded. When `true` (the default), `.gitignore` is ignored and
    /// every requested path is committed — the original always-write
    /// behavior. Useful for vaults that gitignore `.obsidian/`, build
    /// artifacts, or per-user clutter and want a backstop against an MCP
    /// client accidentally committing them.
    #[serde(default = "default_include_ignored")]
    pub include_ignored: bool,
    /// turbovault-5nn: when `true`, every git-backend mutation MUST carry a
    /// caller-supplied commit message — a tool called without one (or with a
    /// blank/whitespace-only one) is refused loudly instead of falling back to
    /// the auto-derived subject (`write_note <path>`, etc.). Default `false`
    /// preserves the auto-derive behavior. Only meaningful on the git backend
    /// (the direct backend produces no commits, so a message is moot).
    #[serde(default)]
    pub require_commit_message: bool,
}

fn default_include_ignored() -> bool {
    true
}

// Manual `Default` so `VaultGitConfig::default().include_ignored == true`,
// matching the serde-default for that field (derive(Default) on a bool yields
// `false`, which would disagree with the missing-field deserialization).
impl Default for VaultGitConfig {
    fn default() -> Self {
        Self {
            branch: None,
            author: None,
            merge_strategy: GitMergeStrategy::default(),
            include_ignored: default_include_ignored(),
            require_commit_message: false,
        }
    }
}

/// Configuration for a single vault
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultConfig {
    /// Unique identifier for this vault
    pub name: String,
    /// Path to the vault directory
    pub path: PathBuf,
    /// Whether this is the default vault
    pub is_default: bool,

    // Optional overrides
    /// See [`ServerConfig::reconcile_external_changes`].
    #[serde(alias = "watch_for_changes")]
    pub reconcile_external_changes: Option<bool>,
    pub max_file_size: Option<u64>,
    pub allowed_extensions: Option<HashSet<String>>,
    pub excluded_paths: Option<HashSet<String>>,
    pub enable_caching: Option<bool>,
    pub cache_ttl: Option<u64>,
    pub template_dirs: Option<Vec<PathBuf>>,
    pub allowed_operations: Option<HashSet<String>>,

    /// Write backend selection. Default `Direct` — a permanent per-vault
    /// choice (see [`WriteBackend`]).
    #[serde(default)]
    pub write_backend: WriteBackend,
    /// Git substrate settings. Only used when `write_backend == Git`.
    #[serde(default)]
    pub git: Option<VaultGitConfig>,
}

impl VaultConfig {
    /// Create a new vault config with builder
    pub fn builder(name: impl Into<String>, path: impl Into<PathBuf>) -> VaultConfigBuilder {
        VaultConfigBuilder::new(name, path)
    }

    /// Validate the vault configuration
    pub fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            return Err(Error::config_error("Vault name cannot be empty"));
        }

        if !self.path.exists() {
            std::fs::create_dir_all(&self.path).map_err(|e| {
                Error::config_error(format!(
                    "Vault path does not exist and could not be created: {} ({})",
                    self.path.display(),
                    e
                ))
            })?;
        }

        if !self.path.is_dir() {
            return Err(Error::config_error(format!(
                "Vault path is not a directory: {}",
                self.path.display()
            )));
        }

        Ok(())
    }
}

/// Builder for VaultConfig
pub struct VaultConfigBuilder {
    name: String,
    path: PathBuf,
    is_default: bool,
    reconcile_external_changes: Option<bool>,
    max_file_size: Option<u64>,
    allowed_extensions: Option<HashSet<String>>,
    excluded_paths: Option<HashSet<String>>,
    enable_caching: Option<bool>,
    cache_ttl: Option<u64>,
    template_dirs: Option<Vec<PathBuf>>,
    allowed_operations: Option<HashSet<String>>,
    write_backend: WriteBackend,
    git: Option<VaultGitConfig>,
}

impl VaultConfigBuilder {
    /// Create a new builder
    pub fn new(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            is_default: false,
            reconcile_external_changes: None,
            max_file_size: None,
            allowed_extensions: None,
            excluded_paths: None,
            enable_caching: None,
            cache_ttl: None,
            template_dirs: None,
            allowed_operations: None,
            write_backend: WriteBackend::default(),
            git: None,
        }
    }

    /// Mark as default vault
    pub fn as_default(mut self) -> Self {
        self.is_default = true;
        self
    }

    /// Set reconcile_external_changes
    pub fn reconcile_external_changes(mut self, watch: bool) -> Self {
        self.reconcile_external_changes = Some(watch);
        self
    }

    /// Select the write backend (GWS.11).
    pub fn write_backend(mut self, backend: WriteBackend) -> Self {
        self.write_backend = backend;
        self
    }

    /// Set the per-vault git substrate config (typically combined with
    /// `write_backend(WriteBackend::Git)`).
    pub fn git(mut self, git: VaultGitConfig) -> Self {
        self.git = Some(git);
        self
    }

    /// Build and validate
    pub fn build(self) -> Result<VaultConfig> {
        // Expand tilde and environment variables in the path
        let expanded_path = shellexpand::full(&self.path.to_string_lossy())
            .map(|p| PathBuf::from(p.into_owned()))
            .unwrap_or(self.path);

        let config = VaultConfig {
            name: self.name,
            path: expanded_path,
            is_default: self.is_default,
            reconcile_external_changes: self.reconcile_external_changes,
            max_file_size: self.max_file_size,
            allowed_extensions: self.allowed_extensions,
            excluded_paths: self.excluded_paths,
            enable_caching: self.enable_caching,
            cache_ttl: self.cache_ttl,
            template_dirs: self.template_dirs,
            allowed_operations: self.allowed_operations,
            write_backend: self.write_backend,
            git: self.git,
        };
        config.validate()?;
        Ok(config)
    }
}

/// Global server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// List of configured vaults
    pub vaults: Vec<VaultConfig>,
    /// Configuration profile name
    pub profile: String,

    // Core settings
    /// Keep derived state (link graph, note cache, search and similarity
    /// indexes, and the plugin change feed) in agreement with edits made
    /// outside this process, by comparing a `(size, mtime)` scan against what
    /// was last observed before serving any of them.
    ///
    /// On by default, and it should stay on for any vault a human or another
    /// tool also touches, which is nearly all of them. Turning it off is only
    /// sensible for a vault nothing else writes, and it trades tool answers
    /// that agree with each other for a scan that a debounce already keeps to a
    /// few percent of wall clock.
    ///
    /// Renamed from `watch_for_changes`, which described a filesystem watcher
    /// this never was (see `VaultManager::ensure_fresh` for why not). The old
    /// spelling still deserializes.
    #[serde(alias = "watch_for_changes")]
    pub reconcile_external_changes: bool,
    pub max_file_size: u64,
    pub allowed_extensions: HashSet<String>,
    pub excluded_paths: HashSet<String>,
    pub enable_caching: bool,
    pub cache_ttl: u64,
    pub log_level: String,

    // Advanced settings
    pub template_dirs: Vec<PathBuf>,
    pub default_template_variables: serde_json::Value,
    pub editor_backup_enabled: bool,
    pub editor_atomic_writes: bool,
    pub max_backup_files: usize,
    pub max_edit_history: usize,
    pub backup_retention_days: u32,

    // Link graph settings
    pub link_graph_enabled: bool,
    pub link_suggestions_enabled: bool,
    pub max_link_suggestions: usize,
    pub link_similarity_threshold: f32,

    // Search settings
    pub full_text_search_enabled: bool,
    pub index_rebuild_interval: u64,

    // Multi-vault
    pub multi_vault_enabled: bool,

    // Admin
    pub metrics_enabled: bool,
    pub debug_mode: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            vaults: vec![],
            profile: "default".to_string(),
            reconcile_external_changes: true,
            max_file_size: 10 * 1024 * 1024, // 10MB
            allowed_extensions: [".md", ".txt", ".canvas"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            excluded_paths: [".obsidian", ".git", ".DS_Store", "node_modules"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            enable_caching: true,
            cache_ttl: 3600,
            log_level: "INFO".to_string(),
            template_dirs: vec![],
            default_template_variables: serde_json::json!({}),
            editor_backup_enabled: true,
            editor_atomic_writes: true,
            max_backup_files: 100,
            max_edit_history: 100,
            backup_retention_days: 7,
            link_graph_enabled: true,
            link_suggestions_enabled: true,
            max_link_suggestions: 10,
            link_similarity_threshold: 0.3,
            full_text_search_enabled: true,
            index_rebuild_interval: 3600,
            multi_vault_enabled: false,
            metrics_enabled: false,
            debug_mode: false,
        }
    }
}

impl ServerConfig {
    /// Create new configuration
    pub fn new() -> Self {
        Self::default()
    }

    /// Validate configuration
    pub fn validate(&self) -> Result<()> {
        if self.vaults.is_empty() {
            return Err(Error::config_error("At least one vault must be configured"));
        }

        // Check unique vault names
        let names: HashSet<_> = self.vaults.iter().map(|v| &v.name).collect();
        if names.len() != self.vaults.len() {
            return Err(Error::config_error("Vault names must be unique"));
        }

        // Check unique default vaults
        let defaults: Vec<_> = self.vaults.iter().filter(|v| v.is_default).collect();
        if defaults.len() > 1 {
            return Err(Error::config_error("Only one vault can be default"));
        }

        // Validate each vault
        for vault in &self.vaults {
            vault.validate()?;
        }

        Ok(())
    }

    /// Get default vault config
    pub fn default_vault(&self) -> Result<&VaultConfig> {
        self.vaults
            .iter()
            .find(|v| v.is_default)
            .or_else(|| self.vaults.first())
            .ok_or_else(|| Error::config_error("No default vault configured"))
    }

    /// Save vault configuration to file (for persistence)
    pub async fn save_vaults(&self, path: &Path) -> Result<()> {
        let yaml = yaml_serde::to_string(&self.vaults)
            .map_err(|e| Error::config_error(format!("Failed to serialize vaults: {}", e)))?;

        tokio::fs::write(path, yaml).await.map_err(|e| {
            Error::config_error(format!(
                "Failed to save vaults to {}: {}",
                path.display(),
                e
            ))
        })
    }

    /// Load vault configuration from file
    pub async fn load_vaults(path: &Path) -> Result<Vec<VaultConfig>> {
        if !path.exists() {
            return Ok(Vec::new()); // Return empty if file doesn't exist
        }

        let content = tokio::fs::read_to_string(path).await.map_err(|e| {
            Error::config_error(format!(
                "Failed to load vaults from {}: {}",
                path.display(),
                e
            ))
        })?;

        let vaults = yaml_serde::from_str(&content)
            .map_err(|e| Error::config_error(format!("Invalid vault configuration: {}", e)))?;

        Ok(vaults)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_vault_config_builder() {
        let temp = TempDir::new().unwrap();
        let vault = VaultConfig::builder("main", temp.path())
            .as_default()
            .reconcile_external_changes(true)
            .build();

        assert!(vault.is_ok());
        let v = vault.unwrap();
        assert_eq!(v.name, "main");
        assert!(v.is_default);
    }

    #[test]
    fn test_server_config_validation() {
        let mut config = ServerConfig::new();
        config.vaults.clear();
        assert!(config.validate().is_err());
    }

    // -------- GWS.11 write-backend + git config --------

    #[test]
    fn vault_config_defaults_to_direct_backend_and_no_git() {
        let temp = TempDir::new().unwrap();
        let v = VaultConfig::builder("main", temp.path()).build().unwrap();
        assert_eq!(v.write_backend, WriteBackend::Direct);
        assert!(v.git.is_none());
    }

    #[test]
    fn vault_config_builder_sets_git_backend() {
        let temp = TempDir::new().unwrap();
        let v = VaultConfig::builder("g", temp.path())
            .write_backend(WriteBackend::Git)
            .git(VaultGitConfig {
                branch: Some("main".to_string()),
                author: Some(GitAuthor {
                    name: "TurboVault".to_string(),
                    email: "tv@localhost".to_string(),
                }),
                merge_strategy: GitMergeStrategy::FastForward,
                include_ignored: false,
                require_commit_message: false,
            })
            .build()
            .unwrap();
        assert_eq!(v.write_backend, WriteBackend::Git);
        let g = v.git.unwrap();
        assert_eq!(g.branch.as_deref(), Some("main"));
        assert_eq!(g.merge_strategy, GitMergeStrategy::FastForward);
        assert!(!g.include_ignored);
        assert_eq!(g.author.unwrap().email, "tv@localhost");
    }

    #[test]
    fn vault_config_yaml_roundtrip_with_git_section() {
        let temp = TempDir::new().unwrap();
        let v = VaultConfig::builder("g", temp.path())
            .write_backend(WriteBackend::Git)
            .git(VaultGitConfig::default())
            .build()
            .unwrap();
        let yaml = yaml_serde::to_string(&v).unwrap();
        let back: VaultConfig = yaml_serde::from_str(&yaml).unwrap();
        assert_eq!(back.write_backend, WriteBackend::Git);
        assert!(back.git.is_some());
        // VaultGitConfig defaults survive a roundtrip.
        let g = back.git.unwrap();
        assert_eq!(g.merge_strategy, GitMergeStrategy::MergeCommit);
        assert!(g.include_ignored, "include_ignored defaults to true");
    }

    #[test]
    fn vault_config_yaml_direct_omits_git_section() {
        let temp = TempDir::new().unwrap();
        let v = VaultConfig::builder("l", temp.path()).build().unwrap();
        let yaml = yaml_serde::to_string(&v).unwrap();
        // The roundtrip preserves the direct default + None git.
        let back: VaultConfig = yaml_serde::from_str(&yaml).unwrap();
        assert_eq!(back.write_backend, WriteBackend::Direct);
        assert!(back.git.is_none());
    }

    #[test]
    fn write_backend_serializes_lowercase() {
        let yaml = yaml_serde::to_string(&WriteBackend::Git).unwrap();
        assert!(yaml.contains("git"), "got: {yaml}");
        let yaml = yaml_serde::to_string(&WriteBackend::Direct).unwrap();
        assert!(yaml.contains("direct"), "got: {yaml}");
        // `legacy` is kept as a serde alias so pre-rename configs still load.
        let back: WriteBackend = yaml_serde::from_str("legacy\n").unwrap();
        assert_eq!(back, WriteBackend::Direct);
        let back: WriteBackend = yaml_serde::from_str("direct\n").unwrap();
        assert_eq!(back, WriteBackend::Direct);
    }

    #[test]
    fn merge_strategy_serializes_kebab_case() {
        let yaml = yaml_serde::to_string(&GitMergeStrategy::MergeCommit).unwrap();
        assert!(yaml.contains("merge-commit"), "got: {yaml}");
        let back: GitMergeStrategy = yaml_serde::from_str("fast-forward\n").unwrap();
        assert_eq!(back, GitMergeStrategy::FastForward);
    }
}
