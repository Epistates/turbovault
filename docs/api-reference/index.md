# API reference

TurboVault speaks MCP. At runtime `tools/list`, `resources/templates/list` and `prompts/list` are the authoritative catalog, with every schema and annotation. This page describes the conventions all of it follows; [Tools](tools.md) lists the tools.

## Tool results

Each tool returns `structuredContent` that matches its advertised `outputSchema`, and the same JSON, compact, as text content. Most results share one envelope:

```json
{
  "vault": "personal",
  "operation": "search",
  "success": true,
  "data": [ ... ],
  "count": 10,
  "took_ms": 4,
  "warnings": ["Showing 10 of 37. Call again with offset 10 for more"],
  "next_steps": ["read_note"],
  "meta": { "pagination": { "total": 37, "offset": 0, "limit": 10, "returned": 10, "truncated": true, "next_offset": 10 } }
}
```

`count`, `warnings`, `next_steps` and `meta` are left out when empty.

## Common arguments

- **`vault`**: every tool that works inside a vault takes an optional `vault`, the registered name of the vault to run against. Without it the call uses the active vault; naming one doesn't change the active vault. An unknown name is refused with the registered names listed.
- **`limit` and `offset`**: list tools return 50 items by default (each tool documents its own cap) and page with `offset`. `meta.pagination` reports `returned`, `truncated` and, when there's more, `next_offset`; `total` is included where counting is cheap. Every paged list is totally ordered, so pages never repeat or skip.
- **`response_format`**: `search`, `explain_vault` and `get_vault_context` accept `concise` to leave out fields an agent rarely needs; `detailed` is the default.
- **`expected_hash`**: `read_note` returns a note's `hash`; writes that replace a note take it as `expected_hash` and are refused if the note changed since. `blind` overwrites without checking, and `absent` insists the note doesn't exist yet.

## Errors

A failed call returns an MCP error whose kind comes from the error's category, with a message that says what to do next and `data` holding `{category, path}`. Paths are always vault-relative.

| Category | MCP error | Examples |
|---|---|---|
| `not_found` | resource not found | A missing note (with up to 3 similarly named paths as `suggestions`) |
| `invalid_input` | invalid params | Bad arguments, a path outside the vault, a file over the size limit, malformed frontmatter |
| `conflict` | invalid params | A stale `expected_hash`, a rollback that would discard a later edit |
| `permission_denied` | permission denied | A protected folder (`.obsidian`, `.git`, ...), a vault whose `allowed_operations` refuses the change |
| `unavailable` | unavailable | No active vault, a feature the binary was built without |
| `internal` | internal | A failure inside the server |

## Resources

Every note is a resource:

- `resources/templates/list` advertises `obsidian://note/{vault}/{+path}`.
- `resources/list` lists every note of every vault in use, paged.
- `resources/read` returns the note as `text/markdown`, with the same protections as `read_note`.
- `resources/subscribe` on a note's URI sends `notifications/resources/updated` whenever it changes, whether TurboVault or another editor changed it. Clients that list resources also get `notifications/resources/list_changed` when notes come or go.

`search` and `find_similar` return a `resource_link` for each note they find. The Obsidian Flavored Markdown guides are resources too: `obsidian://syntax/quick-ref`, `obsidian://syntax/complete-guide` and `obsidian://examples/sample-note` (see [OFM](ofm.md)).

## Prompts

| Prompt | Arguments | Workflow |
|---|---|---|
| `triage_vault` | `focus` | Find and prioritize the vault's problems |
| `repair_broken_links` | `note` | Find broken links and fix them |
| `review_period` | `period`, `folder` | Review what changed over a period |
| `summarize_note` | `path` | Summarize a note with its links and backlinks |
| `ground_draft` | `draft`, `folder` | Check a draft against what the vault says |

`completion/complete` completes prompt arguments (periods, focus values, note paths, folders) and the resource template's `vault` and `path`.

## Progress, cancellation and confirmation

- `vault_health` (at `depth: full`), `explain_vault`, `note_quality` (whole vault), `find_duplicates`, `export`, `batch_execute` and `add_vault` report progress by stage when the request carries a progress token.
- A cancelled call stops at the start of a stage that hasn't written anything; a call either did nothing or finished.
- `delete_note`, a `move_note` that replaces a note, `rollback_note` with `force`, a `batch_execute` that deletes or replaces notes, and `add_vault` ask the user to confirm through elicitation when the client supports it, unless the server runs with `--no-confirm-destructive`.

## Example workflows

**Search, then read:**

```
search {query: "rust async", limit: 5}
read_note {path: "<first result's path>"}
```

**Health, then repair:**

```
vault_health {}
graph_report {kind: "broken_links"}
batch_execute {operations: [...]}        // one commit on a Git-backed vault
vault_health {}
```

**Create from a template:**

```
list_templates {}
create_from_template {template_id: "task", file_path: "tasks/auth.md",
                      fields: "{\"title\": \"User authentication\", \"priority\": \"high\"}"}
```

**Work in another vault without switching:**

```
search {query: "Q4 goals", vault: "work"}
```
