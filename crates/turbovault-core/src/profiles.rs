//! Pre-configured profiles for different deployment scenarios, selected with
//! the binary's `--profile` flag.
//!
//! A profile sets three things, and nothing else:
//!
//! - the log level ([`ConfigProfile::log_filter`]; `RUST_LOG` overrides it),
//! - whether mutating tools are refused ([`ConfigProfile::is_read_only`]):
//!   `readonly` hides every tool not annotated read-only and rejects direct
//!   calls to them,
//! - the base [`ServerConfig`] every vault manager is built from
//!   ([`ConfigProfile::create_config`]). Of its fields, the vault layer reads
//!   `max_file_size`, `allowed_extensions`, `excluded_paths` and
//!   `reconcile_external_changes`; a vault's own config overrides them.
//!
//! The profiles:
//! - Development (the default): debug logging
//! - Production: info logging
//! - ReadOnly: warn logging, mutating tools refused
//! - HighPerformance: warn logging
//! - Minimal: error logging, no reconciliation with external edits
//! - MultiVault, Collaboration: info logging

use crate::config::ServerConfig;

/// Profile selector for pre-configured deployments
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigProfile {
    /// Development: Verbose logging, all operations, metrics enabled
    Development,
    /// Production: Security hardened, optimized, observability
    Production,
    /// ReadOnly: Search/analysis only, no write operations
    ReadOnly,
    /// HighPerformance: Optimized for large vaults (5000+ files)
    HighPerformance,
    /// Minimal: Bare essentials only
    Minimal,
    /// MultiVault: Multiple vault support with isolation
    MultiVault,
    /// Collaboration: Team features, webhooks, exports
    Collaboration,
}

impl ConfigProfile {
    /// Create a ServerConfig from this profile
    #[allow(deprecated)] // still sets the fields nothing reads, for callers that inspect them
    pub fn create_config(self) -> ServerConfig {
        let mut config = ServerConfig::new();

        match self {
            Self::Development => {
                config.log_level = "DEBUG".to_string();
                config.metrics_enabled = true;
                config.debug_mode = true;
                config.cache_ttl = 60; // 1 minute (frequent refresh)
                config.reconcile_external_changes = true;
                config.link_graph_enabled = true;
                config.full_text_search_enabled = true;
            }

            Self::Production => {
                config.log_level = "INFO".to_string();
                config.metrics_enabled = true;
                config.debug_mode = false;
                config.max_file_size = 10 * 1024 * 1024; // 10MB
                config.cache_ttl = 3600; // 1 hour
                config.reconcile_external_changes = true;
                config.link_graph_enabled = true;
                config.full_text_search_enabled = true;
                config.editor_atomic_writes = true;
                config.editor_backup_enabled = true;
            }

            Self::ReadOnly => {
                config.log_level = "WARN".to_string();
                config.metrics_enabled = true;
                config.max_file_size = 10 * 1024 * 1024;
                config.cache_ttl = 300; // 5 minutes
                // A vault this process never writes is the one most likely to
                // be edited underneath it: read-only means somebody else is
                // doing the writing.
                config.reconcile_external_changes = true;
                config.link_graph_enabled = true;
                config.full_text_search_enabled = true;
                config.editor_atomic_writes = false;
                config.editor_backup_enabled = false;
            }

            Self::HighPerformance => {
                config.log_level = "WARN".to_string();
                config.metrics_enabled = false; // Disable for performance
                config.debug_mode = false;
                config.cache_ttl = 7200; // 2 hours
                config.reconcile_external_changes = true;
                config.enable_caching = true;
                config.link_suggestions_enabled = false; // Too expensive
                config.full_text_search_enabled = false;
            }

            Self::Minimal => {
                config.log_level = "ERROR".to_string();
                config.metrics_enabled = false;
                config.debug_mode = false;
                config.cache_ttl = 10800; // 3 hours
                // Consistent with the rest of this profile rather than an
                // exception to it: with the link graph and search off there is
                // barely any derived state left to keep in agreement.
                config.reconcile_external_changes = false;
                config.link_graph_enabled = false;
                config.full_text_search_enabled = false;
                config.link_suggestions_enabled = false;
            }

            Self::MultiVault => {
                config.log_level = "INFO".to_string();
                config.metrics_enabled = true;
                config.debug_mode = false;
                config.cache_ttl = 1800; // 30 minutes
                config.reconcile_external_changes = true;
                config.multi_vault_enabled = true;
                // vaults will be populated externally
            }

            Self::Collaboration => {
                config.log_level = "INFO".to_string();
                config.metrics_enabled = true;
                config.debug_mode = false;
                config.reconcile_external_changes = true;
                config.multi_vault_enabled = true;
                config.link_graph_enabled = true;
                config.full_text_search_enabled = true;
                config.editor_backup_enabled = true;
                // Collaboration features enabled
            }
        }

        config
    }

