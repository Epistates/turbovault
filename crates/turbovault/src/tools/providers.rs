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
use turbovault_core::events::{VaultChange, VaultEventSink, WriteAttribution};
#[cfg(feature = "plugin-api")]
use turbovault_plugin_api::{
    EventAttribution, HookBus, HookEvent, Plugin, PluginContext, PluginError, PluginErrorCode,
    PluginIdentity, PluginProvider, PluginRequestContext, PluginStorage, ShutdownTrigger,
    ToolResult as PluginToolResult, VaultApi, WriteProvenance, namespaced_prompt_name,
    namespaced_resource_template, namespaced_resource_uri, namespaced_tool_name,
    validate_mcp_tool_name,
};

/// Wall-clock budget for a single plugin tool call.
///
/// A compiled-in plugin shares the host's runtime, so a call that never
/// returns would hold its request open indefinitely. The budget is generous —
/// it exists to convert a hang into a reportable failure, not to police
/// legitimately slow work, which belongs in a task the plugin owns.
#[cfg(feature = "plugin-api")]
const PLUGIN_CALL_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

/// Publishes host-observed vault changes onto the plugin hook bus.
///
/// This is the only adapter between TurboVault's internal change vocabulary
/// and the plugin-facing event contract, so the two can evolve separately.
#[cfg(feature = "plugin-api")]
struct HookBusSink {
    hooks: HookBus,
}

#[cfg(feature = "plugin-api")]
impl VaultEventSink for HookBusSink {
    fn publish(
        &self,
        vault: &str,
        change: VaultChange,
        content_hash: Option<String>,
        attribution: WriteAttribution,
    ) {
        let event = match change {
            VaultChange::Created { path } => HookEvent::FileCreated { path },
            VaultChange::Modified { path } => HookEvent::FileModified { path },
            VaultChange::Deleted { path } => HookEvent::FileDeleted { path },
            VaultChange::Renamed { from, to } => HookEvent::FileRenamed { from, to },
            VaultChange::ResyncRequired { reason } => HookEvent::ResyncRequired { reason },
        };
        let plugin_id = attribution.plugin_id.clone();
        let event_attribution = match attribution.source {
            Some(source) => {
                let mut provenance = WriteProvenance::new(source);
                provenance.correlation_id = attribution.correlation_id;
                provenance.note = attribution.note;
                EventAttribution::Attributed(provenance)
            }
            // Nothing known about the writer. Saying so is the point: a
            // consumer must not treat an unattributed change as its own.
            None => EventAttribution::ExternalOrUnknown,
        };
        // A closed bus (shutdown) or an absent subscriber is not a write
        // failure; the feed is advisory by contract.
        let _ = self
            .hooks
            .publish(vault, event, content_hash, plugin_id, event_attribution);
    }
}

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
    resources: Arc<Vec<Resource>>,
    resource_templates: Arc<Vec<ResourceTemplate>>,
    prompts: Arc<Vec<Prompt>>,
}

#[cfg(feature = "plugin-api")]
impl PluginProviderAdapter {
    fn new(
        descriptor: turbovault_plugin_api::PluginDescriptor,
        provider: Arc<dyn PluginProvider>,
    ) -> Result<Self> {
        let tools = provider.tools();
        let resources = provider.resources();
        let resource_templates = provider.resource_templates();
        let prompts = provider.prompts();
        // Local names are validated and de-duplicated here; the corresponding
        // public names are built and checked for cross-plugin collisions where
        // the plugin is mounted.
        let mut names = std::collections::HashSet::new();
        for tool in &tools {
            validate_mcp_tool_name(&tool.name).map_err(|error| {
                anyhow!(
                    "plugin {:?} has an invalid local tool name: {error}",
                    descriptor.id
                )
            })?;
            if !names.insert(tool.name.clone()) {
                return Err(anyhow!(
                    "plugin {:?} advertises tool {:?} more than once",
                    descriptor.id,
                    tool.name
                ));
            }
        }

        let mut uris = std::collections::HashSet::new();
        for resource in &resources {
            if !uris.insert(resource.uri.clone()) {
                return Err(anyhow!(
                    "plugin {:?} advertises resource {:?} more than once",
                    descriptor.id,
                    resource.uri
                ));
            }
        }

        let mut prompt_names = std::collections::HashSet::new();
        for prompt in &prompts {
            if !prompt_names.insert(prompt.name.clone()) {
                return Err(anyhow!(
                    "plugin {:?} advertises prompt {:?} more than once",
                    descriptor.id,
                    prompt.name
                ));
            }
        }

        Ok(Self {
            descriptor,
            provider,
            tools: Arc::new(tools),
            resources: Arc::new(resources),
            resource_templates: Arc::new(resource_templates),
            prompts: Arc::new(prompts),
        })
    }

