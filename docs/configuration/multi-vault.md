# Multiple vaults

A server can hold any number of vaults (up to 50). One is the active vault, the one tools use when a call doesn't name another.

## Registering vaults

**In the config file** (`~/.turbovault/config.yaml` or `--config`), each with its own settings:

```yaml
vaults:
  - name: work
    path: ~/Documents/Work
    is_default: true
    write_backend: git
  - name: personal
    path: ~/Documents/Personal
  - name: archive
    path: ~/Documents/Archive
    allowed_operations: read
```

**At runtime**, the agent calls `add_vault` (with `create: true` to make the folder first). Runtime registration is checked against the vault root policy: see [Security](index.md#security). On a network transport it's off unless `--allowed-vault-roots` is set.

`list_vaults` lists them, `set_active_vault` changes the active one, and `remove_vault` unregisters one (its files are left alone).

## Naming a vault per call

Every tool that works inside a vault takes an optional `vault` argument:

```json
{"name": "search", "arguments": {"query": "roadmap", "vault": "work"}}
```

An explicit `vault` wins; otherwise the active vault is used. Naming a vault never changes the active one, so concurrent HTTP sessions can each work in their own vault without switching it under each other. An unknown name is refused with the registered names listed. Note resources carry the vault in their URI (`obsidian://note/{vault}/{path}`).

## What's kept between sessions

Claude Desktop starts a new server process per conversation. TurboVault persists the registered vaults and the active vault in its cache directory (`~/.cache/turbovault/projects/<hash of the working directory>/`), so a vault registered in one conversation is there in the next. Processes sharing a project merge their registrations rather than overwriting each other's.

Vaults in the config file are registered first; cached vaults fill in names the file doesn't use; `--vault` is registered as `default` only if that name is free. The active vault is the config file's `is_default` vault, otherwise the one the cache last had active.

## Isolation

Each vault has its own note cache, link graph, search index, similarity engine, audit trail and write lock. A batch and a fanout work within one vault. Removing a vault drops all of its server-side state.
