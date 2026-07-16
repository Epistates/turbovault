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
}

impl ObsidianMcpServer {
    /// Create a vault-agnostic server and assemble its focused providers.
    pub fn new() -> Result<Self> {
        let core = CoreToolHandler::new()?;
        Self::from_core(core)
    }

    fn from_core(core: CoreToolHandler) -> Result<Self> {
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
        })
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
