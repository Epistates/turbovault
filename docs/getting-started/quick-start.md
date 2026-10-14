# Quick start

## 1. Install

```bash
cargo install turbovault
```

or download a binary from the [releases page](https://github.com/Epistates/turbovault/releases). See [Installation](installation.md) for the options.

## 2. Connect a client

**Claude Code:**

```bash
claude mcp add turbovault -- turbovault --vault ~/Documents/Vault
```

**Claude Desktop:** add this to `claude_desktop_config.json` (`~/Library/Application Support/Claude/` on macOS, `%APPDATA%\Claude\` on Windows) and restart Claude Desktop:

```json
{
  "mcpServers": {
    "turbovault": {
      "command": "/Users/you/.cargo/bin/turbovault",
      "args": ["--vault", "/Users/you/Documents/Vault", "--profile", "production"]
    }
  }
}
```

Use the full path to the binary (`which turbovault`), since Claude Desktop doesn't use your shell's `PATH`.

You can also leave out `--vault` and ask the agent to register one with `add_vault`. A registered vault is remembered for the next session.

## 3. Ask something

Try:

- "What's in my vault?" (the agent calls `get_vault_context`)
- "Find my notes about project planning" (`search`)
- "What links to Projects/Roadmap.md?" (`get_links`)
- "How healthy is the vault's link structure?" (`vault_health`, then `graph_report` for the details)
- "Show my open tasks due this week" (`query_tasks`)

Every write is held to the note's hash from its last read, so the agent can't overwrite an edit you made since. Deleting a note asks you to confirm first when your client supports it.

## Next steps

- [Tools](../api-reference/tools.md): what the agent can do
- [Configuration guide](../configuration/index.md): profiles, a config file, several vaults, a Git backend
- [Troubleshooting](../troubleshooting/index.md)
