//! Stable contracts for compiled-in TurboVault plugins.
//!
//! This crate is intentionally smaller than TurboVault's internal API. A
//! plugin receives a curated [`VaultApi`] facade and an advisory [`HookBus`],
//! never a raw vault manager or MCP server.
//!
//! Plugins are compiled into the host behind default-off Cargo features. The
//! host mounts each plugin under [`PluginDescriptor::id`], so a local tool
//! called `list` in the `tasks` plugin is advertised as `tasks_list`.

mod error;
mod hooks;
mod provider;
mod vault;

pub use error::{PluginError, PluginErrorCode, PluginResult};
pub use hooks::{
    EventAttribution, HookBus, HookEvent, HookLifecycle, HookRecvError, HookSubscription,
    PublishError, VaultEventEnvelope, WriteProvenance,
};
pub use provider::{
    MCP_TOOL_NAME_MAX_LEN, Plugin, PluginContext, PluginDescriptor, PluginProvider,
    PluginRequestContext, namespaced_tool_name, validate_mcp_tool_name, validate_plugin_id,
};
pub use turbomcp_types::{Tool, ToolResult};
pub use vault::{
    NoteSnapshot, VaultApi, VaultDescriptor, VaultHost, WriteNoteRequest, WritePrecondition,
    WriteReceipt,
};