    /// Every profile, in the order `--profile` documents them.
    pub const ALL: [Self; 7] = [
        Self::Development,
        Self::Production,
        Self::ReadOnly,
        Self::HighPerformance,
        Self::Minimal,
        Self::MultiVault,
        Self::Collaboration,
    ];

    /// The `tracing` filter this profile logs at (`debug`, `info`, `warn` or
    /// `error`), taken from its [`ServerConfig::log_level`].
    pub fn log_filter(self) -> String {
        self.create_config().log_level.to_lowercase()
    }

    /// Whether this profile refuses mutating tools.
    pub fn is_read_only(self) -> bool {
        self == Self::ReadOnly
    }

    /// Recommend a profile based on vault size
    pub fn recommend(vault_size: usize) -> Self {
        match vault_size {
            0..=100 => Self::Minimal,
            101..=1000 => Self::Development,
            1001..=5000 => Self::Production,
            _ => Self::HighPerformance,
        }
    }

    /// Get profile name
    pub fn name(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Production => "production",
            Self::ReadOnly => "read-only",
            Self::HighPerformance => "high-performance",
            Self::Minimal => "minimal",
            Self::MultiVault => "multi-vault",
            Self::Collaboration => "collaboration",
        }
    }

    /// Get profile description
    pub fn description(self) -> &'static str {
        match self {
            Self::Development => "Verbose logging, all operations enabled",
            Self::Production => "Optimized for reliability and security",
            Self::ReadOnly => "Search and analysis only, no mutations",
            Self::HighPerformance => "Tuned for large vaults (5000+ files)",
            Self::Minimal => "Bare essentials only",
            Self::MultiVault => "Multiple vault support with isolation",
            Self::Collaboration => "Team features, webhooks, and exports",
        }
    }
}

impl std::str::FromStr for ConfigProfile {
    type Err = String;

    /// Parse a profile name. Case, `-` and `_` are ignored, so `readonly`,
    /// `read-only` and `Read_Only` all name [`ConfigProfile::ReadOnly`]. An
    /// unknown name is an error rather than a fallback: silently running a
    /// misspelled `readonly` with full write access is the failure this
    /// prevents.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let key = |name: &str| name.to_lowercase().replace(['-', '_'], "");
        let wanted = key(s);
        Self::ALL
            .into_iter()
            .find(|profile| key(profile.name()) == wanted)
            .ok_or_else(|| {
                let names: Vec<&str> = Self::ALL.iter().map(|p| p.name()).collect();
                format!(
                    "unknown profile '{s}'; expected one of: {}",
                    names.join(", ")
                )
            })
    }
}

impl std::fmt::Display for ConfigProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

#[cfg(test)]
#[allow(deprecated)] // the profile tests still check every field a profile sets
mod tests {
    use super::*;

