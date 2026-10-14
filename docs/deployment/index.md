# Deployment

## Choosing a transport

| Transport | Use it for | Notes |
|---|---|---|
| `stdio` (default) | A local client that starts the server: Claude Desktop, Claude Code, IDEs | No network exposure. See [Claude Desktop and Claude Code](claude-desktop.md) |
| `http` | Remote clients and shared team servers | Streamable HTTP at `/mcp`. Needs `--auth-mode` to listen beyond loopback |
| `unix` | Local services on the same machine | Socket in a private (0700) directory by default |
| `websocket`, `tcp` | Local integrations | Loopback only: TurboMCP has no authentication for them |

Network transports need the binary built with them (`http`, `websocket`, `tcp`, `unix`; the prebuilt binaries and the Docker image have all of them).

## A shared HTTP server

```bash
turbovault --config /etc/turbovault/config.yaml \
  --transport http --host 0.0.0.0 --port 3000 \
  --auth-mode jwt \
  --auth-issuer https://auth.example.com \
  --auth-jwks-uri https://auth.example.com/.well-known/jwks.json \
  --auth-resource https://vault.example.com/mcp \
  --auth-required-scopes vault
```

- Clients send `Authorization: Bearer <token>`; the server publishes protected resource metadata at `/.well-known/oauth-protected-resource` so OAuth-capable clients can find the authorization server.
- Each client is rate limited (600 requests a minute by default beyond loopback). Behind a reverse proxy, pass `--trusted-proxies` so clients are told apart by their forwarded address, and terminate TLS at the proxy.
- Runtime vault registration is off on a network transport unless `--allowed-vault-roots` is set. Prefer listing the vaults in the config file.
- Each request can name its vault with the `vault` argument, so sessions don't interfere through the active vault.

## systemd

```ini
# /etc/systemd/system/turbovault.service
[Unit]
Description=TurboVault MCP server
After=network-online.target
Wants=network-online.target

[Service]
User=turbovault
ExecStart=/usr/local/bin/turbovault \
  --config /etc/turbovault/config.yaml \
  --profile production --init \
  --transport http --host 127.0.0.1 --port 3000
Restart=on-failure
RestartSec=5s
Environment=RUST_LOG=info

NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=/srv/vaults /var/cache/turbovault
Environment=XDG_CACHE_HOME=/var/cache/turbovault

[Install]
WantedBy=multi-user.target
```

This binds to loopback for a reverse proxy on the same host; to listen on a public interface, add the `--auth-mode` flags. The cache directory holds the persisted vault registry and search indexes, so it must be writable.

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now turbovault
journalctl -u turbovault -f
```

## Kubernetes

- Run the image as a Deployment with the vault on a PersistentVolume mounted at `/var/obsidian-vault`, and the cache (`XDG_CACHE_HOME`) on a volume too if search indexes should survive restarts.
- Pass `--transport http --host 0.0.0.0` with `--auth-mode`; the server won't start beyond loopback without it.
- Use a `tcpSocket` liveness probe on the port; there's no HTTP health endpoint.
- One replica per vault: two processes writing one vault serialize through file locks, but each keeps its own index.

## Read-only deployments

`--profile read-only` hides and refuses every mutating tool. To make only some vaults read-only, set `allowed_operations: read` on them in the config file.

## Monitoring

- **Logs**: stderr, JSON on stdio, `--output-format` elsewhere. `RUST_LOG` overrides the profile's level.
- **Traces**: `--otlp-endpoint http://collector:4317` exports a `tool_call` span per call (tool, vault, outcome, duration; never arguments or note content).
- **Metrics**: `--metrics-port 9464` serves Prometheus metrics on `127.0.0.1:9464/metrics`, including `mcp_tool_calls_total` and `mcp_tool_duration_seconds`.

## Backups

TurboVault writes ordinary files. A Git-backed vault (`write_backend: git`) keeps every change as a commit; otherwise the audit trail under `.turbovault/` holds a snapshot of each note before each change, and `rollback_note` restores one. Neither replaces a backup of the vault itself.
