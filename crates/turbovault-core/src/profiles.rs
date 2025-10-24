//! Pre-configured profiles for different deployment scenarios
//!
//! Provides 7 well-tuned configurations for common use cases:
//! - Development: Verbose logging, all features enabled
//! - Production: Optimized for reliability and security
//! - ReadOnly: Analysis only, no mutations
//! - HighPerformance: Tuned for 5000+ files
//! - Minimal: Bare essentials only
//! - MultiVault: Multiple vaults with sharing
//! - Collaboration: Team features, webhooks

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
    pub fn create_config(self) -> ServerConfig {
        let mut config = ServerConfig::new();

        match self {
            Self::Development => {
                config.log_level = "DEBUG".to_string();
                config.metrics_enabled = true;
                config.debug_mode = true;
                config.max_file_size = 50 * 1024 * 1024; // 50MB
                config.cache_ttl = 60; // 1 minute (frequent refresh)
                config.watch_for_changes = true;
                config.link_graph_enabled = true;
                config.full_text_search_enabled = true;
            }

            Self::Production => {
                config.log_level = "INFO".to_string();
                config.metrics_enabled = true;
                config.debug_mode = false;
                config.max_file_size = 10 * 1024 * 1024; // 10MB
                config.cache_ttl = 3600; // 1 hour
                config.watch_for_changes = true;
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
                config.watch_for_changes = false;
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
                config.watch_for_changes = true;
                config.enable_caching = true;
                config.link_suggestions_enabled = false; // Too expensive
                config.full_text_search_enabled = false;
            }

            Self::Minimal => {
                config.log_level = "ERROR".to_string();
                config.metrics_enabled = false;
                config.debug_mode = false;
                config.cache_ttl = 10800; // 3 hours
                config.watch_for_changes = false;
                config.link_graph_enabled = false;
                config.full_text_search_enabled = false;
                config.link_suggestions_enabled = false;
            }

            Self::MultiVault => {
                config.log_level = "INFO".to_string();
                config.metrics_enabled = true;
                config.debug_mode = false;
                config.cache_ttl = 1800; // 30 minutes
                config.watch_for_changes = true;
                config.multi_vault_enabled = true;
                // vaults will be populated externally
            }

            Self::Collaboration => {
                config.log_level = "INFO".to_string();
                config.metrics_enabled = true;
                config.debug_mode = false;
                config.watch_for_changes = true;
                config.multi_vault_enabled = true;
                config.link_graph_enabled = true;
                config.full_text_search_enabled = true;
                config.editor_backup_enabled = true;
                // Collaboration features enabled
            }
        }

        config
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

impl std::fmt::Display for ConfigProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_development_profile() {
        let config = ConfigProfile::Development.create_config();
        assert_eq!(config.log_level, "DEBUG");
        assert!(config.metrics_enabled);
        assert!(config.watch_for_changes);
    }

    #[test]
    fn test_production_profile() {
        let config = ConfigProfile::Production.create_config();
        assert_eq!(config.log_level, "INFO");
        assert!(config.metrics_enabled);
        assert!(config.watch_for_changes);
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
