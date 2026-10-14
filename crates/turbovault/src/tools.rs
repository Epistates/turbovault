//! MCP tool implementations for Obsidian vault

use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use turbomcp::prelude::*;
use turbovault_audit::{AuditFilter, AuditLog, OperationType, SnapshotStore};
use turbovault_core::ServerConfig;
use turbovault_core::config::{GitMergeStrategy as ConfigMergeStrategy, VaultConfig, WriteBackend};
use turbovault_core::error::{Error, ErrorCategory};
use turbovault_core::events::{VaultChange, VaultEventSink, WriteAttribution};
use turbovault_core::prelude::MultiVaultManager;
use turbovault_tools::{
    AnalysisTools, AuditTools, BatchOperation, BatchTools, CommitLocks, DiffTools, DuplicateTools,
    ExportTools, FanoutInfo, FileTools, GitMergeStrategy, GraphTools, GroundingTools,
    MetadataTools, OkfTools, QualityTools, RelationshipTools, SearchEngine, SearchQuery,
    SearchTools, SimilarityEngine, SliceResult, SliceSpec, TemplateEngine, VaultLifecycleTools,
    VaultRepo, VaultRootPolicy, ViewerTools, WriteMode, obsidian_uri, slice_content,
};
use turbovault_vault::{ChangeListener, CommitOrigin, VaultManager};

mod core_prompts;
mod engine_slots;
mod note_resources;
mod providers;
pub use providers::ObsidianMcpServer;

use engine_slots::EngineSlots;

#[cfg(feature = "plugin-api")]
mod plugin_host;
#[cfg(feature = "plugin-api")]
mod plugin_storage;

#[cfg(feature = "sql")]
use turbovault_tools::FrontmatterSqlEngine;

/// A frontmatter key-value filter for advanced search
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct FrontmatterFilter {
    /// Frontmatter key to match (e.g. "type", "status", "project")
    pub key: String,
    /// Value to match against (substring match)
    pub value: String,
}

/// Turn a domain error into the error a tool returns.
///
/// The MCP error kind follows [`Error::category`], so a client can tell a
/// missing note from bad input from a fault in the server (TurboMCP carries
/// the kind in the result's `_meta`, and `data` below with it). The message
/// says what to do next where there is something to do, since a model reads
/// it to decide its next call. Paths in it are made vault-relative, and a
/// missing note gets suggestions, where the result leaves the server (see
/// `ObsidianMcpServer::call_tool`): only that knows which vault is meant.
fn to_mcp_error(e: Error) -> McpError {
    tracing::warn!("MCP error: {:?}", e);
    let category = e.category();
    let text = e.to_string();
    let message = match error_hint(&e) {
        Some(hint) => format!("{text}. {hint}"),
        None => text,
    };
    let mut data = serde_json::json!({ "category": category_name(category) });
    if let Some(path) = e.path() {
        data["path"] = serde_json::json!(turbovault_core::path_to_slash(path));
    }
    let error = match category {
        ErrorCategory::NotFound => {
            McpError::new(turbomcp_core::error::ErrorKind::ResourceNotFound, message)
        }
        // A stale `expected_hash` is a request to retry with different
        // arguments, which is what invalid params tells a client.
        ErrorCategory::InvalidInput | ErrorCategory::Conflict => McpError::invalid_params(message),
        ErrorCategory::PermissionDenied => McpError::permission_denied(message),
        ErrorCategory::Unavailable => McpError::unavailable(message),
        _ => McpError::internal(message),
    };
    error.with_data(data)
}

/// The snake_case name an [`ErrorCategory`] goes by in error `data`.
fn category_name(category: ErrorCategory) -> &'static str {
    match category {
        ErrorCategory::NotFound => "not_found",
        ErrorCategory::InvalidInput => "invalid_input",
        ErrorCategory::PermissionDenied => "permission_denied",
        ErrorCategory::Conflict => "conflict",
        ErrorCategory::Unavailable => "unavailable",
        _ => "internal",
    }
}

/// What a caller can do about `error`, for the ones where that is known.
fn error_hint(error: &Error) -> Option<String> {
    if matches!(error, Error::OperationNotAllowed { .. }) {
        return Some(
            "The vault's allowed_operations setting permits only the operations listed. The \
             server's operator can change it; until then, use another vault for this change"
                .to_string(),
        );
    }
    let hint = match error {
        Error::FileNotFound { .. } => {
            "Paths are relative to the vault root and include the extension, such as \
             `projects/Plan.md`. Use search to find a note by name"
        }
        Error::PathTraversalAttempt { .. } => {
            "Use a path relative to the vault root, with no leading `/` and no `..` that leaves \
             the vault"
        }
        Error::ProtectedPath { component, .. } => {
            return Some(format!(
                "`{component}` holds {}, so the note tools never read or write it",
                protected_contents(component)
            ));
        }
        Error::FileTooLarge { .. } => {
            "The server's max_file_size setting caps what the note tools read and write"
        }
        Error::ConcurrencyError { .. } => {
            "The note changed since you read it. Read it again for its current hash, then retry \
             with that hash as expected_hash"
        }
        _ => return None,
    };
    Some(hint.to_string())
}

/// `text` with every path under one of `roots` written relative to it.
fn relativize_paths(text: &str, roots: &[PathBuf]) -> String {
    let mut text = text.to_string();
    for root in roots {
        let root = root.to_string_lossy();
        let root = root.trim_end_matches(['/', '\\']);
        if root.is_empty() {
            continue;
        }
        for separator in ['/', '\\'] {
            text = text.replace(&format!("{root}{separator}"), "");
        }
    }
    text
}

/// The `limit` paths among `candidates` whose file names are closest to the
/// one `missing` names, closest first. A same-named note in another folder
/// ranks first; nothing that is not at least a near match is offered.
fn closest_paths(missing: &str, candidates: &[String], limit: usize) -> Vec<String> {
    fn stem(path: &str) -> String {
        let name = path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(path)
            .to_lowercase();
        name.strip_suffix(".md").map(str::to_string).unwrap_or(name)
    }
    let wanted = stem(missing);
    if wanted.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(f64, &String)> = candidates
        .iter()
        .filter_map(|candidate| {
            let score = strsim::normalized_levenshtein(&wanted, &stem(candidate));
            (score >= 0.6).then_some((score, candidate))
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, path)| path.clone())
        .collect()
}

/// What a protected directory holds, for [`error_hint`].
fn protected_contents(component: &str) -> &'static str {
    match component.to_ascii_lowercase().as_str() {
        ".obsidian" => "Obsidian's settings and plugin code",
        ".git" => "the vault's Git repository",
        ".turbovault" => "TurboVault's audit trail and state",
        _ => "files this vault's excluded_paths keep out of reach",
    }
}

/// Most results a search-style tool returns in one call.
const MAX_SEARCH_RESULTS: usize = 100;
/// Most items a vault-wide report returns in one call.
const MAX_REPORT_ITEMS: usize = 500;
/// Most audit entries `audit_log` returns in one call.
const MAX_AUDIT_ENTRIES: usize = 1000;

/// A result-count argument as the tool uses it: `default` when absent, and
/// otherwise at most `max`. A huge value can overflow arithmetic or ask for
/// an allocation that aborts the whole process, so no count reaches the
/// engines unclamped. Zero still means no results; a tool whose engine
/// can't take zero answers it without asking.
fn clamp_count(requested: Option<usize>, default: usize, max: usize) -> usize {
    requested.unwrap_or(default).min(max)
}

/// How much a tool that offers `response_format` puts in its result.
/// `detailed` is everything, and the default, so a caller that doesn't ask
/// gets what it always has; `concise` leaves out the parts the tool names in
/// its parameter description, for an agent that only needs the gist.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ResponseFormat {
    Concise,
    #[default]
    Detailed,
}

impl ResponseFormat {
    fn is_concise(format: Option<Self>) -> bool {
        format.unwrap_or_default() == Self::Concise
    }
}

/// How many items a list-returning tool gives back when the caller doesn't
/// say: enough for an overview, and never megabytes by accident.
const DEFAULT_PAGE_SIZE: usize = 50;
/// Furthest a ranked list (search results, suggestions) can be paged into.
const MAX_RANKED_OFFSET: usize = 1000;

/// Where one page of a list sits in the whole, reported under
/// `meta.pagination`. Every list-returning tool takes the same two inputs:
/// `limit` (how many to return, capped per tool) and `offset` (how many to
/// skip, default 0). `next_offset` is the `offset` that returns the next
/// page, and is absent on the last one. `total` is absent where counting
/// every match would cost what the tool exists to avoid, such as a ranked
/// search.
#[derive(serde::Serialize, schemars::JsonSchema, Debug, Clone, Copy, PartialEq, Eq)]
struct Page {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    total: Option<usize>,
    offset: usize,
    limit: usize,
    returned: usize,
    truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_offset: Option<usize>,
}

/// The `offset` and `limit` a tool asked for, resolved: `limit` defaults to
/// `default` and is capped at `max`, as [`clamp_count`] does.
#[derive(Debug, Clone, Copy)]
struct PageRequest {
    offset: usize,
    limit: usize,
}

impl PageRequest {
    fn new(offset: Option<usize>, limit: Option<usize>, default: usize, max: usize) -> Self {
        Self {
            offset: offset.unwrap_or(0),
            limit: clamp_count(limit, default, max),
        }
    }

