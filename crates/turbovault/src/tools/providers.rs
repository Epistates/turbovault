//! Stable, flat MCP facade over focused tool providers.
//!
//! TurboMCP's `CompositeHandler` intentionally namespaces mounted handlers.
//! Turbo Vault predates that convention, so this module keeps the public tool
//! and resource identifiers flat while using namespaced routes internally.

mod analysis;
mod audit;
mod batch;
mod content;
mod context;
mod discovery;
mod export;
mod fanout;
mod files;
mod graph;
mod metadata;
mod relationship;
mod templates;
mod vault;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use turbomcp::ServerCapabilities;
use turbomcp::prelude::*;
use turbomcp_core::marker::MaybeSend;
use turbomcp_server::CompositeHandler;
use turbomcp_types::ResourceTemplate;
use turbovault_core::prelude::MultiVaultManager;

#[cfg(feature = "plugin-api")]
use turbovault_plugin_api::{
    HookBus, Plugin, PluginContext, PluginError, PluginErrorCode, PluginProvider,
    PluginRequestContext, ToolResult as PluginToolResult, VaultApi,
};

use self::analysis::AnalysisProvider;
use self::audit::AuditProvider;
use self::batch::BatchProvider;
use self::content::ContentProvider;
use self::context::ContextProvider;
use self::discovery::DiscoveryProvider;
use self::export::ExportProvider;
use self::fanout::FanoutProvider;
use self::files::FileProvider;
use self::graph::GraphProvider;
use self::metadata::MetadataProvider;
use self::relationship::RelationshipProvider;
use self::templates::TemplateProvider;
use self::vault::VaultProvider;
use super::CoreToolHandler;

#[cfg(feature = "plugin-api")]
#[derive(Clone)]
struct PluginProviderAdapter {
    descriptor: turbovault_plugin_api::PluginDescriptor,
    provider: Arc<dyn PluginProvider>,
    tools: Arc<Vec<Tool>>,
}

#[cfg(feature = "plugin-api")]
impl PluginProviderAdapter {
    fn new(
        descriptor: turbovault_plugin_api::PluginDescriptor,
        provider: Arc<dyn PluginProvider>,
    ) -> Result<Self> {
        let tools = provider.tools();
        let mut names = std::collections::HashSet::new();
        for tool in &tools {
            let valid_name = !tool.name.is_empty()
                && tool.name.chars().all(|character| {
                    character.is_ascii_alphanumeric() || character == '_' || character == '-'
                });
            if !valid_name {
                return Err(anyhow!(
                    "plugin {:?} has invalid local tool name {:?}",
                    descriptor.id,
                    tool.name
                ));
            }
            if !names.insert(tool.name.clone()) {
                return Err(anyhow!(
                    "plugin {:?} advertises tool {:?} more than once",
                    descriptor.id,
                    tool.name
                ));
            }
        }
        Ok(Self {
            descriptor,
            provider,
            tools: Arc::new(tools),
        })
    }
}

#[cfg(feature = "plugin-api")]
fn plugin_error(error: PluginError) -> McpError {
    match error.code {
        PluginErrorCode::InvalidInput | PluginErrorCode::NotFound | PluginErrorCode::Conflict => {
            McpError::invalid_request(error.message)
        }
        PluginErrorCode::Unavailable | PluginErrorCode::Internal => {
            McpError::internal(error.message)
        }
    }
}

#[cfg(feature = "plugin-api")]
#[allow(clippy::manual_async_fn)]
impl McpHandler for PluginProviderAdapter {
    fn server_info(&self) -> ServerInfo {
        ServerInfo::new(&self.descriptor.name, &self.descriptor.version)
    }

    fn list_tools(&self) -> Vec<Tool> {
        self.tools.as_ref().clone()
    }

    fn list_resources(&self) -> Vec<Resource> {
        Vec::new()
    }

    fn list_prompts(&self) -> Vec<Prompt> {
        Vec::new()
    }