    /// Republish every content URI inside this plugin's namespace.
    ///
    /// A plugin works entirely in local paths — it never spells its own
    /// namespace, just as it never spells its tool prefix — so the URIs it
    /// returns have to be lifted back into the public space the client asked
    /// against. Doing it here also means a plugin cannot serve content under a
    /// URI belonging to the core vault or to another plugin.
    fn namespace_contents(&self, mut result: ResourceResult) -> ResourceResult {
        let scheme = format!("{}://", self.descriptor.id);
        for contents in &mut result.contents {
            let uri = match contents {
                turbomcp_types::ResourceContents::Text(text) => &mut text.uri,
                turbomcp_types::ResourceContents::Blob(blob) => &mut blob.uri,
            };
            if !uri.starts_with(&scheme) {
                *uri = format!("{scheme}{uri}");
            }
        }
        result
    }
}

/// Run one plugin entry point under the host's failure budget.
///
/// A compiled-in plugin shares the process, so a panic or a hang in one is a
/// panic or a hang in the server. Neither is contained here — this is a
/// contract boundary, not a sandbox — but both are converted into a single
/// failed request instead of a poisoned or stalled one.
#[cfg(feature = "plugin-api")]
async fn guarded<T>(
    plugin_id: &str,
    subject: &str,
    call: impl std::future::Future<Output = turbovault_plugin_api::PluginResult<T>>,
) -> turbovault_plugin_api::PluginResult<T> {
    use futures::FutureExt;
    let call = std::panic::AssertUnwindSafe(call);
    match tokio::time::timeout(PLUGIN_CALL_BUDGET, call.catch_unwind()).await {
        Ok(Ok(result)) => result,
        Ok(Err(panic)) => {
            let detail = panic_message(&panic);
            log::error!("plugin {plugin_id:?} panicked handling {subject}: {detail}");
            Err(PluginError::internal(format!(
                "plugin {plugin_id:?} panicked while handling {subject}: {detail}"
            )))
        }
        Err(_elapsed) => {
            log::error!(
                "plugin {plugin_id:?} exceeded the {PLUGIN_CALL_BUDGET:?} budget handling {subject}"
            );
            Err(PluginError::timeout(format!(
                "plugin {plugin_id:?} did not complete {subject} within {PLUGIN_CALL_BUDGET:?}"
            )))
        }
    }
}

#[cfg(feature = "plugin-api")]
fn plugin_error(error: PluginError) -> McpError {
    match error.code {
        PluginErrorCode::InvalidInput
        | PluginErrorCode::NotFound
        | PluginErrorCode::Conflict
        | PluginErrorCode::PermissionDenied => McpError::invalid_request(error.message),
        // `_` covers codes added to the non-exhaustive contract after this
        // build: an unknown category is a server-side problem, which is the
        // safe default for a caller that cannot act on it.
        PluginErrorCode::Unavailable | PluginErrorCode::Internal | PluginErrorCode::Timeout | _ => {
            McpError::internal(error.message)
        }
    }
}

