# Plugin Architecture

TurboVault's v1 plugin model is compiled-in, Cargo-feature-gated Rust. It is a
curated extension surface: plugins are reviewed and shipped with TurboVault as
optional Cargo features. There is no dynamic loader, FFI boundary, sandbox, or
runtime installation.

## Boundary

Plugin crates depend on `turbovault-plugin-api`, not on the server's internal
managers. A plugin factory receives:

- `VaultApi`, a small cloneable facade for active-vault identity, note listing,
  complete reads, create-only writes, and compare-and-swap writes.
- `HookBus`, a bounded advisory event stream with explicit lag and closed
  states.

`VaultApi` requires every write to choose `CreateOnly` or `Match(version)`.
There is intentionally no blind-overwrite escape hatch. Version tokens are
opaque and backend-native: SHA-256 on the direct backend and Git blob IDs on
the Git backend.

Plugins receive a curated request context containing request, user, session,
client, and serializable metadata fields. Raw transport sessions,
authentication principals, vault managers, and the MCP server never cross the
boundary.

## Registration and names

The main crate's `plugin-api` feature is default-off. A vertical plugin feature
must include it and its optional plugin dependency:

```toml
[features]
plugin-tasks = ["plugin-api", "dep:turbovault-plugin-tasks"]
```

At startup, compiled-in factories are passed to
`ObsidianMcpServer::new_with_plugins`. The host validates descriptors, rejects
duplicate namespaces and tools, and wraps the object-safe provider for
TurboMCP's clone-based composite handler.

Core tool names stay flat for compatibility. Plugin tools are always
advertised as `<plugin_id>_<local_tool>`. A `list_tasks` tool owned by the
`tasks` plugin is therefore `tasks_list_tasks`.

When at least one plugin is registered, MCP `serverInfo.description` lists its
namespaces and explains that naming convention. The default server omits that
guidance entirely. The exact enabled catalog remains authoritative through
`tools/list`.

Plugin-local and fully namespaced names are validated at registration against
[MCP SEP-986](https://modelcontextprotocol.io/seps/986-specify-format-for-tool-names):
1–64 ASCII letters, digits, underscores, dashes, dots, or forward slashes.

## Hook lifecycle and backpressure

The hook bus is a fixed-size broadcast ring. Delivery is best-effort, not a
durable log:

- `Lagged { skipped }` means events were discarded for that subscriber. It
  must re-read authoritative state through `VaultApi`.
- `Closed` means the producer stopped and all buffered events were drained.
- Event provenance is best-effort correlation metadata. It is useful for
  reaction-loop prevention but is never an authentication or authorization
  boundary. `ExternalOrUnknown` must fail open.

The current host publishes writes made through `VaultApi`. A future
vault-events plugin can translate filesystem watcher events into the same
shared envelope; external edits remain attribution-blind unless they can be
correlated by content identity.

## Plugin-owned Rust APIs

The host contract governs only the capabilities used to mount a plugin into
TurboVault. A plugin may also expose a broader Rust library API for direct
non-MCP consumers. That public surface remains owned by the plugin maintainer
and is reviewed for stability, dependency cost, and maintenance burden like
any other in-tree public API; it does not need to be forced through `VaultApi`.

## Contribution rules

- Put verticals under `crates/plugins/`.
- Keep every vertical feature default-off.
- Depend on `turbovault-plugin-api`; do not reach into server internals.
- Publish only local tool names and let the host add the namespace.
- Treat hook delivery and provenance as advisory.
- Add provider-contract, namespace, lifecycle, and lag/resync tests.
- Test both feature-off compatibility and feature-on behavior. Every plugin
  must be exercised by the workspace `--all-features` CI suite, plus focused
  tests in its own crate.
