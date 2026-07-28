//! Batch operation tools for coordinated multi-file operations

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use turbovault_batch::{BatchOperation, BatchResult, OperationRecord};
use turbovault_core::ChangePlan;
use turbovault_core::prelude::*;
use turbovault_vault::{EditEngine, VaultManager};

/// Batch operation tools
#[derive(Clone)]
pub struct BatchTools {
    pub manager: Arc<VaultManager>,
}

impl BatchTools {
    /// Create new batch tools
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self { manager }
    }

    /// Read a file from the vault (mirrors `GitFileTools::read_file`).
    async fn read_file(&self, path: &str) -> Result<String> {
        self.manager.read_file(&PathBuf::from(path)).await
    }

    /// write-substrate-layering M4d (R3/R4): the manager-routed batch. Folds
    /// every op into ONE [`ChangePlan`] via [`Self::plan`] (the same
    /// `translate_op`/`fold_*` helpers, including the formerly git-only
    /// `EditNote`/`UpdateFrontmatter`/`ManageTags`/`CreateFromTemplate` arms
    /// and per-op CAS preconditions) and applies it through
    /// [`VaultManager::apply_changes`] — so the SAME batch surface runs on
    /// both substrates. `message` becomes the git commit subject (ignored on
    /// direct). On a direct vault the plan gets today's `DirectSubstrate::apply`
    /// semantics (precondition-gate → sequential → `atomic:true`); direct
    /// best-effort `failed_at` reporting is M5.2.
    ///
    /// Reindex is flushed first so any link-aware op (`MoveNote`/`DeleteNote`)
    /// resolves against a coherent graph. A batch failure (stale precondition,
    /// intra-batch path collision, unreadable link source) is reported as a
    /// soft `BatchResult { success: false, errors }` — NOT a hard `Err` — so the
    /// pre-M4d batch wire shape (R10) holds. On git `apply_changes` aborts the
    /// whole plan atomically (nothing written). On direct only the precondition
    /// GATE is atomic — the apply loop is sequential with no rollback, so a
    /// mid-loop failure can leave partial state while this still reports
    /// `executed: 0`; true direct best-effort/`failed_at` reporting is M5.2.
    pub async fn batch_execute(
        &self,
        operations: Vec<BatchOperation>,
        message: &str,
    ) -> Result<BatchResult> {
        let started = Instant::now();
        let transaction_id = uuid::Uuid::new_v4().to_string();
        let total = operations.len();

        if operations.is_empty() {
            return Ok(BatchResult {
                success: false,
                executed: 0,
                total: 0,
                failed_at: None,
                changes: vec![],
                errors: vec!["Batch cannot be empty".to_string()],
                records: vec![],
                transaction_id,
                duration_ms: started.elapsed().as_millis() as u64,
            });
        }

        self.manager.flush_reindex().await;
        // A batch failure (intra-batch collision, a stale/absent precondition,
        // an unreadable link source) is reported as `success: false` in the
        // BatchResult envelope — NOT propagated as a hard error — preserving
        // the pre-M4d wire shape (R10): the batch tool call itself succeeds and
        // the caller inspects `success`/`errors`. On git `apply_changes` is
        // whole-plan atomic (nothing written on failure); on direct only the
        // precondition gate is atomic — a mid-apply failure may leave partial
        // state (M5.2 adds `failed_at`).
        let mut plan = match self.plan(&operations).await {
            Ok(plan) => plan,
            Err(e) => return Ok(failed_batch(total, e, transaction_id, started)),
        };
        plan.message = message.to_string();
        if let Err(e) = self.manager.apply_changes(&plan).await {
            return Ok(failed_batch(total, e, transaction_id, started));
        }

        let changes = operations.iter().map(describe_op).collect();
        let records = operations
            .iter()
            .enumerate()
            .map(|(idx, op)| OperationRecord {
                operation_index: idx,
                operation: format!("{:?}", op),
                success: true,
                error: None,
                affected_files: op.affected_files(),
            })
            .collect();

        Ok(BatchResult {
            success: true,
            executed: total,
            total,
            failed_at: None,
            changes,
            errors: vec![],
            records,
            transaction_id,
            duration_ms: started.elapsed().as_millis() as u64,
        })
    }

    // -------- write-substrate-layering M4a: the ChangePlan translation --------
    //
    // Ports `GitFileTools::translate_op` + its per-op `fold_*` helpers
    // (git_file_tools.rs) as a backend-agnostic, git2-free builder. It
    // RETURNS a plan; it never writes (`manager.apply_changes` is bite 3's
    // wiring). Version tokens are carried as opaque `String`s (§6.1/R5) —
    // this layer never parses or validates hex format; only the substrate
    // that eventually applies the plan does that.

    /// Translate a batch of [`BatchOperation`]s into ONE [`ChangePlan`],
    /// reusing the same `compute_*` helpers `MetadataTools`/`TemplateEngine`
    /// use in their own single-op mutators — `EditNote`/`UpdateFrontmatter`/
    /// `ManageTags`/`CreateFromTemplate` run on both backends (write-
    /// substrate-layering deleted the old git-only refusal, decision 1).
    /// Pure builder — does not write. Rejects an intra-batch path collision
    /// (turbovault-0g4.5): a path may be mutated by at most one operation
    /// per batch.
    pub async fn plan(&self, operations: &[BatchOperation]) -> Result<ChangePlan> {
        let mut plan = ChangePlan::new(format!("batch_execute ({} ops)", operations.len()));
        let mut seen_paths: std::collections::HashSet<String> = std::collections::HashSet::new();

        for (idx, op) in operations.iter().enumerate() {
            let before = plan.touched_paths().len();
            plan = self.translate_op(plan, op).await?;
            if let Some(dup) = plan
                .touched_paths()
                .into_iter()
                .skip(before)
                .find(|p| !seen_paths.insert(p.clone()))
            {
                return Err(Error::config_error(format!(
                    "intra-batch path collision (turbovault-0g4.5): operation {} writes '{}', which an earlier operation in this batch already writes. A path may be mutated by at most one operation per batch — split the conflicting writes across separate batches.",
                    idx, dup
                )));
            }
        }

        Ok(plan)
    }

    /// Atomic move + inbound-wikilink rewrite, as a plan. Ports
    /// `GitFileTools::move_file_with_link_updates` minus the apply — the
    /// rename plus every backlinking source's rewrite, all in one
    /// [`ChangePlan`], each source guarded by its own `expect_blob`.
    pub async fn plan_move_with_links(
        &self,
        from: &str,
        to: &str,
        expected_hash: Option<&str>,
        message: &str,
    ) -> Result<ChangePlan> {
        let plan = ChangePlan::new(message.to_string());
        // `None` dest precondition = expect_absent (this builder's existing
        // clobber-guard behavior; qae.6.4 adds the dest axis at the batch op only).
        let (plan, _link_sources_updated) = self
            .fold_move_with_links(plan, from, to, expected_hash, None)
            .await?;
        Ok(plan)
    }

    /// Delete + inbound-wikilink stale-wrap, as a plan. Ports
    /// `GitFileTools::delete_file_with_link_rewrite_to_stale` minus the
    /// apply — the remove plus each linker's `~~[[old]]~~` rewrite, all in
    /// one [`ChangePlan`].
    pub async fn plan_delete_with_stale_links(
        &self,
        path: &str,
        expected_hash: Option<&str>,
        message: &str,
    ) -> Result<ChangePlan> {
        let plan = ChangePlan::new(message.to_string());
        let (plan, _link_sources_updated) = self
            .fold_delete_with_stale_links(plan, path, expected_hash)
            .await?;
        Ok(plan)
    }

    // -------- write-substrate-layering M4d: manager-routed link-aware writes --

    /// Atomic move + inbound-wikilink rewrite through the manager (both
    /// substrates). Flushes the reindex queue first so the backlink resolution
    /// reads a coherent link graph (replaces the MCP layer's pre-move flush),
    /// builds the one-plan rename+rewrite via [`Self::fold_move_with_links`],
    /// and applies it via [`VaultManager::apply_changes`]. Returns the
    /// vault-relative source paths whose wikilinks were rewritten. `message`
    /// is the git commit subject (ignored on direct).
    pub async fn move_file_with_link_updates(
        &self,
        from: &str,
        to: &str,
        expected_hash: Option<&str>,
        dest_expected_hash: Option<&str>,
        message: &str,
    ) -> Result<Vec<String>> {
        self.manager.flush_reindex().await;
        let (plan, updated) = self
            .fold_move_with_links(
                ChangePlan::new(message.to_string()),
                from,
                to,
                expected_hash,
                dest_expected_hash,
            )
            .await?;
        self.manager.apply_changes(&plan).await?;
        Ok(updated)
    }

    /// Atomic delete + inbound-wikilink stale-wrap through the manager (both
    /// substrates). Same flush-then-`apply_changes` shape as
    /// [`Self::move_file_with_link_updates`]; returns the rewritten source
    /// paths.
    pub async fn delete_file_with_link_rewrite_to_stale(
        &self,
        path: &str,
        expected_hash: Option<&str>,
        message: &str,
    ) -> Result<Vec<String>> {
        self.manager.flush_reindex().await;
        let (plan, updated) = self
            .fold_delete_with_stale_links(ChangePlan::new(message.to_string()), path, expected_hash)
            .await?;
        self.manager.apply_changes(&plan).await?;
        Ok(updated)
    }

    // -------- internals (ported from git_file_tools.rs) --------

    async fn translate_op(&self, plan: ChangePlan, op: &BatchOperation) -> Result<ChangePlan> {
        Ok(match op {
            BatchOperation::CreateNote {
                path,
                content,
                force,
            } => {
                if force.unwrap_or(false) {
                    plan.upsert(path, content.as_bytes())
                } else {
                    plan.create(path, content.as_bytes())
                }
            }
            BatchOperation::WriteNote {
                path,
                content,
                expected_hash,
            } => upsert_expecting(
                plan,
                path,
                content.as_bytes().to_vec(),
                expected_hash.as_deref(),
            ),
            BatchOperation::DeleteNote {
                path,
                expected_hash,
                on_backlinks,
            } => {
                self.fold_delete_note(
                    plan,
                    path,
                    expected_hash.as_deref(),
                    on_backlinks.as_deref(),
                )
                .await?
            }
            BatchOperation::MoveNote {
                from,
                to,
                expected_hash,
                dest_expected_hash,
                update_backlinks,
            } => {
                self.fold_move_note(
                    plan,
                    from,
                    to,
                    expected_hash.as_deref(),
                    dest_expected_hash.as_deref(),
                    *update_backlinks,
                )
                .await?
            }
            BatchOperation::UpdateLinks {
                file,
                old_target,
                new_target,
                expected_hash,
            } => {
                if old_target.is_empty() {
                    return Err(Error::config_error(
                        "UpdateLinks: old_target must not be empty (str::replace(\"\", _) would insert new_target at every character boundary)",
                    ));
                }
                let current = self.read_file(file).await?;
                let updated = current.replace(old_target, new_target);
                upsert_expecting(plan, file, updated.into_bytes(), expected_hash.as_deref())
            }
            BatchOperation::EditNote {
                path,
                edits,
                expected_hash,
            } => {
                self.fold_edit_note(plan, path, edits, expected_hash.as_deref())
                    .await?
            }
            BatchOperation::UpdateFrontmatter {
                path,
                frontmatter,
                merge,
                expected_hash,
            } => {
                self.fold_update_frontmatter(
                    plan,
                    path,
                    frontmatter,
                    *merge,
                    expected_hash.as_deref(),
                )
                .await?
            }
            BatchOperation::ManageTags {
                path,
                operation,
                tags,
                expected_hash,
            } => {
                self.fold_manage_tags(plan, path, operation, tags, expected_hash.as_deref())
                    .await?
            }
            BatchOperation::CreateFromTemplate {
                template_id,
                path,
                fields,
                force,
            } => {
                self.fold_create_from_template(plan, template_id, path, fields, *force)
                    .await?
            }
        })
    }

    /// DeleteNote arm (turbovault-0g4.7) — backlink-aware delete: refuse
    /// (default) / rewrite-stale-callout / force.
    async fn fold_delete_note(
        &self,
        plan: ChangePlan,
        path: &str,
        expected_hash: Option<&str>,
        on_backlinks: Option<&str>,
    ) -> Result<ChangePlan> {
        Ok(match on_backlinks.unwrap_or("refuse") {
            // Bare delete — leave inbound links dangling (pre-0g4.7 behavior).
            "force" => remove_expecting(plan, path, expected_hash),
            // Atomically strikethrough every linker in the same plan.
            "rewrite-stale-callout" => {
                self.fold_delete_with_stale_links(plan, path, expected_hash)
                    .await?
                    .0
            }
            "refuse" => {
                let backlinks = self.list_inbound_backlinks(path).await?;
                if !backlinks.is_empty() {
                    return Err(Error::config_error(format!(
                        "DeleteNote refused (turbovault-0g4.7): '{}' has {} inbound backlink(s) [{}]. Pass on_backlinks=\"rewrite-stale-callout\" to strikethrough every linker in the same commit, or \"force\" to delete and leave them broken.",
                        path,
                        backlinks.len(),
                        backlinks.join(", ")
                    )));
                }
                remove_expecting(plan, path, expected_hash)
            }
            other => {
                return Err(Error::config_error(format!(
                    "DeleteNote: unknown on_backlinks mode '{}' (expected refuse|rewrite-stale-callout|force)",
                    other
                )));
            }
        })
    }

    /// MoveNote arm (turbovault-0g4.6) — default rewrites inbound wikilinks
    /// in the same plan; update_backlinks=false is rename-only.
    async fn fold_move_note(
        &self,
        plan: ChangePlan,
        from: &str,
        to: &str,
        expected_hash: Option<&str>,
        dest_expected_hash: Option<&str>,
        update_backlinks: Option<bool>,
    ) -> Result<ChangePlan> {
        if update_backlinks.unwrap_or(true) {
            Ok(self
                .fold_move_with_links(plan, from, to, expected_hash, dest_expected_hash)
                .await?
                .0)
        } else {
            // Rename-only: no backlink rewrite, inbound links dangle. The
            // destination precondition decodes from `dest_expected_hash`
            // (omitted → expect_absent, the clobber guard).
            let content = self.read_file(from).await?;
            let mut p = plan.remove(from).upsert(to, content.into_bytes());
            if let Some(token) = expected_hash {
                p = p.expect_blob(from, token.to_string());
            }
            Ok(apply_dest_precondition(p, to, dest_expected_hash))
        }
    }

    /// EditNote arm (turbovault-0g4.1) — SEARCH/REPLACE blocks folded into
    /// the batch plan (the same `EditEngine` path `edit_file` uses, minus
    /// the dry-run/hash reporting a batch doesn't need).
    async fn fold_edit_note(
        &self,
        plan: ChangePlan,
        path: &str,
        edits: &str,
        expected_hash: Option<&str>,
    ) -> Result<ChangePlan> {
        let current = self.read_file(path).await?;
        let engine = EditEngine::new();
        let blocks = engine.parse_blocks(edits)?;
        let (_result, new_content) = engine.apply_edits(&current, &blocks, false)?;
        Ok(upsert_expecting(
            plan,
            path,
            new_content.into_bytes(),
            expected_hash,
        ))
    }

    /// UpdateFrontmatter arm (turbovault-0g4.2) — reuse
    /// `MetadataTools::compute_update_frontmatter` (read + merge in
    /// memory), fold the resulting content into the batch plan.
    async fn fold_update_frontmatter(
        &self,
        plan: ChangePlan,
        path: &str,
        frontmatter: &std::collections::HashMap<String, serde_json::Value>,
        merge: Option<bool>,
        expected_hash: Option<&str>,
    ) -> Result<ChangePlan> {
        let mt = crate::MetadataTools::new(Arc::clone(&self.manager));
        let fm_map: serde_json::Map<String, serde_json::Value> =
            frontmatter.clone().into_iter().collect();
        let (new_content, _info) = mt
            .compute_update_frontmatter(path, fm_map, merge.unwrap_or(true))
            .await?;
        Ok(upsert_expecting(
            plan,
            path,
            new_content.into_bytes(),
            expected_hash,
        ))
    }

    /// ManageTags arm (turbovault-0g4.3) — reuse
    /// `MetadataTools::compute_manage_tags`; "list" is read-only (returns
    /// None) and rejected inside a batch.
    async fn fold_manage_tags(
        &self,
        plan: ChangePlan,
        path: &str,
        operation: &str,
        tags: &[String],
        expected_hash: Option<&str>,
    ) -> Result<ChangePlan> {
        let mt = crate::MetadataTools::new(Arc::clone(&self.manager));
        let (maybe, _info) = mt.compute_manage_tags(path, operation, Some(tags)).await?;
        let new_content = maybe.ok_or_else(|| {
            Error::config_error(format!(
                "ManageTags operation '{}' produces no write; only 'add'/'remove' are valid in a batch ('list' is read-only)",
                operation
            ))
        })?;
        Ok(upsert_expecting(
            plan,
            path,
            new_content.into_bytes(),
            expected_hash,
        ))
    }

    /// CreateFromTemplate arm (turbovault-0g4.4) — render via
    /// `TemplateEngine::compute_from_template`, then strict-create
    /// (default, expect_absent) or force-upsert (overwrite).
    async fn fold_create_from_template(
        &self,
        plan: ChangePlan,
        template_id: &str,
        path: &str,
        fields: &std::collections::HashMap<String, String>,
        force: Option<bool>,
    ) -> Result<ChangePlan> {
        let engine = crate::TemplateEngine::new(Arc::clone(&self.manager));
        let (content, _info) = engine
            .compute_from_template(template_id, path, fields.clone())
            .await?;
        Ok(if force.unwrap_or(false) {
            plan.upsert(path, content.into_bytes())
        } else {
            plan.create(path, content.into_bytes())
        })
    }

    /// Fold an atomic move + inbound-wikilink rewrite onto an existing
    /// plan. Resolves backlinks via the in-memory link graph, rewrites each
    /// source (OFM-aware), and chains `remove(from)` + `upsert(to)` +
    /// `expect_absent(to)` (+ optional `expect_blob(from)`) + each source's
    /// `upsert`/`expect_blob`. Returns the augmented plan and the list of
    /// rewritten source paths.
    ///
    /// Git2-free at this layer: the per-source CAS token is minted via
    /// [`VaultManager::hash_bytes`], which asks this vault's OWN configured
    /// substrate for the token format it will itself validate at apply time
    /// (sha256 for Direct, a git blob oid for Git) — this fold never
    /// hardcodes one backend's convention or parses hex itself.
    ///
    /// Resolves against the link graph, so the caller must ensure it is
    /// coherent (the MCP layer drains the reindex queue before a
    /// backlink-aware move; unit tests call `manager.initialize()`).
    async fn fold_move_with_links(
        &self,
        plan: ChangePlan,
        from: &str,
        to: &str,
        expected_from: Option<&str>,
        dest_expected_hash: Option<&str>,
    ) -> Result<(ChangePlan, Vec<String>)> {
        use crate::wikilink_rewriter::rewrite_wikilinks;

        let content = self.read_file(from).await?;

        let backlink_paths = {
            let lg = self.manager.link_graph();
            let graph = lg.read().await;
            graph
                .backlinks(&self.manager.vault_path().join(from))
                .map_err(|e| Error::config_error(format!("backlink lookup: {}", e)))?
                .into_iter()
                .map(|(p, _links)| p)
                .collect::<Vec<_>>()
        };

        // Read each source, rewrite, capture a content hash for the
        // precondition. Skip sources whose rewritten content equals the
        // original (no actual link change — e.g. the `[[from]]` literal
        // sits in a code fence).
        let mut link_updates: Vec<(String, String, String)> = Vec::new();
        for full_src in &backlink_paths {
            let rel = full_src
                .strip_prefix(self.manager.vault_path())
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|_| full_src.clone());
            let rel_str = rel
                .to_str()
                .ok_or_else(|| Error::config_error(format!("non-utf8 source path: {:?}", rel)))?
                .to_string();
            let src_content = self.read_file(&rel_str).await?;
            let rewritten = rewrite_wikilinks(&src_content, from, to);
            if rewritten == src_content {
                continue;
            }
            let src_hash = self.manager.hash_bytes(src_content.as_bytes())?;
            link_updates.push((rel_str, rewritten, src_hash));
        }

        // Source rename + each link source's rewrite, with preconditions.
        let mut plan = plan.remove(from).upsert(to, content.into_bytes());
        if let Some(token) = expected_from {
            plan = plan.expect_blob(from, token.to_string());
        }
        plan = apply_dest_precondition(plan, to, dest_expected_hash);
        for (rel_path, rewritten, hash) in &link_updates {
            plan = plan
                .upsert(rel_path.clone(), rewritten.clone().into_bytes())
                .expect_blob(rel_path.clone(), hash.clone());
        }

        let updated = link_updates.into_iter().map(|(p, _, _)| p).collect();
        Ok((plan, updated))
    }

    /// Fold a delete + inbound-wikilink stale-wrap onto an existing plan.
    /// Resolves backlinks via the link graph, wraps each linker's
    /// references to `path` as `~~[[old]]~~` strikethrough, and chains
    /// `remove(path)` (+ optional `expect_blob(path)`) + each source's
    /// `upsert`/`expect_blob`. Returns the augmented plan and the list of
    /// rewritten source paths. Same [`VaultManager::hash_bytes`]-minted
    /// token + link-graph-coherence notes as [`Self::fold_move_with_links`].
    async fn fold_delete_with_stale_links(
        &self,
        plan: ChangePlan,
        path: &str,
        expected_target: Option<&str>,
    ) -> Result<(ChangePlan, Vec<String>)> {
        use crate::wikilink_rewriter::wrap_wikilinks_as_stale;

        let backlink_paths = {
            let lg = self.manager.link_graph();
            let graph = lg.read().await;
            graph
                .backlinks(&self.manager.vault_path().join(path))
                .map_err(|e| Error::config_error(format!("backlink lookup: {}", e)))?
                .into_iter()
                .map(|(p, _links)| p)
                .collect::<Vec<_>>()
        };

        let mut link_updates: Vec<(String, String, String)> = Vec::new();
        for full_src in &backlink_paths {
            let rel = full_src
                .strip_prefix(self.manager.vault_path())
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|_| full_src.clone());
            let rel_str = rel
                .to_str()
                .ok_or_else(|| Error::config_error(format!("non-utf8 source path: {:?}", rel)))?
                .to_string();
            let src_content = self.read_file(&rel_str).await?;
            let rewritten = wrap_wikilinks_as_stale(&src_content, path);
            if rewritten == src_content {
                continue;
            }
            let src_hash = self.manager.hash_bytes(src_content.as_bytes())?;
            link_updates.push((rel_str, rewritten, src_hash));
        }

        let mut plan = plan.remove(path);
        if let Some(token) = expected_target {
            plan = plan.expect_blob(path, token.to_string());
        }
        for (rel_path, rewritten, hash) in &link_updates {
            plan = plan
                .upsert(rel_path.clone(), rewritten.clone().into_bytes())
                .expect_blob(rel_path.clone(), hash.clone());
        }

        let updated = link_updates.into_iter().map(|(p, _, _)| p).collect();
        Ok((plan, updated))
    }

    /// Return the list of vault-relative source paths that have inbound
    /// wikilinks targeting `path`. Used by [`Self::fold_delete_note`]'s
    /// "refuse-if-backlinks" default — every backlink found must be counted,
    /// so a non-UTF-8 source path errors loudly here (matching
    /// [`Self::fold_move_with_links`] / [`Self::fold_delete_with_stale_links`])
    /// rather than being silently dropped, which would undercount and let
    /// the refuse-by-default safety check pass with a real backlink still
    /// present.
    async fn list_inbound_backlinks(&self, path: &str) -> Result<Vec<String>> {
        let backlink_paths = {
            let lg = self.manager.link_graph();
            let graph = lg.read().await;
            graph
                .backlinks(&self.manager.vault_path().join(path))
                .map_err(|e| Error::config_error(format!("backlink lookup: {}", e)))?
                .into_iter()
                .map(|(p, _)| p)
                .collect::<Vec<_>>()
        };
        let mut out = Vec::new();
        for full_src in backlink_paths {
            let rel = full_src
                .strip_prefix(self.manager.vault_path())
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|_| full_src.clone());
            let rel_str = rel
                .to_str()
                .ok_or_else(|| Error::config_error(format!("non-utf8 source path: {:?}", rel)))?
                .to_string();
            out.push(rel_str);
        }
        Ok(out)
    }
}