/// Map a plugin failure raised while reading a resource.
///
/// A missing resource is a resource error, not a bad request: a client that
/// asked for a URI the plugin no longer serves needs `-32004` to recognize it.
#[cfg(feature = "plugin-api")]
fn plugin_resource_error(uri: &str, error: PluginError) -> McpError {
    match error.code {
        PluginErrorCode::NotFound => McpError::resource_not_found(uri),
        _ => plugin_error(error),
    }
}

/// Map a plugin failure raised while rendering a prompt.
#[cfg(feature = "plugin-api")]
fn plugin_prompt_error(name: &str, error: PluginError) -> McpError {
    match error.code {
        PluginErrorCode::NotFound => McpError::prompt_not_found(name),
        _ => plugin_error(error),
    }
}

/// Best-effort human-readable text from a caught panic payload.
#[cfg(feature = "plugin-api")]
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "panic payload was not a string".to_string()
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
    ) -> impl std::future::Future<Output = McpResult<PluginToolResult>> + MaybeSend + 'a {
        async move {
            if !self.tools.iter().any(|tool| tool.name == name) {
                return Err(McpError::tool_not_found(name));
            }
            let context = plugin_request_context(ctx);
            guarded(
                &self.descriptor.id,
                &format!("tool {name:?}"),
                self.provider.call_tool(name, args, context),
            )
            .await
            .map_err(plugin_error)
        }
    }

    fn read_resource<'a>(
        &'a self,
        uri: &'a str,
        ctx: &'a RequestContext,
    ) -> impl std::future::Future<Output = McpResult<ResourceResult>> + MaybeSend + 'a {
        async move {
            // No enumeration gate: the host routes this plugin's whole scheme,
            // so a URI expanded from a template has to reach the plugin too.
            // What exists inside the namespace is the plugin's to decide, and
            // the default implementation answers not-found.
            let context = plugin_request_context(ctx);
            guarded(
                &self.descriptor.id,
                &format!("resource {uri:?}"),
                self.provider.read_resource(uri, context),
            )
            .await
            .map(|result| self.namespace_contents(result))
            .map_err(|error| plugin_resource_error(uri, error))
        }
    }

    fn get_prompt<'a>(
        &'a self,
        name: &'a str,
        args: Option<serde_json::Value>,
        ctx: &'a RequestContext,
    ) -> impl std::future::Future<Output = McpResult<PromptResult>> + MaybeSend + 'a {
        async move {
            if !self.prompts.iter().any(|prompt| prompt.name == name) {
                return Err(McpError::prompt_not_found(name));
            }
            let context = plugin_request_context(ctx);
            guarded(
                &self.descriptor.id,
                &format!("prompt {name:?}"),
                self.provider.get_prompt(name, args, context),
            )
            .await
            .map_err(|error| plugin_prompt_error(name, error))
        }
    }
}

/// Copy the curated subset of request data that crosses the plugin boundary.
#[cfg(feature = "plugin-api")]
fn plugin_request_context(ctx: &RequestContext) -> PluginRequestContext {
    PluginRequestContext::new(ctx.request_id.clone())
        .with_user_id(ctx.user_id.clone())
        .with_session_id(ctx.session_id.clone())
        .with_client_id(ctx.client_id.clone())
        .with_metadata(
            ctx.metadata
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        )
}

