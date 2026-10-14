# FAQ

**Do I need Obsidian installed?**
No. A vault is a folder of Markdown files; TurboVault works on the folder directly. Obsidian can stay open on it at the same time, and TurboVault picks up its edits.

**Will it change my notes without asking?**
Only through write tools, and every write that replaces a note is held to the hash from the agent's last read, so it can't overwrite an edit you made since. Deleting a note, a move that replaces one and a forced rollback ask you to confirm when your client supports it. For no writes at all, run with `--profile read-only`, or set `allowed_operations: read` on a vault.

**Can I undo a change?**
Yes. Every change is in the audit trail (`audit_log`), and `rollback_note` restores the note as it was before. On a Git-backed vault every change is also a commit.

**Does it work with Obsidian Sync, iCloud, Dropbox or Syncthing?**
Yes. TurboVault writes ordinary files, and it notices edits arriving through a sync client the same way it notices edits made in Obsidian.

**How do I use several vaults?**
List them in `~/.turbovault/config.yaml`, or let the agent register them with `add_vault`. Each call can name its vault with the `vault` argument. See [Multiple vaults](../configuration/multi-vault.md).

**How big a vault can it handle?**
It's measured at 10,000, 50,000 and 100,000 notes; at 50,000, startup takes about 2.5 s and most reads a few milliseconds.

**Can I use it without Claude?**
Yes, with any MCP client, over stdio or (with authentication) HTTP. The crates can also be used directly from Rust.

**Does it support plugins?**
Compiled-in ones. The `vector_search` plugin ships in the prebuilt binaries and is enabled with `--plugins vector_search`. See [Plugins](../development/plugins.md).

**I upgraded from 3.x and a tool is gone.**
4.0 merged 79 tools into 52. [The migration guide](../migration/4.0.md) maps every old tool to its replacement.

**Where do I report a bug?**
<https://github.com/epistates/turbovault/issues>. Security problems go through [private reporting](../../SECURITY.md).
