use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    Completion, CompletionRequest, HookBus, PluginError, PluginResult, PluginStorage, Prompt,
    PromptResult, Resource, ResourceResult, ResourceTemplate, ShutdownSignal, Tool, ToolResult,
    VaultApi,
};

/// Maximum tool-name length recommended by MCP SEP-986.
pub const MCP_TOOL_NAME_MAX_LEN: usize = 64;

/// Maximum length of a plugin-local resource path.
const PLUGIN_RESOURCE_PATH_MAX_LEN: usize = 512;

/// Stable identity and compatibility metadata for a compiled-in plugin.
///
/// Construct with [`Self::new`]; the type is `#[non_exhaustive]` so identity
/// metadata can grow without breaking existing plugins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
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
    /// Construct a descriptor.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        version: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            version: version.into(),
            description: description.into(),
        }
    }

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

/// What a plugin declares it needs beyond the note APIs.
///
/// Capabilities are declared by the plugin crate rather than listed centrally
/// in this crate: the plugin owns the integration and therefore knows exactly
/// what it reads, and a central list would duplicate that knowledge and drift
/// from it. The default grants nothing — a plugin that declares nothing gets
/// only the note surface every plugin gets.
///
/// The host validates a declaration at mount time, so an over-broad or
/// malformed request stops the server from starting instead of surfacing as a
/// confusing runtime denial.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PluginCapabilities {
    /// Exact vault-relative application-config files this plugin may read,
    /// for example `.obsidian/plugins/obsidian-tasks-plugin/data.json`.
    ///
    /// Exact paths, not directory prefixes: a reviewer can see precisely what
    /// a plugin reads, and granting a whole plugin folder would hand over
    /// files nobody enumerated. A directory-scoped form can be added later if
    /// a real plugin needs genuinely dynamic file names.
    pub config_reads: Vec<String>,
}

impl PluginCapabilities {
    /// A declaration granting nothing beyond the note APIs.
    pub fn none() -> Self {
        Self::default()
    }

    /// Declare one exact readable config path.
    pub fn with_config_read(mut self, path: impl Into<String>) -> Self {
        self.config_reads.push(path.into());
        self
    }