/// Fold `upsert(path, bytes)` plus an optional `expect_blob` CAS
/// precondition into `plan`. The shared tail of every content-replacing
/// batch op (WriteNote / UpdateLinks / EditNote / UpdateFrontmatter /
/// ManageTags). Git2-free: `expected_hash` is carried verbatim as the
/// precondition's opaque token — no format validation at this layer (§6.2:
/// only the substrate that applies the plan parses it).
fn upsert_expecting(
    plan: ChangePlan,
    path: &str,
    bytes: Vec<u8>,
    expected_hash: Option<&str>,
) -> ChangePlan {
    let mut p = plan.upsert(path, bytes);
    if let Some(token) = expected_hash {
        p = p.expect_blob(path, token.to_string());
    }
    p
}

/// Fold `remove(path)` plus an optional `expect_blob` precondition into
/// `plan` — the shared tail of the bare-delete branches.
fn remove_expecting(plan: ChangePlan, path: &str, expected_hash: Option<&str>) -> ChangePlan {
    let mut p = plan.remove(path);
    if let Some(token) = expected_hash {
        p = p.expect_blob(path, token.to_string());
    }
    p
}

/// Decode a batch move's `dest_expected_hash` sentinel-or-oid string
/// (`<oid> | "absent" | "exists" | "blind"`, turbovault-qae.6.4) into the
/// destination precondition on `to`. Omitted (`None`) defaults to `expect_absent`
/// — the clobber guard, preserving the pre-qae.6.4 behavior. A bare oid is
/// carried verbatim (the substrate that applies the plan parses it, §6.2).
fn apply_dest_precondition(
    plan: ChangePlan,
    to: &str,
    dest_expected_hash: Option<&str>,
) -> ChangePlan {
    match dest_expected_hash {
        None | Some("absent") => plan.expect_absent(to),
        Some("blind") => plan,
        Some("exists") => plan.with_precondition(to, turbovault_core::Precondition::ExpectExists),
        Some(oid) => plan.expect_blob(to, oid.to_string()),
    }
}