    /// A request against a ranked source, which has to produce every item
    /// up to the end of the page: the offset stops at [`MAX_RANKED_OFFSET`]
    /// so no request can make it rank without bound.
    fn ranked(offset: Option<usize>, limit: Option<usize>, default: usize, max: usize) -> Self {
        let mut request = Self::new(offset, limit, default, max);
        request.offset = request.offset.min(MAX_RANKED_OFFSET);
        request
    }

    /// How many leading items a ranked source must produce to fill this
    /// page and tell whether another follows.
    fn fetch(&self) -> usize {
        self.offset.saturating_add(self.limit).saturating_add(1)
    }

    /// This page of `items`, the whole list.
    fn slice<T>(&self, items: Vec<T>) -> (Vec<T>, Page) {
        let total = items.len();
        self.take(items, Some(total))
    }

    /// This page of `items`, the first [`Self::fetch`] items of a list whose
    /// full length is unknown.
    fn slice_ranked<T>(&self, items: Vec<T>) -> (Vec<T>, Page) {
        self.take(items, None)
    }

    /// This page of `items`, the first [`Self::fetch`] items of a list of
    /// `total` items.
    fn slice_leading<T>(&self, items: Vec<T>, total: usize) -> (Vec<T>, Page) {
        self.take(items, Some(total))
    }

    /// Cut the array under `key` in `data` to this page. `ranked` says the
    /// array holds only the first [`Self::fetch`] items of a longer list.
    fn slice_field(&self, data: &mut serde_json::Value, key: &str, ranked: bool) -> Page {
        let items = match data.get_mut(key).map(serde_json::Value::take) {
            Some(serde_json::Value::Array(items)) => items,
            _ => Vec::new(),
        };
        let (items, page) = if ranked {
            self.slice_ranked(items)
        } else {
            self.slice(items)
        };
        data[key] = serde_json::Value::Array(items);
        page
    }

    fn take<T>(&self, items: Vec<T>, total: Option<usize>) -> (Vec<T>, Page) {
        let mut rest = items.into_iter().skip(self.offset);
        let page: Vec<T> = rest.by_ref().take(self.limit).collect();
        let more = rest.next().is_some();
        let info = Page {
            total,
            offset: self.offset,
            limit: self.limit,
            returned: page.len(),
            truncated: more,
            next_offset: more.then(|| self.offset + self.limit),
        };
        (page, info)
    }
}

/// Render the OKF orientation block for `get_vault_context`: a compact signal
/// telling the agent whether this vault is an OKF bundle, where the
/// progressive-disclosure entry point is, and what its type vocabulary looks
/// like. `None` (reported as `null`) when the vault is not an OKF bundle.
fn okf_context_block(bundle: &turbovault_core::okf::BundleInfo) -> Option<OkfContext> {
    if !bundle.is_okf_bundle {
        return None;
    }
    Some(OkfContext {
        is_okf_bundle: true,
        concept_docs: bundle.concept_docs,
        concept_ratio: (bundle.concept_ratio * 100.0).round() / 100.0,
        entry_point: bundle.has_root_index.then_some("index.md"),
        has_root_log: bundle.has_root_log,
        top_types: bundle
            .top_types
            .iter()
            .take(8)
            .map(|(type_, count)| OkfTypeCount {
                type_: type_.clone(),
                count: *count,
            })
            .collect(),
        guidance: if bundle.has_root_index {
            "This vault is an OKF bundle. Read index.md first and follow its links (progressive disclosure, OKF §6) instead of blind search."
        } else {
            "This vault is an OKF bundle but has no root index.md. Run generate_index to create progressive-disclosure entry points (OKF §6)."
        },
        tools: ["okf_validate", "generate_index", "append_log_entry"],
    })
}

/// How to work with a vault that is an OKF bundle, as `get_vault_context`
/// reports it under `okf`.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct OkfContext {
    is_okf_bundle: bool,
    concept_docs: usize,
    concept_ratio: f64,
    entry_point: Option<&'static str>,
    has_root_log: bool,
    top_types: Vec<OkfTypeCount>,
    guidance: &'static str,
    tools: [&'static str; 3],
}

/// One OKF concept `type` and how many notes carry it.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct OkfTypeCount {
    #[serde(rename = "type")]
    type_: String,
    count: usize,
}

/// turbovault-0bh: auto-derive a commit subject from a batch's op tally.
/// Used as the fallback when the caller doesn't supply `commit_message`.
/// Example: `batch: 5 creates, 2 updates, 1 delete`.
fn derive_batch_message(operations: &[turbovault_tools::BatchOperation]) -> String {
    use turbovault_tools::BatchOperation;
    let mut creates = 0u32;
    let mut updates = 0u32;
    let mut deletes = 0u32;
    let mut moves = 0u32;
    let mut link_updates = 0u32;
    let mut edits = 0u32;
    let mut fm_updates = 0u32;
    let mut tag_updates = 0u32;
    let mut template_creates = 0u32;
    for op in operations {
        match op {
            BatchOperation::CreateNote { .. } => creates += 1,
            BatchOperation::WriteNote { .. } => updates += 1,
            BatchOperation::DeleteNote { .. } => deletes += 1,
            BatchOperation::MoveNote { .. } => moves += 1,
            BatchOperation::UpdateLinks { .. } => link_updates += 1,
            BatchOperation::EditNote { .. } => edits += 1,
            BatchOperation::UpdateFrontmatter { .. } => fm_updates += 1,
            BatchOperation::ManageTags { .. } => tag_updates += 1,
            BatchOperation::CreateFromTemplate { .. } => template_creates += 1,
        }
    }
    let pluralize = |n: u32, word: &str| -> String {
        if n == 1 {
            format!("1 {}", word)
        } else {
            format!("{} {}s", n, word)
        }
    };
    let mut parts = Vec::new();
    if creates > 0 {
        parts.push(pluralize(creates, "create"));
    }
    if updates > 0 {
        parts.push(pluralize(updates, "update"));
    }
    if deletes > 0 {
        parts.push(pluralize(deletes, "delete"));
    }
    if moves > 0 {
        parts.push(pluralize(moves, "move"));
    }
    if link_updates > 0 {
        parts.push(pluralize(link_updates, "link update"));
    }
    if edits > 0 {
        parts.push(pluralize(edits, "edit"));
    }
    if fm_updates > 0 {
        parts.push(pluralize(fm_updates, "frontmatter update"));
    }
    if tag_updates > 0 {
        parts.push(pluralize(tag_updates, "tag update"));
    }
    if template_creates > 0 {
        parts.push(pluralize(template_creates, "template create"));
    }
    if parts.is_empty() {
        // Should be unreachable — empty batches are rejected upstream.
        "batch_execute (0 ops)".to_string()
    } else {
        format!("batch: {}", parts.join(", "))
    }
}

/// Map one batch operation to the change it makes, for the vault event feed.
///
/// Derived from the typed operation rather than the result record because a
/// record only reports an operation name and the paths it touched — enough to
/// say "something happened here", not enough to distinguish a creation from a
/// deletion.
fn batch_operation_change(operation: &turbovault_tools::BatchOperation) -> VaultChange {
    use turbovault_tools::BatchOperation;
    match operation {
        BatchOperation::CreateNote { path, .. }
        | BatchOperation::CreateFromTemplate { path, .. } => {
            VaultChange::Created { path: path.clone() }
        }
        BatchOperation::DeleteNote { path, .. } => VaultChange::Deleted { path: path.clone() },
        BatchOperation::MoveNote { from, to, .. } => VaultChange::Renamed {
            from: from.clone(),
            to: to.clone(),
        },
        // An upsert whose prior state the batch does not report back; treat it
        // as a modification, which tells a consumer to re-read either way.
        BatchOperation::WriteNote { path, .. }
        | BatchOperation::EditNote { path, .. }
        | BatchOperation::UpdateFrontmatter { path, .. }
        | BatchOperation::ManageTags { path, .. } => VaultChange::Modified { path: path.clone() },
        BatchOperation::UpdateLinks { file, .. } => VaultChange::Modified { path: file.clone() },
    }
}

/// Standardized response envelope for all tools (LLMX improvement)
/// Generic, non-cumbersome, forward-looking design
///
/// Each tool advertises `StandardResponse<Payload>` as its outputSchema, so
/// the schema of `data` is the schema of the payload type it returns.
#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema, Debug, Clone)]
pub struct StandardResponse<T: serde::Serialize> {
    /// Which vault this operation was performed on
    pub vault: String,
    /// Operation name for context (e.g., "read_note", "search")
    pub operation: String,
    /// Was the operation successful?
    pub success: bool,
    /// The actual result data (any type)
    pub data: T,
    /// Count of items in result (if applicable)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    /// How long the operation took in milliseconds
    pub took_ms: u64,
    /// Non-fatal warnings or notes (e.g., "Note had duplicate links")
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Suggested next operations based on result (e.g., ["write_note", "search"])
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_steps: Vec<String>,
    /// Flexible metadata object for forward-looking extensibility
    /// Can include: version, timestamp, correlation_id, suggestions, deprecation notices, etc.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub meta: serde_json::Map<String, serde_json::Value>,
}

/// The payload of a tool whose whole answer is that it did the thing, such
/// as `{"status": "removed"}`.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct StatusPayload {
    status: &'static str,
}