    fn call_tool<'a>(
        &'a self,
        name: &'a str,
        args: serde_json::Value,
        ctx: &'a RequestContext,
    ) -> impl std::future::Future<Output = McpResult<PluginToolResult>> + MaybeSend + 'a {
        async move {
            if !self.tools.iter().any(|tool| tool.name == name) {
                return Err(McpError::tool_not_found(name));
            }
            let context = PluginRequestContext {
                request_id: ctx.request_id.clone(),
                user_id: ctx.user_id.clone(),
                session_id: ctx.session_id.clone(),
                client_id: ctx.client_id.clone(),
                metadata: ctx
                    .metadata
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            };
            self.provider
                .call_tool(name, args, context)
                .await
                .map_err(plugin_error)
        }
    }

    fn read_resource<'a>(
        &'a self,
        uri: &'a str,
        _ctx: &'a RequestContext,
    ) -> impl std::future::Future<Output = McpResult<ResourceResult>> + MaybeSend + 'a {
        async move { Err(McpError::resource_not_found(uri)) }
    }

    fn get_prompt<'a>(
        &'a self,
        name: &'a str,
        _args: Option<serde_json::Value>,
        _ctx: &'a RequestContext,
    ) -> impl std::future::Future<Output = McpResult<PromptResult>> + MaybeSend + 'a {
        async move { Err(McpError::prompt_not_found(name)) }
    }
}

/// Obsidian MCP server with stable public names and focused internal providers.
#[derive(Clone)]
pub struct ObsidianMcpServer {
    core: CoreToolHandler,
    composite: CompositeHandler,
    tools: Arc<Vec<Tool>>,
    resources: Arc<Vec<Resource>>,
    resource_templates: Arc<Vec<ResourceTemplate>>,
    prompts: Arc<Vec<Prompt>>,
    tool_routes: Arc<HashMap<String, String>>,
    resource_routes: Arc<HashMap<String, String>>,
    prompt_routes: Arc<HashMap<String, String>>,
    #[cfg(feature = "plugin-api")]
    hooks: HookBus,
}

impl ObsidianMcpServer {
    /// Create a vault-agnostic server and assemble its focused providers.
    pub fn new() -> Result<Self> {
        let core = CoreToolHandler::new()?;
        Self::from_core(core)
    }

    fn from_core(core: CoreToolHandler) -> Result<Self> {
        #[cfg(feature = "plugin-api")]
        {
            Self::assemble(core, Vec::new())
        }
        #[cfg(not(feature = "plugin-api"))]
        {
            Self::assemble(core)
        }
    }

    /// Create a server with compiled-in plugin factories.
    ///
    /// Each plugin is mounted under its validated descriptor ID. This API is
    /// available only with the default-off `plugin-api` Cargo feature.
    #[cfg(feature = "plugin-api")]
    pub fn new_with_plugins(plugins: Vec<Arc<dyn Plugin>>) -> Result<Self> {
        let core = CoreToolHandler::new()?;
        Self::assemble(core, plugins)
    }

