//! Contract tests for the compiled-in plugin boundary.
//!
//! These cover the properties a plugin author is entitled to rely on: what a
//! plugin can reach, what it is told about, and what happens when it
//! misbehaves.

#![cfg(feature = "plugin-api")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use turbomcp::prelude::*;
use turbovault::ObsidianMcpServer;
use turbovault_core::VaultConfig;
use turbovault_plugin_api::{
    Completion, CompletionRequest, CompletionTarget, EventAttribution, EventCursor, HookBus,
    HookEvent, HookRecvError, HookSubscription, Plugin, PluginCapabilities, PluginContext,
    PluginDescriptor, PluginError, PluginProvider, PluginRequestContext, PluginResult,
    PluginStorage, Prompt, PromptResult, Resource, ResourceResult, ResourceTemplate,
    ShutdownSignal, Tool, ToolResult, VaultApi, WriteNoteRequest, WritePrecondition,
    WriteProvenance,
};

/// Where the probe plugin persists its position in the event feed.
const CURSOR_KEY: &str = "feed/cursor.json";

/// A plugin that exercises the curated API and can be told to misbehave.
struct ProbePlugin {
    capabilities: PluginCapabilities,
    shutdown_called: Arc<AtomicBool>,
    worker_started: Arc<AtomicBool>,
    worker_stopped: Arc<AtomicBool>,
}

