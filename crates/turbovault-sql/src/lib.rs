//! # turbovault-sql
//!
//! SQL query engine for Obsidian vault frontmatter powered by GlueSQL.
//!
//! Builds in-memory tables from vault data and lets MCP clients execute
//! arbitrary SQL queries. Three tables are auto-populated:
//!
//! - **`files`** — One row per markdown file with `path` + all frontmatter keys
//! - **`tags`** — Unnested `(path, tag)` pairs from frontmatter tag arrays
//! - **`links`** — `(source, target, link_type, is_valid)` from the vault link graph
//!
//! ## Example queries
//!
//! ```sql
//! -- Find all active tasks
//! SELECT path, status, priority FROM files
//! WHERE type = 'task' AND status = 'active'
//! ORDER BY priority DESC;
//!
//! -- Tag frequency report
//! SELECT tag, COUNT(*) AS cnt FROM tags
//! GROUP BY tag ORDER BY cnt DESC LIMIT 10;
//!
//! -- Notes tagged 'work' with broken outgoing links
//! SELECT DISTINCT t.path FROM tags t
//! JOIN links l ON t.path = l.source
//! WHERE t.tag = 'work' AND l.is_valid = FALSE;
//! ```

mod convert;
mod engine;

pub use engine::{FrontmatterSqlEngine, MAX_RESULT_ROWS, QUERY_TIMEOUT, SqlSession};
