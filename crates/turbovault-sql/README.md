# turbovault-sql

SQL query engine for Obsidian vault frontmatter, powered by [GlueSQL](https://gluesql.org).

Part of the [TurboVault](https://github.com/Epistates/turbovault) workspace.

## Overview

Builds in-memory tables from vault data and exposes them for arbitrary SQL queries via MCP tools. Three tables are auto-populated on each session:

| Table | Schema | Source |
|-------|--------|--------|
| `files` | Schemaless (path + all frontmatter keys) | Vault markdown files |
| `tags` | `(path TEXT, tag TEXT)` | Unnested from frontmatter tag arrays |
| `links` | `(source TEXT, target TEXT, link_type TEXT, is_valid BOOLEAN)` | Vault link graph |

## Example Queries

```sql
-- Find all active tasks
SELECT path, status, priority FROM files
WHERE type = 'task' AND status = 'active'
ORDER BY priority DESC;

-- Tag frequency report
SELECT tag, COUNT(*) AS cnt FROM tags
GROUP BY tag ORDER BY cnt DESC LIMIT 10;

-- Notes tagged 'work' that have broken outgoing links
SELECT DISTINCT t.path FROM tags t
JOIN links l ON t.path = l.source
WHERE t.tag = 'work' AND l.is_valid = FALSE;

-- Cross-reference: files with most outgoing links
SELECT source, COUNT(*) AS link_count FROM links
GROUP BY source ORDER BY link_count DESC LIMIT 10;
```

## Usage

This crate is feature-gated in TurboVault. Enable with:

```bash
cargo build --features sql
```

### One-shot query

```rust
use turbovault_sql::FrontmatterSqlEngine;

let engine = FrontmatterSqlEngine::new(vault_manager);
let result = engine.query("SELECT path, status FROM files WHERE status = 'active'").await?;
```

### Session (build tables once, query many times)

```rust
let engine = FrontmatterSqlEngine::new(vault_manager);
let mut session = engine.session().await?;

let tasks = session.query("SELECT path FROM files WHERE type = 'task'").await?;
let tags = session.query("SELECT tag, COUNT(*) as cnt FROM tags GROUP BY tag").await?;
```

## Supported SQL

Only `SELECT` (and `SHOW COLUMNS`) statements run: the tables are a read-only view of the vault, so anything else is refused before it runs. Each query runs on a blocking thread for at most `QUERY_TIMEOUT` (10 seconds), and each result keeps its first `MAX_RESULT_ROWS` (1000) rows, reporting `truncated` and `total_rows` when cut. GlueSQL can't cancel a query, so one that times out finishes in the background.

Within `SELECT`, GlueSQL supports a practical subset of SQL:

- `WHERE`, `ORDER BY`, `LIMIT`
- `JOIN` (INNER, LEFT, RIGHT, FULL, CROSS)
- `GROUP BY`, `HAVING`
- Aggregates: `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`, `STDEV`, `VARIANCE`
- Subqueries
- `CAST` for type coercion (e.g., `CAST('2024-01-01' AS DATE)`)
- String functions: `CONCAT`, `UPPER`, `LOWER`, `SUBSTRING`
- Date functions: `FORMAT`, date arithmetic

**Not supported:** Window functions, CTEs (`WITH` clause).

## Architecture

```
turbovault-sql/
  src/
    lib.rs       -- Public API, module docs
    engine.rs    -- FrontmatterSqlEngine, SqlSession, table builders
    convert.rs   -- GlueSQL Value <-> serde_json conversion
```

## License

MIT
