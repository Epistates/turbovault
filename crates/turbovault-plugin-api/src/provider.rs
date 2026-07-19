use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{HookBus, PluginError, PluginResult, Tool, ToolResult, VaultApi};

/// Maximum tool-name length recommended by MCP SEP-986.
pub const MCP_TOOL_NAME_MAX_LEN: usize = 64;

/// Stable identity and compatibility metadata for a compiled-in plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDescriptor {
    /// Namespace used for every MCP capability, for example `tasks`.
    pub id: String,
    /// Human-readable plugin name.
    pub name: String,
    /// Plugin version.
    pub version: String,
    /// Short human-readable purpose.
    pub description: String,
}

impl PluginDescriptor {
    /// Validate the descriptor before the host mounts any capabilities.
    pub fn validate(&self) -> PluginResult<()> {
        validate_plugin_id(&self.id)?;
        if self.name.trim().is_empty() {
            return Err(PluginError::invalid_input("plugin name must not be empty"));
        }
        if self.version.trim().is_empty() {
            return Err(PluginError::invalid_input(
                "plugin version must not be empty",
            ));
        }
        Ok(())
    }
}

/// Validate a plugin namespace.
///
/// IDs must start with a lowercase ASCII letter and then contain only
/// lowercase ASCII letters, digits, or underscores.
pub fn validate_plugin_id(id: &str) -> PluginResult<()> {
    let mut chars = id.chars();
    if id.len() > MCP_TOOL_NAME_MAX_LEN
        || !matches!(chars.next(), Some('a'..='z'))
        || !chars.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
    {
        return Err(PluginError::invalid_input(format!(
            "invalid plugin id {id:?}; expected at most {MCP_TOOL_NAME_MAX_LEN} ASCII characters matching [a-z][a-z0-9_]*"
        )));
    }
    Ok(())
}

/// Validate a complete tool name against MCP SEP-986.
///
/// TurboVault enforces the specification recommendation at registration time:
/// names contain 1–64 ASCII letters, digits, underscores, dashes, dots, or
/// forward slashes. See <https://modelcontextprotocol.io/seps/986-specify-format-for-tool-names>.
pub fn validate_mcp_tool_name(name: &str) -> PluginResult<()> {
    let length_is_valid = (1..=MCP_TOOL_NAME_MAX_LEN).contains(&name.len());
    let characters_are_valid = name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/'));
    if !length_is_valid || !characters_are_valid {
        return Err(PluginError::invalid_input(format!(
            "invalid MCP tool name {name:?}; expected 1–{MCP_TOOL_NAME_MAX_LEN} ASCII characters from [A-Za-z0-9_./-]"
        )));
    }
    Ok(())
}

/// Build and validate the public MCP name for a plugin-local tool.
pub fn namespaced_tool_name(plugin_id: &str, local_name: &str) -> PluginResult<String> {
    validate_plugin_id(plugin_id)?;
    validate_mcp_tool_name(local_name)?;
    let public_name = format!("{plugin_id}_{local_name}");
    validate_mcp_tool_name(&public_name)?;
    Ok(public_name)
}

/// Curated request data passed to plugin tool calls.
///
/// Authentication internals and raw transport/session handles intentionally do
/// not cross the plugin boundary.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginRequestContext {
    /// Host request identifier.
    pub request_id: String,
    /// Authenticated user identifier, when available.
    pub user_id: Option<String>,
    /// Stateful transport session identifier, when available.
    pub session_id: Option<String>,
    /// Client application identifier, when available.
    pub client_id: Option<String>,
    /// Serializable request metadata copied by the host.
    pub metadata: BTreeMap<String, Value>,
}

/// Capabilities provided to a plugin when it is constructed.
#[derive(Debug, Clone)]
pub struct PluginContext {
    /// Curated vault operations.
    pub vault: VaultApi,
    /// Bounded, advisory hook/event bus.
    pub hooks: HookBus,
}

/// Object-safe MCP tool provider implemented by a plugin.
#[async_trait]
pub trait PluginProvider: Send + Sync {
    /// Return local, unprefixed tool descriptors.
    fn tools(&self) -> Vec<Tool>;

    /// Call a local, unprefixed tool.
    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        context: PluginRequestContext,
    ) -> PluginResult<ToolResult>;
}

/// Factory contract for a compiled-in plugin.
pub trait Plugin: Send + Sync {
    /// Return plugin identity before construction.
    fn descriptor(&self) -> PluginDescriptor;

    /// Build the provider using host-curated capabilities.
    fn build(&self, context: PluginContext) -> PluginResult<Arc<dyn PluginProvider>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_ids_are_stable_mcp_namespaces() {
        for valid in ["tasks", "vault_events", "vector2"] {
            validate_plugin_id(valid).expect("valid namespace");
        }
        for invalid in ["", "Tasks", "2vector", "vault-events", "vault events"] {
            let error = validate_plugin_id(invalid).expect_err("invalid namespace");
            assert_eq!(error.code, crate::PluginErrorCode::InvalidInput);
        }
        assert!(validate_plugin_id(&"p".repeat(MCP_TOOL_NAME_MAX_LEN + 1)).is_err());
    }

    #[test]
    fn tool_names_enforce_mcp_sep_986() {
        for valid in [
            "a",
            "getUser",
            "user-profile/update",
            "DATA_EXPORT_v2",
            "admin.tools.list",
        ] {
            validate_mcp_tool_name(valid).expect("SEP-986 name");
        }
        validate_mcp_tool_name(&"a".repeat(MCP_TOOL_NAME_MAX_LEN)).expect("64 characters");

        for invalid in ["", "has space", "has,comma", "has:colon", "unicode_é"] {
            let error = validate_mcp_tool_name(invalid).expect_err("invalid SEP-986 name");
            assert_eq!(error.code, crate::PluginErrorCode::InvalidInput);
        }
        assert!(validate_mcp_tool_name(&"a".repeat(MCP_TOOL_NAME_MAX_LEN + 1)).is_err());
    }

    #[test]
    fn namespaced_tool_names_validate_the_final_public_name() {
        assert_eq!(
            namespaced_tool_name("vector_search", "query/v2").expect("namespaced name"),
            "vector_search_query/v2"
        );
        let oversized_plugin_id = "p".repeat(MCP_TOOL_NAME_MAX_LEN);
        assert!(namespaced_tool_name(&oversized_plugin_id, "x").is_err());
    }

    #[test]
    fn descriptors_require_user_facing_identity() {
        let descriptor = PluginDescriptor {
            id: "tasks".to_string(),
            name: " ".to_string(),
            version: "1.0.0".to_string(),
            description: String::new(),
        };
        assert!(descriptor.validate().is_err());

        let descriptor = PluginDescriptor {
            name: "Tasks".to_string(),
            version: String::new(),
            ..descriptor
        };
        assert!(descriptor.validate().is_err());
    }
}
