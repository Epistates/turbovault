use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use turbomcp::VisibilityConfig;
use turbovault_core::VaultConfig;

/// User-facing tool visibility settings loaded from TurboVault config.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolVisibilitySettings {
    /// If non-empty, only these exact tool names are listed and callable.
    pub allowed: Vec<String>,
    /// Exact tool names omitted from `tools/list` but still callable by name.
    pub hidden: Vec<String>,
    /// Exact tool names omitted from `tools/list` and rejected on direct calls.
    pub disabled: Vec<String>,
    /// Hide tools that are not annotated read-only by TurboMCP.
    pub require_read_only: bool,
}

/// CLI/env overrides merged with file-based tool visibility settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolVisibilityOverrides {
    pub allowed: Vec<String>,
    pub hidden: Vec<String>,
    pub disabled: Vec<String>,
    pub require_read_only: bool,
}

/// Top-level shape of the TurboVault `--config` / `TURBOVAULT_CONFIG` YAML file.
///
/// Both the `tool_visibility:` section AND the `vaults:` section live here so
/// the file has ONE canonical shape readable by both consumers
/// (`ToolVisibilitySettings::from_yaml_file` for the visibility rules,
/// `TurboVaultConfigFile::load` for vault registration). xj8-followon: the
/// earlier xj8 wiring had `ServerConfig::load_vaults` read the same path as a
/// bare `Vec<VaultConfig>`, conflicting with the tool-visibility parser. See
/// also turbovault-wbk (closed by this unification).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TurboVaultConfigFile {
    pub tool_visibility: ToolVisibilitySettings,
    pub vaults: Vec<VaultConfig>,
}

impl TurboVaultConfigFile {
    /// Parse a TurboVault YAML config file.
    pub fn from_yaml_str(yaml: &str) -> anyhow::Result<Self> {
        yaml_serde::from_str(yaml).context("invalid TurboVault YAML config")
    }

    /// Load and parse a TurboVault YAML config file from disk.
    pub async fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let yaml = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("failed to read TurboVault config {}", path.display()))?;
        Self::from_yaml_str(&yaml)
            .with_context(|| format!("failed to parse TurboVault config {}", path.display()))
    }
}

impl ToolVisibilitySettings {
    /// Parse the `tool_visibility` section from a TurboVault YAML config.
    pub fn from_yaml_str(yaml: &str) -> anyhow::Result<Self> {
        let config = TurboVaultConfigFile::from_yaml_str(yaml)?;
        Ok(config.tool_visibility)
    }

    /// Load the `tool_visibility` section from a YAML config file.
    pub async fn from_yaml_file(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let config = TurboVaultConfigFile::load(path).await?;
        Ok(config.tool_visibility)
    }

    /// Merge CLI/env overrides into file settings.
    pub fn merge_cli(&mut self, overrides: ToolVisibilityOverrides) {
        extend_clean(&mut self.allowed, overrides.allowed);
        extend_clean(&mut self.hidden, overrides.hidden);
        extend_clean(&mut self.disabled, overrides.disabled);
        self.require_read_only |= overrides.require_read_only;
    }

    /// Convert TurboVault settings to TurboMCP's runtime visibility config.
    pub fn into_visibility_config(self) -> VisibilityConfig {
        let mut config = VisibilityConfig::new();

        if !self.allowed.is_empty() {
            config = config.with_allowed_tools(self.allowed);
        }

        if !self.disabled.is_empty() {
            config = config.with_disabled_tools(self.disabled);
        }

        if !self.hidden.is_empty() {
            config = config.with_hidden_tools(self.hidden);
        }

        if self.require_read_only {
            config = config.require_read_only_tools();
        }

        config
    }

    pub fn has_rules(&self) -> bool {
        !self.allowed.is_empty()
            || !self.hidden.is_empty()
            || !self.disabled.is_empty()
            || self.require_read_only
    }
}

/// Default TurboVault user config path.
pub fn default_config_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(".turbovault").join("config.yaml"))
}

fn extend_clean(target: &mut Vec<String>, values: Vec<String>) {
    for value in values {
        let value = value.trim();
        if !value.is_empty() && !target.iter().any(|existing| existing == value) {
            target.push(value.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_visibility_from_yaml() {
        let yaml = r#"
tool_visibility:
  allowed:
    - read_note
    - search
    - full_health_analysis
  hidden:
    - full_health_analysis
  disabled:
    - delete_note
  require_read_only: true
"#;

        let settings = ToolVisibilitySettings::from_yaml_str(yaml).unwrap();
        let config = settings.into_visibility_config();

        assert!(config.tools.is_listed("read_note"));
        assert!(!config.tools.is_listed("full_health_analysis"));
        assert!(config.tools.is_enabled("full_health_analysis"));
        assert!(!config.tools.is_enabled("delete_note"));
        assert!(config.require_read_only_tools);
    }

    #[test]
    fn empty_config_keeps_all_tools_visible_and_callable() {
        let settings = ToolVisibilitySettings::from_yaml_str("{}").unwrap();
        let config = settings.into_visibility_config();

        assert!(config.tools.is_listed("read_note"));
        assert!(config.tools.is_enabled("delete_note"));
        assert!(!config.require_read_only_tools);
    }

    #[test]
    fn cli_overrides_merge_with_file_settings() {
        let yaml = r#"
tool_visibility:
  hidden:
    - full_health_analysis
  disabled:
    - delete_note
"#;

        let mut settings = ToolVisibilitySettings::from_yaml_str(yaml).unwrap();
        settings.merge_cli(ToolVisibilityOverrides {
            allowed: vec!["read_note".to_string()],
            hidden: vec!["query_frontmatter_sql".to_string()],
            disabled: vec!["write_note".to_string()],
            require_read_only: true,
        });

        let config = settings.into_visibility_config();

        assert!(config.tools.is_listed("read_note"));
        assert!(!config.tools.is_enabled("delete_note"));
        assert!(!config.tools.is_enabled("write_note"));
        assert!(!config.tools.is_listed("full_health_analysis"));
        assert!(!config.tools.is_listed("query_frontmatter_sql"));
        assert!(config.require_read_only_tools);
    }
}