/// The outputSchema of a tool that returns one page of a list: the
/// [`StandardResponse`] envelope with `meta.pagination` spelled out, which
/// such a tool always sets. Only its schema is used; the field list mirrors
/// `StandardResponse`, and the output schema test checks every paged tool's
/// real result against it.
#[derive(serde::Serialize, schemars::JsonSchema)]
#[allow(dead_code)]
pub(crate) struct Paged<T: serde::Serialize> {
    vault: String,
    operation: String,
    success: bool,
    data: T,
    /// The number of items in this page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    count: Option<usize>,
    took_ms: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    next_steps: Vec<String>,
    meta: PagedMeta,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct PagedMeta {
    pagination: Page,
    /// Anything else the tool reports about the call.
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

impl<T: serde::Serialize> StandardResponse<T> {
    /// Create a new standard response (accepts `Into<String>` for flexibility)
    pub fn new(vault: impl Into<String>, operation: impl Into<String>, data: T) -> Self {
        Self {
            vault: vault.into(),
            operation: operation.into(),
            success: true,
            data,
            count: None,
            took_ms: 0,
            warnings: vec![],
            next_steps: vec![],
            meta: serde_json::Map::new(),
        }
    }

    /// Set item count
    pub fn with_count(mut self, count: usize) -> Self {
        self.count = Some(count);
        self
    }

    /// Set operation time
    pub fn with_duration(mut self, ms: u64) -> Self {
        self.took_ms = ms;
        self
    }

    /// Add a warning
    pub fn with_warning(mut self, warning: impl Into<String>) -> Self {
        self.warnings.push(warning.into());
        self
    }

    /// Add suggested next step
    pub fn with_next_step(mut self, step: impl Into<String>) -> Self {
        self.next_steps.push(step.into());
        self
    }

    /// Add metadata value (forward-looking extensibility)
    pub fn with_meta(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.meta.insert(key.into(), value);
        self
    }

    /// Set success flag
    pub fn with_success(mut self, success: bool) -> Self {
        self.success = success;
        self
    }

    /// Record which page of a longer list `data` is: `count` becomes the
    /// number returned, `meta.pagination` says where it sits in the whole,
    /// and a cut list gets a warning naming the offset to continue from.
    fn with_page(mut self, page: Page) -> Self {
        self.count = Some(page.returned);
        if let Some(next) = page.next_offset {
            let of = page
                .total
                .map_or_else(String::new, |total| format!(" of {total}"));
            self.warnings.push(format!(
                "Showing {}{of}. Call again with offset {next} for more",
                page.returned
            ));
        }
        self.meta
            .insert("pagination".to_string(), serde_json::json!(page));
        self
    }

    /// Shorthand for serializing to JSON with consistent error handling
    pub fn to_json(self) -> McpResult<serde_json::Value> {
        serde_json::to_value(self).map_err(|e| McpError::internal(e.to_string()))
    }

    /// Add multiple next steps at once (reduces boilerplate)
    pub fn with_next_steps(mut self, steps: &[&str]) -> Self {
        for step in steps {
            self.next_steps.push(step.to_string());
        }
        self
    }

    /// Add common next step pattern for read operations
    pub fn with_read_next_steps(self) -> Self {
        self.with_next_steps(&["write_note", "get_backlinks"])
    }

    /// Add common next step pattern for write operations
    pub fn with_write_next_steps(self) -> Self {
        self.with_next_steps(&["read_note", "query_metadata"])
    }

    /// Add common next step pattern for search operations
    pub fn with_search_next_steps(self) -> Self {
        self.with_next_steps(&["advanced_search", "recommend_related"])
    }

    /// Add common next step pattern for analysis operations
    pub fn with_analysis_next_steps(self) -> Self {
        self.with_next_steps(&["quick_health_check", "full_health_analysis"])
    }
}

/// A long tool call's progress through its stages, reported to a client
/// that sent a progress token, and the points where a client's cancellation
/// is honoured.
///
/// Cancellation is checked only at the start of a stage that has not changed
/// anything yet, so a cancelled call either did nothing or finished; a stage
/// that writes is announced with [`Stages::report`], which never stops.
pub(super) struct Stages<'a> {
    ctx: &'a RequestContext,
    total: f64,
    done: f64,
}

impl<'a> Stages<'a> {
    /// A call of `total` stages.
    pub(super) fn new(ctx: &'a RequestContext, total: usize) -> Self {
        Self {
            ctx,
            total: total as f64,
            done: 0.0,
        }
    }

    /// Begin the next stage, or stop if the client cancelled the call.
    pub(super) async fn next(&mut self, message: &str) -> McpResult<()> {
        if self.ctx.is_cancelled() {
            return Err(McpError::cancelled(
                "The client cancelled the call before it changed anything",
            ));
        }
        self.report(message).await;
        Ok(())
    }

    /// Begin the next stage without stopping for a cancellation, for a stage
    /// that follows a change already made.
    pub(super) async fn report(&mut self, message: &str) {
        // Progress is optional; a client that can't take it is no reason to
        // fail the call.
        let _ = self
            .ctx
            .report_progress(self.done, Some(self.total), Some(message))
            .await;
        self.done += 1.0;
    }

    /// Every stage is done.
    pub(super) async fn finish(self) {
        let _ = self
            .ctx
            .report_progress(self.total, Some(self.total), Some("Done"))
            .await;
    }
}

/// Shared implementation handler used by the focused provider facade.
#[derive(Clone)]
pub(super) struct CoreToolHandler {
    multi_vault_mgr: Arc<MultiVaultManager>,
    /// Cache of vault managers by vault name to persist state across calls
    vault_managers: Arc<RwLock<HashMap<String, Arc<VaultManager>>>>,
    /// Cache for persisting vault state across server restarts (project-aware)
    persistent_cache: Arc<RwLock<Option<turbovault_core::cache::VaultCache>>>,
    /// Audit logs per vault (keyed by vault name)
    audit_logs: Arc<RwLock<HashMap<String, Arc<AuditLog>>>>,
    /// Snapshot stores per vault (keyed by vault name)
    snapshot_stores: Arc<RwLock<HashMap<String, Arc<SnapshotStore>>>>,
    /// Similarity engines per vault (keyed by vault name, lazy-initialized)
    similarity_engines: Arc<EngineSlots<SimilarityEngine>>,
    /// Search engines per vault (keyed by vault name, lazy-initialized)
    search_engines: Arc<EngineSlots<SearchEngine>>,
    /// Shared per-vault `CommitLocks` registries for the git substrate
    /// (keyed by vault name, lazy-initialized). `VaultRepo` itself is `!Sync`
    /// (libgit2 raw pointers), so we cache the lock registry — which IS
    /// `Send + Sync` — and open a fresh `VaultRepo` per call via
    /// `open_with_locks(...)`. That keeps all in-process callers serialized
    /// on one commit-section mutex per worktree at trivial open cost.
    git_locks: Arc<RwLock<HashMap<String, Arc<CommitLocks>>>>,
    /// GWS.13 active fanouts, keyed by **base vault name**
    /// (the original vault, NOT the auto-registered fanout vault). At most
    /// one active fanout per base vault — `begin_fanout` errors loudly
    /// on a second concurrent attempt.
    active_fanouts: Arc<RwLock<HashMap<String, ActiveFanoutRecord>>>,
    /// Where observed vault changes are reported.
    ///
    /// Installed once during server assembly (before any provider clones this
    /// handler) and shared by every clone, so the write paths do not need to
    /// know whether a consumer exists. `None` until installed, and no sink at
    /// all when the server is built without the plugin boundary.
    event_sink: Arc<std::sync::OnceLock<Arc<dyn VaultEventSink>>>,
    /// Which directories `add_vault` and `create_vault` may register. Set
    /// once at startup from `--allowed-vault-roots` and the transport, and
    /// shared by every clone.
    vault_root_policy: Arc<std::sync::RwLock<VaultRootPolicy>>,
    /// Notes published as MCP resources, and the sessions subscribed to them
    /// or listing them. Fed by every vault's change listener.
    resource_hub: Arc<note_resources::ResourceHub>,
    /// A task checking subscribed vaults for outside edits is running.
    subscription_sweep: Arc<std::sync::atomic::AtomicBool>,
    /// Ask the user before a destructive call, when the client can ask.
    confirm_destructive: Arc<std::sync::atomic::AtomicBool>,
}

/// One row of `ObsidianMcpServer.active_fanouts`.
#[derive(Debug, Clone)]
struct ActiveFanoutRecord {
    fanout_id: String,
    info: FanoutInfo,
    /// Auto-registered transient vault name (e.g. `<base>-fanout-<fanout_id>`)
    /// that subagents `set_active_vault` to during the fanout.
    fanout_vault_name: String,
}

/// Shared setup for the `write_note` operation, which is implemented at two
/// provider boundaries — the MCP handler and the plugin `VaultHost`. Bundling
/// active-vault selection + commit-message resolution here keeps both from
/// re-deriving it.
pub(super) struct CompleteNoteWrite {
    pub(super) vault_name: String,
    pub(super) manager: Arc<VaultManager>,
    pub(super) message: String,
}

impl CoreToolHandler {
    /// Create a new server instance (vault-agnostic - no vault required at startup)
    pub fn new() -> Result<Self> {
        Self::with_config(ServerConfig::default())
    }

