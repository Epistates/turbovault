# Configuration guide

TurboVault is configured with command-line flags (each has an environment variable) and an optional YAML file. Flags take precedence over the file.

## Profiles

`--profile` (`TURBOVAULT_PROFILE`) sets the log level, which `RUST_LOG` overrides, and the defaults every vault starts from. An unknown name stops startup.

| Profile | Log level | Also |
|---|---|---|
| `development` (default) | debug | |
| `production` | info | |
| `read-only` (or `readonly`) | warn | Every tool not annotated read-only is hidden and refused, the same gate as `--require-read-only-tools` |
| `high-performance` | warn | |
| `minimal` | error | Doesn't reconcile with edits made outside TurboVault |
| `multi-vault`, `collaboration` | info | |

## The config file

TurboVault reads `~/.turbovault/config.yaml` when it exists; `--config <PATH>` (`TURBOVAULT_CONFIG`) points at another file. It has three optional sections:

- `vaults`: the vaults to register at startup, each with its own settings
- `tool_visibility`: which tools are listed and callable
- `security`: who may reach the server and which directories it serves

[`config.example.yaml`](../../config.example.yaml) shows every key, and a test loads it, so it stays valid.

### Vault settings

| Key | Default | Meaning |
|---|---|---|
| `name` | | The vault's registered name |
| `path` | | The vault folder (`~` and `$VAR` are expanded) |
| `is_default` | `false` | Make this the active vault |
| `write_backend` | `direct` | `direct` (atomic renames) or `git` (one commit per write) |
| `git.branch` | current branch | Branch the Git backend commits to |
| `git.merge_strategy` | `merge-commit` | How a fanout merges back: `merge-commit` or `fast-forward` |
| `git.author` | repository config | `name` and `email` for commits |
| `git.include_ignored` | `true` | Write paths `.gitignore` matches; `false` refuses any write that touches one |
| `git.require_commit_message` | `false` | Refuse writes that don't pass `commit_message` |
| `max_file_size` | 10485760 | Largest file read, written or scanned, in bytes |
| `allowed_extensions` | `.md .txt .canvas .base` | What discovery considers; not access control |
| `excluded_paths` | `.obsidian .git .DS_Store node_modules` | Folders the note tools never enter; a vault's list is added to the server's, never in place of it |
| `reconcile_external_changes` | `true` | Notice edits made outside TurboVault before serving derived results |
| `allowed_operations` | `all` | `read`, `write`, `delete`, `move` or `all` (one name or a list); `read` alone makes the vault read-only |

`enable_caching`, `cache_ttl`, `template_dirs` and `watch_for_changes` were never read and are gone in 4.0. A file that still sets them loads, and they're ignored.

The `--vault` shorthand registers one vault named `default`. Give it a write backend with `--vault-write-backend git` (`TURBOVAULT_VAULT_WRITE_BACKEND`) and Git settings as JSON with `--vault-backend-opts '{"branch":"main"}'`.

## Tool visibility

By default `tools/list` shows the 30 core tools; the 22 advanced ones (audit and rollback, fanout, export, OKF maintenance, quality and duplicate analysis, SQL, `move_file` and similar) are callable by name but not listed. The full split is in [Tools](../api-reference/tools.md).

| Flag | Config key | Effect |
|---|---|---|
| `--show-advanced-tools` | `show_advanced_tools` | List the advanced tools too |
| `--allowed-tools <names>` | `allowed` | Only these tools are listed and callable |
| `--hidden-tools <names>` | `hidden` | Not listed, still callable by name |
| `--disabled-tools <names>` | `disabled` | Neither listed nor callable |
| `--disabled-tags <tags>` | `disabled_tags` | Disable every tool with one of these tags |
| `--require-read-only-tools` | `require_read_only` | List only tools annotated read-only |
| `--hidden-tags <tags>` | `hidden_tags` | Accepted but has no effect yet; a warning is logged |

Each flag has a `TURBOVAULT_` environment variable (`TURBOVAULT_ALLOWED_TOOLS` and so on). Flags and file values are merged.

## Security

```yaml
security:
  allowed_vault_roots: [~/Vaults]
  auth:
    mode: jwt
    resource: https://vault.example.com/mcp
    issuer: https://auth.example.com
    jwks_uri: https://auth.example.com/.well-known/jwks.json
    required_scopes: [vault]
```

