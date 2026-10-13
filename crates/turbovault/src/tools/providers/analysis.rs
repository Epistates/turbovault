//! AnalysisProvider MCP capabilities.

use std::ops::Deref;

use turbovault_core::Precondition;
use turbovault_tools::okf_tools::LogEntryResult;
use turbovault_tools::{
    CompareResult, DiffResult, DuplicateGroup, GenerateIndexReport, GroundingAnalysis,
    OkfValidateReport, QualityScore, SimilarityResult, UngroundedReport, VaultQualityReport,
    VisualizationResult,
};

use super::super::*;

#[derive(Clone)]
pub(super) struct AnalysisProvider(CoreToolHandler);

impl AnalysisProvider {
    pub(super) fn new(core: CoreToolHandler) -> Self {
        Self(core)
    }
}

impl Deref for AnalysisProvider {
    type Target = CoreToolHandler;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Response payload for `visualize`.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct VisualizePayload {
    /// Vault-relative path of the HTML file written.
    output: String,
    summary: VisualizationResult,
}

#[turbomcp::server(name = "obsidian-vault", version = env!("CARGO_PKG_VERSION"))]
impl AnalysisProvider {
    // ─── DIFF TOOLS ──────────────────────────────────────────────────

    #[tool(
        description = "Diff two notes: a unified diff, line and word changes, and a similarity score. Use compare_notes for a similarity verdict without the diff.",
        title = "Diff Notes",
        tags = ["read"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<DiffResult>,
    )]
    async fn diff_notes(
        &self,
        #[description(
            "Vault-relative path of the first note, including the extension, such as `drafts/Plan.md`."
        )]
        left: String,
        #[description("Vault-relative path of the second note, including the extension.")]
        right: String,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = DiffTools::new(manager);
        let result = tools
            .diff_notes(&left, &right)
            .await
            .map_err(to_mcp_error)?;
        StandardResponse::new(&vault_name, "diff_notes", &result)
            .with_duration(start.elapsed().as_millis() as u64)
            .with_next_steps(&["read_note", "edit_note", "compare_notes"])
            .to_json()
    }

    #[tool(
        description = "Diff a note's current content against how it was before an operation in the audit trail. Find the operation_id with audit_log. Not available on Git-backed vaults, whose history is in Git.",
        title = "Diff Note Version",
        tags = ["read", "audit"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<DiffResult>,
    )]
    async fn diff_note_version(
        &self,
        #[description(
            "Vault-relative path of the note, including the extension, such as `projects/Plan.md`."
        )]
        path: String,
        #[description("The `id` of an audit_log entry that changed this note.")]
        operation_id: String,
    ) -> McpResult<serde_json::Value> {
        self.refuse_audit_on_git_backend("diff_note_version")
            .await?;
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let audit_tools = self.get_audit_tools().await?;

        // Get the snapshot from the audit entry
        let entry = audit_tools
            .audit_log()
            .get_entry(&operation_id)
            .await
            .map_err(to_mcp_error)?
            .ok_or_else(|| {
                McpError::new(
                    turbomcp_core::error::ErrorKind::ResourceNotFound,
                    format!(
                        "Audit entry not found: {operation_id}. Use audit_log to list operation ids"
                    ),
                )
            })?;

        let snapshot_id = entry
            .before_snapshot_id
            .as_ref()
            .or(entry.after_snapshot_id.as_ref())
            .ok_or_else(|| {
                McpError::new(
                    turbomcp_core::error::ErrorKind::ResourceNotFound,
                    "No snapshot was kept for this operation".to_string(),
                )
            })?;

        let snapshot_content = audit_tools
            .snapshot_store()
            .retrieve(snapshot_id)
            .await
            .map_err(to_mcp_error)?;

        // Read current content
        let current_content = manager
            .read_file(&std::path::PathBuf::from(&path))
            .await
            .map_err(to_mcp_error)?;

        let result = DiffTools::diff_content(
            &snapshot_content,
            &current_content,
            &format!(
                "{} (version {})",
                path,
                &operation_id[..8.min(operation_id.len())]
            ),
            &format!("{} (current)", path),
        );

        StandardResponse::new(&vault_name, "diff_note_version", &result)
            .with_duration(start.elapsed().as_millis() as u64)
            .with_next_steps(&["audit_log", "rollback_note", "read_note"])
            .to_json()
    }

    // ─── QUALITY TOOLS ───────────────────────────────────────────────

    #[tool(
        description = "Score one note's quality from 0 to 100 on readability, structure, completeness and staleness, plus a weighted overall score and suggestions to improve it.",
        title = "Evaluate Note Quality",
        tags = ["read", "health"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<QualityScore>,
    )]
    async fn evaluate_note_quality(
        &self,
        #[description(
            "Vault-relative path of the note, including the extension, such as `projects/Plan.md`."
        )]
        path: String,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = QualityTools::new(manager);
        let result = tools.evaluate_note(&path).await.map_err(to_mcp_error)?;
        StandardResponse::new(&vault_name, "evaluate_note_quality", &result)
            .with_duration(start.elapsed().as_millis() as u64)
            .with_next_steps(&["vault_quality_report", "edit_note", "find_stale_notes"])
            .to_json()
    }

    #[tool(
        description = "Score every note's quality and summarize: average and distribution, per-dimension averages, the lowest- and highest-scoring notes, and recommendations. Slow on a large vault; use evaluate_note_quality for one note.",
        title = "Vault Quality Report",
        tags = ["read", "health"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<VaultQualityReport>,
    )]
    async fn vault_quality_report(
        &self,
        #[description(
            "How many of the lowest- and highest-scoring notes to list. Default 10, maximum 500."
        )]
        bottom_n: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = QualityTools::new(manager);
        let result = tools
            .vault_quality_report(clamp_count(bottom_n, 10, MAX_REPORT_ITEMS))
            .await
            .map_err(to_mcp_error)?;
        StandardResponse::new(&vault_name, "vault_quality_report", &result)
            .with_count(result.total_notes)
            .with_duration(start.elapsed().as_millis() as u64)
            .with_next_steps(&["evaluate_note_quality", "find_stale_notes"])
            .to_json()
    }

    #[tool(
        description = "Extract what a judge needs to check a note for hallucination, contradiction or redundancy: the factual claims in its prose, its declared citations (a `# Citations` section), structural signals (Schema and Examples sections), and an `uncited` flag for claims with no citations. TurboVault doesn't score grounding itself.",
        title = "Analyze Note Grounding",
        tags = ["read", "quality", "okf"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<GroundingAnalysis>,
    )]
    async fn analyze_note_grounding(
        &self,
        #[description(
            "Vault-relative path of the note, including the extension, such as `concepts/Revenue.md`."
        )]
        path: String,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = GroundingTools::new(manager);
        let result = tools.analyze_note(&path).await.map_err(to_mcp_error)?;
        StandardResponse::new(&vault_name, "analyze_note_grounding", &result)
            .with_duration(start.elapsed().as_millis() as u64)
            .with_next_steps(&["find_ungrounded_notes", "okf_validate"])
            .to_json()
    }

    #[tool(
        description = "List notes that make factual claims but declare no citations (no `# Citations` section), most claims first. These are the notes most worth a grounding review; analyze_note_grounding shows one note's claims.",
        title = "Find Ungrounded Notes",
        tags = ["read", "quality", "okf"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = Paged<UngroundedReport>,
    )]
    async fn find_ungrounded_notes(
        &self,
        #[description("Most notes to return. Default 50, maximum 500.")] limit: Option<usize>,
        #[description(
            "How many notes to skip, for the next page. Default 0, at most 1000; use `meta.pagination.next_offset` from the previous call."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = GroundingTools::new(manager);
        let request = PageRequest::ranked(offset, limit, DEFAULT_PAGE_SIZE, MAX_REPORT_ITEMS);
        let mut result = tools
            .find_ungrounded_notes(request.fetch())
            .await
            .map_err(to_mcp_error)?;
        let (notes, page) =
            request.slice_leading(std::mem::take(&mut result.notes), result.ungrounded_count);
        result.notes = notes;
        StandardResponse::new(&vault_name, "find_ungrounded_notes", &result)
            .with_page(page)
            // This tool's `count` has always been every ungrounded note, not the
            // page; `meta.pagination.returned` is the page.
            .with_count(result.ungrounded_count)
            .with_duration(start.elapsed().as_millis() as u64)
            .with_next_steps(&["analyze_note_grounding", "read_note"])
            .to_json()
    }

    // ==================== Open Knowledge Format (OKF) ====================

    #[tool(
        description = "Validate the vault as an Open Knowledge Format (OKF) bundle: checks every note for OKF v0.1 conformance (parseable frontmatter with a non-empty `type`) and surfaces each concept's OKF metadata (type, title, description, resource, timestamp, citation count) plus the bundle's type vocabulary. Reports non-conformant files for use as a CI gate",
        title = "Validate OKF Bundle",
        tags = ["read", "okf", "frontmatter"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<OkfValidateReport>,
    )]
    async fn okf_validate(
        &self,
        #[description(
            "Vault-relative folder to check, such as `concepts/`. Omit to check the whole vault."
        )]
        subtree: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = OkfTools::new(manager);
        let result = tools
            .validate(subtree.as_deref())
            .await
            .map_err(to_mcp_error)?;

        let total = result.total;
        let conformant = result.non_conformant == 0;
        StandardResponse::new(&vault_name, "okf_validate", &result)
            .with_count(total)
            .with_success(conformant)
            .with_duration(start.elapsed().as_millis() as u64)
            .with_next_steps(&["generate_index", "read_note"])
            .to_json()
    }

    #[tool(
        description = "Write or refresh OKF index.md files: each indexed folder gets an index.md listing its notes (with their frontmatter descriptions) and subfolders, so a reader can navigate one level at a time. Replaces an existing index.md; an index that would not change is left alone.",
        title = "Generate OKF Index",
        tags = ["write", "okf"],
        read_only = false,
        destructive = true,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<GenerateIndexReport>,
    )]
    async fn generate_index(
        &self,
        #[description(
            "Vault-relative folder to index, such as `concepts/`. Omit for the vault root."
        )]
        directory: Option<String>,
        #[description("Also index every folder below it. Default false.")] recursive: Option<bool>,
        #[description(
            "Report the indexes that would be written without writing them. Default false."
        )]
        dry_run: Option<bool>,
        #[description(
            "Commit message for every index written, on a Git-backed vault. Required there when the vault sets git.require_commit_message; otherwise each index gets a derived message."
        )]
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let dry_run = dry_run.unwrap_or(false);
        let (vault_name, manager) = self.get_vault_pair().await?;
        // turbovault-qae.5.2: `generate_index` can write several index.md
        // files in one call (recursive), each with its own auto-derived
        // subject — so unlike a single-write tool this doesn't compute one
        // fallback subject via `resolve_commit_message` (that would flatten
        // every file's message to the same string). It still reuses
        // `require_commit_message` for the trim/filter/gate itself. A dry
        // run writes nothing (the tool layer below never reads
        // `commit_message` when `dry_run`), so the gate does not apply.
        let commit_message = if dry_run {
            commit_message
        } else {
            self.require_commit_message(commit_message).await?
        };
        let tools = OkfTools::new(manager);
        let result = tools
            .generate_index(
                directory.as_deref(),
                recursive.unwrap_or(false),
                dry_run,
                commit_message.as_deref(),
            )
            .await
            .map_err(to_mcp_error)?;

        let written = result
            .indexes
            .iter()
            .filter(|index| index.written)
            .map(|index| VaultChange::Modified {
                path: index.path.clone(),
            })
            .collect::<Vec<_>>();
        if !written.is_empty() {
            self.after_write(
                &vault_name,
                written,
                WriteAttribution::host("generate_index"),
            )
            .await;
        }

        let count = result.indexes.len();
        StandardResponse::new(&vault_name, "generate_index", &result)
            .with_count(count)
            .with_duration(start.elapsed().as_millis() as u64)
            .with_next_steps(&["okf_validate", "explain_vault"])
            .to_json()
    }

    #[tool(
        description = "Add an entry to a folder's OKF log.md history (OKF spec section 7), under a `## YYYY-MM-DD` heading with the newest date first: a new date becomes the top section, an existing date gains another bullet. Creates log.md if there isn't one.",
        title = "Append OKF Log Entry",
        tags = ["write", "okf"],
        read_only = false,
        destructive = false,
        idempotent = false,
        open_world = false,
        output_schema = StandardResponse<LogEntryResult>,
    )]
    async fn append_log_entry(
        &self,
        #[description("The entry's text, one line of prose.")] text: String,
        #[description(
            "Vault-relative folder whose log.md to append to, such as `concepts/`. Omit for the vault root."
        )]
        directory: Option<String>,
        #[description(
            "The entry's leading bold word, such as `Update`, `Creation` or `Deprecation`. Default `Update`."
        )]
        kind: Option<String>,
        #[description("The entry's date as `YYYY-MM-DD`. Default today, in local time.")]
        date: Option<String>,
        #[description(
            "Commit message on a Git-backed vault. Required there when the vault sets git.require_commit_message; otherwise derived from the log path."
        )]
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let message = self
            .resolve_commit_message(commit_message, || {
                format!(
                    "append_log_entry {}",
                    turbovault_tools::log_rel_for(directory.as_deref())
                )
            })
            .await?;
        let tools = OkfTools::new(manager);
        let result = tools
            .append_log_entry(
                directory.as_deref(),
                kind.as_deref(),
                &text,
                date.as_deref(),
                Some(&message),
            )
            .await
            .map_err(to_mcp_error)?;

        StandardResponse::new(&vault_name, "append_log_entry", &result)
            .with_duration(start.elapsed().as_millis() as u64)
            .with_next_steps(&["okf_validate", "generate_index"])
            .to_json()
    }

    #[tool(
        description = "Write the vault's link graph as one self-contained HTML page into the vault: an interactive graph of every note, a panel showing each note's rendered text and backlinks, and filtering and search. It opens in any browser with no server; the graph and markdown libraries load from a CDN.",
        title = "Visualize Vault",
        tags = ["write", "export", "okf"],
        read_only = false,
        destructive = true,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<VisualizePayload>,
    )]
    async fn visualize(
        &self,
        #[description(
            "Vault-relative path to write the HTML to. Default `viz.html`; an existing file there is replaced."
        )]
        output: Option<String>,
        #[description("Title shown in the page. Default the vault folder's name.")] name: Option<
            String,
        >,
        #[description(
            "Commit message on a Git-backed vault. Required there when the vault sets git.require_commit_message; otherwise derived from the output path."
        )]
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = ViewerTools::new(manager.clone());
        let (html, mut summary) = tools
            .generate(name.as_deref())
            .await
            .map_err(to_mcp_error)?;

        let out_rel = output.unwrap_or_else(|| "viz.html".to_string());
        let message = self
            .resolve_commit_message(commit_message, || format!("visualize {out_rel}"))
            .await?;
        manager
            .write_file(
                std::path::Path::new(&out_rel),
                &html,
                Precondition::for_replace(None, true),
                &message,
            )
            .await
            .map_err(to_mcp_error)?;

        // Refresh the displayed byte count from the written content.
        summary.html_bytes = html.len();

        StandardResponse::new(
            &vault_name,
            "visualize",
            VisualizePayload {
                output: out_rel,
                summary: summary.clone(),
            },
        )
        .with_count(summary.nodes)
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_step("explain_vault")
        .to_json()
    }

    #[tool(
        description = "List notes not modified for a number of days, least recently modified first, with their age. Use it to find notes due for review.",
        title = "Find Stale Notes",
        tags = ["read", "health"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = Paged<Vec<QualityScore>>,
    )]
    async fn find_stale_notes(
        &self,
        #[description(
            "Only notes not modified for at least this many days count as stale. Default 90."
        )]
        threshold_days: Option<u64>,
        #[description("Most notes to return. Default 20, maximum 500.")] limit: Option<usize>,
        #[description(
            "How many of the stalest notes to skip, for the next page. Default 0, at most 1000."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = QualityTools::new(manager);
        let request = PageRequest::ranked(offset, limit, 20, MAX_REPORT_ITEMS);
        let result = tools
            .find_stale_notes(threshold_days.unwrap_or(90), request.fetch())
            .await
            .map_err(to_mcp_error)?;
        let (result, page) = request.slice_ranked(result);
        StandardResponse::new(&vault_name, "find_stale_notes", &result)
            .with_page(page)
            .with_duration(start.elapsed().as_millis() as u64)
            .with_next_steps(&["evaluate_note_quality", "read_note", "edit_note"])
            .to_json()
    }

    // ─── SIMILARITY TOOLS ────────────────────────────────────────────

    #[tool(
        description = "Find notes about a topic even when they don't use your exact words, ranked by TF-IDF cosine similarity to the query. Use search for exact keywords and phrases.",
        title = "Semantic Search",
        tags = ["read", "search", "semantic"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = Paged<Vec<SimilarityResult>>,
    )]
    async fn semantic_search(
        &self,
        #[description("What to look for, in plain words, such as `quarterly revenue forecast`.")]
        query: String,
        #[description("Most notes to return. Default 10, maximum 100.")] limit: Option<usize>,
        #[description("How many top results to skip, for the next page. Default 0, at most 1000.")]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let vault_name = self.get_active_vault_name().await?;
        let engine = self.get_similarity_engine().await?;
        let request = PageRequest::ranked(offset, limit, 10, MAX_SEARCH_RESULTS);
        let (results, page) = request.slice_ranked(engine.semantic_search(&query, request.fetch()));
        StandardResponse::new(&vault_name, "semantic_search", &results)
            .with_page(page)
            .with_duration(start.elapsed().as_millis() as u64)
            .with_next_steps(&["read_note", "find_similar_notes", "advanced_search"])
            .to_json()
    }

    #[tool(
        description = "Find the notes whose content is most like a given note's, ranked by TF-IDF cosine similarity. Use find_duplicates to scan the whole vault for near copies.",
        title = "Find Similar Notes",
        tags = ["read", "search", "semantic"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = Paged<Vec<SimilarityResult>>,
    )]
    async fn find_similar_notes(
        &self,
        #[description(
            "Vault-relative path of the note to compare against, including the extension, such as `projects/Plan.md`."
        )]
        path: String,
        #[description("Most notes to return. Default 10, maximum 100.")] limit: Option<usize>,
        #[description("How many top results to skip, for the next page. Default 0, at most 1000.")]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let vault_name = self.get_active_vault_name().await?;
        let engine = self.get_similarity_engine().await?;
        let request = PageRequest::ranked(offset, limit, 10, MAX_SEARCH_RESULTS);
        let (results, page) =
            request.slice_ranked(engine.find_similar_notes(&path, request.fetch()));
        StandardResponse::new(&vault_name, "find_similar_notes", &results)
            .with_page(page)
            .with_duration(start.elapsed().as_millis() as u64)
            .with_next_steps(&["read_note", "semantic_search", "get_backlinks"])
            .to_json()
    }

    // ─── DUPLICATE TOOLS ─────────────────────────────────────────────

    #[tool(
        description = "Scan the whole vault for pairs of notes with nearly the same content, each with its similarity score, most similar first. Slow on a large vault; compare_notes checks one pair.",
        title = "Find Duplicates",
        tags = ["read", "search", "semantic"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<Vec<DuplicateGroup>>,
    )]
    async fn find_duplicates(
        &self,
        #[description(
            "Lowest similarity, from 0.0 to 1.0, for a pair to count as duplicates. Default 0.8."
        )]
        threshold: Option<f64>,
        #[description(
            "Stop after finding this many pairs. Default 20, maximum 500. The scan ends early, so it can't be paged; raise the limit to see more."
        )]
        limit: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = DuplicateTools::new(manager);
        let result = tools
            .find_duplicates(
                threshold.unwrap_or(0.8),
                clamp_count(limit, 20, MAX_REPORT_ITEMS),
            )
            .await
            .map_err(to_mcp_error)?;
        let count = result.len();
        StandardResponse::new(&vault_name, "find_duplicates", &result)
            .with_count(count)
            .with_duration(start.elapsed().as_millis() as u64)
            .with_next_steps(&["compare_notes", "diff_notes", "read_note"])
            .to_json()
    }

    #[tool(
        description = "Compare two notes for overlap: a similarity score, the terms they share, a short diff summary, and a recommendation such as merging them. Use diff_notes for the full line-by-line diff.",
        title = "Compare Notes",
        tags = ["read", "semantic"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<CompareResult>,
    )]
    async fn compare_notes(
        &self,
        #[description(
            "Vault-relative path of the first note, including the extension, such as `drafts/Plan.md`."
        )]
        left: String,
        #[description("Vault-relative path of the second note, including the extension.")]
        right: String,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = DuplicateTools::new(manager);
        let result = tools
            .compare_notes(&left, &right)
            .await
            .map_err(to_mcp_error)?;
        StandardResponse::new(&vault_name, "compare_notes", &result)
            .with_duration(start.elapsed().as_millis() as u64)
            .with_next_steps(&["diff_notes", "read_note", "find_duplicates"])
            .to_json()
    }
}
