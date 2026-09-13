//! Stable contracts for compiled-in TurboVault plugins.
//!
//! This crate is intentionally smaller than TurboVault's internal API. A
//! plugin receives a curated [`VaultApi`] facade and an advisory [`HookBus`],
//! never a raw vault manager or MCP server.
//!
//! Plugins are compiled into the host behind default-off Cargo features. The
//! host mounts each plugin under [`PluginDescriptor::id`] and namespaces all
//! three MCP primitives, so in the `tasks` plugin a local tool `list` is
//! advertised as `tasks_list`, a prompt `review` as `tasks_review`, and a
//! resource `index/stats.json` as `tasks://index/stats.json`. A plugin names
//! only the local half.
//!
//! # Trust model
//!
//! Plugins are statically linked into the TurboVault process. **This API is a
//! contract, not a sandbox.** A compiled-in plugin runs with the host's full
//! privileges: it can open files, spawn threads, and reach the network
//! regardless of what it declares here. The boundary exists to keep plugins
//! decoupled from TurboVault's internals, to make what a plugin touches
//! reviewable, and to stop *accidental* over-reach — not to contain hostile
//! code. Only compile in plugins you would accept as a dependency.
//!
//! Within that model the host does enforce real invariants: capabilities are
//! per-plugin and declared up front, writes carry mandatory preconditions,
//! event attribution is host-stamped, and a plugin's tool calls are bounded in
//! time and isolated from panics.
//!
//! # Compatibility
//!
//! Public types are `#[non_exhaustive]` and built through constructors and
//! builder setters, so this crate can gain fields, variants, and trait methods
//! (always with defaults) without a breaking release. Construct types via
//! their `new` functions rather than struct literals.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod completion;
mod error;
mod hooks;
mod lifecycle;
mod provider;
mod storage;
mod vault;

pub use completion::{Completion, CompletionRequest, CompletionTarget};
pub use error::{PluginError, PluginErrorCode, PluginResult};
pub use hooks::{
    EventAttribution, EventCursor, HookBus, HookEvent, HookLifecycle, HookRecvError,
    HookSubscription, PublishError, VaultEventEnvelope, WriteProvenance,
};
pub use lifecycle::{ShutdownSignal, ShutdownTrigger};
pub use provider::{
    MCP_TOOL_NAME_MAX_LEN, Plugin, PluginCapabilities, PluginContext, PluginDescriptor,
    PluginProvider, PluginRequestContext, namespaced_prompt_name, namespaced_resource_template,
    namespaced_resource_uri, namespaced_tool_name, validate_mcp_tool_name, validate_plugin_id,
};
pub use storage::{PluginStorage, PluginStore};
pub use turbomcp_types::{
    Prompt, PromptResult, Resource, ResourceResult, ResourceTemplate, Tool, ToolResult,
};
pub use vault::{
    NoteListing, NoteSnapshot, PluginIdentity, VaultApi, VaultDescriptor, VaultHost,
    WriteNoteRequest, WritePrecondition, WriteReceipt,
};

/// Compiles the README's examples as doctests.
///
/// The README is a plugin author's first contact with this crate, so its code
/// has to keep working as the contract changes rather than quietly rotting.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeExamples;