    fn assemble(
        core: CoreToolHandler,
        #[cfg(feature = "plugin-api")] plugins: Vec<Arc<dyn Plugin>>,
    ) -> Result<Self> {
        let mut composite = CompositeHandler::new("obsidian-vault", env!("CARGO_PKG_VERSION"));
        let mut tools = Vec::new();
        let mut resources = Vec::new();
        let mut resource_templates = Vec::new();
        let mut prompts = Vec::new();
        let mut tool_routes = HashMap::new();
        let mut resource_routes = HashMap::new();
        let mut prompt_routes = HashMap::new();

        macro_rules! mount {
            ($provider:expr, $prefix:literal) => {{
                let provider = $provider;

                for tool in provider.list_tools() {
                    let public_name = tool.name.clone();
                    let routed = format!("{}_{}", $prefix, public_name);
                    if tool_routes.insert(public_name.clone(), routed).is_some() {
                        return Err(anyhow!(
                            "tool {public_name:?} is owned by multiple providers"
                        ));
                    }
                    tools.push(tool);
                }

                for resource in provider.list_resources() {
                    let public_uri = resource.uri.clone();
                    let routed = format!("{}://{}", $prefix, public_uri);
                    if resource_routes.insert(public_uri.clone(), routed).is_some() {
                        return Err(anyhow!(
                            "resource {public_uri:?} is owned by multiple providers"
                        ));
                    }
                    resources.push(resource);
                }

                for template in provider.list_resource_templates() {
                    resource_templates.push(template);
                }

                for prompt in provider.list_prompts() {
                    let public_name = prompt.name.clone();
                    let routed = format!("{}_{}", $prefix, public_name);
                    if prompt_routes.insert(public_name.clone(), routed).is_some() {
                        return Err(anyhow!(
                            "prompt {public_name:?} is owned by multiple providers"
                        ));
                    }
                    prompts.push(prompt);
                }

                composite = composite
                    .try_mount(provider, $prefix)
                    .map_err(|error| anyhow!(error))?;
            }};
        }

        // Mount order intentionally matches the historical monolithic handler.
        // This preserves tools/list ordering as well as every descriptor field.
        mount!(ContextProvider::new(core.clone()), "context");
        mount!(FileProvider::new(core.clone()), "files");
        mount!(GraphProvider::new(core.clone()), "graph");
        mount!(DiscoveryProvider::new(core.clone()), "discovery");
        mount!(TemplateProvider::new(core.clone()), "templates");
        mount!(VaultProvider::new(core.clone()), "vault");
        mount!(BatchProvider::new(core.clone()), "batch");
        mount!(FanoutProvider::new(core.clone()), "fanout");
        mount!(ExportProvider::new(core.clone()), "export");
        mount!(MetadataProvider::new(core.clone()), "metadata");
        mount!(RelationshipProvider::new(core.clone()), "relationship");
        mount!(ContentProvider::new(core.clone()), "content");
        mount!(AnalysisProvider::new(core.clone()), "analysis");
        mount!(AuditProvider::new(core.clone()), "audit");

        #[cfg(feature = "plugin-api")]
        let hooks = {
            const DEFAULT_HOOK_CAPACITY: usize = 1_024;
            let hooks = HookBus::new(DEFAULT_HOOK_CAPACITY);
            let vault = VaultApi::new(super::plugin_host::vault_host(core.clone(), hooks.clone()));

            for plugin in plugins {
                let descriptor = plugin.descriptor();
                descriptor
                    .validate()
                    .map_err(|error| anyhow!("invalid plugin descriptor: {error}"))?;
                let prefix = descriptor.id.clone();
                let provider = plugin
                    .build(PluginContext {
                        vault: vault.clone(),
                        hooks: hooks.clone(),
                    })
                    .map_err(|error| anyhow!("plugin {prefix:?} failed to build: {error}"))?;
                let adapter = PluginProviderAdapter::new(descriptor, provider)?;

                for mut tool in adapter.list_tools() {
                    let local_name = tool.name.clone();
                    let public_name = format!("{prefix}_{local_name}");
                    tool.name = public_name.clone();
                    if tool_routes
                        .insert(public_name.clone(), public_name.clone())
                        .is_some()
                    {
                        return Err(anyhow!(
                            "plugin tool {public_name:?} collides with an existing public tool"
                        ));
                    }
                    tools.push(tool);
                }

                composite = composite
                    .try_mount(adapter, &prefix)
                    .map_err(|error| anyhow!("could not mount plugin {prefix:?}: {error}"))?;
            }
            hooks
        };

        Ok(Self {
            core,
            composite,
            tools: Arc::new(tools),
            resources: Arc::new(resources),
            resource_templates: Arc::new(resource_templates),
            prompts: Arc::new(prompts),
            tool_routes: Arc::new(tool_routes),
            resource_routes: Arc::new(resource_routes),
            prompt_routes: Arc::new(prompt_routes),
            #[cfg(feature = "plugin-api")]
            hooks,
        })
    }