**Vault registration.** `add_vault` checks every path, whether it registers a folder or creates one. A filesystem root, the home directory and anything containing it, hidden directories under home (`~/.ssh`, `~/.config`), application data (`~/Library` apart from the iCloud and cloud storage folders, `AppData`) and system directories are never accepted. With `allowed_vault_roots` set, only directories inside those roots are. Without it, any other directory is accepted on stdio, and runtime registration is off on network transports. Vaults from `--vault` and the config file are the operator's choice and aren't restricted; vaults restored from the cache are checked like a new registration. Tool-supplied paths expand `~` only, never environment variables.

**Network exposure.** The HTTP transport listens beyond loopback only with authentication. WebSocket and TCP have no authentication, so they listen on loopback only. `--allow-unauthenticated-network` lifts both refusals for a deployment that authenticates in front of TurboVault.

**Authentication (HTTP).** With `--auth-mode bearer` or `jwt`, every request needs `Authorization: Bearer <token>`, and the server publishes OAuth protected resource metadata (RFC 9728) at `/.well-known/oauth-protected-resource`. `bearer` accepts one of a fixed set of shared secrets of at least 32 characters, from `--auth-token-file` or one token in `TURBOVAULT_AUTH_TOKEN`. `jwt` checks the issuer, audience (`--auth-resource` unless `--auth-audience` says otherwise), expiry and signature against the issuer's JWKS, and requires `--auth-required-scopes`.

**Rate limiting.** Per client: off on loopback, 600 requests a minute beyond it, unless `--rate-limit` says otherwise (`0` turns it off). Behind a reverse proxy, list it in `--trusted-proxies` so clients are told apart by their forwarded address.

**Unix socket.** Without `--socket-path`, the socket is `turbovault.sock` in a private (0700) directory under `$XDG_RUNTIME_DIR`, or `~/.turbovault/run`.

## Observability

Logs go to stderr; on stdio they're JSON. `--output-format json|human|text` picks the format on other transports.

Each tool call is a `tool_call` span carrying the tool's name, the vault, its outcome (`ok`, `error` for a tool error, `failed` for a protocol error) and its duration in milliseconds. Spans carry no arguments and no note content. With `--otlp-endpoint` they're exported over OTLP, under `--service-name` (default `turbovault`).

With `--metrics-port`, Prometheus metrics are served on `http://127.0.0.1:<port>/metrics`, loopback only. Tool calls count toward `mcp_tool_calls_total` (labels `tool` and `status`) and `mcp_tool_duration_seconds` (label `tool`). To scrape from another host, put an authenticated proxy in front.

There's no HTTP health endpoint. Check liveness with an MCP `ping`, or a TCP connect to the HTTP port.

## Confirming destructive calls

`delete_note`, a `move_note` that replaces the note at its destination (`dest_expected_hash` other than `absent`), `rollback_note` with `force`, a `batch_execute` that deletes or replaces notes (one question for the whole batch), and `add_vault`, which gives the agent a new folder, ask the user to confirm when the client supports form elicitation. Declining, cancelling or leaving the box unchecked stops the call with nothing changed. A client without elicitation is never asked. `--no-confirm-destructive` turns confirmation off.

## Search index

Each vault's search index persists under TurboVault's cache directory (`$XDG_CACHE_HOME/turbovault`, `~/.cache/turbovault`, or `%LOCALAPPDATA%\turbovault\cache` on Windows), in `search/<hash of the vault path>/`, never inside the vault. A restart re-indexes only notes whose content changed. Set `TURBOVAULT_SEARCH_INDEX=memory` to keep indexes in memory only.

## Flags

