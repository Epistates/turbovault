# TurboVault Plugin API

`turbovault-plugin-api` is the stable boundary between TurboVault and its
compiled-in, feature-gated plugins.

It deliberately exposes a small `VaultApi` facade instead of TurboVault's
internal managers or MCP server. Plugins can read and safely write notes,
advertise namespaced MCP tools, and consume a bounded best-effort vault event
stream.

## Trust model

The v1 plugin model is curated and compiled in. It does not load dynamic code,
provide FFI, or sandbox plugins.

**This is a contract, not a sandbox.** A compiled-in plugin runs with the
host's full privileges — it can open files, spawn threads, and reach the
network no matter what it declares here. The boundary exists to decouple
plugins from TurboVault's internals, make what a plugin touches reviewable, and
prevent *accidental* over-reach. Only compile in plugins you would accept as a
dependency.

Within that model the host does enforce real invariants:

- Capabilities are per-plugin and declared up front.
- Writes carry a mandatory create-or-CAS precondition; blind overwrites are not
  exposed.
- Event attribution is stamped by the host from the mounted descriptor.
- Tool calls are bounded in wall-clock time and isolated from panics.

## Writing a plugin

```rust
use std::sync::Arc;
use turbovault_plugin_api::{
    Plugin, PluginCapabilities, PluginContext, PluginDescriptor, PluginProvider, PluginResult,
};

struct TasksPlugin;

impl Plugin for TasksPlugin {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor::new("tasks", "Tasks", "1.0.0", "Obsidian Tasks integration")
    }

    // Declare the exact application-config files this plugin reads. The
    // default is an empty list: nothing beyond the note APIs.
    fn capabilities(&self) -> PluginCapabilities {
        PluginCapabilities::none()
            .with_config_read(".obsidian/plugins/obsidian-tasks-plugin/data.json")
    }

    fn build(&self, context: PluginContext) -> PluginResult<Arc<dyn PluginProvider>> {
        // `context.vault` is bound to THIS plugin's declaration.
        todo!("return your provider")
    }
}
```

The host mounts each plugin under its descriptor ID, so a local tool called
`list` in the `tasks` plugin is advertised as `tasks_list`.

## Resources and prompts

A plugin contributes all three MCP primitives, not just tools. The distinction
is who initiates: the model calls a **tool**, a user attaches a **resource** as
context, and a user invokes a **prompt** by name. State a plugin maintains is
usually better offered as a resource than as a tool nothing thinks to call.

Every primitive is namespaced the same way, and a plugin never spells its own
namespace — it names local identifiers and the host publishes them:

| Primitive | Declared locally | Published as |
| --- | --- | --- |
| Tool | `list` | `tasks_list` |
| Prompt | `review` | `tasks_review` |
| Resource | `index/stats.json` | `tasks://index/stats.json` |

```rust
# use turbovault_plugin_api::*;
# struct P;
# #[async_trait::async_trait]
# impl PluginProvider for P {
#     fn tools(&self) -> Vec<Tool> { Vec::new() }
#     async fn call_tool(&self, _n: &str, _a: serde_json::Value, _c: PluginRequestContext)
#         -> PluginResult<ToolResult> { unimplemented!() }
fn resources(&self) -> Vec<Resource> {
    vec![Resource::new("index/stats.json", "Index statistics")]
}

async fn read_resource(&self, uri: &str, _context: PluginRequestContext)
    -> PluginResult<ResourceResult> {
    match uri {
        "index/stats.json" => ResourceResult::json(uri, &serde_json::json!({"notes": 1}))
            .map_err(|error| PluginError::internal(error.to_string())),
        other => Err(PluginError::not_found(format!("unknown resource {other:?}"))),
    }
}
# }
```

The host rewrites the URI on every returned content into the plugin's
namespace, so what a client reads back matches what it asked for and no plugin
can serve content under a URI it does not own. Resource reads and prompt
renders are bounded and panic-isolated exactly like tool calls.

### Resource templates, not a changing list

TurboVault deliberately advertises `listChanged: false` for tools, resources,
and prompts, and sends no `notifications/*/list_changed`: the catalog is fixed
when the server is assembled. So do not model a URI space that tracks the vault
as an enumerated list — a list a client cached at startup would go stale in
silence.