    /// Like [`Self::new`], with `config` as the base every vault manager is
    /// built from (a profile's [`turbovault_core::ConfigProfile::create_config`]).
    /// Its `vaults` are ignored; vaults are registered at runtime.
    pub fn with_config(mut config: ServerConfig) -> Result<Self> {
        config.vaults.clear();
        let mgr = MultiVaultManager::empty(config)?;
        Ok(Self {
            multi_vault_mgr: Arc::new(mgr),
            vault_managers: Arc::new(RwLock::new(HashMap::new())),
            persistent_cache: Arc::new(RwLock::new(None)),
            audit_logs: Arc::new(RwLock::new(HashMap::new())),
            snapshot_stores: Arc::new(RwLock::new(HashMap::new())),
            similarity_engines: Arc::default(),
            search_engines: Arc::default(),
            git_locks: Arc::new(RwLock::new(HashMap::new())),
            active_fanouts: Arc::new(RwLock::new(HashMap::new())),
            event_sink: Arc::new(std::sync::OnceLock::new()),
            vault_root_policy: Arc::new(std::sync::RwLock::new(VaultRootPolicy::anywhere())),
            resource_hub: Arc::default(),
            subscription_sweep: Arc::default(),
            confirm_destructive: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        })
    }

    /// Restrict which directories `add_vault` and `create_vault` accept.
    pub fn set_vault_root_policy(&self, policy: VaultRootPolicy) {
        *self
            .vault_root_policy
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = policy;
    }

    /// The policy `add_vault` and `create_vault` check paths against.
    pub fn vault_root_policy(&self) -> VaultRootPolicy {
        self.vault_root_policy
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Whether destructive calls ask the user first, through the client's
    /// elicitation support. On by default; a client that can't ask is never
    /// asked either way.
    pub fn set_confirm_destructive(&self, enabled: bool) {
        self.confirm_destructive
            .store(enabled, std::sync::atomic::Ordering::Release);
    }

    /// Ask the user to confirm `action` before a destructive call goes
    /// ahead, when the client supports form elicitation. A client that
    /// doesn't, or a server with confirmation turned off, goes ahead as the
    /// call's own flags say, exactly as before. A user who declines, or an
    /// answer that can't be had, stops the call with nothing changed.
    pub(super) async fn confirm_destructive(
        &self,
        ctx: &RequestContext,
        action: String,
    ) -> McpResult<()> {
        if !self
            .confirm_destructive
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Ok(());
        }
        let Some(session) = ctx.session() else {
            return Ok(());
        };
        let can_ask = matches!(
            session.client_capabilities().await,
            Ok(Some(capabilities))
                if capabilities
                    .elicitation
                    .as_ref()
                    .is_some_and(|elicitation| elicitation.supports_form())
        );
        if !can_ask {
            return Ok(());
        }
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "confirm": {
                    "type": "boolean",
                    "title": "Go ahead",
                    "description": "Check to go ahead.",
                    "default": false,
                }
            },
            "required": ["confirm"],
        });
        let answer = ctx
            .elicit_form(action.clone(), schema)
            .await
            .map_err(|error| {
                McpError::user_rejected(format!(
                    "Couldn't confirm with the user ({error}), so nothing was changed"
                ))
            })?;
        let confirmed = answer.action == turbomcp_types::ElicitAction::Accept
            && answer
                .content
                .as_ref()
                .and_then(|content| content.get("confirm"))
                .and_then(serde_json::Value::as_bool)
                == Some(true);
        if confirmed {
            Ok(())
        } else {
            Err(McpError::user_rejected(
                "The user declined, so nothing was changed. Ask them before trying again",
            ))
        }
    }

    /// Lifecycle tools that check paths against this server's root policy.
    pub(super) fn lifecycle_tools(&self) -> VaultLifecycleTools {
        VaultLifecycleTools::new(self.multi_vault_mgr.clone())
            .with_root_policy(self.vault_root_policy())
    }

    /// Install the sink that observed vault changes are reported to.
    ///
    /// Call before any provider clones this handler. Subsequent calls are
    /// ignored — a server has one change feed for its whole lifetime.
    #[cfg(feature = "plugin-api")]
    pub(super) fn set_event_sink(&self, sink: Arc<dyn VaultEventSink>) {
        let _ = self.event_sink.set(sink);
    }

    /// Initialize the persistent cache (should be called after server creation)
    pub async fn init_cache(&self) -> Result<()> {
        let cache = turbovault_core::cache::VaultCache::init().await?;
        let mut cache_lock = self.persistent_cache.write().await;
        *cache_lock = Some(cache);
        Ok(())
    }

    /// Get the multi-vault manager
    pub fn multi_vault(&self) -> Arc<MultiVaultManager> {
        self.multi_vault_mgr.clone()
    }

    /// turbovault-z5c: lift the `add_vault` MCP tool's auto-init logic
    /// to a public method so the CLI `--init` flag can actually
    /// initialize registered vaults at startup. Walks every registered
    /// vault, constructs its VaultManager (scans files + builds link
    /// graph), and caches it. Errors on the first failure.
    pub async fn initialize_registered_vaults(&self) -> Result<()> {
        let vaults = self
            .multi_vault_mgr
            .list_vaults()
            .await
            .map_err(|e| Error::config_error(format!("list_vaults: {}", e)))?;
        for vault_info in vaults {
            let name = vault_info.name.clone();
            // Skip if already cached (idempotent for sequential calls).
            {
                let cache = self.vault_managers.read().await;
                if cache.contains_key(&name) {
                    continue;
                }
            }
            self.activate_vault_manager(&name)
                .await
                .map_err(|e| Error::config_error(e.to_string()))?;
            tracing::info!("Initialized vault '{}' (--init)", name);
        }
        Ok(())
    }

    /// Helper to save vault state to cache. A fanout's transient vault is
    /// not saved: it is gone once the fanout ends, and a restored one would
    /// point at a scratch worktree that no longer exists. While one is
    /// active, its base vault is saved as the active vault instead.
    async fn persist_vault_state(&self) -> Result<()> {
        if let Some(cache) = self.persistent_cache.read().await.as_ref() {
            let fanouts = self.active_fanouts.read().await;
            let base_of = |name: &str| {
                fanouts
                    .iter()
                    .find(|(_, record)| record.fanout_vault_name == name)
                    .map(|(base, _)| base.clone())
            };
            let vault_configs: Vec<_> = self
                .multi_vault_mgr
                .list_vaults()
                .await?
                .into_iter()
                .filter(|vault| base_of(&vault.name).is_none())
                .map(|vault| vault.config)
                .collect();
            let active_vault = self.multi_vault_mgr.get_active_vault().await;
            let active_vault = base_of(&active_vault).unwrap_or(active_vault);

            cache.save_vaults(&vault_configs, &active_vault).await?;
            tracing::debug!("Vault state persisted to cache");
        }
        Ok(())
    }

    /// Drop a removed vault from the cache. Saving the remaining vaults
    /// would not do it, since a save keeps vaults it doesn't list.
    async fn forget_vault_state(&self, name: &str) -> Result<()> {
        if let Some(cache) = self.persistent_cache.read().await.as_ref() {
            let active_vault = self.multi_vault_mgr.get_active_vault().await;
            cache.remove_vault(name, &active_vault).await?;
        }
        Ok(())
    }

    /// turbovault-84k: scan every git-backend vault (or just `vault_filter`
    /// if specified) for `wip-*` worktrees that aren't backed by an entry
    /// in `active_fanouts`. Pure read; never mutates. Used by the startup
    /// warning and the `list_orphan_fanouts` MCP tool.
    pub async fn scan_orphan_fanouts(
        &self,
        vault_filter: Option<&str>,
    ) -> Result<Vec<OrphanFanoutEntry>> {
        let vaults = self.multi_vault_mgr.list_vaults().await?;
        let active_worktree_names: std::collections::HashSet<String> = {
            let guard = self.active_fanouts.read().await;
            guard
                .values()
                .map(|r| r.info.worktree_name.clone())
                .collect()
        };
        let mut out = Vec::new();
        for entry in vaults {
            let cfg = entry.config.clone();
            if let Some(filter) = vault_filter
                && cfg.name != filter
            {
                continue;
            }
            if !matches!(cfg.write_backend, WriteBackend::Git) {
                continue;
            }
            let locks = self.get_or_init_git_locks(&cfg.name).await;
            let cfg_name = cfg.name.clone();
            let path = cfg.path.clone();
            let active_clone = active_worktree_names.clone();
            let found: Result<Vec<OrphanFanoutEntry>> =
                tokio::task::spawn_blocking(move || -> Result<Vec<OrphanFanoutEntry>> {
                    let repo = VaultRepo::open_with_locks(&path, locks)
                        .map_err(|e| anyhow::anyhow!("open vault {}: {}", cfg_name, e))?;
                    Ok(repo
                        .list_orphan_fanouts()
                        .map_err(|e| anyhow::anyhow!("list_orphan_fanouts({}): {}", cfg_name, e))?
                        .into_iter()
                        .filter(|o| !active_clone.contains(&o.worktree_name))
                        .map(|o| OrphanFanoutEntry {
                            vault_name: cfg_name.clone(),
                            worktree_name: o.worktree_name,
                            wip_branch: o.wip_branch,
                            worktree_path: o.worktree_path,
                        })
                        .collect())
                })
                .await
                .map_err(|e| anyhow::anyhow!("scan_orphan_fanouts task: {}", e))?;
            out.extend(found?);
        }
        Ok(out)
    }

    /// turbovault-84k: best-effort `abandon_fanout_by_info` over every
    /// entry in `active_fanouts`. Called from the SIGTERM/SIGINT handler.
    /// Logs each result, then clears the registry. Survives individual
    /// failures so a slow vault can't block shutdown.
    pub async fn shutdown_fanouts_best_effort(&self) {
        let snapshot: Vec<(String, ActiveFanoutRecord)> = {
            let guard = self.active_fanouts.read().await;
            guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        };
        if snapshot.is_empty() {
            return;
        }
        tracing::info!("shutdown: abandoning {} active fanout(s)", snapshot.len());
        let vaults = match self.multi_vault_mgr.list_vaults().await {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!("shutdown: list_vaults: {}", e);
                return;
            }
        };
        for (base_vault, record) in snapshot {
            let Some(base_cfg) = vaults
                .iter()
                .find(|v| v.config.name == base_vault)
                .map(|v| v.config.clone())
            else {
                tracing::warn!(
                    "shutdown: base vault {} disappeared, skipping abandon",
                    base_vault
                );
                continue;
            };
            let locks = self.get_or_init_git_locks(&base_vault).await;
            let info = record.info.clone();
            let path = base_cfg.path.clone();
            let join = tokio::task::spawn_blocking(move || -> Result<()> {
                let repo = VaultRepo::open_with_locks(&path, locks)
                    .map_err(|e| anyhow::anyhow!("open base vault: {}", e))?;
                repo.abandon_fanout_by_info(&info)
                    .map_err(|e| anyhow::anyhow!("abandon_fanout_by_info: {}", e))
            })
            .await;
            match join {
                Ok(Ok(())) => tracing::info!(
                    "shutdown: abandoned fanout fanout_id={} on base vault {}",
                    record.fanout_id,
                    base_vault
                ),
                Ok(Err(e)) => tracing::warn!(
                    "shutdown: failed to abandon fanout fanout_id={} on {}: {}",
                    record.fanout_id,
                    base_vault,
                    e
                ),
                Err(e) => {
                    tracing::warn!("shutdown: abandon task panicked for {}: {}", base_vault, e)
                }
            }
            if let Err(e) = self
                .multi_vault_mgr
                .remove_vault(&record.fanout_vault_name)
                .await
            {
                tracing::warn!(
                    "shutdown: failed to deregister fanout vault {}: {}",
                    record.fanout_vault_name,
                    e
                );
            }
        }
        self.active_fanouts.write().await.clear();
    }

    /// turbovault-84k: startup hook — scan + log a warning per orphan
    /// detected. Operator decides whether to clean up (manual `git
    /// worktree remove` / `git branch -D`, or call `list_orphan_fanouts`
    /// MCP tool for inspection).
    pub async fn log_orphan_fanouts_warnings(&self) {
        match self.scan_orphan_fanouts(None).await {
            Ok(orphans) if orphans.is_empty() => {}
            Ok(orphans) => {
                tracing::warn!(
                    "Startup: detected {} orphan fanout worktree(s). Inspect via list_orphan_fanouts MCP tool; clean up with `git worktree remove <name>` + `git branch -D wip/<id>`.",
                    orphans.len()
                );
                for o in &orphans {
                    tracing::warn!(
                        "  orphan: vault={} worktree={} branch={} path={}",
                        o.vault_name,
                        o.worktree_name,
                        o.wip_branch,
                        o.worktree_path.display()
                    );
                }
            }
            Err(e) => tracing::warn!("Startup orphan fanout scan failed: {}", e),
        }
    }
}