    /// Return the shared bounded hook bus.
    #[cfg(feature = "plugin-api")]
    pub fn hook_bus(&self) -> HookBus {
        self.hooks.clone()
    }

    /// Initialize the persistent cache after server creation.
    pub async fn init_cache(&self) -> Result<()> {
        self.core.init_cache().await
    }

    /// Get the shared multi-vault manager.
    pub fn multi_vault(&self) -> Arc<MultiVaultManager> {
        self.core.multi_vault()
    }

    /// Initialize every registered vault (used by the CLI `--init` path).
    pub async fn initialize_registered_vaults(&self) -> Result<()> {
        self.core.initialize_registered_vaults().await
    }

    /// Warn about worktrees left behind by interrupted fanout sessions.
    pub async fn log_orphan_fanouts_warnings(&self) {
        self.core.log_orphan_fanouts_warnings().await;
    }

    /// Best-effort cleanup for fanout worktrees during graceful shutdown.
    pub async fn shutdown_fanouts_best_effort(&self) {
        self.core.shutdown_fanouts_best_effort().await;
    }

    #[doc(hidden)]
    pub async fn get_active_write_tools_test(&self) -> McpResult<turbovault_tools::WriteTools> {
        self.core.get_active_write_tools_test().await
    }

    #[doc(hidden)]
    pub async fn get_active_vault_manager_test(
        &self,
    ) -> McpResult<Arc<turbovault_vault::VaultManager>> {
        self.core.get_active_vault_manager_test().await
    }

    #[doc(hidden)]
    pub async fn get_reindex_queue_test(
        &self,
        vault_name: &str,
    ) -> Option<Arc<turbovault_tools::ReindexQueue>> {
        self.core.get_reindex_queue_test(vault_name).await
    }

    #[doc(hidden)]
    pub async fn flush_reindex_for_active_vault_test(&self) -> McpResult<()> {
        self.core.flush_reindex_for_active_vault_test().await
    }

    #[doc(hidden)]
    pub async fn resolve_commit_message_test(
        &self,
        message: Option<String>,
        fallback: String,
    ) -> McpResult<String> {
        self.core
            .resolve_commit_message_test(message, fallback)
            .await
    }

    #[doc(hidden)]
    pub async fn spawn_ref_listener_with_interval_test(
        &self,
        vault_name: &str,
        interval: std::time::Duration,
    ) {
        self.core
            .spawn_ref_listener_with_interval_test(vault_name, interval)
            .await;
    }

    #[doc(hidden)]
    pub async fn has_git_drainer_test(&self, vault_name: &str) -> bool {
        self.core.has_git_drainer_test(vault_name).await
    }

    #[doc(hidden)]
    pub async fn has_git_ref_listener_test(&self, vault_name: &str) -> bool {
        self.core.has_git_ref_listener_test(vault_name).await
    }

    #[doc(hidden)]
    pub async fn has_git_locks_test(&self, vault_name: &str) -> bool {
        self.core.has_git_locks_test(vault_name).await
    }

    #[doc(hidden)]
    pub async fn remove_vault_test(&self, vault_name: &str) -> McpResult<serde_json::Value> {
        self.core.remove_vault_test(vault_name).await
    }

    #[doc(hidden)]
    pub async fn get_or_init_git_locks_test(
        &self,
        vault_name: &str,
    ) -> Arc<turbovault_tools::CommitLocks> {
        self.core.get_or_init_git_locks_test(vault_name).await
    }

    #[doc(hidden)]
    pub async fn register_active_fanout_test(
        &self,
        base_vault: &str,
        fanout_id: &str,
        info: turbovault_tools::FanoutInfo,
        fanout_vault_name: &str,
    ) {
        self.core
            .register_active_fanout_test(base_vault, fanout_id, info, fanout_vault_name)
            .await;
    }