Publish that space as a template instead. The host routes a plugin's *entire*
scheme, so a URI expanded from a template reaches `read_resource` whether or
not anything listed it, and the template itself never changes:

```rust
# use turbovault_plugin_api::*;
# struct P;
# #[async_trait::async_trait]
# impl PluginProvider for P {
#     fn tools(&self) -> Vec<Tool> { Vec::new() }
#     async fn call_tool(&self, _n: &str, _a: serde_json::Value, _c: PluginRequestContext)
#         -> PluginResult<ToolResult> { unimplemented!() }
fn resource_templates(&self) -> Vec<ResourceTemplate> {
    vec![ResourceTemplate::new("note/{path}", "Indexed note")]
}
# }
```

Because the whole scheme routes to you, `read_resource` receives URIs nobody
enumerated — validate what you are given and return `PluginError::not_found`
for anything you do not serve. Brace structure is checked when the plugin is
mounted, so a typo in an expression name fails at startup rather than when a
client tries to expand it.

### Completing arguments

A template nobody can complete is a URI a person has to guess. Implement
`complete` and a client can offer the values that actually exist, for template
expressions and prompt arguments alike:

```rust
# use turbovault_plugin_api::*;
# struct P { vault: VaultApi }
# #[async_trait::async_trait]
# impl PluginProvider for P {
#     fn tools(&self) -> Vec<Tool> { Vec::new() }
#     async fn call_tool(&self, _n: &str, _a: serde_json::Value, _c: PluginRequestContext)
#         -> PluginResult<ToolResult> { unimplemented!() }
async fn complete(&self, request: CompletionRequest, _context: PluginRequestContext)
    -> PluginResult<Completion> {
    match &request.target {
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
        _ => Ok(Completion::none()),
    }
}
# }
```

`request.value` is what the user has typed so far and `request.resolved` holds
the expressions already filled in, so a multi-expression template can narrow as
it goes. Clients call this on each keystroke — answer from state you already
hold rather than scanning the vault.

Return everything you know: the host truncates to the limit MCP places on one
response and sets `hasMore` itself, so a plugin never has to track the
protocol's cap. Completion is bounded and panic-isolated like every other entry
point, and the server advertises the capability only when a plugin actually
contributes a prompt or a template.

## Writing notes

Every operation names the vault it targets. TurboVault serves one active vault
at a time and that selection can change between a plugin's read and its write;
naming the vault turns that race into a refusal rather than a write landing in
a vault the plugin never read.

```rust
# use turbovault_plugin_api::*;
# async fn example(vault: VaultApi) -> PluginResult<()> {
let active = vault.active_vault().await?;
let note = vault.read_note(&active.name, "inbox.md").await?;

vault
    .write_note(
        WriteNoteRequest::new(
            &active.name,
            "inbox.md",
            format!("{}\n\n- [ ] follow up", note.content),
            // Replace exactly the version we read, or fail.
            WritePrecondition::Match(note.version),
        )
        .with_commit_message("tasks: add follow-up"),
    )
    .await?;
# Ok(())
# }
```

`WritePrecondition::CreateOnly` and `Match` are enforced atomically on a
git-backed vault. On a direct (working-tree) vault `CreateOnly` is atomic via
exclusive file creation, while `Match` is a read-compare-then-write that can
still interleave under concurrent writers.

## Reacting to changes: watch and reconcile

A plugin that maintains derived state needs both halves of this pattern. The
event feed is the fast path; the listing is how you catch up on what the feed
could not tell you.

**Watch.** `HookBus` carries every change TurboVault performs or observes — its
own MCP tool writes, plugin writes, commits that arrive on the ref from outside
the process, and edits made by anyone else at all. That last case is the host
comparing state rather than being told: before serving anything derived it
compares a `(size, mtime)` scan against what it last recorded, and publishes
what moved with `plugin_id: None`. So a note that Obsidian saved, or that a sync
client delivered, reaches this feed the same way a tool write does, on both
write backends. The bound is a debounce, at least 500ms and scaled up on a very
large vault; your own tool calls are gated on it too, so a plugin that never
calls a host tool still keeps up.