/// turbovault-84k: one fanout artifact found by [`ObsidianMcpServer::
/// scan_orphan_fanouts`]. Wraps the substrate's `OrphanFanout` with the
/// owning vault's name (which the substrate alone can't know).
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct OrphanFanoutEntry {
    pub vault_name: String,
    pub worktree_name: String,
    pub wip_branch: String,
    pub worktree_path: PathBuf,
}

impl Default for CoreToolHandler {
    fn default() -> Self {
        Self::new().expect("Failed to create default CoreToolHandler")
    }
}

impl CoreToolHandler {
    /// Attach and cache audit state for a newly constructed vault manager.
    async fn initialize_audit_for_manager(&self, vault_name: &str, manager: &mut VaultManager) {
        let vault_path = manager.vault_path().clone();
        match AuditLog::new(&vault_path).await {
            Ok(audit_log) => {
                let audit_log = Arc::new(audit_log);
                let retention = self
                    .multi_vault_mgr
                    .server_config()
                    .snapshot_retention
                    .clone();
                let snapshot_store = Arc::new(
                    SnapshotStore::new(audit_log.snapshot_dir().to_path_buf())
                        .with_retention(retention),
                );
                manager.set_audit_log(audit_log.clone(), snapshot_store.clone());

                self.audit_logs
                    .write()
                    .await
                    .insert(vault_name.to_string(), audit_log);
                self.snapshot_stores
                    .write()
                    .await
                    .insert(vault_name.to_string(), snapshot_store);
            }
            Err(error) => {
                tracing::warn!(
                    "Failed to initialize audit log for {}: {} (audit trail disabled)",
                    vault_name,
                    error
                );
            }
        }
    }

    /// Get a vault manager for the currently active vault (cached)
    async fn get_active_vault_manager(&self) -> McpResult<Arc<VaultManager>> {
        let vault_name = self.get_active_vault_name().await?;

        // Check cache first
        {
            let cache = self.vault_managers.read().await;
            if let Some(manager) = cache.get(&vault_name) {
                return Ok(manager.clone());
            }
        }

        // Not in cache - build, wire and publish it
        self.activate_vault_manager(&vault_name).await
    }

    /// Finish a failed tool result for the model that reads it. A path under
    /// a registered vault is written relative to it, so no absolute server
    /// path leaves the server, and a note that was not found gets up to
    /// three close matches from the active vault. Other results pass through.
    async fn polish_error_result(&self, mut result: ToolResult) -> ToolResult {
        if result.is_error != Some(true) {
            return result;
        }
        let roots = self.vault_roots().await;
        for content in &mut result.content {
            if let Content::Text(text) = content {
                text.text = relativize_paths(&text.text, &roots);
            }
        }

        let Some(data) = result
            .meta
            .as_mut()
            .and_then(|meta| meta.get_mut(turbomcp_core::meta_keys::ERROR_DATA))
        else {
            return result;
        };
        let Some(path) = data
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(|path| relativize_paths(path, &roots))
        else {
            return result;
        };
        data["path"] = serde_json::json!(path);
        if data["category"] != "not_found" {
            return result;
        }
        let suggestions = self.closest_note_paths(&path).await;
        if suggestions.is_empty() {
            return result;
        }
        data["suggestions"] = serde_json::json!(suggestions);
        if let Some(Content::Text(text)) = result.content.first_mut() {
            text.text = format!("{}. Did you mean: {}?", text.text, suggestions.join(", "));
        }
        result
    }

    /// Write a successful result's text block as compact JSON. TurboMCP
    /// renders a JSON result twice, as `structuredContent` and as a
    /// pretty-printed text copy for clients that read only the text; the
    /// indentation is a third or more of that copy's tokens and tells a
    /// model nothing. `structuredContent` is left exactly as it was.
    fn compact_json_text(mut result: ToolResult) -> ToolResult {
        if result.is_error == Some(true) {
            return result;
        }
        let Some(structured) = result.structured_content.as_ref() else {
            return result;
        };
        if let [Content::Text(text)] = result.content.as_mut_slice()
            && let Ok(compact) = serde_json::to_string(structured)
        {
            text.text = compact;
        }
        result
    }

