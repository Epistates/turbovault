use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use turbomcp::{McpHandler, VisibilityConfig, VisibilityLayer};

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
    /// Disable ALL tools carrying any of these tags (e.g. `["delete", "admin"]`).
    pub disabled_tags: Vec<String>,
    /// Hide tools with these tags from `tools/list` (still callable by name).
    ///
    /// NOTE: tag-based hiding requires `VisibilityLayer::hide_tags()` which is not
    /// yet available in turbomcp. This field is parsed and stored for forward
    /// compatibility; a warning is logged at startup if it is non-empty.
    pub hidden_tags: Vec<String>,
    /// Hide tools that are not annotated read-only by TurboMCP.
    pub require_read_only: bool,
}

/// CLI/env overrides merged with file-based tool visibility settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolVisibilityOverrides {
    pub allowed: Vec<String>,
    pub hidden: Vec<String>,
    pub disabled: Vec<String>,
    pub disabled_tags: Vec<String>,
    pub hidden_tags: Vec<String>,
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
        extend_clean(&mut self.disabled_tags, overrides.disabled_tags);
        extend_clean(&mut self.hidden_tags, overrides.hidden_tags);
        self.require_read_only |= overrides.require_read_only;
    }

    /// Apply all visibility rules (name-based and tag-based) to a `VisibilityLayer`.
    ///
    /// Prefer this over `into_visibility_config` in application code; it handles
    /// both the `VisibilityConfig` (name rules) and `disable_tags` (tag rules).
    pub fn apply_to_layer<H: McpHandler>(self, layer: VisibilityLayer<H>) -> VisibilityLayer<H> {
        if !self.hidden_tags.is_empty() {
            log::warn!(
                "tool_visibility.hidden_tags is set but tag-based hiding is not yet supported \
                 by turbomcp (hide_tags() API pending). The setting will be ignored. \
                 Use disabled_tags to fully block tagged tools instead."
            );
        }
        let disabled_tags = self.disabled_tags.clone();
        let layer = layer.with_visibility_config(self.into_visibility_config());
        if !disabled_tags.is_empty() {
            layer.disable_tags(disabled_tags)
        } else {
            layer
        }
    }

    /// Convert name-based settings to TurboMCP's `VisibilityConfig`.
    ///
    /// Tag rules are not represented in `VisibilityConfig`; use `apply_to_layer`
    /// to apply the full set of rules including tag-based ones.
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
            || !self.disabled_tags.is_empty()
            || !self.hidden_tags.is_empty()
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
            disabled_tags: vec![],
            hidden_tags: vec![],
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

    #[test]
    fn disabled_tags_round_trips_through_yaml() {
        let yaml = r#"
tool_visibility:
  disabled_tags:
    - delete
    - admin
"#;
        let settings = ToolVisibilitySettings::from_yaml_str(yaml).unwrap();
        assert_eq!(settings.disabled_tags, vec!["delete", "admin"]);
        assert!(settings.hidden_tags.is_empty());
    }

    #[test]
    fn hidden_tags_round_trips_through_yaml() {
        let yaml = r#"
tool_visibility:
  hidden_tags:
    - semantic
    - export
"#;
        let settings = ToolVisibilitySettings::from_yaml_str(yaml).unwrap();
        assert_eq!(settings.hidden_tags, vec!["semantic", "export"]);
        assert!(settings.disabled_tags.is_empty());
    }

    #[test]
    fn cli_overrides_merge_disabled_tags() {
        let yaml = r#"
tool_visibility:
  disabled_tags:
    - delete
"#;
        let mut settings = ToolVisibilitySettings::from_yaml_str(yaml).unwrap();
        settings.merge_cli(ToolVisibilityOverrides {
            disabled_tags: vec!["admin".to_string(), "delete".to_string()], // "delete" is duplicate
            ..Default::default()
        });
        // "delete" must not be added twice
        assert_eq!(settings.disabled_tags, vec!["delete", "admin"]);
    }

    #[test]
    fn has_rules_true_when_disabled_tags_set() {
        let settings = ToolVisibilitySettings {
            disabled_tags: vec!["delete".to_string()],
            ..Default::default()
        };
        assert!(settings.has_rules());
    }

    #[test]
    fn has_rules_true_when_hidden_tags_set() {
        let settings = ToolVisibilitySettings {
            hidden_tags: vec!["semantic".to_string()],
            ..Default::default()
        };
        assert!(settings.has_rules());
    }

    #[test]
    fn has_rules_false_when_all_empty() {
        assert!(!ToolVisibilitySettings::default().has_rules());
    }

    #[test]
    fn tag_deduplication_in_merge() {
        let mut settings = ToolVisibilitySettings {
            disabled_tags: vec!["write".to_string()],
            ..Default::default()
        };
        // Merging the same tag twice should result in it appearing once.
        settings.merge_cli(ToolVisibilityOverrides {
            disabled_tags: vec!["write".to_string(), "write".to_string()],
            ..Default::default()
        });
        assert_eq!(settings.disabled_tags, vec!["write"]);
    }
}
