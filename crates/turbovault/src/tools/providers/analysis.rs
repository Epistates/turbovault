//! AnalysisProvider MCP capabilities.

use std::ops::Deref;

use super::super::*;
use turbovault_core::Precondition;

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

#[turbomcp::server(name = "obsidian-vault", version = "2.0.0")]
impl AnalysisProvider {
    // ─── DIFF TOOLS ──────────────────────────────────────────────────

    #[tool(
        description = "Compare two notes side-by-side showing unified diff, line-level and word-level changes, and similarity score",
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
        tags = ["read", "audit"],
        read_only = true,
    )]
    async fn diff_note_version(
        &self,
        path: String,
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
        tags = ["write", "okf"],
    )]
    async fn generate_index(
        &self,
        directory: Option<String>,
        recursive: Option<bool>,
        dry_run: Option<bool>,
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
        tags = ["write", "okf"],
    )]
    async fn append_log_entry(
        &self,
        text: String,
        directory: Option<String>,
        kind: Option<String>,
        date: Option<String>,
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
        tags = ["write", "export", "okf"],
    )]
    async fn visualize(
        &self,
        output: Option<String>,
        name: Option<String>,
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
            serde_json::json!({ "output": out_rel, "summary": summary }),
        )
        .with_count(summary.nodes)
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_step("explain_vault")
        .to_json()
    }

    #[tool(
        description = "Find notes that have not been updated recently, sorted by staleness (most stale first)",
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
