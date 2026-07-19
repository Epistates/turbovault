# TurboVault Plugin API

`turbovault-plugin-api` is the stable boundary between TurboVault and its
compiled-in, feature-gated plugins.

It deliberately exposes a small `VaultApi` facade instead of TurboVault's
internal managers or MCP server. Plugins can read and safely write notes,
advertise namespaced MCP tools, and consume a bounded best-effort vault event
stream.

The v1 plugin model is curated and compiled in. It does not load dynamic code,
provide FFI, or sandbox plugins.