/// The soft `success: false` [`BatchResult`] a manager-routed batch returns
/// when the plan cannot be built or applied — nothing was written (the plan
/// aborts atomically). Mirrors the pre-M4d executor's failure envelope so the
/// batch tool's wire shape is unchanged (R10).
fn failed_batch(
    total: usize,
    error: Error,
    transaction_id: String,
    started: Instant,
) -> BatchResult {
    BatchResult {
        success: false,
        executed: 0,
        total,
        failed_at: None,
        changes: vec![],
        errors: vec![error.to_string()],
        records: vec![],
        transaction_id,
        duration_ms: started.elapsed().as_millis() as u64,
    }
}

/// Human-readable one-line summary of a batch op, for `BatchResult::changes`
/// (mirrors the git-path `describe_op` so the wire shape is backend-uniform).
fn describe_op(op: &BatchOperation) -> String {
    match op {
        BatchOperation::CreateNote { path, .. } => format!("created {}", path),
        BatchOperation::WriteNote { path, .. } => format!("wrote {}", path),
        BatchOperation::DeleteNote { path, .. } => format!("deleted {}", path),
        BatchOperation::MoveNote { from, to, .. } => format!("moved {} -> {}", from, to),
        BatchOperation::UpdateLinks { file, .. } => format!("updated links in {}", file),
        BatchOperation::EditNote { path, .. } => format!("edited {}", path),
        BatchOperation::UpdateFrontmatter { path, .. } => {
            format!("updated frontmatter in {}", path)
        }
        BatchOperation::ManageTags {
            path, operation, ..
        } => format!("{} tags in {}", operation, path),
        BatchOperation::CreateFromTemplate {
            template_id, path, ..
        } => format!("created {} from template {}", path, template_id),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_batch_tools_creation() {
        // Tests in integration tests file
    }
}
