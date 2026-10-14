# Troubleshooting

## The client shows no tools

- Run the exact `command` from the client config in a terminal with `--version`. Claude Desktop doesn't use your shell's `PATH`, so give the full path.
- Check the client's log. Claude Desktop keeps each server's stderr in `~/Library/Logs/Claude/mcp-server-turbovault.log` on macOS.
- Only the 30 core tools are listed by default. If a tool you expect is missing, it may be advanced: add `--show-advanced-tools`, or call it by name. `--allowed-tools`, `--disabled-tools` and `--profile read-only` also narrow the list.

## "Vault path does not exist"

The path must exist and be a directory. `--vault` and the config file expand `~` and `$VAR`; paths an agent passes to `add_vault` expand `~` only.

## `add_vault` is refused

The message names the rule that refused it:

- **"cannot be registered as a vault"**: the path is a filesystem root, your home directory or something containing it, a hidden or application-data directory under home, or a system directory. These are never accepted at runtime; list the vault in the config file if you really mean it.
- **"outside the directories this server allows vaults in"**: `--allowed-vault-roots` is set and the path isn't inside one of them.
- **"registering vaults at runtime is turned off"**: you're on a network transport without `--allowed-vault-roots`. Set it, or list the vaults in the config file.

## The server won't start on a network transport

- **"refusing to listen ... without authentication"**: HTTP beyond loopback needs `--auth-mode bearer` or `jwt`. Inside Docker, `--host 0.0.0.0` counts as beyond loopback.
- **"the websocket transport has no authentication, so it only listens on loopback"** (or tcp): those transports can't authenticate; use HTTP for network access.
- **Bearer tokens refused at startup**: each token must be at least 32 characters.

`--allow-unauthenticated-network` lifts these refusals for a deployment that authenticates in a proxy in front of TurboVault.

## A write is refused

- **A conflict**: the note changed since it was read. Read it again and retry with the new `hash`. On a Git-backed vault the same happens when a file the write would touch has changes Git hasn't committed; commit or discard them first.
- **Permission denied on `.obsidian`, `.git` or `.turbovault`**: those folders are protected; the note tools never enter them.
- **"allowed_operations"**: the vault is configured to refuse this kind of change (for example `allowed_operations: read`).
- **"Git index contains staged changes"**: something staged changes in the vault's repository. Commit or unstage them.
- **"commit ... landed, but writing it to the working tree failed"**: the change is committed, and the next write (or a restart) writes it out. Look for another Git process holding `index.lock`, or a full disk.

## Search or the graph misses a recent edit

TurboVault checks for outside edits before serving search, graph and similarity results, at most every 500 ms (longer on very large vaults). If an edit still doesn't appear:

- Make sure `reconcile_external_changes` isn't `false` for the vault, and that the profile isn't `minimal`.
- Tools that preserve modification times (`rsync -t`, `cp -p`) can hide an edit that doesn't change the file's size. Touch the file, or restart the server.
- A note over `max_file_size` (10 MiB by default) isn't indexed.

To rebuild a vault's search index from scratch, stop the server and delete its directory under `~/.cache/turbovault/search/`.

## Memory or speed

- Startup on 50,000 notes takes about 2.5 s and 0.9 GB; most reads take a few milliseconds. See the [performance table](../api-reference/tools.md#performance).
- `--init` scans at startup instead of on the first call.
- The similarity engine (`find_similar`, `find_duplicates`, `compare_notes`) is built on first use and is the largest user of memory on a big vault.

## Logs

```bash
RUST_LOG=debug turbovault --vault /path/to/vault 2>debug.log
```

Logs go to stderr. Remove personal paths before attaching a log to an issue.

## Reporting a problem

Open an issue at <https://github.com/epistates/turbovault/issues> with `turbovault --version`, your OS, the transport and profile, roughly how many notes the vault has, the steps to reproduce, and the relevant log lines. Report security problems privately: see [SECURITY.md](../../SECURITY.md).

See also the [FAQ](faq.md).