    #[test]
    fn profile_names_parse_loosely_and_unknown_names_fail() {
        for profile in ConfigProfile::ALL {
            assert_eq!(profile.name().parse::<ConfigProfile>(), Ok(profile));
        }
        assert_eq!("readonly".parse(), Ok(ConfigProfile::ReadOnly));
        assert_eq!("Read_Only".parse(), Ok(ConfigProfile::ReadOnly));
        assert_eq!(
            "highperformance".parse(),
            Ok(ConfigProfile::HighPerformance)
        );
        let err = "prod".parse::<ConfigProfile>().unwrap_err();
        assert!(err.contains("production"), "{err}");
    }

    #[test]
    fn only_readonly_refuses_writes_and_levels_are_filters() {
        for profile in ConfigProfile::ALL {
            assert_eq!(profile.is_read_only(), profile == ConfigProfile::ReadOnly);
            assert!(
                ["debug", "info", "warn", "error"].contains(&profile.log_filter().as_str()),
                "{profile}"
            );
        }
    }

    /// The default profile must not change what a vault runs with: every
    /// field the vault layer reads matches `ServerConfig::default()`.
    #[test]
    fn development_keeps_the_defaults_the_vault_layer_reads() {
        let dev = ConfigProfile::Development.create_config();
        let default = ServerConfig::default();
        assert_eq!(dev.max_file_size, default.max_file_size);
        assert_eq!(dev.allowed_extensions, default.allowed_extensions);
        assert_eq!(dev.excluded_paths, default.excluded_paths);
        assert_eq!(
            dev.reconcile_external_changes,
            default.reconcile_external_changes
        );
    }

    #[test]
    fn test_development_profile() {
        let config = ConfigProfile::Development.create_config();
        assert_eq!(config.log_level, "DEBUG");
        assert!(config.metrics_enabled);
        assert!(config.reconcile_external_changes);
    }

    #[test]
    fn test_production_profile() {
        let config = ConfigProfile::Production.create_config();
        assert_eq!(config.log_level, "INFO");
        assert!(config.metrics_enabled);
        assert!(config.reconcile_external_changes);
        assert!(config.editor_atomic_writes);
    }

    #[test]
    fn test_readonly_profile() {
        let config = ConfigProfile::ReadOnly.create_config();
        assert!(!config.editor_atomic_writes);
        assert!(!config.editor_backup_enabled);
        assert!(config.link_graph_enabled);
    }

    #[test]
    fn test_high_performance_profile() {
        let config = ConfigProfile::HighPerformance.create_config();
        assert!(!config.metrics_enabled);
        assert!(config.cache_ttl > 3600); // 2+ hours
    }

    #[test]
    fn test_minimal_profile() {
        let config = ConfigProfile::Minimal.create_config();
        assert_eq!(config.log_level, "ERROR");
        assert!(!config.metrics_enabled);
    }

    #[test]
    fn test_recommend_small_vault() {
        let profile = ConfigProfile::recommend(50);
        assert_eq!(profile, ConfigProfile::Minimal);
    }

    #[test]
    fn test_recommend_medium_vault() {
        let profile = ConfigProfile::recommend(500);
        assert_eq!(profile, ConfigProfile::Development);
    }

    #[test]
    fn test_recommend_production_vault() {
        let profile = ConfigProfile::recommend(2000);
        assert_eq!(profile, ConfigProfile::Production);
    }

    #[test]
    fn test_recommend_large_vault() {
        let profile = ConfigProfile::recommend(10000);
        assert_eq!(profile, ConfigProfile::HighPerformance);
    }

    #[test]
    fn test_profile_names() {
        assert_eq!(ConfigProfile::Development.name(), "development");
        assert_eq!(ConfigProfile::Production.name(), "production");
        assert_eq!(ConfigProfile::ReadOnly.name(), "read-only");
    }

    #[test]
    fn test_profile_descriptions() {
        assert!(!ConfigProfile::Development.description().is_empty());
        assert!(!ConfigProfile::Production.description().is_empty());
    }
}