/// A plugin the server has mounted, kept as one record.
///
/// The descriptor and the provider are always used together — to route, to
/// start, to stop — and holding them in separate index-aligned vectors made
/// their correspondence something the code had to maintain rather than state.
#[cfg(feature = "plugin-api")]
struct MountedPlugin {
    descriptor: turbovault_plugin_api::PluginDescriptor,
    provider: Arc<dyn PluginProvider>,
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
    /// Mounted plugins, retained so the server can start and stop each
    /// plugin's background work and route what the composite does not.
    #[cfg(feature = "plugin-api")]
    plugins: Arc<Vec<MountedPlugin>>,
    /// Fires the shutdown signal handed to every plugin's background work.
    #[cfg(feature = "plugin-api")]
    plugin_shutdown: turbovault_plugin_api::ShutdownTrigger,
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
        let (hooks, shutdown, mounted_plugins) = {
            const DEFAULT_HOOK_CAPACITY: usize = 1_024;
            // URI schemes the vault itself publishes under. A plugin mounted on
            // one of these would capture every unlisted URI in it, since a
            // plugin's whole scheme routes to the plugin.
            let reserved_schemes = resources
                .iter()
                .filter_map(|resource| resource.uri.split_once("://"))
                .map(|(scheme, _)| scheme.to_string())
                .collect::<std::collections::HashSet<_>>();
            let hooks = HookBus::new(DEFAULT_HOOK_CAPACITY);
            let shutdown = ShutdownTrigger::new();
            let mut mounted_plugins: Vec<MountedPlugin> = Vec::new();

            for plugin in plugins {
                let descriptor = plugin.descriptor();
                descriptor
                    .validate()
                    .map_err(|error| anyhow!("invalid plugin descriptor: {error}"))?;
                let prefix = descriptor.id.clone();
                if reserved_schemes.contains(&prefix) {
                    return Err(anyhow!(
                        "plugin {prefix:?} would take over the {prefix}:// resource scheme, which TurboVault already publishes under"
                    ));
                }

                // Capabilities are bound to THIS plugin's facade. Two plugins
                // never share a `VaultApi`, so a config path granted to one is
                // not reachable from the other, and a malformed declaration
                // stops the server here rather than surfacing as a confusing
                // denial at the first call.
                let identity = PluginIdentity::new(prefix.clone(), plugin.capabilities()).map_err(
                    |error| anyhow!("plugin {prefix:?} declared invalid capabilities: {error}"),
                )?;
                let vault = VaultApi::new(
                    super::plugin_host::vault_host(core.clone(), &prefix),
                    identity,
                );
                // Storage is namespaced by construction rather than declared:
                // the plugin id is baked into the store, so there is no
                // argument a plugin could pass to reach another's data.
                let storage =
                    PluginStorage::new(super::plugin_storage::plugin_store(core.clone(), &prefix));

                let provider = plugin
                    .build(PluginContext::new(
                        vault,
                        hooks.clone(),
                        storage,
                        shutdown.signal(),
                    ))
                    .map_err(|error| anyhow!("plugin {prefix:?} failed to build: {error}"))?;
                let adapter =
                    PluginProviderAdapter::new(descriptor.clone(), Arc::clone(&provider))?;

                for mut tool in adapter.list_tools() {
                    let local_name = tool.name.clone();
                    let public_name =
                        namespaced_tool_name(&prefix, &local_name).map_err(|error| {
                            anyhow!(
                                "plugin {prefix:?} tool {local_name:?} has an invalid public name: {error}"
                            )
                        })?;
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

                // Public URI and route are the same string: the composite
                // splits `<prefix>://<local>` back apart when it routes a read,
                // exactly as it splits `<prefix>_<local>` for a tool call.
                for mut resource in adapter.list_resources() {
                    let local_uri = resource.uri.clone();
                    let public_uri =
                        namespaced_resource_uri(&prefix, &local_uri).map_err(|error| {
                            anyhow!(
                                "plugin {prefix:?} resource {local_uri:?} has an invalid public URI: {error}"
                            )
                        })?;
                    resource.uri = public_uri.clone();
                    if resource_routes
                        .insert(public_uri.clone(), public_uri.clone())
                        .is_some()
                    {
                        return Err(anyhow!(
                            "plugin resource {public_uri:?} collides with an existing public resource"
                        ));
                    }
                    resources.push(resource);
                }

                // Templates need no route entry: reads fall through to the
                // owning plugin by scheme, which is the point of a template.
                for mut template in adapter.list_resource_templates() {
                    let local_template = template.uri_template.clone();
                    template.uri_template = namespaced_resource_template(&prefix, &local_template)
                        .map_err(|error| {
                        anyhow!(
                            "plugin {prefix:?} resource template {local_template:?} is invalid: {error}"
                        )
                    })?;
                    resource_templates.push(template);
                }

                for mut prompt in adapter.list_prompts() {
                    let local_name = prompt.name.clone();
                    let public_name =
                        namespaced_prompt_name(&prefix, &local_name).map_err(|error| {
                            anyhow!(
                                "plugin {prefix:?} prompt {local_name:?} has an invalid public name: {error}"
                            )
                        })?;
                    prompt.name = public_name.clone();
                    if prompt_routes
                        .insert(public_name.clone(), public_name.clone())
                        .is_some()
                    {
                        return Err(anyhow!(
                            "plugin prompt {public_name:?} collides with an existing public prompt"
                        ));
                    }
                    prompts.push(prompt);
                }

                composite = composite
                    .try_mount(adapter, &prefix)
                    .map_err(|error| anyhow!("could not mount plugin {prefix:?}: {error}"))?;
                mounted_plugins.push(MountedPlugin {
                    descriptor,
                    provider,
                });
            }

            // Every write path reports through the core handler; this is what
            // turns those reports into plugin-visible events. Installed before
            // the handler is used so no mutation can slip past unobserved.
            core.set_event_sink(Arc::new(HookBusSink {
                hooks: hooks.clone(),
            }));

            (hooks, shutdown, mounted_plugins)
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
            #[cfg(feature = "plugin-api")]
            plugins: Arc::new(mounted_plugins),
            #[cfg(feature = "plugin-api")]
            plugin_shutdown: shutdown,
        })
    }

    /// Return the shared bounded hook bus.
    #[cfg(feature = "plugin-api")]
    pub fn hook_bus(&self) -> HookBus {
        self.hooks.clone()
    }

    /// Return the mounted plugin whose URI scheme owns `uri`, if any.
    #[cfg(feature = "plugin-api")]
    fn plugin_owning_scheme(&self, uri: &str) -> Option<&MountedPlugin> {
        let (scheme, _) = uri.split_once("://")?;
        self.plugins
            .iter()
            .find(|plugin| plugin.descriptor.id == scheme)
    }

    /// Return the mounted plugin whose namespace owns the public name `name`.
    ///
    /// Longest prefix wins, so a plugin named `tasks` does not capture a name
    /// belonging to one named `tasks_beta`.
    #[cfg(feature = "plugin-api")]
    fn plugin_owning_name<'a>(&self, name: &'a str) -> Option<(&MountedPlugin, &'a str)> {
        self.plugins
            .iter()
            .filter_map(|plugin| {
                name.strip_prefix(&format!("{}_", plugin.descriptor.id))
                    .map(|local| (plugin, local))
            })
            .max_by_key(|(plugin, _)| plugin.descriptor.id.len())
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

    /// Graceful shutdown. Idempotent, best-effort, and safe to call from a
    /// signal handler as well as after the transport's `serve()` returns.
    ///
    /// Closes the plugin hook bus so subscribers observe
    /// [`turbovault_plugin_api::HookRecvError::Closed`] instead of waiting
    /// forever, then abandons fanout worktrees registered by this process.
    pub async fn shutdown(&self) {
        #[cfg(feature = "plugin-api")]
        {
            // Signal first, then await: a worker needs to be told to stop
            // before `shutdown` can meaningfully wait for it to finish.
            self.plugin_shutdown.shutdown();
            // Plugins before the bus: a plugin draining work may still want to
            // write, and it can only observe the bus closing after it has
            // stopped.
            for plugin in self.plugins.iter() {
                plugin.provider.shutdown().await;
            }
            self.hooks.close();
        }
        self.core.shutdown_fanouts_best_effort().await;
    }

    /// Start every mounted plugin's background work.
    ///
    /// Call once, on the server's runtime, after vault registration and before
    /// serving. Separate from construction because `Plugin::build` is
    /// synchronous and may run outside a runtime, so a plugin cannot spawn
    /// there.
    ///
    /// A plugin that fails to start aborts startup: it would otherwise serve
    /// tools backed by state nothing is maintaining.
    pub async fn start_plugins(&self) -> Result<()> {
        #[cfg(feature = "plugin-api")]
        for plugin in self.plugins.iter() {
            plugin.provider.start().await.map_err(|error| {
                anyhow!("plugin {:?} failed to start: {error}", plugin.descriptor.id)
            })?;
        }
        Ok(())
    }

    #[doc(hidden)]
    pub async fn get_active_vault_manager_test(
        &self,
    ) -> McpResult<Arc<turbovault_vault::VaultManager>> {
        self.core.get_active_vault_manager_test().await
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
        let info = ServerInfo::new("obsidian-vault", env!("CARGO_PKG_VERSION"));
        #[cfg(feature = "plugin-api")]
        if !self.plugins.is_empty() {
            let namespaces = self
                .plugins
                .iter()
                .map(|plugin| plugin.descriptor.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return info.with_description(format!(
                "TurboVault with optional plugin namespaces: {namespaces}. Plugin tools use <plugin_id>_<local_tool>; use tools/list for the exact enabled catalog."
            ));
        }
        info
    }

    /// Advertise only what TurboVault actually implements.
    ///
    /// TurboMCP derives `listChanged: true` from a non-empty listing, but the
    /// catalog here is fixed when the server is assembled — plugins mount once
    /// and nothing mutates the lists afterwards — and TurboVault emits no
    /// `notifications/*/list_changed`. Repeating the derived claim would tell a
    /// client it will be informed of changes it will never hear about; a client
    /// that trusts that promise stops re-listing. A URI space that genuinely
    /// varies is published as a resource template instead, which is stable.
    fn server_capabilities(&self) -> ServerCapabilities {
        let mut capabilities = self.composite.server_capabilities();
        if let Some(tools) = capabilities.tools.as_mut() {
            tools.list_changed = Some(false);
        }
        if let Some(resources) = capabilities.resources.as_mut() {
            resources.list_changed = Some(false);
        }
        if let Some(prompts) = capabilities.prompts.as_mut() {
            prompts.list_changed = Some(false);
        }
        // Completion is only meaningful for a prompt argument or a template
        // expression, so advertise it exactly when something can be completed.
        // The vault's own catalog has neither.
        #[cfg(feature = "plugin-api")]
        if !self.prompts.is_empty() || !self.resource_templates.is_empty() {
            capabilities.completions = Some(turbomcp_types::CompletionCapabilities {});
        }
        capabilities
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
            let routed = match self.resource_routes.get(uri) {
                Some(routed) => routed.as_str(),
                // Not enumerated. A plugin owns its whole URI scheme, so a URI
                // expanded from one of its templates routes to it even though
                // no listing named it — otherwise a template could be
                // advertised and never read. Confined to mounted plugin
                // namespaces so internal provider prefixes stay private.
                #[cfg(feature = "plugin-api")]
                None => self
                    .plugin_owning_scheme(uri)
                    .map(|_| uri)
                    .ok_or_else(|| McpError::resource_not_found(uri))?,
                // Without plugins nothing owns a scheme, so nothing resolves
                // beyond the enumerated catalog.
                #[cfg(not(feature = "plugin-api"))]
                None => return Err(McpError::resource_not_found(uri)),
            };
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

    /// Suggest values for a prompt argument or resource-template expression.
    ///
    /// Routed here rather than through the composite, which does not implement
    /// completion. The reference names a public identifier, so this strips the
    /// namespace before asking the plugin, exactly as the composite does for
    /// the primitives it does route.
    fn complete<'a>(
        &'a self,
        params: serde_json::Value,
        ctx: &'a RequestContext,
    ) -> impl std::future::Future<Output = McpResult<serde_json::Value>> + MaybeSend + 'a {
        async move {
            #[cfg(feature = "plugin-api")]
            {
                self.complete_through_plugin(params, ctx).await
            }
            #[cfg(not(feature = "plugin-api"))]
            {
                let _ = (params, ctx);
                Err(McpError::capability_not_supported("completion/complete"))
            }
        }
    }
}

