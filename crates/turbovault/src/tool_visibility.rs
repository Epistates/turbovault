use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use turbomcp::VisibilityConfig;

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

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct TurboVaultConfigFile {
    tool_visibility: ToolVisibilitySettings,
}

impl ToolVisibilitySettings {
    /// Parse the `tool_visibility` section from a TurboVault YAML config.
    pub fn from_yaml_str(yaml: &str) -> anyhow::Result<Self> {
        let config: TurboVaultConfigFile =
            yaml_serde::from_str(yaml).context("invalid TurboVault YAML config")?;
        Ok(config.tool_visibility)
    }

    /// Load the `tool_visibility` section from a YAML config file.
    pub async fn from_yaml_file(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let yaml = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("failed to read tool visibility config {}", path.display()))?;
        Self::from_yaml_str(&yaml)
            .with_context(|| format!("failed to parse tool visibility config {}", path.display()))
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