    /// Validate every declared capability.
    ///
    /// Declarations are normalized on the way in, so a plugin cannot declare
    /// `.obsidian/../.git/config` and have it accepted, and cannot declare a
    /// path in one spelling then request it in another.
    pub fn validate(&self) -> PluginResult<()> {
        for declared in &self.config_reads {
            let normalized = crate::vault::normalize_config_path(declared)?;
            if &normalized != declared {
                return Err(PluginError::invalid_input(format!(
                    "config_reads entry {declared:?} must be declared in normalized form ({normalized:?})"
                )));
            }
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

/// Build and validate the public MCP name for a plugin-local prompt.
///
/// MCP has no SEP-986 equivalent for prompt names, so TurboVault applies the
/// tool-name rule to them as well: one identifier convention across a server's
/// primitives is easier to reason about than two, and nothing the rule allows
/// is unsafe in a prompt name.
pub fn namespaced_prompt_name(plugin_id: &str, local_name: &str) -> PluginResult<String> {
    namespaced_tool_name(plugin_id, local_name)
}

/// Build and validate the public URI for a plugin-local resource.
///
/// A plugin names a local path and the host publishes it under the plugin's own
/// URI scheme, so `index/stats` in the `tasks` plugin becomes
/// `tasks://index/stats`. This mirrors tool namespacing: a plugin never spells
/// its own namespace, and cannot publish under one it does not own.
pub fn namespaced_resource_uri(plugin_id: &str, local_path: &str) -> PluginResult<String> {
    validate_plugin_id(plugin_id)?;
    validate_plugin_resource_path(local_path)?;
    Ok(format!("{plugin_id}://{local_path}"))
}

/// Build and validate the public URI template for a plugin-local resource
/// template.
///
/// Namespaced exactly like a concrete resource URI, with the RFC 6570 brace
/// structure checked as well so a typo in an expression name is caught when the
/// plugin is mounted rather than when a client tries to expand it.
pub fn namespaced_resource_template(plugin_id: &str, local_template: &str) -> PluginResult<String> {
    validate_plugin_id(plugin_id)?;
    validate_plugin_resource_path(local_template)?;
    turbomcp_types::validate_uri_template(local_template).map_err(|error| {
        PluginError::invalid_input(format!(
            "invalid plugin resource template {local_template:?}: {error}"
        ))
    })?;
    Ok(format!("{plugin_id}://{local_template}"))
}

/// Validate the local path half of a plugin resource URI.
///
/// A second `://` is rejected because the host routes a resource read by
/// matching the scheme, and an embedded scheme would make the split ambiguous.
fn validate_plugin_resource_path(path: &str) -> PluginResult<()> {
    let length_is_valid = (1..=PLUGIN_RESOURCE_PATH_MAX_LEN).contains(&path.len());
    let characters_are_valid = path.bytes().all(|byte| byte.is_ascii_graphic());
    if !length_is_valid || !characters_are_valid || path.contains("://") {
        return Err(PluginError::invalid_input(format!(
            "invalid plugin resource path {path:?}; expected 1–{PLUGIN_RESOURCE_PATH_MAX_LEN} printable ASCII characters without whitespace or an embedded scheme"
        )));
    }
    Ok(())
}

/// Curated request data passed to plugin tool calls.
///
/// Authentication internals and raw transport/session handles intentionally do
/// not cross the plugin boundary.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
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

impl PluginRequestContext {
    /// Construct a request context for `request_id`.
    pub fn new(request_id: impl Into<String>) -> Self {
        Self {
            request_id: request_id.into(),
            ..Self::default()
        }
    }

    /// Attach the authenticated user identifier.
    pub fn with_user_id(mut self, user_id: Option<String>) -> Self {
        self.user_id = user_id;
        self
    }

    /// Attach the transport session identifier.
    pub fn with_session_id(mut self, session_id: Option<String>) -> Self {
        self.session_id = session_id;
        self
    }

    /// Attach the client application identifier.
    pub fn with_client_id(mut self, client_id: Option<String>) -> Self {
        self.client_id = client_id;
        self
    }

    /// Attach copied request metadata.
    pub fn with_metadata(mut self, metadata: BTreeMap<String, Value>) -> Self {
        self.metadata = metadata;
        self
    }
}

/// Capabilities provided to a plugin when it is constructed.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PluginContext {
    /// Curated vault operations, bound to this plugin's declaration.
    pub vault: VaultApi,
    /// Bounded, advisory hook/event bus.
    pub hooks: HookBus,
    /// Durable, plugin-private, per-vault key/value storage.
    pub storage: PluginStorage,
    /// Cooperative shutdown signal for background work.
    pub shutdown: ShutdownSignal,
}

impl PluginContext {
    /// Assemble a context. Called by the host.
    pub fn new(
        vault: VaultApi,
        hooks: HookBus,
        storage: PluginStorage,
        shutdown: ShutdownSignal,
    ) -> Self {
        Self {
            vault,
            hooks,
            storage,
            shutdown,
        }
    }
}

/// Object-safe MCP tool provider implemented by a plugin.
///
/// New methods are added with defaults, so a plugin never has to change to
/// keep compiling against a later version of this contract.
#[async_trait]
pub trait PluginProvider: Send + Sync {
    /// Return local, unprefixed tool descriptors.
    fn tools(&self) -> Vec<Tool>;

    /// Call a local, unprefixed tool.
    ///
    /// The host applies a wall-clock budget to this call and isolates panics,
    /// so a hung or panicking plugin degrades to one failed tool call rather
    /// than a stalled or downed server. Long work belongs in a task the plugin
    /// owns and tears down in [`Self::shutdown`].
    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        context: PluginRequestContext,
    ) -> PluginResult<ToolResult>;

    /// Return local, unprefixed resource descriptors.
    ///
    /// Resources are the primitive a client attaches as context on the user's
    /// initiative, which is different from a tool the model decides to call.
    /// State a plugin maintains — index statistics, a rendered summary — is
    /// usually better offered here than as a tool nothing thinks to invoke.
    ///
    /// Give each `uri` a plugin-local path; the host publishes it under this
    /// plugin's scheme. Default: none.
    ///
    /// List only what is worth enumerating. A URI space too large or too
    /// volatile to enumerate belongs in [`Self::resource_templates`] instead.
    fn resources(&self) -> Vec<Resource> {
        Vec::new()
    }

    /// Return local, unprefixed URI templates for resources this plugin serves
    /// but does not enumerate.
    ///
    /// This is how a plugin exposes a URI space that is large, open-ended, or
    /// changes as the vault does — `note/{path}` rather than one entry per
    /// note. TurboVault never sends `list_changed` notifications, so a resource
    /// that comes and goes has to be reachable through a template; an
    /// enumerated list a client cached at startup would go stale silently.
    ///
    /// The host routes this plugin's entire scheme, so a read of any URI under
    /// it reaches [`Self::read_resource`] whether or not it was listed.
    /// Default: none.
    fn resource_templates(&self) -> Vec<ResourceTemplate> {
        Vec::new()
    }

    /// Read one local, unprefixed resource path.
    ///
    /// Called for any URI in this plugin's scheme, including ones matching a
    /// template rather than an enumerated entry, so validate what you receive.
    ///
    /// The host rewrites the URI on every returned content into this plugin's
    /// namespace, so what a client reads back always matches what it asked for
    /// and a plugin cannot publish content under a URI it does not own.
    ///
    /// Bounded and panic-isolated exactly like [`Self::call_tool`]. Default:
    /// not found.
    async fn read_resource(
        &self,
        uri: &str,
        context: PluginRequestContext,
    ) -> PluginResult<ResourceResult> {
        let _ = context;
        Err(PluginError::not_found(format!("unknown resource {uri:?}")))
    }

    /// Return local, unprefixed prompt descriptors.
    ///
    /// Prompts are user-invoked, so this is how a plugin offers a workflow a
    /// person starts deliberately rather than one the model reaches for.
    /// Names are namespaced like tools. Default: none.
    fn prompts(&self) -> Vec<Prompt> {
        Vec::new()
    }

    /// Render one local, unprefixed prompt.
    ///
    /// Bounded and panic-isolated exactly like [`Self::call_tool`]. Default:
    /// not found.
    async fn get_prompt(
        &self,
        name: &str,
        arguments: Option<Value>,
        context: PluginRequestContext,
    ) -> PluginResult<PromptResult> {
        let _ = (arguments, context);
        Err(PluginError::not_found(format!("unknown prompt {name:?}")))
    }

    /// Suggest values for an argument a user is part-way through typing.
    ///
    /// This is what makes a resource template usable rather than merely
    /// declared: a client can offer the paths that exist instead of asking a
    /// person to guess one. It applies to prompt arguments equally.
    ///
    /// Called on every keystroke in some clients, so answer from state the
    /// plugin already holds. The host applies the same wall-clock budget as a
    /// tool call and truncates the result to the protocol's limit, so returning
    /// everything known is safe.
    ///
    /// Default: no suggestions, which is a valid answer for any argument.
    async fn complete(
        &self,
        request: CompletionRequest,
        context: PluginRequestContext,
    ) -> PluginResult<Completion> {
        let _ = (request, context);
        Ok(Completion::none())
    }

    /// Begin background work.
    ///
    /// Called once, on the host's runtime, after every plugin is mounted and
    /// before the transport starts serving. This is where a worker belongs:
    /// [`Plugin::build`] is synchronous and may run outside a runtime, so
    /// spawning there is not sound.
    ///
    /// Return promptly. Spawn the loop and keep its handle; the
    /// [`ShutdownSignal`] from [`PluginContext`] tells it when to stop, and
    /// [`Self::shutdown`] is where to await the handle.
    ///
    /// A failure here stops the server from starting: a plugin that cannot
    /// begin its work would otherwise serve tools backed by state it is not
    /// maintaining. Default: nothing.
    async fn start(&self) -> PluginResult<()> {
        Ok(())
    }

    /// Release resources during graceful host shutdown.
    ///
    /// Called once, after the host stops accepting requests and after the
    /// shutdown signal has fired, so a worker has already been told to stop.
    /// Await your task handles here. Default: nothing.
    async fn shutdown(&self) {}
}

/// Factory contract for a compiled-in plugin.
pub trait Plugin: Send + Sync {
    /// Return plugin identity before construction.
    fn descriptor(&self) -> PluginDescriptor;