#[cfg(feature = "plugin-api")]
impl ObsidianMcpServer {
    async fn complete_through_plugin(
        &self,
        params: serde_json::Value,
        ctx: &RequestContext,
    ) -> McpResult<serde_json::Value> {
        use turbomcp_protocol::types::completion::{
            CompleteRequestParams, CompleteResult, CompletionData, CompletionReference,
            MAX_COMPLETION_VALUES,
        };
        use turbovault_plugin_api::CompletionTarget;

        let request: CompleteRequestParams = serde_json::from_value(params).map_err(|error| {
            McpError::invalid_params(format!("invalid completion request: {error}"))
        })?;

        // Resolve the public reference to the plugin that owns it, and to the
        // local identifier that plugin knows the target by.
        let (plugin, target) = match &request.reference {
            CompletionReference::Prompt(prompt) => {
                let (plugin, local) = self
                    .plugin_owning_name(&prompt.name)
                    .ok_or_else(|| McpError::prompt_not_found(&prompt.name))?;
                (plugin, CompletionTarget::Prompt(local.to_string()))
            }
            CompletionReference::ResourceTemplate(resource) => {
                let plugin = self
                    .plugin_owning_scheme(&resource.uri)
                    .ok_or_else(|| McpError::resource_not_found(&resource.uri))?;
                let local = resource
                    .uri
                    .split_once("://")
                    .map(|(_, local)| local.to_string())
                    .unwrap_or_default();
                (plugin, CompletionTarget::ResourceTemplate(local))
            }
        };

        let resolved = request
            .context
            .and_then(|context| context.arguments)
            .map(|arguments| arguments.into_iter().collect())
            .unwrap_or_default();
        let completion_request = turbovault_plugin_api::CompletionRequest::new(
            target,
            request.argument.name,
            request.argument.value,
        )
        .with_resolved(resolved);

        let mut completion = guarded(
            &plugin.descriptor.id,
            "completion",
            plugin
                .provider
                .complete(completion_request, plugin_request_context(ctx)),
        )
        .await
        .map_err(plugin_error)?;

        // MCP caps a single completion response. Enforced here rather than
        // trusted to each plugin: a plugin that returns everything it knows is
        // behaving reasonably, and silently emitting an over-long list would
        // put an invalid message on the wire.
        let has_more = completion.values.len() > MAX_COMPLETION_VALUES;
        if has_more {
            completion.values.truncate(MAX_COMPLETION_VALUES);
        }
        let data = CompletionData {
            values: completion.values,
            total: completion.total,
            has_more: has_more.then_some(true).or(completion.has_more),
        };
        serde_json::to_value(CompleteResult::new(data))
            .map_err(|error| McpError::internal(format!("could not encode completion: {error}")))
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
            turbovault_plugin_api::PluginDescriptor::new(
                "contract",
                "Contract Test Plugin",
                "1.0.0",
                "Exercises the stable plugin boundary",
            )
        }