Delivery is bounded and best-effort: a subscriber that falls behind receives
`HookRecvError::Lagged`, and the bus is in-memory, so it says nothing about what
happened while the process was down. Reconcile is still the answer to both.

Use `VaultEventEnvelope::plugin_id` for loop prevention. It is stamped by the
host from the mounted descriptor, unlike `WriteProvenance`, which is
caller-supplied and advisory.

**Reconcile.** `VaultApi::list_notes_detailed` returns each note's path, size,
and modification time — one stat per note, no content read. Compare against
what you stored, batch-read only what moved, and persist the result:

```rust
# use turbovault_plugin_api::*;
# use std::collections::HashMap;
# async fn reconcile(
#     vault_api: VaultApi,
#     storage: PluginStorage,
#     vault: &str,
#     known: HashMap<String, NoteListing>,
# ) -> PluginResult<()> {
let current = vault_api.list_notes_detailed(vault).await?;
let stale: Vec<String> = current
    .iter()
    .filter(|note| !known.get(&note.path).is_some_and(|seen| note.looks_unchanged_from(seen)))
    .map(|note| note.path.clone())
    .collect();

for note in vault_api.read_notes(vault, &stale).await? {
    // ... re-embed, re-index, whatever this plugin maintains
}
storage.put(vault, "index/state.json", b"...").await?;
# Ok(())
# }
```

Run reconcile at startup and after any `Lagged`. Between those, the feed keeps
you current.

**Know when a stored position is meaningless.** `VaultEventEnvelope::sequence`
counts from zero every time the host starts, so persisting a bare sequence
number is worse than persisting nothing: it looks comparable to the next run's
numbering and is not, and a consumer that trusts it silently skips everything
that changed while it was down. `EventCursor` pairs the sequence with the
`HookBus::epoch` that issued it, which turns that into a question you can ask:

```rust
# use turbovault_plugin_api::*;
# async fn resume(storage: PluginStorage, hooks: HookBus, vault: &str) -> PluginResult<bool> {
let stored: Option<EventCursor> = storage
    .get(vault, "feed/cursor.json")
    .await?
    .and_then(|bytes| serde_json::from_slice(&bytes).ok());

// Reconcile unless the stored position belongs to this run. A matching epoch
// is not a promise that nothing was missed — `Lagged` reports that separately.
let must_reconcile = !stored.is_some_and(|cursor| cursor.resumes_on(&hooks));
# Ok(must_reconcile)
# }
```

Record a position with `EventCursor::after(&envelope)` once the event is
applied, and persist it through `PluginStorage`. TurboVault deliberately does
not journal the feed to disk: reconciling from listings is cheaper than
maintaining a durable log with its own retention and compaction, and for a
Git-backed vault it would duplicate history Git already keeps.

## Plugin-private storage

`PluginStorage` is durable per-vault key/value storage, namespaced to the
calling plugin by construction — there is no capability to declare and no
argument that reaches another plugin's data. It lives under the vault's
protected state directory, so an agent cannot read a plugin's index through
`read_note`, and it travels and is deleted with the vault it describes.

Individual writes are atomic; there is no transaction across keys.

## Background work

Spawn workers in `PluginProvider::start`, not `Plugin::build` — `build` is
synchronous and may run outside a runtime. `start` runs on the host's runtime
after every plugin is mounted and before the transport serves; returning an
error there stops the server from starting.

`PluginContext::shutdown` is the signal to stop. Select on it alongside
whatever else the loop waits on, and await your task handles in
`PluginProvider::shutdown`, which the host calls after the signal has fired.

```rust
# use turbovault_plugin_api::*;
# async fn worker(shutdown: ShutdownSignal, mut events: HookSubscription) {
loop {
    tokio::select! {
        _ = shutdown.cancelled() => break,
        event = events.recv() => match event {
            Ok(envelope) => { /* apply the change */ }
            Err(HookRecvError::Lagged { .. }) => { /* reconcile, then continue */ }
            Err(HookRecvError::Closed) => break,
            Err(HookRecvError::Empty) => continue,
        },
    }
}
# }
```

## Compatibility

Public types are `#[non_exhaustive]` and built through constructors and builder
setters, and new trait methods always arrive with defaults. Construct types via
their `new` functions rather than struct literals so this crate can grow
without a breaking release.