    /// Declare what this plugin needs beyond the note APIs.
    ///
    /// Default: nothing. The host validates the declaration and binds it to
    /// this plugin's [`crate::VaultApi`] before construction, so a capability
    /// is scoped to the plugin that asked for it.
    fn capabilities(&self) -> PluginCapabilities {
        PluginCapabilities::none()
    }

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
    fn resource_uris_are_published_under_the_plugin_scheme() {
        assert_eq!(
            namespaced_resource_uri("tasks", "index/stats.json").expect("namespaced uri"),
            "tasks://index/stats.json"
        );
        // A second scheme would make the host's prefix split ambiguous.
        for invalid in ["", "has space", "other://stats", "unicode_é"] {
            let error = namespaced_resource_uri("tasks", invalid).expect_err("invalid local path");
            assert_eq!(error.code, crate::PluginErrorCode::InvalidInput);
        }
        assert!(namespaced_resource_uri("Tasks", "stats").is_err());
        assert!(
            namespaced_resource_uri("tasks", &"p".repeat(PLUGIN_RESOURCE_PATH_MAX_LEN + 1))
                .is_err()
        );
    }

    #[test]
    fn resource_templates_are_checked_for_brace_structure() {
        assert_eq!(
            namespaced_resource_template("tasks", "note/{path}").expect("namespaced template"),
            "tasks://note/{path}"
        );
        // Caught at mount time rather than when a client tries to expand it.
        for invalid in ["note/{path", "note/{a{b}}", "note/}path{"] {
            let error =
                namespaced_resource_template("tasks", invalid).expect_err("malformed template");
            assert_eq!(error.code, crate::PluginErrorCode::InvalidInput);
        }
    }

    #[test]
    fn prompt_names_share_the_tool_naming_rule() {
        assert_eq!(
            namespaced_prompt_name("tasks", "review_overdue").expect("namespaced prompt"),
            "tasks_review_overdue"
        );
        assert!(namespaced_prompt_name("tasks", "has space").is_err());
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