    /// Every registered vault's root, as configured and as it resolves.
    async fn vault_roots(&self) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        for vault in self.multi_vault_mgr.list_vaults().await.unwrap_or_default() {
            if let Ok(real) = vault.path.canonicalize()
                && real != vault.path
            {
                roots.push(real);
            }
            roots.push(vault.path);
        }
        roots
    }

    /// Up to three notes in the active vault whose names are closest to
    /// `missing`, for a not-found error.
    async fn closest_note_paths(&self, missing: &str) -> Vec<String> {
        let Ok(manager) = self.get_active_vault_manager().await else {
            return Vec::new();
        };
        let Ok(files) = manager.scan_vault().await else {
            return Vec::new();
        };
        let candidates: Vec<String> = files
            .iter()
            .map(|file| manager.relative_path(file))
            .collect();
        closest_paths(missing, &candidates, 3)
    }

    /// turbovault-qae.5.5: THE activation path — the ONE place that builds a
    /// `VaultManager` and publishes it into `vault_managers`.
    ///
    /// Build → audit → initialize → wire → publish is a single operation, so a
    /// caller cannot publish an unwired manager: it never gets its hands on a
    /// manager that is not already wired and cached. That is the structural
    /// form of the bite-3a (turbovault-qae.5.3) fix, which was previously a
    /// three-step dance open-coded at every insert site — a git-backend manager
    /// cached before its reindex machinery was wired makes
    /// `get_active_vault_manager` cache-hit the unwired manager forever, and
    /// search staleness never closes. Every new call site gets the wiring for
    /// free; there is no site left that could forget it.
    async fn activate_vault_manager(&self, vault_name: &str) -> McpResult<Arc<VaultManager>> {
        let vault_config = self
            .multi_vault_mgr
            .get_vault_config(vault_name)
            .await
            .map_err(|e| McpError::internal(format!("No config for vault '{vault_name}': {e}")))?;

        let mut server_config = self.multi_vault_mgr.server_config().clone();
        let mut vault_config = vault_config;
        vault_config.is_default = true; // Mark as default so VaultManager::new() can find it
        server_config.vaults = vec![vault_config];

        let mut manager = VaultManager::new(server_config).map_err(|e| {
            McpError::internal(format!(
                "Failed to create vault manager for '{vault_name}': {e}"
            ))
        })?;

        // Audit state attaches through `&mut` — it has to land between `new()`
        // and the `Arc`, which is exactly what used to force each call site
        // into its own multi-step publish. Owning the whole sequence here is
        // what resolves it.
        self.initialize_audit_for_manager(vault_name, &mut manager)
            .await;

        // Initialize vault (scan files and build link graph) on first access
        manager.initialize().await.map_err(|e| {
            McpError::internal(format!("Failed to initialize vault '{vault_name}': {e}"))
        })?;

        let manager = Arc::new(manager);

        // M4c (bite 3a, turbovault-qae.5.3): wire the manager-owned reindex
        // tasks + change-listener (git vaults; no-op on Direct) BEFORE
        // publishing to the cache, so no concurrent first-access caller can
        // cache-hit and observe an unwired manager. If we lose the
        // double-check race below, our wired manager is dropped and its `Drop`
        // aborts the tasks it started.
        // Renamed from `wire_manager_reindex`: with the freshness gate this
        // wires the change-listener that feeds search, similarity and the
        // plugin feed, not only the git reindex drainer.
        self.wire_manager_indexes(vault_name, &manager).await;

        // Publish — double-check to handle concurrent initialization races
        let mut cache = self.vault_managers.write().await;
        // Another task may have activated between our read-check and here; first writer wins
        if let Some(existing) = cache.get(vault_name) {
            return Ok(existing.clone());
        }
        cache.insert(vault_name.to_string(), manager.clone());

        Ok(manager)
    }

    /// Wiring for a freshly-built manager: register the R7 change-listener that
    /// feeds THIS server's tantivy + similarity engines and the plugin change
    /// feed, and on a git vault start the manager's own reindex tasks (drainer +
    /// HEAD-ref listener).
    ///
    /// The listener is registered on **every** backend. It used to be git-only,
    /// on the reasoning that only a commit could change the vault behind our
    /// back, and that was never true: an Obsidian save does not commit, and a
    /// Direct vault has no commits at all. The manager's freshness sweep reports
    /// through this same listener, so registering it everywhere is what lets an
    /// external edit reach the search index and the plugin feed regardless of
    /// which backend the vault is on. write-substrate-layering M4e deleted the
    /// server's own duplicate reindex machinery (the old `GitFileTools` path),
    /// so this is the ONLY such wiring.
    async fn wire_manager_indexes(&self, vault_name: &str, manager: &Arc<VaultManager>) {
        // Register the R7 change-listener BEFORE starting the drainer, so a
        // drain pass can't fire into an unset listener. The listener captures
        // ONLY the search + similarity engine maps — NOT a whole `self` clone,
        // which would hold `vault_managers` → the `Arc<VaultManager>` → this
        // listener: a reference cycle that defeats the manager's `Drop`-based
        // task teardown (the manager would never drop absent an explicit
        // `remove_vault`, undoing the `Weak<Self>` care the tasks take), and
        // which would also let this listener re-enter the freshness gate that
        // awaits it. Those two maps never hold the manager, so neither is
        // possible; search / similarity stay ABOVE the vault layer — the
        // manager only invokes this callback (R2 dependency inversion).
        let search_engines = Arc::clone(&self.search_engines);
        let similarity_engines = Arc::clone(&self.similarity_engines);
        // Publish the vault's notes as resources before the listener can
        // report a change to them. The hub holds no manager either.
        let resource_hub = Arc::clone(&self.resource_hub);
        resource_hub.seed(
            vault_name,
            manager
                .cached_notes()
                .await
                .iter()
                .map(|note| manager.relative_path(&note.path)),
        );
        // The sink holds only the hook bus, never a manager, so capturing it
        // does not reintroduce the reference cycle the maps above avoid.
        #[cfg(feature = "plugin-api")]
        let event_sink = Arc::clone(&self.event_sink);
        let name = vault_name.to_string();
        let listener: ChangeListener =
            Arc::new(move |changed: Vec<(String, bool, CommitOrigin)>| {
                // Announce the changes this process did not make. A local write was
                // already reported at the write site, with the attribution only
                // that site knows; reporting it again here would double-report
                // every mutation TurboVault performs.
                #[cfg(feature = "plugin-api")]
                if let Some(sink) = event_sink.get() {
                    for (path, present, origin) in &changed {
                        if !matches!(origin, CommitOrigin::External) {
                            continue;
                        }
                        // Neither a commit diff nor a freshness sweep says
                        // whether a path existed before, only whether it is
                        // there now, so an external creation arrives as
                        // `Modified`. Consumers that maintain derived state
                        // treat the two identically.
                        let change = if *present {
                            VaultChange::Modified { path: path.clone() }
                        } else {
                            VaultChange::Deleted { path: path.clone() }
                        };
                        sink.publish(&name, change, None, WriteAttribution::default());
                    }
                }
                let changed: Vec<(String, bool)> = changed
                    .into_iter()
                    .map(|(path, present, _)| (path, present))
                    .collect();
                // Every change, this process's or another editor's, is news
                // to a subscriber.
                resource_hub.apply(&name, &changed);
                let search_engines = Arc::clone(&search_engines);
                let similarity_engines = Arc::clone(&similarity_engines);
                let name = name.clone();
                // Handed back rather than spawned: the manager awaits this
                // before its freshness gate returns, so a caller that gated on
                // the link graph cannot then read a search index that is still
                // catching up. Spawning here would leave exactly that gap.
                Box::pin(async move {
                    if changed.is_empty() {
                        return;
                    }
                    // A build in progress keeps the report and applies it
                    // when it publishes, so nothing reported mid-build is lost.
                    if let Some(engine) = search_engines.report(&name, &changed)
                        && let Err(e) = engine.apply_changes(changed.clone()).await
                    {
                        tracing::warn!(
                            "change-listener: search apply failed for '{name}', evicting: {e}"
                        );
                        search_engines.remove(&name);
                    }
                    // The similarity engine takes the same report: only the
                    // changed notes are re-tokenized, never the vault.
                    if let Some(engine) = similarity_engines.report(&name, &changed)
                        && let Err(e) = engine.apply_changes(changed).await
                    {
                        tracing::warn!(
                            "change-listener: similarity apply failed for '{name}', evicting: {e}"
                        );
                        similarity_engines.remove(&name);
                    }
                })
            });
        manager.set_change_listener(listener);

        let is_git = self
            .multi_vault_mgr
            .get_vault_config(vault_name)
            .await
            .map(|c| matches!(c.write_backend, WriteBackend::Git))
            .unwrap_or(false);
        if is_git {
            // Start the background drainer + HEAD-ref listener only after the
            // change-listener is registered. No-op on a Direct vault, which has
            // no queue to drain.
            manager.ensure_reindex_started();
        }
    }

    /// Get audit tools for the active vault
    async fn get_audit_tools(&self) -> McpResult<AuditTools> {
        let vault_name = self.get_active_vault_name().await?;
        // Ensure vault manager exists (triggers audit log creation)
        let _ = self.get_active_vault_manager().await?;

        let logs = self.audit_logs.read().await;
        let stores = self.snapshot_stores.read().await;

        let audit_log = logs.get(&vault_name).cloned().ok_or_else(|| {
            McpError::internal("Audit log not available for this vault".to_string())
        })?;
        let snapshot_store = stores
            .get(&vault_name)
            .cloned()
            .ok_or_else(|| McpError::internal("Snapshot store not available".to_string()))?;

        Ok(AuditTools::new(audit_log, snapshot_store))
    }

    /// Get or build search engine for a given vault (cached, lazy-initialized)
    async fn get_search_engine(
        &self,
        vault_name: &str,
        manager: &Arc<VaultManager>,
    ) -> McpResult<Arc<SearchEngine>> {
        // Gate before the cache check, not after, and before the cold build
        // too. Both branches have to reflect the same observation of the vault:
        // a cold build reads disk and is fresh by construction, so skipping the
        // gate there would leave the index ahead of the link graph instead of
        // behind it. Incoherent either way. The manager's change-listener
        // applies whatever moved to the engine below before this returns.
        manager.ensure_fresh().await;

        // One build per vault however many callers arrive at once; the build
        // opens the vault's persisted index and re-indexes what changed since
        // it was written, or indexes the entire vault when there is none.
        let (engine, missed) = self
            .search_engines
            .get_or_build(vault_name, || SearchEngine::for_vault(manager.clone()))
            .await
            .map_err(|e| McpError::internal(format!("Failed to build search engine: {}", e)))?;
        // Notes that changed after the build read them.
        if let Err(e) = engine.apply_changes(missed).await {
            tracing::warn!("search engine for '{vault_name}' missed changes, evicting: {e}");
            self.search_engines.remove(vault_name);
        }
        Ok(engine)
    }

    /// Get or build similarity engine for the active vault
    async fn get_similarity_engine(&self) -> McpResult<Arc<SimilarityEngine>> {
        let vault_name = self.get_active_vault_name().await?;
        let manager = self.get_active_vault_manager().await?;
        // Gated for the same reason as the search engine: an external edit
        // invalidates this cache entry, and only the sweep can notice.
        manager.ensure_fresh().await;

        let (engine, missed) = self
            .similarity_engines
            .get_or_build(&vault_name, || SimilarityEngine::new(manager))
            .await
            .map_err(|e| McpError::internal(format!("Failed to build similarity engine: {}", e)))?;
        // Notes that changed after the build read them.
        if let Err(e) = engine.apply_changes(missed).await {
            tracing::warn!("similarity engine for '{vault_name}' missed changes, evicting: {e}");
            self.similarity_engines.remove(&vault_name);
        }
        Ok(engine)
    }

    /// Look up a registered vault's configuration by name.
    ///
    /// Unlike the active-vault helpers, this answers for a vault the caller
    /// names — which is what any operation scoped to a specific vault needs.
    #[cfg(feature = "plugin-api")]
    async fn vault_config_by_name(&self, vault_name: &str) -> McpResult<VaultConfig> {
        self.multi_vault_mgr
            .get_vault_config(vault_name)
            .await
            .map_err(|e| {
                McpError::new(
                    turbomcp_core::error::ErrorKind::ResourceNotFound,
                    format!("no vault named {vault_name:?}: {e}. Use list_vaults to see the names"),
                )
            })
    }

    /// Helper to get active vault name
    async fn get_active_vault_name(&self) -> McpResult<String> {
        let vault_name = self.multi_vault_mgr.get_active_vault().await;
        if vault_name.is_empty() {
            return Err(McpError::unavailable(
                "No active vault. Call list_vaults and then set_active_vault, or add_vault to \
                 register one"
                    .to_string(),
            ));
        }
        Ok(vault_name)
    }

    /// turbovault-5nn: true when the active vault is on the git backend AND has
    /// `git.require_commit_message = true`. Only meaningful on git (the direct
    /// backend produces no commits).
    async fn active_vault_requires_commit_message(&self) -> bool {
        self.multi_vault_mgr
            .get_active_vault_config()
            .await
            .map(|c| {
                matches!(c.write_backend, WriteBackend::Git)
                    && c.git
                        .as_ref()
                        .map(|g| g.require_commit_message)
                        .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    /// write-substrate-layering M4d: true when the active vault is on the git
    /// backend. The manager already dispatches the substrate per write; this is
    /// only for the handlers' backend-dependent DEFAULTS (delete_note's
    /// force-by-default, move_note's update-backlinks-by-default) which differ
    /// by backend and must be preserved (R10).
    async fn active_vault_is_git(&self) -> McpResult<bool> {
        Ok(self
            .multi_vault_mgr
            .get_active_vault_config()
            .await
            .map(|c| matches!(c.write_backend, WriteBackend::Git))
            .unwrap_or(false))
    }

    /// turbovault-qae.5.2: check-only half of the commit-message gate —
    /// trims/filters the caller-supplied message (whitespace-only counts as
    /// missing) and, when none remains, refuses if the active vault requires
    /// one. Returns `Ok(None)` when no message was given and none is
    /// required, leaving the fallback to the caller. Single-write tools go
    /// through `resolve_commit_message` below; multi-write tools (e.g.
    /// `generate_index`, which can't collapse several auto-derived subjects
    /// into one fallback) call this directly.
    async fn require_commit_message(
        &self,
        commit_message: Option<String>,
    ) -> McpResult<Option<String>> {
        let provided = commit_message
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty());
        match provided {
            Some(m) => Ok(Some(m)),
            None if self.active_vault_requires_commit_message().await => {
                Err(McpError::invalid_params(
                    "this vault requires an explicit commit message (git.require_commit_message = true); pass a non-empty `commit_message` for this operation".to_string(),
                ))
            }
            None => Ok(None),
        }
    }

    /// turbovault-5nn: resolve a mutation's commit subject. A caller-supplied
    /// message (trimmed; whitespace-only counts as missing) always wins. When
    /// none is given, the active vault's `git.require_commit_message` decides:
    /// `true` → refuse loudly so nothing is auto-derived; `false` → use
    /// `fallback` (the historical auto-derived subject).
    async fn resolve_commit_message(
        &self,
        commit_message: Option<String>,
        fallback: impl FnOnce() -> String,
    ) -> McpResult<String> {
        Ok(self
            .require_commit_message(commit_message)
            .await?
            .unwrap_or_else(fallback))
    }

    /// Resolve the shared state for a `write_note` (vault name, manager, commit
    /// message). Paired with [`Self::finish_complete_note_write`].
    async fn prepare_complete_note_write(
        &self,
        path: &str,
        commit_message: Option<String>,
        fallback_operation: &str,
    ) -> McpResult<CompleteNoteWrite> {
        // Resolve the manager first and take the vault name from it, so the
        // whole write sees ONE active-vault snapshot even if `set_active_vault`
        // races on another connection.
        let manager = self.get_active_vault_manager().await?;
        let vault_name = manager.vault_name().to_string();
        let message = self
            .resolve_commit_message(commit_message, || format!("{fallback_operation} {path}"))
            .await?;
        Ok(CompleteNoteWrite {
            vault_name,
            manager,
            message,
        })
    }

    /// Everything a completed mutation owes the rest of the server: report the
    /// change, with the attribution only the write site knows.
    ///
    /// Every write tool ends here. Derived state is not kept here: every write
    /// goes through `VaultManager`, whose change-listener (wired for every
    /// backend in `wire_manager_indexes`) has already applied the change to the
    /// search index and the similarity engine before the write returns. This
    /// used to evict both caches again as well, which threw away the search
    /// index the listener had just updated and made the next search rebuild it
    /// from every note in the vault.
    ///
    /// `vault_name` is the vault that was WRITTEN, which is not necessarily the
    /// active vault by the time this runs — `set_active_vault` may land between
    /// the mutation and the report.
    async fn after_write(
        &self,
        vault_name: &str,
        changes: impl IntoIterator<Item = VaultChange>,
        attribution: WriteAttribution,
    ) {
        let Some(sink) = self.event_sink.get() else {
            // No consumer configured; skip building envelopes nobody reads.
            return;
        };
        for change in changes {
            sink.publish(vault_name, change, None, attribution.clone());
        }
    }

    /// [`Self::after_write`] for a mutation that touched exactly one path.
    async fn after_write_one(
        &self,
        vault_name: &str,
        change: VaultChange,
        attribution: WriteAttribution,
    ) {
        self.after_write(vault_name, [change], attribution).await;
    }

    /// Whether `path` currently exists in `manager`'s vault.
    ///
    /// Used to report a write as a creation or a modification. A failed stat
    /// is reported as "exists" so an unreadable path degrades to the more
    /// conservative `Modified`.
    async fn path_exists(manager: &Arc<VaultManager>, path: &str) -> bool {
        match manager.resolve_path(Path::new(path)) {
            Ok(resolved) => tokio::fs::try_exists(&resolved).await.unwrap_or(true),
            Err(_) => true,
        }
    }

    /// Helper to get both vault name and manager (eliminates 31 repeated preambles)
    /// This is the most common pattern across all tools
    async fn get_vault_pair(&self) -> McpResult<(String, Arc<VaultManager>)> {
        let vault_name = self.get_active_vault_name().await?;
        let manager = self.get_active_vault_manager().await?;
        Ok((vault_name, manager))
    }
    // ==================== Test support (turbovault-6fo.18) ====================
    //
    // Public wrappers that expose internals the e2e tests need to drive
    // the substrate as a real MCP handler would. These mirror the
    // private helpers; suffixed `_test` to discourage production use.

    /// Test-only: expose the active vault's `VaultManager` so e2e tests
    /// can read the link graph after a substrate write.
    pub async fn get_active_vault_manager_test(&self) -> McpResult<Arc<VaultManager>> {
        self.get_active_vault_manager().await
    }

    /// Run the active vault's freshness gate, if there is one.
    ///
    /// Best effort on purpose. A request with no active vault, or one whose
    /// vault cannot be opened, is about to fail on its own terms with a better
    /// message than anything this could produce, and refusing to reconcile is
    /// not a reason to refuse the request.
    #[cfg(feature = "plugin-api")]
    pub(crate) async fn ensure_active_vault_fresh(&self) {
        if let Ok(manager) = self.get_active_vault_manager().await {
            manager.ensure_fresh().await;
        }
    }

    /// turbovault-5nn test-only: resolve a commit subject through the same
    /// require_commit_message gate the mutation tools use (eager `fallback`).
    pub async fn resolve_commit_message_test(
        &self,
        commit_message: Option<String>,
        fallback: String,
    ) -> McpResult<String> {
        self.resolve_commit_message(commit_message, || fallback)
            .await
    }

    /// Test-only: check whether a `CommitLocks` registry is cached for
    /// `vault_name`. Used by turbovault-1ne cleanup tests.
    pub async fn has_git_locks_test(&self, vault_name: &str) -> bool {
        self.git_locks.read().await.contains_key(vault_name)
    }

    /// Test-only: invoke the `remove_vault` MCP tool from integration
    /// tests. Used by turbovault-1ne tests.
    pub async fn remove_vault_test(&self, name: &str) -> McpResult<serde_json::Value> {
        self.remove_vault_with_cleanup(name).await
    }

    async fn remove_vault_with_cleanup(&self, name: &str) -> McpResult<serde_json::Value> {
        {
            let fanouts = self.active_fanouts.read().await;
            if let Some(record) = fanouts.get(name) {
                return Err(McpError::invalid_params(format!(
                    "vault {name} has an active fanout (fanout_id={}); abandon_fanout first",
                    record.fanout_id
                )));
            }
            if let Some(record) = fanouts
                .values()
                .find(|record| record.fanout_vault_name == name)
            {
                return Err(McpError::invalid_params(format!(
                    "vault {name} is a fanout worktree (fanout_id={}); abandon_fanout from the base vault first",
                    record.fanout_id
                )));
            }
        }

        VaultLifecycleTools::new(self.multi_vault_mgr.clone())
            .remove_vault(name)
            .await
            .map_err(to_mcp_error)?;
        self.search_engines.remove(name);
        self.similarity_engines.remove(name);
        self.vault_managers.write().await.remove(name);
        self.git_locks.write().await.remove(name);
        self.resource_hub.forget(name);

        if let Err(error) = self.forget_vault_state(name).await {
            tracing::warn!("Failed to persist vault state after removal: {error}");
        }
        StandardResponse::new(name, "remove_vault", StatusPayload { status: "removed" })
            .with_next_step("list_vaults")
            .to_json()
    }

    /// Test-only: expose the lazy `CommitLocks` initializer for tests
    /// that need to drive substrate operations against the same lock
    /// registry the server uses. Used by turbovault-1ne tests.
    pub async fn get_or_init_git_locks_test(&self, vault_name: &str) -> Arc<CommitLocks> {
        self.get_or_init_git_locks(vault_name).await
    }

    /// Test-only: install an `active_fanouts` entry so tests can
    /// observe the remove_vault refusal without driving the full
    /// `begin_fanout` MCP wire path. Used by turbovault-1ne.
    pub async fn register_active_fanout_test(
        &self,
        base_vault: &str,
        fanout_id: &str,
        info: FanoutInfo,
        fanout_vault_name: &str,
    ) {
        let rec = ActiveFanoutRecord {
            fanout_id: fanout_id.to_string(),
            info,
            fanout_vault_name: fanout_vault_name.to_string(),
        };
        self.active_fanouts
            .write()
            .await
            .insert(base_vault.to_string(), rec);
    }

    /// Test-only: clear the `active_fanouts` entry for `base_vault`.
    /// Used by turbovault-1ne cleanup.
    pub async fn clear_active_fanout_test(&self, base_vault: &str) {
        self.active_fanouts.write().await.remove(base_vault);
    }

    /// turbovault-8df / TV-009: legacy audit MCP tools (audit_log /
    /// audit_stats / rollback_preview / rollback_note) are not wired to
    /// the git substrate's history yet. On a git-backend vault they
    /// would silently return empty / inert results, which is the worst
    /// failure shape — looks like "nothing happened" but the writes did
    /// happen via the substrate. Refuse loudly instead, naming the
    /// alternative (git log) so callers know where to look.
    ///
    /// Phase A of the architecture §15.2 cutover plan; Phase B (wire
    /// rollback_note/rollback_preview to git-history restore) is part
    /// of GWS.15 cutover or a follow-on (turbovault-8df remediation
    /// notes).
    async fn refuse_audit_on_git_backend(&self, tool_name: &str) -> McpResult<()> {
        let cfg = self
            .multi_vault_mgr
            .get_active_vault_config()
            .await
            .map_err(|e| McpError::internal(format!("No active vault config: {}", e)))?;
        if cfg.write_backend == WriteBackend::Git {
            return Err(McpError::unavailable(format!(
                "{} is not wired for the git substrate (turbovault-8df). Use `git log` / `git revert` / `git show` from the vault directory directly — every substrate write is one commit with a clear subject. Audit ergonomics through MCP are a planned cutover deliverable; until then this tool returns nothing useful for write_backend=git.",
                tool_name
            )));
        }
        Ok(())
    }

    /// Compute a content-version token in the active vault's backend-native
    /// format. Returned by `read_note`'s `hash` field and accepted by
    /// `write_note`/`edit_note`/`delete_note`/`move_note`'s `expected_hash`
    /// param. The token a read RETURNS must be the token CAS ACCEPTS — that
    /// is the contract this helper closes for the git backend.
    ///
    /// - **Direct backend:** 64-char SHA-256 of the working-tree bytes (the
    ///   token `turbovault_vault::compute_hash` has always produced).
    /// - **Git backend:** 40-char hex git blob OID, matching `expect_blob`'s
    ///   tree-side precondition exactly.
    ///
    /// turbovault-6sj / TV-011 (pre-this-fix, `read_note` always returned the
    /// SHA-256, so a round-trip on the git backend failed with
    /// `expected_hash must be a 40-char git blob oid hex`).
    async fn hash_for_active_backend(&self, content: &str) -> McpResult<String> {
        Self::hash_for_backend(self.active_backend().await?, content)
    }

    /// The active vault's write backend.
    ///
    /// Split out from [`Self::hash_for_active_backend`] so a caller hashing
    /// many notes resolves the backend once instead of re-reading the vault
    /// config per note.
    async fn active_backend(&self) -> McpResult<WriteBackend> {
        self.multi_vault_mgr
            .get_active_vault_config()
            .await
            .map(|cfg| cfg.write_backend)
            .map_err(|e| McpError::internal(format!("No active vault config: {}", e)))
    }

    /// Compute a content-version token in `backend`'s native format.
    fn hash_for_backend(backend: WriteBackend, content: &str) -> McpResult<String> {
        match backend {
            WriteBackend::Direct => Ok(turbovault_vault::compute_hash(content)),
            WriteBackend::Git => VaultRepo::blob_oid_of(content.as_bytes())
                .map(|oid| oid.to_string())
                .map_err(|e| McpError::internal(format!("blob_oid_of failed: {}", e))),
            other => Err(McpError::internal(format!(
                "no content hash for the {} write backend",
                other.name()
            ))),
        }
    }

    async fn get_or_init_git_locks(&self, vault_name: &str) -> Arc<CommitLocks> {
        if let Some(l) = self.git_locks.read().await.get(vault_name) {
            return Arc::clone(l);
        }
        let fresh = Arc::new(CommitLocks::new());
        self.git_locks
            .write()
            .await
            .insert(vault_name.to_string(), Arc::clone(&fresh));
        fresh
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind_and_text(error: Error) -> (String, String, serde_json::Value) {
        let result = to_mcp_error(error).to_tool_result();
        let meta = result.meta.clone().unwrap_or_default();
        (
            meta["io.turbomcp/errorKind"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            result.first_text().unwrap_or_default().to_string(),
            meta["io.turbomcp/errorData"].clone(),
        )
    }

    #[test]
    fn a_refused_operation_names_allowed_operations() {
        let (kind, text, data) = kind_and_text(Error::operation_not_allowed(
            "vault 'notes' does not allow write (allowed_operations: read)",
        ));
        assert_eq!(kind, "permission_denied");
        assert_eq!(data["category"], "permission_denied");
        assert!(
            text.starts_with("vault 'notes' does not allow write"),
            "{text}"
        );
        assert!(text.contains("allowed_operations setting"), "{text}");
        assert!(!text.contains("Wrapped error"), "{text}");
    }

    #[test]
    fn a_stale_hash_is_a_conflict_with_a_retry_hint() {
        let (kind, text, data) =
            kind_and_text(Error::concurrency_error("hash mismatch on notes/a.md"));
        assert_eq!(kind, "invalid_params");
        assert_eq!(data["category"], "conflict");
        assert!(text.contains("Read it again"), "{text}");
    }

    #[test]
    fn a_fault_in_the_server_stays_internal() {
        let (kind, _, data) = kind_and_text(Error::other(
            "commit abc landed, but writing it to the working tree failed",
        ));
        assert_eq!(kind, "internal");
        assert_eq!(data["category"], "internal");
    }

    #[test]
    fn closest_paths_prefers_the_same_name_in_another_folder() {
        let candidates = [
            "projects/Plan.md".to_string(),
            "Plans.md".to_string(),
            "Plant care.md".to_string(),
            "Unrelated.md".to_string(),
        ];
        assert_eq!(
            closest_paths("Plan.md", &candidates, 3),
            ["projects/Plan.md", "Plans.md"]
        );
        assert!(closest_paths("zzz.md", &candidates, 3).is_empty());
    }

    #[test]
    fn relativize_paths_strips_every_vault_root() {
        let roots = [
            PathBuf::from("/srv/vault"),
            PathBuf::from(r"C:\Vaults\Work"),
        ];
        assert_eq!(
            relativize_paths(
                r"File not found: /srv/vault/notes/a.md and C:\Vaults\Work\b.md",
                &roots
            ),
            r"File not found: notes/a.md and b.md"
        );
    }
}