    #[doc(hidden)]
    pub async fn clear_active_fanout_test(&self, base_vault: &str) {
        self.core.clear_active_fanout_test(base_vault).await;
    }
}

impl Default for ObsidianMcpServer {
    fn default() -> Self {
        Self::new().expect("Failed to create default ObsidianMcpServer")
    }
}

#[allow(clippy::manual_async_fn)]
impl McpHandler for ObsidianMcpServer {
    fn server_info(&self) -> ServerInfo {
        ServerInfo::new("obsidian-vault", env!("CARGO_PKG_VERSION"))
    }

    fn server_capabilities(&self) -> ServerCapabilities {
        self.composite.server_capabilities()
    }

    fn list_tools(&self) -> Vec<Tool> {
        self.tools.as_ref().clone()
    }

    fn list_resources(&self) -> Vec<Resource> {
        self.resources.as_ref().clone()
    }

    fn list_resource_templates(&self) -> Vec<ResourceTemplate> {
        self.resource_templates.as_ref().clone()
    }

    fn list_prompts(&self) -> Vec<Prompt> {
        self.prompts.as_ref().clone()
    }

    fn call_tool<'a>(
        &'a self,
        name: &'a str,
        args: serde_json::Value,
        ctx: &'a RequestContext,
    ) -> impl std::future::Future<Output = McpResult<ToolResult>> + MaybeSend + 'a {
        async move {
            let routed = self
                .tool_routes
                .get(name)
                .ok_or_else(|| McpError::tool_not_found(name))?;
            self.composite.call_tool(routed, args, ctx).await
        }
    }

    fn read_resource<'a>(
        &'a self,
        uri: &'a str,
        ctx: &'a RequestContext,
    ) -> impl std::future::Future<Output = McpResult<ResourceResult>> + MaybeSend + 'a {
        async move {
            let routed = self
                .resource_routes
                .get(uri)
                .ok_or_else(|| McpError::resource_not_found(uri))?;
            self.composite.read_resource(routed, ctx).await
        }
    }

    fn get_prompt<'a>(
        &'a self,
        name: &'a str,
        args: Option<serde_json::Value>,
        ctx: &'a RequestContext,
    ) -> impl std::future::Future<Output = McpResult<PromptResult>> + MaybeSend + 'a {
        async move {
            let routed = self
                .prompt_routes
                .get(name)
                .ok_or_else(|| McpError::prompt_not_found(name))?;
            self.composite.get_prompt(routed, args, ctx).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn structured(result: ToolResult) -> serde_json::Value {
        result
            .structured_content
            .expect("tool result should contain structured content")
    }

    #[cfg(feature = "plugin-api")]
    struct ContractPlugin;

    #[cfg(feature = "plugin-api")]
    impl Plugin for ContractPlugin {
        fn descriptor(&self) -> turbovault_plugin_api::PluginDescriptor {
            turbovault_plugin_api::PluginDescriptor {
                id: "contract".to_string(),
                name: "Contract Test Plugin".to_string(),
                version: "1.0.0".to_string(),
                description: "Exercises the stable plugin boundary".to_string(),
            }
        }

        fn build(
            &self,
            context: PluginContext,
        ) -> turbovault_plugin_api::PluginResult<Arc<dyn PluginProvider>> {
            Ok(Arc::new(ContractProvider {
                vault: context.vault,
            }))
        }
    }

    #[cfg(feature = "plugin-api")]
    struct ContractProvider {
        vault: VaultApi,
    }

    #[cfg(feature = "plugin-api")]
    #[async_trait::async_trait]
    impl PluginProvider for ContractProvider {
        fn tools(&self) -> Vec<Tool> {
            vec![Tool::new(
                "round_trip",
                "Create and read a note through VaultApi",
            )]
        }

        async fn call_tool(
            &self,
            name: &str,
            arguments: serde_json::Value,
            context: PluginRequestContext,
        ) -> turbovault_plugin_api::PluginResult<ToolResult> {
            if name != "round_trip" {
                return Err(PluginError::not_found(format!("unknown tool {name:?}")));
            }
            let path = arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| PluginError::invalid_input("path is required"))?;
            let content = arguments
                .get("content")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| PluginError::invalid_input("content is required"))?;
            let precondition = arguments
                .get("expected_version")
                .and_then(serde_json::Value::as_str)
                .map(|version| turbovault_plugin_api::WritePrecondition::Match(version.to_string()))
                .unwrap_or(turbovault_plugin_api::WritePrecondition::CreateOnly);
            let vault = self.vault.active_vault().await?;
            let receipt = self
                .vault
                .write_note(turbovault_plugin_api::WriteNoteRequest {
                    path: path.to_string(),
                    content: content.to_string(),
                    precondition,
                    commit_message: Some(format!("contract plugin creates {path}")),
                    provenance: Some(turbovault_plugin_api::WriteProvenance {
                        source: "contract-test".to_string(),
                        correlation_id: Some(context.request_id),
                        note: None,
                    }),
                })
                .await?;
            let snapshot = self.vault.read_note(path).await?;
            let notes = self.vault.list_notes().await?;
            ToolResult::json(&serde_json::json!({
                "vault": vault,
                "receipt": receipt,
                "snapshot": snapshot,
                "notes": notes,
            }))
            .map_err(|error| PluginError::internal(error.to_string()))
        }
    }

    #[cfg(feature = "plugin-api")]
    #[tokio::test]
    async fn plugin_tools_are_namespaced_and_use_the_curated_vault_api() {
        use turbovault_core::VaultConfig;
        use turbovault_plugin_api::{EventAttribution, HookEvent};

        let temp = tempfile::TempDir::new().expect("temp vault");
        let server = ObsidianMcpServer::new_with_plugins(vec![Arc::new(ContractPlugin)])
            .expect("plugin composition");
        let config = VaultConfig::builder("plugin-test", temp.path())
            .build()
            .expect("vault config");
        server
            .multi_vault()
            .add_vault(config)
            .await
            .expect("register vault");
        server
            .multi_vault()
            .set_active_vault("plugin-test")
            .await
            .expect("select vault");

        let advertised = server.list_tools();
        assert_eq!(advertised.len(), 75);
        assert!(
            advertised
                .iter()
                .any(|tool| tool.name == "contract_round_trip")
        );
        assert!(!advertised.iter().any(|tool| tool.name == "round_trip"));

        let mut events = server.hook_bus().subscribe().expect("hook subscription");
        let ctx = RequestContext::with_id("plugin-request");
        let result = server
            .call_tool(
                "contract_round_trip",
                serde_json::json!({"path": "plugin.md", "content": "# Plugin"}),
                &ctx,
            )
            .await
            .expect("namespaced plugin call");
        let result = structured(result);
        let initial_version = result["receipt"]["version"]
            .as_str()
            .expect("initial version")
            .to_string();
        assert_eq!(result["vault"]["name"], "plugin-test");
        assert_eq!(result["snapshot"]["content"], "# Plugin");
        assert_eq!(result["receipt"]["version"], result["snapshot"]["version"]);
        assert_eq!(result["notes"], serde_json::json!(["plugin.md"]));

        let event = events.recv().await.expect("plugin write event");
        assert_eq!(event.vault, "plugin-test");
        assert_eq!(
            event.event,
            HookEvent::FileCreated {
                path: "plugin.md".to_string()
            }
        );
        assert!(matches!(
            event.attribution,
            EventAttribution::Attributed(turbovault_plugin_api::WriteProvenance {
                source,
                correlation_id: Some(id),
                ..
            }) if source == "contract-test" && id == "plugin-request"
        ));

        let error = server
            .call_tool("round_trip", serde_json::json!({}), &ctx)
            .await
            .expect_err("unprefixed plugin tool must not be public");
        assert_eq!(error.jsonrpc_code(), -32001);

        let conflict = server
            .call_tool(
                "contract_round_trip",
                serde_json::json!({"path": "plugin.md", "content": "# Overwrite"}),
                &ctx,
            )
            .await
            .expect_err("create-only write must refuse overwrites");
        assert_eq!(conflict.jsonrpc_code(), -32600);

        let updated = server
            .call_tool(
                "contract_round_trip",
                serde_json::json!({
                    "path": "plugin.md",
                    "content": "# Updated",
                    "expected_version": initial_version.clone(),
                }),
                &ctx,
            )
            .await
            .expect("matching CAS write");
        let updated = structured(updated);
        assert_eq!(updated["snapshot"]["content"], "# Updated");
        assert_ne!(updated["receipt"]["version"], result["receipt"]["version"]);
        let modified = events.recv().await.expect("plugin update event");
        assert!(matches!(
            modified.event,
            HookEvent::FileModified { ref path } if path == "plugin.md"
        ));

        let stale = server
            .call_tool(
                "contract_round_trip",
                serde_json::json!({
                    "path": "plugin.md",
                    "content": "# Stale",
                    "expected_version": initial_version,
                }),
                &ctx,
            )
            .await
            .expect_err("stale CAS write must fail");
        assert_eq!(stale.jsonrpc_code(), -32600);
    }

    #[cfg(feature = "plugin-api")]
    #[test]
    fn duplicate_plugin_namespaces_are_rejected_before_serving() {
        let error = ObsidianMcpServer::new_with_plugins(vec![
            Arc::new(ContractPlugin),
            Arc::new(ContractPlugin),
        ])
        .err()
        .expect("duplicate namespace must fail");
        assert!(
            error.to_string().contains("contract_round_trip")
                || error.to_string().contains("duplicate prefix"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn tools_list_is_byte_for_byte_equivalent_to_the_pre_split_catalog() {
        let server = ObsidianMcpServer::new().expect("provider composition");
        let expected = include_str!("providers/tool_catalog.json").trim_end();
        let actual =
            serde_json::to_string_pretty(&server.list_tools()).expect("serialize tool catalog");

        assert_eq!(server.list_tools().len(), 74, "public tool count changed");
        if actual != expected {
            let actual_bytes = actual.as_bytes();
            let expected_bytes = expected.as_bytes();
            let offset = actual_bytes
                .iter()
                .zip(expected_bytes)
                .position(|(left, right)| left != right)
                .unwrap_or_else(|| actual_bytes.len().min(expected_bytes.len()));
            let start = offset.saturating_sub(120);
            let actual_end = (offset + 240).min(actual_bytes.len());
            let expected_end = (offset + 240).min(expected_bytes.len());
            panic!(
                "public tool catalog changed at byte {offset}\nexpected near mismatch:\n{}\nactual near mismatch:\n{}",
                String::from_utf8_lossy(&expected_bytes[start..expected_end]),
                String::from_utf8_lossy(&actual_bytes[start..actual_end]),
            );
        }
        assert_eq!(server.tool_routes.len(), 74);
    }

    #[tokio::test]
    async fn resources_keep_their_public_uris_and_route_through_content_provider() {
        let server = ObsidianMcpServer::new().expect("provider composition");
        let resources = server.list_resources();
        let ctx = RequestContext::new();

        assert_eq!(
            resources
                .iter()
                .map(|resource| resource.uri.as_str())
                .collect::<Vec<_>>(),
            [
                "obsidian://syntax/complete-guide",
                "obsidian://syntax/quick-ref",
                "obsidian://examples/sample-note",
            ]
        );

        for resource in resources {
            let result = server
                .read_resource(&resource.uri, &ctx)
                .await
                .expect("composed resource");
            assert!(
                !result.contents.is_empty(),
                "empty resource: {}",
                resource.uri
            );
        }
    }

    #[test]
    fn composed_capabilities_match_the_public_catalog() {
        let server = ObsidianMcpServer::new().expect("provider composition");
        let capabilities = server.server_capabilities();

        assert_eq!(
            capabilities.tools.expect("tools capability").list_changed,
            Some(true)
        );
        let resources = capabilities.resources.expect("resources capability");
        assert_eq!(resources.list_changed, Some(true));
        assert_eq!(resources.subscribe, None);
        assert!(capabilities.prompts.is_none());
    }

    #[tokio::test]
    async fn every_advertised_tool_routes_past_the_flat_facade() {
        let server = ObsidianMcpServer::new().expect("provider composition");
        let ctx = RequestContext::new();

        for tool in server.list_tools() {
            if let Err(error) = server
                .call_tool(&tool.name, serde_json::json!({}), &ctx)
                .await
            {
                assert_ne!(
                    error.jsonrpc_code(),
                    -32001,
                    "advertised tool was not routable: {}",
                    tool.name
                );
            }
        }
    }

    #[tokio::test]
    async fn internal_composite_names_are_not_part_of_the_public_api() {
        let server = ObsidianMcpServer::new().expect("provider composition");
        let ctx = RequestContext::new();

        let tool_error = server
            .call_tool("files_read_note", serde_json::json!({"path": "x.md"}), &ctx)
            .await
            .expect_err("internal tool route must stay private");
        assert_eq!(tool_error.jsonrpc_code(), -32001);

        let resource_error = server
            .read_resource("content://obsidian://syntax/quick-ref", &ctx)
            .await
            .expect_err("internal resource route must stay private");
        assert_eq!(resource_error.jsonrpc_code(), -32004);
    }

    #[tokio::test]
    async fn providers_share_vault_file_graph_search_and_audit_state() {
        let temp = tempfile::TempDir::new().expect("temp vault");
        let server = ObsidianMcpServer::new().expect("provider composition");
        let ctx = RequestContext::new();

        server
            .call_tool(
                "add_vault",
                serde_json::json!({
                    "name": "shared",
                    "path": temp.path().to_string_lossy(),
                }),
                &ctx,
            )
            .await
            .expect("vault provider should register the vault");

        server
            .call_tool(
                "write_note",
                serde_json::json!({
                    "path": "target.md",
                    "content": "# Target",
                }),
                &ctx,
            )
            .await
            .expect("file provider should create the link target");

        server
            .call_tool(
                "write_note",
                serde_json::json!({
                    "path": "shared.md",
                    "content": "# SharedProviderMarker\n\n[[target]]",
                }),
                &ctx,
            )
            .await
            .expect("file provider should use the registered vault");

        let search = structured(
            server
                .call_tool(
                    "search",
                    serde_json::json!({"query": "SharedProviderMarker"}),
                    &ctx,
                )
                .await
                .expect("discovery provider should see the written note"),
        );
        assert_eq!(search["count"], 1);

        let links = structured(
            server
                .call_tool(
                    "get_forward_links",
                    serde_json::json!({"path": "shared.md"}),
                    &ctx,
                )
                .await
                .expect("graph provider should see the written note"),
        );
        let forward_links = links["data"].as_array().expect("forward-link array");
        assert_eq!(forward_links.len(), 1);
        assert!(
            forward_links[0]
                .as_str()
                .expect("forward-link path")
                .ends_with("target.md")
        );

        let context = structured(
            server
                .call_tool("get_vault_context", serde_json::json!({}), &ctx)
                .await
                .expect("context provider should see the registered vault"),
        );
        assert_eq!(context["vault"], "shared");

        let audit = structured(
            server
                .call_tool("audit_stats", serde_json::json!({}), &ctx)
                .await
                .expect("audit provider should share runtime vault state"),
        );
        assert!(
            audit["data"]["total_operations"]
                .as_u64()
                .expect("audit operation count")
                >= 2
        );
    }
}
