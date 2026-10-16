# Claude Desktop and Claude Code

Both run TurboVault as a local process over stdio, which needs no network setup and no authentication.

## Claude Code

```bash
claude mcp add turbovault -- turbovault --vault ~/Documents/Vault
```

Add `--scope user` to make it available in every project. Arguments after `--` go to TurboVault, so any flag from the [configuration guide](../configuration/index.md) works, for example `--config ~/.turbovault/config.yaml`.

## Claude Desktop

Find the binary's full path (Claude Desktop doesn't use your shell's `PATH`):

```bash
which turbovault          # e.g. /Users/you/.cargo/bin/turbovault
```

Edit `claude_desktop_config.json`, in `~/Library/Application Support/Claude/` on macOS or `%APPDATA%\Claude\` on Windows:

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

On Windows:

```json
{
  "mcpServers": {
    "turbovault": {
      "command": "C:\\Users\\You\\.cargo\\bin\\turbovault.exe",
      "args": ["--vault", "C:\\Users\\You\\Documents\\Vault"]
    }
  }
}
```

Quit and reopen Claude Desktop. TurboVault's tools appear under the tools menu.

To work with several vaults, drop `--vault` and pass `--config` instead, or let the agent register vaults with `add_vault`. Claude Desktop starts a new process per conversation; registered vaults are remembered between them. See [Multiple vaults](../configuration/multi-vault.md).

## What to expect

- The agent sees the 30 core tools. Add `--show-advanced-tools` to the arguments to list the other 22; they're callable by name either way.
- Deleting a note, a move that replaces a note, a forced rollback, a batch that does either, and adding a vault ask you to confirm when the client supports elicitation.
- Notes are also available as resources (`obsidian://note/{vault}/{path}`), and the server offers five prompts, such as `triage_vault` and `review_period`.

## Troubleshooting

- **No tools**: check the JSON is valid and the `command` path exists; run it in a terminal with `--version`.
- **"Vault path does not exist"**: check the path; TurboVault expands `~` and `$VAR` in `--vault` itself.
- **Logs**: Claude Desktop keeps each server's stderr in its MCP log (`~/Library/Logs/Claude/mcp-server-turbovault.log` on macOS).

More in [Troubleshooting](../troubleshooting/index.md).
