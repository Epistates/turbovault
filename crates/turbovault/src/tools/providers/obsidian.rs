//! ObsidianProvider MCP capabilities: canvases, bases, tasks and periodic
//! notes, the Obsidian formats beyond a single Markdown note.

use std::ops::Deref;

use turbovault_parser::{Canvas, CanvasEdge, CanvasNode, CanvasReference};
use turbovault_tools::CanvasTools;
use turbovault_tools::bases::{BaseResult, query_base};
use turbovault_tools::periodic::{self, Period, PeriodicNote, get_periodic_note};
use turbovault_tools::task_query::{Priority, TaskMatch, TaskQuery, TaskState, query_tasks};

use super::super::*;

/// Most nodes `read_canvas` returns in one call.
const MAX_CANVAS_NODES: usize = 1000;
/// Nodes `read_canvas` returns when the caller doesn't say.
const DEFAULT_CANVAS_NODES: usize = 200;

/// What `read_canvas` returns.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct CanvasPage {
    path: String,
    /// The canvas's hash, for write_canvas's expected_hash. Only on a page
    /// holding the whole canvas, since writing back part of one would drop
    /// the rest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hash: Option<String>,
    node_count: usize,
    edge_count: usize,
    /// This page of the nodes.
    nodes: Vec<CanvasNode>,
    /// The edges touching a node on this page.
    edges: Vec<CanvasEdge>,
    /// The notes and files the nodes on this page point at.
    references: Vec<CanvasRef>,
}

/// A note or file a canvas node points at.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct CanvasRef {
    node_id: String,
    /// `file` for a file node, `link` for a link written in a text node.
    kind: &'static str,
    /// The path or link target, as written.
    target: String,
    /// The vault-relative note it resolves to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resolved: Option<String>,
}

/// What `write_canvas` returns.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct CanvasWritten {
    path: String,
    /// Always `written`.
    status: &'static str,
    bytes: usize,
    nodes: usize,
    edges: usize,
}

#[derive(Clone)]
pub(super) struct ObsidianProvider(CoreToolHandler);

impl ObsidianProvider {
    pub(super) fn new(core: CoreToolHandler) -> Self {
        Self(core)
    }
}

impl Deref for ObsidianProvider {
    type Target = CoreToolHandler;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

fn parse_day(field: &str, value: Option<String>) -> McpResult<Option<chrono::NaiveDate>> {
    value
        .map(|text| {
            chrono::NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d").map_err(|_| {
                McpError::invalid_params(format!(
                    "{field} must be a date such as 2026-10-20, not {text:?}"
                ))
            })
        })
        .transpose()
}

fn parse_list<T>(
    field: &str,
    values: Option<Vec<String>>,
    parse: impl Fn(&str) -> Option<T>,
    expected: &str,
) -> McpResult<Vec<T>> {
    values
        .unwrap_or_default()
        .iter()
        .map(|value| {
            parse(&value.trim().to_lowercase()).ok_or_else(|| {
                McpError::invalid_params(format!("{field} {value:?} is not one of {expected}"))
            })
        })
        .collect()
}