impl ProbePlugin {
    fn new(capabilities: PluginCapabilities) -> Self {
        Self {
            capabilities,
            shutdown_called: Arc::new(AtomicBool::new(false)),
            worker_started: Arc::new(AtomicBool::new(false)),
            worker_stopped: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Plugin for ProbePlugin {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor::new("probe", "Probe Plugin", "1.0.0", "Boundary contract probe")
    }

    fn capabilities(&self) -> PluginCapabilities {
        self.capabilities.clone()
    }

    fn build(&self, context: PluginContext) -> PluginResult<Arc<dyn PluginProvider>> {
        let events = context
            .hooks
            .subscribe()
            .map_err(|error| PluginError::internal(error.to_string()))?;
        Ok(Arc::new(ProbeProvider {
            vault: context.vault,
            storage: context.storage,
            hooks: context.hooks,
            events: tokio::sync::Mutex::new(events),
            shutdown: context.shutdown,
            shutdown_called: Arc::clone(&self.shutdown_called),
            worker_started: Arc::clone(&self.worker_started),
            worker_stopped: Arc::clone(&self.worker_stopped),
        }))
    }
}

struct ProbeProvider {
    vault: VaultApi,
    storage: PluginStorage,
    hooks: HookBus,
    events: tokio::sync::Mutex<HookSubscription>,
    shutdown: ShutdownSignal,
    shutdown_called: Arc<AtomicBool>,
    worker_started: Arc<AtomicBool>,
    worker_stopped: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl PluginProvider for ProbeProvider {
    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool::new("write", "Write a note through the curated API"),
            Tool::new("read_config", "Read a declared config file"),
            Tool::new("write_elsewhere", "Write naming a different vault"),
            Tool::new("boom", "Panic on purpose"),
            Tool::new("store", "Round-trip plugin-private storage"),
            Tool::new("survey", "List notes with change-detection metadata"),
            Tool::new("read_many", "Batch-read notes"),
            Tool::new("cursor", "Record and check a durable feed position"),
            Tool::new("feed", "Drain the event feed and report what arrived"),
        ]
    }

    fn resources(&self) -> Vec<Resource> {
        vec![
            Resource::new("index/stats.json", "Probe index statistics"),
            Resource::new("boom", "Panic on purpose"),
        ]
    }

    fn resource_templates(&self) -> Vec<ResourceTemplate> {
        vec![ResourceTemplate::new("note/{path}", "Indexed note")]
    }

    async fn read_resource(
        &self,
        uri: &str,
        _context: PluginRequestContext,
    ) -> PluginResult<ResourceResult> {
        match uri {
            // Deliberately a local URI: the host republishes it in the
            // plugin's namespace.
            "index/stats.json" => ResourceResult::json(uri, &serde_json::json!({"notes": 1}))
                .map_err(|error| PluginError::internal(error.to_string())),
            "boom" => panic!("probe plugin panicked reading a resource"),
            // Expanded from the template, so never enumerated. The plugin
            // decides what exists in its own namespace.
            templated if templated.starts_with("note/") => {
                let path = templated.trim_start_matches("note/");
                let snapshot = self
                    .vault
                    .read_note(&self.vault.active_vault().await?.name, path)
                    .await?;
                ResourceResult::json(templated, &serde_json::json!({"content": snapshot.content}))
                    .map_err(|error| PluginError::internal(error.to_string()))
            }
            other => Err(PluginError::not_found(format!(
                "unknown resource {other:?}"
            ))),
        }
    }

    async fn complete(
        &self,
        request: CompletionRequest,
        _context: PluginRequestContext,
    ) -> PluginResult<Completion> {
        match &request.target {
            // Suggest the notes that exist, which is the whole point: a client
            // can offer real paths instead of asking a person to guess one.
            CompletionTarget::ResourceTemplate(template) if template == "note/{path}" => {
                let active = self.vault.active_vault().await?;
                Ok(Completion::new(
                    self.vault
                        .list_notes(&active.name)
                        .await?
                        .into_iter()
                        .filter(|path| path.starts_with(&request.value)),
                ))
            }
            // More than MCP allows on the wire; the host has to cap it.
            CompletionTarget::Prompt(name) if name == "review" => Ok(Completion::new(
                (0..150).map(|index| format!("topic-{index}")),
            )),
            _ => Ok(Completion::none()),
        }
    }

    fn prompts(&self) -> Vec<Prompt> {
        vec![
            Prompt::new("review", "Review what the probe has indexed"),
            Prompt::new("boom", "Panic on purpose"),
        ]
    }

    async fn get_prompt(
        &self,
        name: &str,
        _arguments: Option<serde_json::Value>,
        _context: PluginRequestContext,
    ) -> PluginResult<PromptResult> {
        match name {
            "review" => Ok(PromptResult::user("Review the probe index.")),
            "boom" => panic!("probe plugin panicked rendering a prompt"),
            other => Err(PluginError::not_found(format!("unknown prompt {other:?}"))),
        }
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
        context: PluginRequestContext,
    ) -> PluginResult<ToolResult> {
        let string = |key: &str| {
            arguments
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        let active = self.vault.active_vault().await?;
        let json = match name {
            "write" => {
                let path = string("path").unwrap_or_else(|| "plugin.md".to_string());
                let receipt = self
                    .vault
                    .write_note(
                        WriteNoteRequest::new(
                            &active.name,
                            &path,
                            string("content").unwrap_or_default(),
                            WritePrecondition::CreateOnly,
                        )
                        .with_commit_message("probe write")
                        .with_provenance(
                            WriteProvenance::new("probe-source")
                                .with_correlation_id(context.request_id),
                        ),
                    )
                    .await?;
                serde_json::json!({ "receipt": receipt, "backend": active.write_backend })
            }
            "write_elsewhere" => {
                let receipt = self
                    .vault
                    .write_note(WriteNoteRequest::new(
                        string("vault").unwrap_or_default(),
                        "elsewhere.md",
                        "# Elsewhere",
                        WritePrecondition::CreateOnly,
                    ))
                    .await?;
                serde_json::json!({ "receipt": receipt })
            }
            "read_config" => {
                let bytes = self
                    .vault
                    .read_config(&active.name, &string("path").unwrap_or_default())
                    .await?;
                serde_json::json!({
                    "config": bytes.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
                })
            }
            "store" => {
                let key = string("key").unwrap_or_else(|| "index/state.json".to_string());
                match string("value") {
                    Some(value) => {
                        self.storage
                            .put(&active.name, &key, value.as_bytes())
                            .await?;
                    }
                    None => self.storage.delete(&active.name, &key).await?,
                }
                let stored = self.storage.get(&active.name, &key).await?;
                serde_json::json!({
                    "value": stored.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
                    "keys": self.storage.list(&active.name, "").await?,
                })
            }
            "survey" => serde_json::json!({
                "notes": self.vault.list_notes_detailed(&active.name).await?,
            }),
            "read_many" => {
                let paths: Vec<String> = arguments
                    .get("paths")
                    .and_then(serde_json::Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                serde_json::json!({ "notes": self.vault.read_notes(&active.name, &paths).await? })
            }
            // The shape a real consumer takes: apply what the feed delivered,
            // then persist where it got to so the next run can ask whether that
            // position still means anything.
            "cursor" => {
                let mut applied = None;
                let mut events = self.events.lock().await;
                loop {
                    match events.try_recv() {
                        Ok(envelope) => applied = Some(EventCursor::after(&envelope)),
                        Err(HookRecvError::Empty | HookRecvError::Closed) => break,
                        Err(HookRecvError::Lagged { .. }) => continue,
                    }
                }
                if let Some(cursor) = &applied {
                    let encoded = serde_json::to_vec(cursor)
                        .map_err(|error| PluginError::internal(error.to_string()))?;
                    self.storage.put(&active.name, CURSOR_KEY, &encoded).await?;
                }
                let stored = self.storage.get(&active.name, CURSOR_KEY).await?;
                let stored: Option<EventCursor> = stored
                    .map(|bytes| serde_json::from_slice(&bytes))
                    .transpose()
                    .map_err(|error| PluginError::internal(error.to_string()))?;
                serde_json::json!({
                    "sequence": stored.as_ref().map(EventCursor::sequence),
                    "resumes": stored
                        .as_ref()
                        .map(|cursor| cursor.resumes_on(&self.hooks)),
                })
            }
            // Drain the feed and report what it delivered, so a test can check
            // that a change nobody told the host about still reaches a plugin.
            "feed" => {
                let mut seen = Vec::new();
                let mut events = self.events.lock().await;
                loop {
                    match events.try_recv() {
                        Ok(envelope) => seen.push(match envelope.event {
                            HookEvent::FileCreated { path } => format!("created {path}"),
                            HookEvent::FileModified { path } => format!("modified {path}"),
                            HookEvent::FileDeleted { path } => format!("deleted {path}"),
                            HookEvent::FileRenamed { from, to } => {
                                format!("renamed {from} -> {to}")
                            }
                            HookEvent::ResyncRequired { reason } => format!("resync {reason}"),
                            // `HookEvent` is `#[non_exhaustive]`, so a real
                            // consumer has to keep working when the host learns
                            // to report something it has never seen.
                            other => format!("unknown {other:?}"),
                        }),
                        Err(HookRecvError::Empty | HookRecvError::Closed) => break,
                        Err(HookRecvError::Lagged { .. }) => continue,
                    }
                }
                serde_json::json!({ "events": seen })
            }
            "boom" => panic!("probe plugin panicked on purpose"),
            other => return Err(PluginError::not_found(format!("unknown tool {other:?}"))),
        };
        ToolResult::json(&json).map_err(|error| PluginError::internal(error.to_string()))
    }

    async fn start(&self) -> PluginResult<()> {
        self.worker_started.store(true, Ordering::SeqCst);
        let shutdown = self.shutdown.clone();
        let stopped = Arc::clone(&self.worker_stopped);
        tokio::spawn(async move {
            // The shape a real worker takes: block on the signal alongside
            // whatever else it waits on, and unwind when told to stop.
            shutdown.cancelled().await;
            stopped.store(true, Ordering::SeqCst);
        });
        Ok(())
    }

    async fn shutdown(&self) {
        self.shutdown_called.store(true, Ordering::SeqCst);
    }
}

struct Harness {
    _temp: Arc<tempfile::TempDir>,
    server: ObsidianMcpServer,
    capabilities: PluginCapabilities,
    shutdown_called: Arc<AtomicBool>,
    worker_started: Arc<AtomicBool>,
    worker_stopped: Arc<AtomicBool>,
}

impl Harness {
    async fn new(capabilities: PluginCapabilities) -> Self {
        Self::over(
            Arc::new(tempfile::TempDir::new().expect("temp vault")),
            capabilities,
        )
        .await
    }

    /// A fresh host over the same vault directory, as a restart would be:
    /// everything on disk survives, everything in memory does not.
    async fn restarted(&self) -> Self {
        Self::over(Arc::clone(&self._temp), self.capabilities.clone()).await
    }

    async fn over(temp: Arc<tempfile::TempDir>, capabilities: PluginCapabilities) -> Self {
        let plugin = ProbePlugin::new(capabilities.clone());
        let shutdown_called = Arc::clone(&plugin.shutdown_called);
        let worker_started = Arc::clone(&plugin.worker_started);
        let worker_stopped = Arc::clone(&plugin.worker_stopped);
        let server = ObsidianMcpServer::new_with_plugins(vec![Arc::new(plugin)])
            .expect("plugin composition");
        let config = VaultConfig::builder("probe-vault", temp.path())
            .build()
            .expect("vault config");
        server
            .multi_vault()
            .add_vault(config)
            .await
            .expect("register vault");
        server
            .multi_vault()
            .set_active_vault("probe-vault")
            .await
            .expect("select vault");
        Self {
            _temp: temp,
            server,
            capabilities,
            shutdown_called,
            worker_started,
            worker_stopped,
        }
    }

    fn path(&self) -> &std::path::Path {
        self._temp.path()
    }

    async fn call(&self, name: &str, arguments: serde_json::Value) -> McpResult<ToolResult> {
        self.server
            .call_tool(name, arguments, &RequestContext::with_id("probe-request"))
            .await
    }
}

/// The gap that made the hook bus decorative: a plugin subscribing to the feed
/// saw only its own writes, never the agent traffic it exists to react to.
#[tokio::test]
async fn core_mcp_writes_reach_plugin_subscribers() {
    let harness = Harness::new(PluginCapabilities::none()).await;
    let mut events = harness
        .server
        .hook_bus()
        .subscribe()
        .expect("hook subscription");
    let ctx = RequestContext::new();

    harness
        .server
        .call_tool(
            "write_note",
            serde_json::json!({"path": "agent.md", "content": "# Agent"}),
            &ctx,
        )
        .await
        .expect("core write");

    let created = events.recv().await.expect("core write event");
    assert_eq!(created.vault, "probe-vault");
    assert_eq!(
        created.event,
        HookEvent::FileCreated {
            path: "agent.md".to_string()
        }
    );
    assert_eq!(
        created.plugin_id, None,
        "a core MCP write did not come from a plugin"
    );

    // Overwriting the same path reports a modification, not a second creation.
    harness
        .server
        .call_tool(
            "write_note",
            serde_json::json!({"path": "agent.md", "content": "# Agent v2", "force": true}),
            &ctx,
        )
        .await
        .expect("core overwrite");
    assert_eq!(
        events.recv().await.expect("core modify event").event,
        HookEvent::FileModified {
            path: "agent.md".to_string()
        }
    );

    harness
        .server
        .call_tool(
            "delete_note",
            serde_json::json!({
                "path": "agent.md",
                "confirm_path": "agent.md",
                "force": true,
            }),
            &ctx,
        )
        .await
        .expect("core delete");
    assert_eq!(
        events.recv().await.expect("core delete event").event,
        HookEvent::FileDeleted {
            path: "agent.md".to_string()
        }
    );
}

/// Loop prevention needs a writer identity the writer cannot forge.
#[tokio::test]
async fn plugin_writes_are_attributed_by_the_host_not_the_caller() {
    let harness = Harness::new(PluginCapabilities::none()).await;
    let mut events = harness
        .server
        .hook_bus()
        .subscribe()
        .expect("hook subscription");

    harness
        .call(
            "probe_write",
            serde_json::json!({"path": "from-plugin.md", "content": "# Plugin"}),
        )
        .await
        .expect("plugin write");

    let event = events.recv().await.expect("plugin write event");
    assert_eq!(
        event.plugin_id.as_deref(),
        Some("probe"),
        "the host must stamp the mounted namespace"
    );
    // Caller-supplied provenance still rides along, but as advisory data.
    assert!(matches!(
        event.attribution,
        EventAttribution::Attributed(ref provenance)
            if provenance.source == "probe-source"
                && provenance.correlation_id.as_deref() == Some("probe-request")
    ));
}

/// A capability is scoped to the plugin that declared it, path by path.
#[tokio::test]
async fn config_reads_are_limited_to_declared_paths() {
    let declared = ".obsidian/plugins/example/data.json";
    let harness = Harness::new(PluginCapabilities::none().with_config_read(declared)).await;
    std::fs::create_dir_all(harness.path().join(".obsidian/plugins/example")).expect("config dir");
    std::fs::write(
        harness.path().join(declared),
        br##"{"globalFilter":"#todo"}"##,
    )
    .expect("config file");
    std::fs::write(harness.path().join(".obsidian/app.json"), b"{}").expect("other config");

    let allowed = harness
        .call("probe_read_config", serde_json::json!({"path": declared}))
        .await
        .expect("declared config read");
    assert_eq!(
        allowed
            .structured_content
            .expect("structured content")
            .get("config")
            .and_then(serde_json::Value::as_str),
        Some(r##"{"globalFilter":"#todo"}"##)
    );

    // A real file in the same directory that the plugin did not declare.
    let undeclared = harness
        .call(
            "probe_read_config",
            serde_json::json!({"path": ".obsidian/app.json"}),
        )
        .await;
    assert!(
        is_error(&undeclared),
        "undeclared config path must be refused: {undeclared:?}"
    );

    // Traversal out of the config space, and the note space itself.
    for rejected in [".obsidian/../secrets.md", "notes/plain.md"] {
        let result = harness
            .call("probe_read_config", serde_json::json!({"path": rejected}))
            .await;
        assert!(is_error(&result), "{rejected} must be refused: {result:?}");
    }

    // A declared path that does not exist is absence, not an error.
    std::fs::remove_file(harness.path().join(declared)).expect("remove config");
    let missing = harness
        .call("probe_read_config", serde_json::json!({"path": declared}))
        .await
        .expect("missing declared config is not an error");
    assert!(
        missing
            .structured_content
            .expect("structured content")
            .get("config")
            .is_some_and(serde_json::Value::is_null)
    );
}

/// A plugin that declares an unnormalized or out-of-scope capability must stop
/// the server from assembling, not fail confusingly at the first call.
#[test]
fn invalid_capability_declarations_are_rejected_at_mount_time() {
    for bad in [".obsidian/../.git/config", "notes/note.md", "/etc/passwd"] {
        let error = ObsidianMcpServer::new_with_plugins(vec![Arc::new(ProbePlugin::new(
            PluginCapabilities::none().with_config_read(bad),
        ))])
        .err()
        .unwrap_or_else(|| panic!("{bad} should have been refused at mount time"));
        assert!(
            error.to_string().contains("invalid capabilities"),
            "unexpected error for {bad}: {error}"
        );
    }
}

/// The active vault can change between a plugin's read and its write. Naming
/// the vault turns that into a refusal instead of a write to the wrong vault.
#[tokio::test]
async fn writes_naming_another_vault_are_refused() {
    let harness = Harness::new(PluginCapabilities::none()).await;
    let other = tempfile::TempDir::new().expect("second vault");
    harness
        .server
        .multi_vault()
        .add_vault(
            VaultConfig::builder("other-vault", other.path())
                .build()
                .expect("vault config"),
        )
        .await
        .expect("register second vault");

    let result = harness
        .call(
            "probe_write_elsewhere",
            serde_json::json!({"vault": "other-vault"}),
        )
        .await;
    assert!(
        is_error(&result),
        "a write naming a non-active vault must be refused: {result:?}"
    );
    assert!(
        !other.path().join("elsewhere.md").exists(),
        "nothing may be written to the vault the plugin did not target"
    );
}

/// A compiled-in plugin shares the process. A panic must degrade to one failed
/// tool call, and must not take the request or the server down with it.
#[tokio::test]
async fn a_panicking_plugin_fails_only_its_own_call() {
    let harness = Harness::new(PluginCapabilities::none()).await;

    let boom = harness.call("probe_boom", serde_json::json!({})).await;
    assert!(
        is_error(&boom),
        "panic should surface as an error: {boom:?}"
    );

    // The server still serves — both its own tools and the plugin's.
    harness
        .server
        .call_tool(
            "write_note",
            serde_json::json!({"path": "after-panic.md", "content": "# Alive"}),
            &RequestContext::new(),
        )
        .await
        .expect("core tools survive a plugin panic");
    harness
        .call(
            "probe_write",
            serde_json::json!({"path": "also-after.md", "content": "# Alive"}),
        )
        .await
        .expect("the plugin is still callable");
}

/// Create-only means create-only: the second write loses.
#[tokio::test]
async fn create_only_writes_refuse_an_existing_path() {
    let harness = Harness::new(PluginCapabilities::none()).await;
    harness
        .call(
            "probe_write",
            serde_json::json!({"path": "once.md", "content": "# First"}),
        )
        .await
        .expect("first create");

    let second = harness
        .call(
            "probe_write",
            serde_json::json!({"path": "once.md", "content": "# Second"}),
        )
        .await;
    assert!(
        is_error(&second),
        "create-only must refuse an existing path: {second:?}"
    );
    assert_eq!(
        std::fs::read_to_string(harness.path().join("once.md")).expect("note on disk"),
        "# First",
        "the refused write must not have changed the note"
    );
}

/// Shutdown tells plugins to release what they hold, then closes the feed so
/// subscribers stop waiting.
#[tokio::test]
async fn shutdown_stops_plugins_then_closes_the_feed() {
    let harness = Harness::new(PluginCapabilities::none()).await;
    let mut events = harness
        .server
        .hook_bus()
        .subscribe()
        .expect("hook subscription");

    harness.server.shutdown().await;

    assert!(
        harness.shutdown_called.load(Ordering::SeqCst),
        "plugins must be told to shut down"
    );
    assert_eq!(
        events.recv().await,
        Err(turbovault_plugin_api::HookRecvError::Closed),
        "subscribers must observe the bus closing rather than wait forever"
    );
}

fn is_error(result: &McpResult<ToolResult>) -> bool {
    match result {
        Ok(tool_result) => tool_result.is_error.unwrap_or(false),
        Err(_) => true,
    }
}

/// Derived state — an embedding index, a cached parse, a watermark — needs
/// somewhere that is not the vault's notes. Writing it as notes would pollute
/// the vault, the link graph, and the search corpus.
#[tokio::test]
async fn plugins_get_private_durable_storage() {
    let harness = Harness::new(PluginCapabilities::none()).await;

    let empty = structured(
        harness
            .call(
                "probe_store",
                serde_json::json!({"key": "index/state.json"}),
            )
            .await
            .expect("read before write"),
    );
    assert!(empty["value"].is_null(), "nothing stored yet");
    assert_eq!(empty["keys"], serde_json::json!([]));

    let written = structured(
        harness
            .call(
                "probe_store",
                serde_json::json!({"key": "index/state.json", "value": "{\"cursor\":42}"}),
            )
            .await
            .expect("store write"),
    );
    assert_eq!(written["value"], "{\"cursor\":42}");
    assert_eq!(written["keys"], serde_json::json!(["index/state.json"]));

    // Namespaced under the protected state directory, so it is durable, out of
    // the note space, and unreachable through the note APIs.
    let on_disk = harness
        .path()
        .join(".turbovault/plugins/probe/index/state.json");
    assert!(on_disk.exists(), "storage must be durable: {on_disk:?}");
    let note_read = harness
        .server
        .call_tool(
            "read_note",
            serde_json::json!({"path": ".turbovault/plugins/probe/index/state.json"}),
            &RequestContext::new(),
        )
        .await;
    assert!(
        is_error(&note_read),
        "plugin storage must not be readable as a note: {note_read:?}"
    );

    // Keys that could climb out of the namespace are refused.
    for escape in ["../other-plugin/state.json", "/etc/passwd", ""] {
        let result = harness
            .call(
                "probe_store",
                serde_json::json!({"key": escape, "value": "x"}),
            )
            .await;
        assert!(is_error(&result), "{escape:?} must be refused: {result:?}");
    }
}

/// Reconciliation must not cost a full read per note, or a plugin will reach
/// past the boundary to the filesystem to get acceptable performance.
#[tokio::test]
async fn listings_carry_change_detection_metadata() {
    let harness = Harness::new(PluginCapabilities::none()).await;
    let ctx = RequestContext::new();
    for (path, content) in [("a.md", "# A"), ("nested/b.md", "# B longer body")] {
        harness
            .server
            .call_tool(
                "write_note",
                serde_json::json!({"path": path, "content": content}),
                &ctx,
            )
            .await
            .expect("seed note");
    }

    let survey = structured(
        harness
            .call("probe_survey", serde_json::json!({}))
            .await
            .expect("survey"),
    );
    let notes = survey["notes"].as_array().expect("listing array");
    assert_eq!(notes.len(), 2);
    assert_eq!(notes[0]["path"], "a.md");
    assert_eq!(notes[0]["size_bytes"], 3);
    assert_eq!(notes[1]["path"], "nested/b.md");
    assert_eq!(notes[1]["size_bytes"], 15);
    assert!(
        notes.iter().all(|note| note["modified_ms"].is_u64()),
        "listings should carry a modification time: {notes:?}"
    );

    // Batch read resolves the vault once and skips what it cannot read, so a
    // note deleted between listing and reading does not sink the pass.
    let batch = structured(
        harness
            .call(
                "probe_read_many",
                serde_json::json!({"paths": ["a.md", "gone.md", "nested/b.md"]}),
            )
            .await
            .expect("batch read"),
    );
    let read = batch["notes"].as_array().expect("snapshot array");
    assert_eq!(read.len(), 2, "the missing note is skipped, not fatal");
    assert_eq!(read[0]["path"], "a.md");
    assert_eq!(read[0]["content"], "# A");
    assert!(
        read[0]["version"].as_str().is_some_and(|v| !v.is_empty()),
        "batch snapshots carry the same CAS token a single read would"
    );
}

/// Background work needs a start hook — `build` is synchronous and may run
/// outside a runtime — and a way to learn the host is going away.
#[tokio::test]
async fn plugin_workers_start_and_are_told_to_stop() {
    let harness = Harness::new(PluginCapabilities::none()).await;
    assert!(
        !harness.worker_started.load(Ordering::SeqCst),
        "construction must not start background work"
    );

    harness.server.start_plugins().await.expect("start plugins");
    assert!(harness.worker_started.load(Ordering::SeqCst));
    assert!(
        !harness.worker_stopped.load(Ordering::SeqCst),
        "the worker should still be running"
    );

    harness.server.shutdown().await;
    // The signal fires before `shutdown` is awaited, so the worker has been
    // told to stop; give the spawned task a moment to observe it.
    for _ in 0..100 {
        if harness.worker_stopped.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        harness.worker_stopped.load(Ordering::SeqCst),
        "the shutdown signal must reach a running worker"
    );
    assert!(harness.shutdown_called.load(Ordering::SeqCst));
}

/// Tools are only one of MCP's three primitives. A plugin that can offer none
/// of the others cannot expose state a user attaches deliberately, or a
/// workflow a user starts by name.
#[tokio::test]
async fn plugins_contribute_namespaced_resources_and_prompts() {
    let harness = Harness::new(PluginCapabilities::none()).await;
    let ctx = RequestContext::new();

    // Published under the plugin's own scheme, exactly as its tools are
    // published under its name prefix.
    let resources = harness.server.list_resources();
    assert!(
        resources
            .iter()
            .any(|resource| resource.uri == "probe://index/stats.json"),
        "plugin resources should be listed: {resources:?}"
    );
    assert!(
        resources
            .iter()
            .all(|resource| resource.uri != "index/stats.json"),
        "the unprefixed local URI must not be public"
    );
    let prompts = harness
        .server
        .list_prompts()
        .into_iter()
        .map(|prompt| prompt.name)
        .collect::<Vec<_>>();
    assert_eq!(prompts, ["probe_review", "probe_boom"]);

    // A server with no plugins advertises no prompts at all, so the capability
    // has to follow the composed catalog rather than be hard-coded.
    let capabilities = harness.server.server_capabilities();
    assert!(capabilities.prompts.is_some());
    assert!(capabilities.resources.is_some());

    let read = harness
        .server
        .read_resource("probe://index/stats.json", &ctx)
        .await
        .expect("plugin resource read");
    let contents = serde_json::to_value(&read).expect("serialize resource result");
    assert_eq!(
        contents["contents"][0]["uri"], "probe://index/stats.json",
        "content URIs must come back in the namespace the client asked against"
    );

    let prompt = harness
        .server
        .get_prompt("probe_review", None, &ctx)
        .await
        .expect("plugin prompt");
    assert!(!prompt.messages.is_empty());

    // Local names stay private, like unprefixed plugin tool names.
    assert!(
        harness
            .server
            .read_resource("index/stats.json", &ctx)
            .await
            .is_err()
    );
    assert!(
        harness
            .server
            .get_prompt("review", None, &ctx)
            .await
            .is_err()
    );
}

/// A URI space that tracks the vault cannot be an enumerated list: TurboVault
/// sends no `list_changed`, so a list a client cached at startup would go stale
/// in silence. Templates are how that space stays reachable, which only works
/// if a URI nothing listed still routes to its plugin.
#[tokio::test]
async fn plugin_resource_templates_route_without_being_enumerated() {
    let harness = Harness::new(PluginCapabilities::none()).await;
    let ctx = RequestContext::new();

    let templates = harness.server.list_resource_templates();
    assert_eq!(
        templates
            .iter()
            .map(|template| template.uri_template.as_str())
            .collect::<Vec<_>>(),
        ["probe://note/{path}"],
        "templates are namespaced like every other plugin primitive"
    );

    harness
        .server
        .call_tool(
            "write_note",
            serde_json::json!({"path": "templated.md", "content": "# Templated"}),
            &ctx,
        )
        .await
        .expect("seed a note");

    // Nothing listed this URI; it exists because the template says the space
    // does and the plugin owns the scheme.
    assert!(
        harness
            .server
            .list_resources()
            .iter()
            .all(|resource| resource.uri != "probe://note/templated.md")
    );
    let read = harness
        .server
        .read_resource("probe://note/templated.md", &ctx)
        .await
        .expect("templated resource read");
    let contents = serde_json::to_value(&read).expect("serialize resource result");
    assert_eq!(contents["contents"][0]["uri"], "probe://note/templated.md");
    assert!(
        contents["contents"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("# Templated"))
    );

    // Owning a scheme is not owning the URI space at large.
    for foreign in [
        "probe://note/missing.md",
        "unmounted://note/x.md",
        "obsidian://not-a-real-doc",
        "content://obsidian://syntax/quick-ref",
    ] {
        assert!(
            harness.server.read_resource(foreign, &ctx).await.is_err(),
            "{foreign} must not resolve"
        );
    }
}

/// A template a client cannot complete is a URI a person has to guess. This is
/// what turns `probe://note/{path}` from a declaration into something usable.
#[tokio::test]
async fn template_and_prompt_arguments_can_be_completed() {
    let harness = Harness::new(PluginCapabilities::none()).await;
    let ctx = RequestContext::new();

    assert!(
        harness.server.server_capabilities().completions.is_some(),
        "a server with completable arguments must advertise completion"
    );

    for path in ["daily.md", "drafts/deep.md", "other.md"] {
        harness
            .server
            .call_tool(
                "write_note",
                serde_json::json!({"path": path, "content": "# Note"}),
                &ctx,
            )
            .await
            .expect("seed note");
    }

    let suggestions = harness
        .server
        .complete(
            serde_json::json!({
                "ref": {"type": "ref/resource", "uri": "probe://note/{path}"},
                "argument": {"name": "path", "value": "d"},
            }),
            &ctx,
        )
        .await
        .expect("resource template completion");
    assert_eq!(
        suggestions["completion"]["values"],
        serde_json::json!(["daily.md", "drafts/deep.md"]),
        "the plugin sees the partial value and filters against real notes"
    );

    // Prompts complete through the same path, by public name.
    let capped = harness
        .server
        .complete(
            serde_json::json!({
                "ref": {"type": "ref/prompt", "name": "probe_review"},
                "argument": {"name": "topic", "value": ""},
            }),
            &ctx,
        )
        .await
        .expect("prompt argument completion");
    let values = capped["completion"]["values"]
        .as_array()
        .expect("completion values");
    assert_eq!(
        values.len(),
        100,
        "MCP caps a completion response; the host must enforce it, not each plugin"
    );
    assert_eq!(capped["completion"]["hasMore"], true);

    // References outside a mounted namespace do not resolve.
    for unroutable in [
        serde_json::json!({
            "ref": {"type": "ref/prompt", "name": "review"},
            "argument": {"name": "topic", "value": ""},
        }),
        serde_json::json!({
            "ref": {"type": "ref/resource", "uri": "unmounted://note/{path}"},
            "argument": {"name": "path", "value": ""},
        }),
    ] {
        assert!(
            harness.server.complete(unroutable, &ctx).await.is_err(),
            "a reference no plugin owns must not resolve"
        );
    }
}

/// A plugin mounted on a scheme the vault already publishes would capture every
/// unlisted URI in it, so the collision has to stop the server assembling.
#[test]
fn a_plugin_may_not_take_over_a_vault_resource_scheme() {
    struct SquatterPlugin;

    impl Plugin for SquatterPlugin {
        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::new("obsidian", "Squatter", "1.0.0", "Claims the vault's scheme")
        }

        fn build(&self, _context: PluginContext) -> PluginResult<Arc<dyn PluginProvider>> {
            unreachable!("assembly must fail before the plugin is built")
        }
    }

    let error = ObsidianMcpServer::new_with_plugins(vec![Arc::new(SquatterPlugin)])
        .err()
        .expect("a plugin claiming obsidian:// must be refused");
    assert!(
        error.to_string().contains("obsidian:// resource scheme"),
        "unexpected error: {error}"
    );
}

/// The failure budget is a property of the boundary, not of one entry point.
#[tokio::test]
async fn resource_and_prompt_failures_are_isolated_like_tool_calls() {
    let harness = Harness::new(PluginCapabilities::none()).await;
    let ctx = RequestContext::new();

    assert!(
        harness
            .server
            .read_resource("probe://boom", &ctx)
            .await
            .is_err(),
        "a panic while reading a resource must surface as an error"
    );
    assert!(
        harness
            .server
            .get_prompt("probe_boom", None, &ctx)
            .await
            .is_err(),
        "a panic while rendering a prompt must surface as an error"
    );

    // Both panics were contained to their own request.
    harness
        .server
        .read_resource("probe://index/stats.json", &ctx)
        .await
        .expect("the plugin still serves resources");
    harness
        .server
        .call_tool(
            "write_note",
            serde_json::json!({"path": "after-panic.md", "content": "# Alive"}),
            &ctx,
        )
        .await
        .expect("core tools survive a plugin panic");
}

/// Feed sequence numbers restart with the process. A consumer that persists one
/// and compares it against the next run would silently skip everything that
/// changed while it was down, so the position has to carry the run that issued
/// it.
#[tokio::test]
async fn a_persisted_feed_position_does_not_resume_after_a_restart() {
    let harness = Harness::new(PluginCapabilities::none()).await;

    harness
        .server
        .call_tool(
            "write_note",
            serde_json::json!({"path": "watched.md", "content": "# Watched"}),
            &RequestContext::new(),
        )
        .await
        .expect("seed an event");

    let recorded = structured(
        harness
            .call("probe_cursor", serde_json::json!({}))
            .await
            .expect("record a position"),
    );
    assert_eq!(recorded["sequence"], 1);
    assert_eq!(recorded["resumes"], true);

    // Same vault directory, new process. Storage survives; the feed does not.
    let restarted = harness.restarted().await;
    let reloaded = structured(
        restarted
            .call("probe_cursor", serde_json::json!({}))
            .await
            .expect("reload the position"),
    );
    assert_eq!(
        reloaded["sequence"], 1,
        "the position itself is durable through plugin storage"
    );
    assert_eq!(
        reloaded["resumes"], false,
        "but it says nothing about the new run, so the plugin must reconcile"
    );
}

/// A plugin learns about edits nobody made through TurboVault.
///
/// This is what makes the change feed usable as the sole input to a plugin's
/// own index. Without it a plugin could only ever be as current as the writes
/// this process happened to perform, which on a vault a human also edits means
/// diverging quietly and forever. Note the vault here is on the default Direct
/// backend, where there are no commits at all to observe: the host reconciles
/// by comparing state, and publishes what moved through the same feed.
#[tokio::test]
async fn a_plugin_is_told_about_edits_made_outside_the_host() {
    let harness = Harness::new(PluginCapabilities::none()).await;

    harness
        .server
        .call_tool(
            "write_note",
            serde_json::json!({"path": "ours.md", "content": "# Ours"}),
            &RequestContext::new(),
        )
        .await
        .expect("seed a note");

    // Clear the feed of what the host itself did, so what remains is only what
    // it had to discover.
    harness
        .call("probe_feed", serde_json::json!({}))
        .await
        .expect("drain");

    std::fs::write(harness._temp.path().join("theirs.md"), "# Theirs\n").expect("external write");
    std::fs::remove_file(harness._temp.path().join("ours.md")).expect("external delete");
    // Past the reconcile debounce, which is the bound the host promises.
    tokio::time::sleep(std::time::Duration::from_millis(750)).await;

    // Any gated tool call is enough to advance the feed; a plugin's own tool
    // call is gated too, precisely so a plugin that never calls a host tool
    // still keeps up.
    let seen = structured(
        harness
            .call("probe_feed", serde_json::json!({}))
            .await
            .expect("drain again"),
    );
    let events: Vec<&str> = seen["events"]
        .as_array()
        .expect("events array")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(
        events.contains(&"modified theirs.md"),
        "a note created outside the host must reach the feed: {events:?}"
    );
    assert!(
        events.contains(&"deleted ours.md"),
        "a note deleted outside the host must reach the feed: {events:?}"
    );
}

fn structured(result: ToolResult) -> serde_json::Value {
    result
        .structured_content
        .expect("tool result should carry structured content")
}