        fn capabilities(&self) -> turbovault_plugin_api::PluginCapabilities {
            turbovault_plugin_api::PluginCapabilities::none()
                .with_config_read(".obsidian/plugins/example/data.json")
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
    struct OversizedNamespacePlugin;

    #[cfg(feature = "plugin-api")]
    impl Plugin for OversizedNamespacePlugin {
        fn descriptor(&self) -> turbovault_plugin_api::PluginDescriptor {
            turbovault_plugin_api::PluginDescriptor::new(
                "p".repeat(turbovault_plugin_api::MCP_TOOL_NAME_MAX_LEN),
                "Oversized Namespace",
                "1.0.0",
                "Exercises final public-name validation",
            )
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
                .write_note(
                    turbovault_plugin_api::WriteNoteRequest::new(
                        &vault.name,
                        path,
                        content,
                        precondition,
                    )
                    .with_commit_message(format!("contract plugin creates {path}"))
                    .with_provenance(
                        turbovault_plugin_api::WriteProvenance::new("contract-test")
                            .with_correlation_id(context.request_id),
                    ),
                )
                .await?;
            let snapshot = self.vault.read_note(&vault.name, path).await?;
            let notes = self.vault.list_notes(&vault.name).await?;
            let config = match arguments
                .get("config_path")
                .and_then(serde_json::Value::as_str)
            {
                Some(config_path) => self
                    .vault
                    .read_config(&vault.name, config_path)
                    .await
                    .map(|bytes| bytes.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))?,
                None => None,
            };
            ToolResult::json(&serde_json::json!({
                "vault": vault,
                "receipt": receipt,
                "snapshot": snapshot,
                "notes": notes,
                "config": config,
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
        let description = server
            .server_info()
            .description
            .expect("enabled plugins should be described during MCP initialization");
        assert!(description.contains("contract"));
        assert!(description.contains("<plugin_id>_<local_tool>"));

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

    #[cfg(feature = "plugin-api")]
    #[test]
    fn final_namespaced_tool_names_must_conform_to_mcp_sep_986() {
        let error = ObsidianMcpServer::new_with_plugins(vec![Arc::new(OversizedNamespacePlugin)])
            .err()
            .expect("oversized public name must fail");
        assert!(
            error.to_string().contains("invalid public name"),
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
        // The fixture is a deliberate tripwire on the public tool surface, so a
        // real change to a tool signature has to be re-blessed on purpose:
        // `UPDATE_TOOL_CATALOG=1 cargo test -p turbovault --lib`. Without this
        // the only way to accept an intended change was to hand-edit ~5k lines
        // of generated JSON.
        if actual != expected && std::env::var_os("UPDATE_TOOL_CATALOG").is_some() {
            let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src/tools/providers/tool_catalog.json");
            std::fs::write(&fixture, format!("{actual}\n")).expect("rewrite tool catalog fixture");
            panic!(
                "tool catalog fixture rewritten at {fixture:?}; re-run without UPDATE_TOOL_CATALOG"
            );
        }
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
        assert!(
            server.server_info().description.is_none(),
            "default server must not inject plugin guidance"
        );
        #[cfg(feature = "plugin-api")]
        for tool in server.list_tools() {
            validate_mcp_tool_name(&tool.name).expect("core tool name must conform to MCP SEP-986");
        }
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

    /// Advertised capabilities are promises. TurboVault's catalog is fixed at
    /// assembly and it sends no `list_changed` notifications, so claiming
    /// otherwise would tell a client to stop re-listing and wait for a message
    /// that never arrives.
    #[test]
    fn composed_capabilities_promise_only_what_is_implemented() {
        let server = ObsidianMcpServer::new().expect("provider composition");
        let capabilities = server.server_capabilities();

        assert_eq!(
            capabilities.tools.expect("tools capability").list_changed,
            Some(false)
        );
        let resources = capabilities.resources.expect("resources capability");
        assert_eq!(resources.list_changed, Some(false));
        assert_eq!(resources.subscribe, None);
        assert!(capabilities.prompts.is_none());
        assert!(
            capabilities.completions.is_none(),
            "the vault's own catalog has no completable argument"
        );
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