#[turbomcp::server(name = "obsidian-vault", version = env!("CARGO_PKG_VERSION"))]
impl ObsidianProvider {
    /// Read a canvas
    #[tool(
        description = "Read an Obsidian canvas (.canvas, JSON Canvas 1.0): its nodes (text, file, link, group), edges, and the notes it points at. A read that returns every node also returns the `hash` write_canvas takes as expected_hash. Large canvases come back a page of nodes at a time.",
        title = "Read Canvas",
        tags = ["read", "canvas"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = Paged<CanvasPage>,
    )]
    async fn read_canvas(
        &self,
        #[description(
            "Vault-relative path of the canvas, ending in .canvas, such as `boards/Plan.canvas`."
        )]
        path: String,
        #[description("Most nodes to return. Default 200, maximum 1000.")] limit: Option<usize>,
        #[description(
            "How many nodes to skip, for the next page. Default 0; use `meta.pagination.next_offset` from the previous call."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let (canvas, hash) = CanvasTools::new(manager.clone())
            .read(&path)
            .await
            .map_err(to_mcp_error)?;
        let node_count = canvas.nodes.len();
        let edge_count = canvas.edges.len();
        let references = canvas.references();
        let request = PageRequest::new(offset, limit, DEFAULT_CANVAS_NODES, MAX_CANVAS_NODES);
        let (nodes, page) = request.slice(canvas.nodes);
        let whole = page.offset == 0 && page.returned == node_count;
        let on_page: std::collections::HashSet<&str> =
            nodes.iter().map(|n| n.id.as_str()).collect();
        let edges: Vec<CanvasEdge> = canvas
            .edges
            .into_iter()
            .filter(|edge| {
                on_page.contains(edge.from_node.as_str()) || on_page.contains(edge.to_node.as_str())
            })
            .collect();
        let graph = manager.link_graph_flushed().await;
        let graph = graph.read().await;
        let resolve = |target: &str| {
            graph
                .resolve(target)
                .map(|resolved| manager.relative_path(resolved))
        };
        let references = references
            .into_iter()
            .map(|reference| match reference {
                CanvasReference::File { node_id, path, .. } => CanvasRef {
                    resolved: resolve(&path),
                    node_id,
                    kind: "file",
                    target: path,
                },
                CanvasReference::Text { node_id, link } => CanvasRef {
                    resolved: resolve(&link.target),
                    node_id,
                    kind: "link",
                    target: link.target,
                },
            })
            .filter(|reference| on_page.contains(reference.node_id.as_str()))
            .collect();
        drop(graph);
        StandardResponse::new(
            vault_name,
            "read_canvas",
            CanvasPage {
                path,
                hash: whole.then_some(hash),
                node_count,
                edge_count,
                nodes,
                edges,
                references,
            },
        )
        .with_page(page)
        .with_next_step("write_canvas")
        .to_json()
    }

    /// Write a canvas
    #[tool(
        description = "Create or replace an Obsidian canvas (.canvas) with a whole JSON Canvas 1.0 document, validated first: unique ids, the fields each node type needs, integer positions and sizes, edges between existing nodes. Replacing an existing canvas needs the expected_hash from a read_canvas that returned every node. Written the way Obsidian writes canvases.",
        title = "Write Canvas",
        tags = ["write", "canvas"],
        read_only = false,
        destructive = true,
        idempotent = false,
        open_world = false,
        output_schema = StandardResponse<CanvasWritten>,
    )]
    async fn write_canvas(
        &self,
        #[description(
            "Vault-relative path of the canvas, ending in .canvas, such as `boards/Plan.canvas`. Missing folders are created."
        )]
        path: String,
        #[description(
            "The whole canvas: `{\"nodes\": [...], \"edges\": [...]}`. A node has id, type (text, file, link or group), integer x, y, width and height, optional color (\"1\" to \"6\" or a hex color), and text (text nodes), file and optional subpath starting with # (file nodes), url (link nodes) or optional label, background and backgroundStyle (groups). An edge has id, fromNode, toNode, and optional fromSide/toSide (top, right, bottom, left), fromEnd/toEnd (none, arrow), color and label."
        )]
        canvas: serde_json::Value,
        #[description(
            "The canvas's `hash` from a read_canvas that returned every node. Required to replace an existing canvas, and the write is refused if it changed since. `blind` replaces without checking; `absent` insists the canvas doesn't exist yet."
        )]
        expected_hash: Option<String>,
        #[description(
            "Commit message on a Git-backed vault. Required there when the vault sets git.require_commit_message; otherwise derived from the path."
        )]
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let canvas: Canvas = serde_json::from_value(canvas).map_err(|error| {
            McpError::invalid_params(format!("canvas is not a JSON Canvas document: {error}"))
        })?;
        let prepared = self
            .prepare_complete_note_write(&path, commit_message, "write_canvas")
            .await?;
        let manager = prepared.manager;
        let existed = CoreToolHandler::path_exists(&manager, &path).await;
        let precondition = turbovault_core::Precondition::from_wire(
            expected_hash.as_deref(),
            turbovault_core::Precondition::ExpectAbsent,
        );
        if precondition == turbovault_core::Precondition::ExpectAbsent && existed {
            return Err(to_mcp_error(turbovault_core::Error::concurrency_error(
                format!(
                    "'{path}' exists. Pass the hash from read_canvas as expected_hash to replace it, or expected_hash=\"blind\"."
                ),
            )));
        }
        let bytes = CanvasTools::new(manager)
            .write(&path, &canvas, precondition, &prepared.message)
            .await
            .map_err(to_mcp_error)?;
        self.after_write_one(
            &prepared.vault_name,
            VaultChange::written(&path, existed),
            WriteAttribution::host("write_canvas"),
        )
        .await;
        StandardResponse::new(
            prepared.vault_name,
            "write_canvas",
            CanvasWritten {
                path,
                status: "written",
                bytes,
                nodes: canvas.nodes.len(),
                edges: canvas.edges.len(),
            },
        )
        .with_next_step("read_canvas")
        .to_json()
    }

    /// Evaluate a base
    #[tool(
        description = "Run an Obsidian base (.base file): its filters, formulas, sort, grouping and limit over the vault's notes, returning the chosen view's rows and columns. Evaluates the Bases expression language (note and file properties, file.hasTag, file.inFolder, file.hasLink, if, date arithmetic, string, number, list and date methods); a function it doesn't evaluate is refused by name. For ad hoc frontmatter queries without a .base file, use query_frontmatter_sql.",
        title = "Query Base",
        tags = ["read", "bases"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = Paged<BaseResult>,
    )]
    async fn query_base(
        &self,
        #[description("Vault-relative path of the base, ending in .base, such as `Reading.base`.")]
        path: String,
        #[description(
            "The view's name. Default: the base's first view; the result lists every view's name."
        )]
        view: Option<String>,
        #[description("Most rows to return. Default 50, maximum 500.")] limit: Option<usize>,
        #[description(
            "How many rows to skip, for the next page. Default 0; use `meta.pagination.next_offset` from the previous call."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let mut result = query_base(
            &manager,
            &path,
            view.as_deref(),
            chrono::Local::now().naive_local(),
        )
        .await
        .map_err(to_mcp_error)?;
        let request = PageRequest::new(offset, limit, DEFAULT_PAGE_SIZE, MAX_REPORT_ITEMS);
        let (rows, page) = request.slice(std::mem::take(&mut result.rows));
        result.rows = rows;
        StandardResponse::new(vault_name, "query_base", result)
            .with_page(page)
            .to_json()
    }

    /// Query tasks
    #[tool(
        description = "Find tasks (Markdown checkboxes) across the vault by status, due and scheduled dates, tag, folder, priority and text, reading Tasks plugin emoji (📅 due, ⏳ scheduled, 🛫 start, ✅ done, 🔺⏫🔼🔽⏬ priority) and Dataview fields ([due:: 2026-10-20]). Sorted by due date (undated last), then priority, then note and line. Every filter given must match.",
        title = "Query Tasks",
        tags = ["read", "tasks"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = Paged<Vec<TaskMatch>>,
    )]
    #[allow(clippy::too_many_arguments)]
    async fn query_tasks(
        &self,
        #[description(
            "Statuses to include: todo `[ ]`, done `[x]`, in_progress `[/]`, cancelled `[-]`, other (a custom checkbox character). Default: all."
        )]
        status: Option<Vec<String>>,
        #[description("Only tasks due on or before this date, YYYY-MM-DD.")] due_before: Option<
            String,
        >,
        #[description("Only tasks due on or after this date, YYYY-MM-DD.")] due_after: Option<
            String,
        >,
        #[description("Only tasks scheduled on or before this date, YYYY-MM-DD.")]
        scheduled_before: Option<String>,
        #[description("Only tasks scheduled on or after this date, YYYY-MM-DD.")]
        scheduled_after: Option<String>,
        #[description(
            "Only tasks carrying this tag, with or without #. `work` also matches `#work/client`."
        )]
        tag: Option<String>,
        #[description(
            "Only tasks in this note (vault-relative path) or folder, at any depth, such as `projects`."
        )]
        path: Option<String>,
        #[description(
            "Priorities to include: highest, high, medium, normal, low, lowest. Default: all."
        )]
        priority: Option<Vec<String>>,
        #[description("Only tasks whose description contains this text, ignoring case.")]
        text: Option<String>,
        #[description("Most tasks to return. Default 50, maximum 500.")] limit: Option<usize>,
        #[description(
            "How many tasks to skip, for the next page. Default 0; use `meta.pagination.next_offset` from the previous call."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let query = TaskQuery {
            status: parse_list(
                "status",
                status,
                |s| match s {
                    "todo" => Some(TaskState::Todo),
                    "done" => Some(TaskState::Done),
                    "in_progress" => Some(TaskState::InProgress),
                    "cancelled" => Some(TaskState::Cancelled),
                    "other" => Some(TaskState::Other),
                    _ => None,
                },
                "todo, done, in_progress, cancelled, other",
            )?,
            due_before: parse_day("due_before", due_before)?,
            due_after: parse_day("due_after", due_after)?,
            scheduled_before: parse_day("scheduled_before", scheduled_before)?,
            scheduled_after: parse_day("scheduled_after", scheduled_after)?,
            tag,
            path,
            priority: parse_list(
                "priority",
                priority,
                |s| match s {
                    "highest" => Some(Priority::Highest),
                    "high" => Some(Priority::High),
                    "medium" => Some(Priority::Medium),
                    "normal" => Some(Priority::Normal),
                    "low" => Some(Priority::Low),
                    "lowest" => Some(Priority::Lowest),
                    _ => None,
                },
                "highest, high, medium, normal, low, lowest",
            )?,
            text,
        };
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tasks = query_tasks(&manager, &query).await.map_err(to_mcp_error)?;
        let request = PageRequest::new(offset, limit, DEFAULT_PAGE_SIZE, MAX_REPORT_ITEMS);
        let (tasks, page) = request.slice(tasks);
        StandardResponse::new(vault_name, "query_tasks", tasks)
            .with_page(page)
            .with_next_step("edit_note")
            .to_json()
    }

    /// Get or create a periodic note
    #[tool(
        description = "Find the daily, weekly, monthly, quarterly or yearly note for a date, named and placed by the vault's own Daily notes or Periodic Notes settings, and return its markdown and hash. With create, a missing note is made from the configured template, its {{date}}, {{title}} and {{time}} variables filled; an existing note is never replaced.",
        title = "Get Periodic Note",
        tags = ["write", "periodic"],
        read_only = false,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<PeriodicNote>,
    )]
    async fn get_periodic_note(
        &self,
        #[description("daily, weekly, monthly, quarterly or yearly.")] period: String,
        #[description("A date in the period, YYYY-MM-DD. Default: today, on the server's clock.")]
        date: Option<String>,
        #[description(
            "True to create the note from its template when it doesn't exist. Default false, which only reports where it would be."
        )]
        create: Option<bool>,
        #[description(
            "Commit message on a Git-backed vault when creating. Required there when the vault sets git.require_commit_message; otherwise derived from the path."
        )]
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let period = match period.trim().to_lowercase().as_str() {
            "daily" => Period::Daily,
            "weekly" => Period::Weekly,
            "monthly" => Period::Monthly,
            "quarterly" => Period::Quarterly,
            "yearly" => Period::Yearly,
            other => {
                return Err(McpError::invalid_params(format!(
                    "period {other:?} is not one of daily, weekly, monthly, quarterly, yearly"
                )));
            }
        };
        let now = chrono::Local::now().naive_local();
        let date = parse_day("date", date)?.unwrap_or(now.date());
        let create = create.unwrap_or(false);
        let manager = self.get_active_vault_manager().await?;
        let vault_name = manager.vault_name().to_string();
        // A lookup writes nothing, so it needs no commit message even where
        // the vault requires one.
        let message = if create {
            let settings =
                periodic::settings(manager.vault_path(), period).map_err(to_mcp_error)?;
            let start = periodic::period_start(period, date, &settings.format);
            let path = periodic::note_path(&settings, start).map_err(to_mcp_error)?;
            self.resolve_commit_message(commit_message, || format!("create periodic note {path}"))
                .await?
        } else {
            String::new()
        };
        let note = get_periodic_note(&manager, period, date, now, create, &message)
            .await
            .map_err(to_mcp_error)?;
        if note.created {
            self.after_write_one(
                &vault_name,
                VaultChange::written(&note.path, false),
                WriteAttribution::host("get_periodic_note"),
            )
            .await;
        }
        StandardResponse::new(vault_name, "get_periodic_note", note)
            .with_next_step("edit_note")
            .to_json()
    }
}