| Flag | Environment variable | Default | |
|---|---|---|---|
| `-v, --vault <PATH>` | `OBSIDIAN_VAULT_PATH` | | Register this folder as the vault `default` |
| `--vault-write-backend <B>` | `TURBOVAULT_VAULT_WRITE_BACKEND` | `direct` | Backend for `--vault` |
| `--vault-backend-opts <JSON>` | `TURBOVAULT_VAULT_BACKEND_OPTS` | | Git settings for `--vault` |
| `-p, --profile <P>` | `TURBOVAULT_PROFILE` | `development` | See [Profiles](#profiles) |
| `--config <PATH>` | `TURBOVAULT_CONFIG` | `~/.turbovault/config.yaml` if present | The config file |
| `--init` | | off | Scan the vault at startup instead of on the first call |
| `-t, --transport <T>` | `TURBOVAULT_TRANSPORT` | `stdio` | `stdio`, `http`, `websocket`, `tcp` or `unix` |
| `--host <HOST>` | `TURBOVAULT_HOST` | `127.0.0.1` | Interface for network transports |
| `--port <PORT>` | `TURBOVAULT_PORT` | `3000` | Port for network transports |
| `--socket-path <PATH>` | `TURBOVAULT_SOCKET_PATH` | private run directory | Unix socket path |
| `--allowed-vault-roots <DIRS>` | `TURBOVAULT_ALLOWED_VAULT_ROOTS` | | Where `add_vault` may register vaults |
| `--auth-mode <M>` | `TURBOVAULT_AUTH_MODE` | `none` | `none`, `bearer` or `jwt` |
| `--auth-token-file <PATH>` | `TURBOVAULT_AUTH_TOKEN_FILE` | | `bearer` tokens, one per line |
| `--auth-resource <URL>` | `TURBOVAULT_AUTH_RESOURCE` | `http://<host>:<port>/mcp` | This server's canonical URL |
| `--auth-issuer <URL>` | `TURBOVAULT_AUTH_ISSUER` | | `jwt` issuer |
| `--auth-jwks-uri <URL>` | `TURBOVAULT_AUTH_JWKS_URI` | | `jwt` signing keys |
| `--auth-audience <AUD>` | `TURBOVAULT_AUTH_AUDIENCE` | `--auth-resource` | `jwt` audience |
| `--auth-required-scopes <S>` | `TURBOVAULT_AUTH_REQUIRED_SCOPES` | | `jwt` scopes every token needs |
| `--auth-authorization-servers <URLS>` | `TURBOVAULT_AUTH_AUTHORIZATION_SERVERS` | the issuer | Advertised in the resource metadata |
| `--rate-limit <N>` | `TURBOVAULT_RATE_LIMIT` | off on loopback, else 600 | Requests per client per window |
| `--rate-limit-window-secs <S>` | `TURBOVAULT_RATE_LIMIT_WINDOW_SECS` | `60` | Rate limit window |
| `--allowed-origins <ORIGINS>` | `TURBOVAULT_ALLOWED_ORIGINS` | | Browser origins accepted besides localhost |
| `--trusted-proxies <CIDRS>` | `TURBOVAULT_TRUSTED_PROXIES` | | Proxies whose forwarded headers are trusted |
| `--allow-unauthenticated-network` | `TURBOVAULT_ALLOW_UNAUTHENTICATED_NETWORK` | off | Listen beyond loopback without auth |
| `--otlp-endpoint <URL>` | `OTEL_EXPORTER_OTLP_ENDPOINT` | | Export traces over OTLP |
| `--service-name <NAME>` | `OTEL_SERVICE_NAME` | `turbovault` | Service name for traces and metrics |
| `--metrics-port <PORT>` | `TURBOVAULT_METRICS_PORT` | | Prometheus metrics on 127.0.0.1 |
| `--output-format <F>` | `TURBOVAULT_OUTPUT_FORMAT` | `json` | Log format on network transports |
| `--no-confirm-destructive` | `TURBOVAULT_NO_CONFIRM_DESTRUCTIVE` | off | Never ask before destructive calls or `add_vault` |
| `--plugins <IDS>` | `TURBOVAULT_PLUGINS` | none | Compiled-in plugins to enable, e.g. `vector_search` |
| `--show-advanced-tools` and the visibility flags | see [Tool visibility](#tool-visibility) | | |

Other environment variables: `RUST_LOG` (overrides the profile's log level) and `TURBOVAULT_SEARCH_INDEX=memory`.

## Examples

```bash
# One vault, production logging, scanned at startup
turbovault --vault ~/Documents/Vault --profile production --init

# Read-only
turbovault --vault ~/Documents/Vault --profile read-only

# Several vaults and a Git backend from a config file
turbovault --config ~/.turbovault/config.yaml

# HTTP for a team, behind JWT auth
turbovault --config /etc/turbovault/config.yaml --transport http --host 0.0.0.0 \
  --auth-mode jwt --auth-issuer https://auth.example.com \
  --auth-jwks-uri https://auth.example.com/.well-known/jwks.json \
  --auth-resource https://vault.example.com/mcp --auth-required-scopes vault
```
