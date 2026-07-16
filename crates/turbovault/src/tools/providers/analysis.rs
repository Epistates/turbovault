//! AnalysisProvider MCP capabilities.

use std::ops::Deref;

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

#[turbomcp::server(name = "obsidian-vault", version = "1.5.0")]
impl AnalysisProvider {
    // ─── DIFF TOOLS ──────────────────────────────────────────────────

    #[tool(
        description = "Compare two notes side-by-side showing unified diff, line-level and word-level changes, and similarity score",
        usage = "Use to understand differences between two notes, find duplicate content, or review changes. Returns unified diff format with added/removed/changed line counts and word-level inline changes",
        performance = "Fast (<50ms typical). Uses line-level then word-level diff for changed lines",
        related = ["read_note", "find_duplicates", "compare_notes"],
        examples = ["diff_notes(left='projects/plan-v1.md', right='projects/plan-v2.md')"],
        tags = ["read"],
        read_only = true,
    )]
    async fn diff_notes(&self, left: String, right: String) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = DiffTools::new(manager);
        let result = tools
            .diff_notes(&left, &right)
            .await
            .map_err(to_mcp_error)?;
        StandardResponse::new(
            &vault_name,
            "diff_notes",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["read_note", "edit_note", "compare_notes"])
        .to_json()
    }

    #[tool(
        description = "Compare current note with a previous version from the audit trail",
        usage = "Use to see what changed in a note over time. Specify operation_id from audit_log to identify the version to compare against",
        performance = "Fast (<50ms for diff, plus audit snapshot read time)",
        related = ["audit_log", "rollback_preview", "diff_notes"],
        examples = ["diff_note_version(path='notes/todo.md', operation_id='abc-123')"],
        tags = ["read", "audit"],
        read_only = true,
    )]
    async fn diff_note_version(
        &self,
        path: String,
        operation_id: String,
    ) -> McpResult<serde_json::Value> {
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
                McpError::internal(format!("Audit entry not found: {}", operation_id))
            })?;

        let snapshot_id = entry
            .before_snapshot_id
            .as_ref()
            .or(entry.after_snapshot_id.as_ref())
            .ok_or_else(|| {
                McpError::internal("No snapshot available for this operation".to_string())
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

        StandardResponse::new(
            &vault_name,
            "diff_note_version",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["audit_log", "rollback_note", "read_note"])
        .to_json()
    }

    // ─── QUALITY TOOLS ───────────────────────────────────────────────

    #[tool(
        description = "Evaluate note quality across readability, structure, completeness, and staleness dimensions (0-100 score per dimension plus composite)",
        usage = "Use to assess individual note quality and get specific improvement recommendations. Examines heading hierarchy, link density, vocabulary diversity, metadata completeness, and modification recency",
        performance = "Fast (<100ms per note). Parses content and checks graph for backlinks",
        related = ["vault_quality_report", "find_stale_notes", "full_health_analysis"],
        examples = ["evaluate_note_quality(path='projects/research.md')"],
        tags = ["read", "health"],
        read_only = true,
    )]
    async fn evaluate_note_quality(&self, path: String) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = QualityTools::new(manager);
        let result = tools.evaluate_note(&path).await.map_err(to_mcp_error)?;
        StandardResponse::new(
            &vault_name,
            "evaluate_note_quality",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["vault_quality_report", "edit_note", "find_stale_notes"])
        .to_json()
    }

    #[tool(
        description = "Generate vault-wide quality report with score distribution, dimension averages, lowest/highest quality notes, and recommendations",
        usage = "Use for vault-wide quality assessment. Identifies notes needing improvement and provides aggregate metrics across readability, structure, completeness, and staleness",
        performance = "Moderate to slow (500ms-5s depending on vault size). Evaluates all notes",
        related = ["evaluate_note_quality", "find_stale_notes", "full_health_analysis", "explain_vault"],
        examples = ["vault_quality_report()", "vault_quality_report(bottom_n=20)"],
        tags = ["read", "health"],
        read_only = true,
    )]
    async fn vault_quality_report(&self, bottom_n: Option<usize>) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = QualityTools::new(manager);
        let result = tools
            .vault_quality_report(bottom_n.unwrap_or(10))
            .await
            .map_err(to_mcp_error)?;
        StandardResponse::new(
            &vault_name,
            "vault_quality_report",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_count(result.total_notes)
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["evaluate_note_quality", "find_stale_notes"])
        .to_json()
    }

    #[tool(
        description = "Extract grounding primitives for a note — the raw material an external LLM judge needs to score hallucination/contradiction/redundancy: candidate factual claims from the prose, declared citations (# Citations), structural signals (Schema/Examples sections), and an 'uncited' flag (makes claims but cites nothing). TurboVault does NOT score grounding itself; it surfaces the data deterministically",
        usage = "Use to feed an LLM-judge grounding evaluation: pass the returned `claims` + `citations` (and the cited sources) to a judge to score how many claims are supported. The `uncited` flag marks hallucination-risk notes. Claims are heuristic sentence-level extractions, not semantic parses",
        performance = "Fast (<50ms typical) — parses one note",
        related = ["find_ungrounded_notes", "evaluate_note_quality", "okf_validate"],
        examples = ["analyze_note_grounding(path=\"tables/orders.md\")"],
        tags = ["read", "quality", "okf"],
        read_only = true,
    )]
    async fn analyze_note_grounding(&self, path: String) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = GroundingTools::new(manager);
        let result = tools.analyze_note(&path).await.map_err(to_mcp_error)?;
        StandardResponse::new(
            &vault_name,
            "analyze_note_grounding",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["find_ungrounded_notes", "okf_validate"])
        .to_json()
    }

    #[tool(
        description = "Scan the vault for hallucination-risk notes: notes that make factual claims in prose but declare no citations (# Citations). Returns them sorted by claim count (most claims first) — the notes most worth grounding review or an LLM-judge pass",
        usage = "Use to triage a knowledge base for ungrounded content before publishing, or to prioritize which notes to send to a grounding judge. `limit` caps the returned list (default 50)",
        performance = "Moderate (scans and parses all notes; proportional to vault size)",
        related = ["analyze_note_grounding", "vault_quality_report", "okf_validate"],
        examples = ["find_ungrounded_notes()", "find_ungrounded_notes(limit=20)"],
        tags = ["read", "quality", "okf"],
        read_only = true,
    )]
    async fn find_ungrounded_notes(&self, limit: Option<usize>) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = GroundingTools::new(manager);
        let result = tools
            .find_ungrounded_notes(limit.unwrap_or(50))
            .await
            .map_err(to_mcp_error)?;
        let count = result.ungrounded_count;
        StandardResponse::new(
            &vault_name,
            "find_ungrounded_notes",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_count(count)
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["analyze_note_grounding", "read_note"])
        .to_json()
    }

    // ==================== Open Knowledge Format (OKF) ====================

    #[tool(
        description = "Validate the vault as an Open Knowledge Format (OKF) bundle: checks every note for OKF v0.1 conformance (parseable frontmatter with a non-empty `type`) and surfaces each concept's OKF metadata (type, title, description, resource, timestamp, citation count) plus the bundle's type vocabulary. Reports non-conformant files for use as a CI gate",
        usage = "Use to check whether a vault is a conformant OKF bundle, to discover the set of concept `type` values in use, or as a pre-publish/CI gate (non_conformant > 0 means the bundle is not conformant). Pass `subtree` (a vault-relative directory) to scope the check. index.md/log.md are treated as reserved files and exempt from the `type` requirement",
        performance = "Moderate (scans and parses all notes; proportional to vault size)",
        related = ["generate_index", "inspect_frontmatter", "query_metadata", "explain_vault"],
        examples = ["okf_validate()", "okf_validate(subtree=\"tables\")"],
        tags = ["read", "okf", "frontmatter"],
        read_only = true,
    )]
    async fn okf_validate(&self, subtree: Option<String>) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = OkfTools::new(manager);
        let result = tools
            .validate(subtree.as_deref())
            .await
            .map_err(to_mcp_error)?;

        let total = result.total;
        let conformant = result.non_conformant == 0;
        StandardResponse::new(
            &vault_name,
            "okf_validate",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_count(total)
        .with_success(conformant)
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["generate_index", "read_note"])
        .to_json()
    }

    #[tool(
        description = "Generate or refresh OKF index.md files for progressive disclosure: each indexed directory gets an index.md listing its concept notes (with their frontmatter descriptions) and subdirectories, so agents and humans can navigate the bundle one level at a time instead of loading everything. Idempotent — unchanged indexes are not rewritten",
        usage = "Use after adding or editing notes to keep navigation indexes current, or to bootstrap progressive disclosure for an OKF bundle. `directory` (vault-relative, default = bundle root) scopes which directory to index; set `recursive=true` to index every subdirectory; set `dry_run=true` to preview what would be written without changing files",
        performance = "Moderate (parses concept notes in the targeted directories to read titles/descriptions)",
        related = ["okf_validate", "explain_vault", "write_note"],
        examples = ["generate_index(recursive=true)", "generate_index(directory=\"tables\")", "generate_index(recursive=true, dry_run=true)"],
        tags = ["write", "okf"],
    )]
    async fn generate_index(
        &self,
        directory: Option<String>,
        recursive: Option<bool>,
        dry_run: Option<bool>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let dry_run = dry_run.unwrap_or(false);
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = OkfTools::new(manager);
        let result = tools
            .generate_index(directory.as_deref(), recursive.unwrap_or(false), dry_run)
            .await
            .map_err(to_mcp_error)?;

        let wrote_any = result.indexes.iter().any(|i| i.written);
        if wrote_any {
            self.invalidate_similarity_cache().await;
            self.invalidate_search_cache().await;
        }

        let count = result.indexes.len();
        StandardResponse::new(
            &vault_name,
            "generate_index",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_count(count)
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["okf_validate", "explain_vault"])
        .to_json()
    }

    #[tool(
        description = "Append an entry to an OKF log.md update history (spec §7). Files the entry under a `## YYYY-MM-DD` date section, newest-first — a new date becomes the top section, an existing date gains another bullet. Creates log.md (with a title) if absent",
        usage = "Use to record a change to a directory's knowledge (e.g. after enriching or restructuring notes). `directory` (vault-relative, default = bundle root) selects which log.md; `kind` is the leading bold word (Update/Creation/Deprecation, default Update); `date` is ISO YYYY-MM-DD (default today)",
        performance = "Fast (<20ms) — reads and rewrites one log.md",
        related = ["generate_index", "okf_validate"],
        examples = [
            "append_log_entry(text=\"Added the orders table reference.\")",
            "append_log_entry(directory=\"tables\", kind=\"Creation\", text=\"Established the tables index.\", date=\"2026-06-13\")"
        ],
        tags = ["write", "okf"],
    )]
    async fn append_log_entry(
        &self,
        text: String,
        directory: Option<String>,
        kind: Option<String>,
        date: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = OkfTools::new(manager);
        let result = tools
            .append_log_entry(
                directory.as_deref(),
                kind.as_deref(),
                &text,
                date.as_deref(),
            )
            .await
            .map_err(to_mcp_error)?;

        StandardResponse::new(
            &vault_name,
            "append_log_entry",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["okf_validate", "generate_index"])
        .to_json()
    }

    #[tool(
        description = "Render the vault's concept graph as a single self-contained HTML file: a force-directed graph of every note, a detail panel with the rendered markdown body and 'cited by' backlinks, plus type filter and search. The bundle is embedded as JSON; the graph/markdown libraries load from a CDN. Shareable as a static artifact — open in any browser, no backend. Covers both OKF cross-links and Obsidian wikilinks",
        usage = "Use to produce a browsable/shareable visualization of a vault or OKF bundle. Writes the HTML to `output` (vault-relative, default `viz.html` at the bundle root) and returns node/edge counts and the file size. `name` overrides the header title (defaults to the vault folder name)",
        performance = "Moderate (parses every note to embed titles/bodies; proportional to vault size)",
        related = ["explain_vault", "get_centrality_ranking", "okf_validate"],
        examples = ["visualize()", "visualize(output=\"reports/graph.html\", name=\"Sales Bundle\")"],
        tags = ["write", "export", "okf"],
    )]
    async fn visualize(
        &self,
        output: Option<String>,
        name: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = ViewerTools::new(manager.clone());
        let (html, mut summary) = tools
            .generate(name.as_deref())
            .await
            .map_err(to_mcp_error)?;

        let out_rel = output.unwrap_or_else(|| "viz.html".to_string());
        manager
            .write_file(std::path::Path::new(&out_rel), &html, None)
            .await
            .map_err(to_mcp_error)?;

        // Refresh the displayed byte count from the written content.
        summary.html_bytes = html.len();

        StandardResponse::new(
            &vault_name,
            "visualize",
            serde_json::json!({ "output": out_rel, "summary": summary }),
        )
        .with_count(summary.nodes)
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_step("explain_vault")
        .to_json()
    }

    #[tool(
        description = "Find notes that have not been updated recently, sorted by staleness (most stale first)",
        usage = "Use to identify neglected content that may need review, updating, or archiving. Configurable threshold in days and result limit",
        performance = "Moderate (200ms-2s depending on vault size). Checks file modification times",
        related = ["evaluate_note_quality", "vault_quality_report", "query_metadata"],
        examples = ["find_stale_notes(threshold_days=90)", "find_stale_notes(threshold_days=30, limit=20)"],
        tags = ["read", "health"],
        read_only = true,
    )]
    async fn find_stale_notes(
        &self,
        threshold_days: Option<u64>,
        limit: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = QualityTools::new(manager);
        let result = tools
            .find_stale_notes(threshold_days.unwrap_or(90), limit.unwrap_or(20))
            .await
            .map_err(to_mcp_error)?;
        let count = result.len();
        StandardResponse::new(
            &vault_name,
            "find_stale_notes",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_count(count)
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["evaluate_note_quality", "read_note", "edit_note"])
        .to_json()
    }

    // ─── SIMILARITY TOOLS ────────────────────────────────────────────

    #[tool(
        description = "Find notes semantically similar to a query using TF-IDF cosine similarity (finds conceptual matches beyond exact keyword overlap)",
        usage = "Use when keyword search returns too few results or you want conceptual similarity. Returns similarity scores (0-1) and shared terms for explainability. More sophisticated than keyword search",
        performance = "Moderate (<500ms for 10k notes). Builds TF-IDF vectors on first call, cached for subsequent queries",
        related = ["search", "find_similar_notes", "recommend_related", "advanced_search"],
        examples = ["semantic_search(query='distributed systems architecture')", "semantic_search(query='machine learning concepts', limit=20)"],
        tags = ["read", "search", "semantic"],
        read_only = true,
    )]
    async fn semantic_search(
        &self,
        query: String,
        limit: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let vault_name = self.get_active_vault_name().await?;
        let engine = self.get_similarity_engine().await?;
        let results = engine.semantic_search(&query, limit.unwrap_or(10));
        let count = results.len();
        StandardResponse::new(
            &vault_name,
            "semantic_search",
            serde_json::to_value(&results).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_count(count)
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["read_note", "find_similar_notes", "advanced_search"])
        .to_json()
    }

    #[tool(
        description = "Find notes most similar in content to a specific note using TF-IDF cosine similarity",
        usage = "Use to discover related notes for linking, find candidates for merging, or identify thematic clusters. More content-aware than graph-based get_related_notes",
        performance = "Moderate (<500ms for 10k notes). Uses pre-built TF-IDF vectors",
        related = ["semantic_search", "recommend_related", "get_related_notes", "find_duplicates"],
        examples = ["find_similar_notes(path='projects/research.md')", "find_similar_notes(path='ideas/concept.md', limit=20)"],
        tags = ["read", "search", "semantic"],
        read_only = true,
    )]
    async fn find_similar_notes(
        &self,
        path: String,
        limit: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let vault_name = self.get_active_vault_name().await?;
        let engine = self.get_similarity_engine().await?;
        let results = engine.find_similar_notes(&path, limit.unwrap_or(10));
        let count = results.len();
        StandardResponse::new(
            &vault_name,
            "find_similar_notes",
            serde_json::to_value(&results).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_count(count)
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["read_note", "semantic_search", "get_backlinks"])
        .to_json()
    }

    // ─── DUPLICATE TOOLS ─────────────────────────────────────────────

    #[tool(
        description = "Find near-duplicate notes across vault using SimHash fingerprinting and TF-IDF cosine similarity verification",
        usage = "Use to identify redundant content, merge candidates, or detect copied notes. Default threshold 0.8 catches close duplicates; lower to 0.6 for looser matching. Two-stage: fast SimHash filtering then precise verification",
        performance = "Moderate (<2s for 10k notes). SimHash O(N^2) candidate filtering then TF-IDF verification",
        related = ["compare_notes", "find_similar_notes", "diff_notes"],
        examples = ["find_duplicates()", "find_duplicates(threshold=0.6, limit=50)"],
        tags = ["read", "search", "semantic"],
        read_only = true,
    )]
    async fn find_duplicates(
        &self,
        threshold: Option<f64>,
        limit: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = DuplicateTools::new(manager);
        let result = tools
            .find_duplicates(threshold.unwrap_or(0.8), limit.unwrap_or(20))
            .await
            .map_err(to_mcp_error)?;
        let count = result.len();
        StandardResponse::new(
            &vault_name,
            "find_duplicates",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_count(count)
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["compare_notes", "diff_notes", "read_note"])
        .to_json()
    }

    #[tool(
        description = "Compare two specific notes showing similarity score, shared terms, diff summary, and actionable recommendation",
        usage = "Use to assess whether two notes should be merged, linked, or kept separate. Returns similarity score (0-1), shared vocabulary, line-level diff statistics, and a recommendation",
        performance = "Moderate (<500ms). Builds TF-IDF vectors and computes diff",
        related = ["find_duplicates", "diff_notes", "find_similar_notes"],
        examples = ["compare_notes(left='projects/plan-v1.md', right='projects/plan-v2.md')"],
        tags = ["read", "semantic"],
        read_only = true,
    )]
    async fn compare_notes(&self, left: String, right: String) -> McpResult<serde_json::Value> {
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = DuplicateTools::new(manager);
        let result = tools
            .compare_notes(&left, &right)
            .await
            .map_err(to_mcp_error)?;
        StandardResponse::new(
            &vault_name,
            "compare_notes",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["diff_notes", "read_note", "find_duplicates"])
        .to_json()
    }
}
