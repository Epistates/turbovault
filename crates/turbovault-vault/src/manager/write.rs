//! The write path: every change goes through `apply_through_substrate`,
//! where `allowed_operations` is enforced, to the vault's write substrate.

use super::*;

impl VaultManager {
    /// Apply `plan` through the substrate, then run the one post-apply sync and
    /// notification every mutator shares (design R7).
    ///
    /// The plan's paths stay claimed across the apply and the sync so a
    /// concurrent freshness sweep cannot catch this write half-applied and
    /// republish it as somebody else's ([`WriteClaim`]).
    pub(super) async fn apply_through_substrate(&self, plan: &ChangePlan) -> Result<ApplyOutcome> {
        self.check_allowed_operations(plan)?;
        let outcome = {
            let _claim = WriteClaim::new(&self.write_claims, plan.touched_paths());
            let outcome = self.substrate.apply(plan).await?;
            self.sync_index(&outcome.changed).await;
            outcome
        };
        self.fire_change_listener(with_origin(outcome.changed.clone(), CommitOrigin::Local))
            .await;
        Ok(outcome)
    }

    /// Refuse a plan the vault's `allowed_operations` doesn't allow, before
    /// any of it lands. Every mutation passes through here.
    ///
    /// An upsert needs `write`, a remove `delete` and a rename `move`. A move
    /// can also be built as a copy plus a remove, which is how a link-aware
    /// move is: a remove whose bytes the plan writes under another path is
    /// one, and both halves need `move` rather than `write` and `delete`. The
    /// bytes are matched through the precondition every such move carries
    /// for its source.
    pub(super) fn check_allowed_operations(&self, plan: &ChangePlan) -> Result<()> {
        let vault = self.config.default_vault()?;
        if vault.allowed_operations.is_none() || vault.permits("all") {
            return Ok(());
        }

        // The bytes each removed path is held to, and the bytes each upsert
        // writes. A remove and an upsert with the same bytes are one move.
        let removed_token = |path: &str| {
            plan.preconditions
                .iter()
                .find_map(|(guarded, precondition)| match precondition {
                    Precondition::ExpectBlob(token) if guarded == path => Some(token.as_str()),
                    _ => None,
                })
        };
        let removed_tokens: HashSet<&str> = plan
            .changes
            .iter()
            .filter_map(|change| match change {
                Change::Remove { path } => removed_token(path),
                _ => None,
            })
            .collect();
        let mut written_tokens = HashSet::new();
        if !removed_tokens.is_empty() {
            for change in &plan.changes {
                if let Change::Upsert { content, .. } = change {
                    written_tokens.insert(self.hash_bytes(content)?);
                }
            }
        }

        let mut needed = std::collections::BTreeSet::new();
        for change in &plan.changes {
            needed.insert(match change {
                Change::Upsert { content, .. }
                    if !removed_tokens.is_empty()
                        && removed_tokens.contains(self.hash_bytes(content)?.as_str()) =>
                {
                    "move"
                }
                Change::Remove { path }
                    if removed_token(path).is_some_and(|token| written_tokens.contains(token)) =>
                {
                    "move"
                }
                Change::Upsert { .. } => "write",
                Change::Remove { .. } => "delete",
                Change::Rename { .. } => "move",
            });
        }

        let refused: Vec<&str> = needed
            .into_iter()
            .filter(|operation| !vault.permits(operation))
            .collect();
        if refused.is_empty() {
            return Ok(());
        }
        let mut allowed: Vec<&str> = vault
            .allowed_operations
            .iter()
            .flatten()
            .map(String::as_str)
            .collect();
        allowed.sort_unstable();
        Err(Error::operation_not_allowed(format!(
            "vault '{}' does not allow {} (allowed_operations: {})",
            vault.name,
            refused.join(" or "),
            allowed.join(", ")
        )))
    }

    /// Read file from cache or disk
    ///
    /// Cache entries are validated against the file's modification time on disk.
    /// If the file was modified externally (git sync, direct writes, other processes),
    /// the stale cache entry is bypassed and fresh content is read from disk.
    ///
    /// NOTE: Always reads raw file content from disk (including frontmatter).
    /// The file cache stores parsed VaultFile with frontmatter stripped from content,
    /// so it cannot be used here — callers expect the complete raw file.
    #[instrument(skip(self), fields(file = ?path), name = "vault_read_file")]
    pub async fn read_file(&self, path: &Path) -> Result<String> {
        let vault_path = self.resolve_path(path)?;
        self.ensure_size_within_limit(&vault_path).await?;

        // Always read from disk to return raw content including frontmatter.
        // The VaultFile cache stores parsed content with frontmatter stripped,
        // which would silently lose frontmatter for callers.
        let content = tokio::fs::read_to_string(&vault_path)
            .await
            .map_err(|e| Error::io_at(self.relative_path(&vault_path), e))?;

        Ok(content)
    }

    /// The version token this vault's active substrate would compute for
    /// `bytes` if they were a path's current content (design §6.3's
    /// `WriteSubstrate::hash_bytes`). For a caller that already has content
    /// in hand and wants to build its own `Precondition::ExpectBlob` (e.g. a
    /// batch fold hashing a backlink source it just read) — asking the
    /// manager rather than hardcoding one backend's hash convention keeps
    /// the token valid for whichever substrate this vault is configured for.
    pub fn hash_bytes(&self, bytes: &[u8]) -> Result<String> {
        self.substrate.hash_bytes(bytes)
    }

    /// Refuse to read a file larger than `max_file_size`.
    ///
    /// The limit is a documented safety property, but it used to be applied
    /// only while scanning directories — so a file the scan skipped could still
    /// be pulled into memory in full by an explicit read. A missing file is not
    /// this check's business; the read itself reports that.
    pub(super) async fn ensure_size_within_limit(&self, resolved: &Path) -> Result<()> {
        let Ok(metadata) = tokio::fs::metadata(resolved).await else {
            return Ok(());
        };
        let size = metadata.len();
        if size > self.config.max_file_size {
            return Err(Error::file_too_large(
                resolved,
                size,
                self.config.max_file_size,
            ));
        }
        Ok(())
    }

    /// Write file to disk atomically, guarded by an explicit [`Precondition`].
    ///
    /// write-substrate-layering M3b: the old hash-option-plus-implicit-
    /// message signature is gone — callers now build the [`Precondition`]
    /// themselves (`Precondition::for_replace` for the historical "no hash =
    /// blind overwrite-or-create" default) and supply the plan's `message`.
    /// This method builds the one-change [`ChangePlan`]
    /// and delegates to `self.substrate`; the fs-write + precondition +
    /// audit work itself lives in `DirectSubstrate::apply`. This method only
    /// does path resolution and the post-apply link-graph/cache sync (R7).
    #[instrument(skip(self, content), fields(file = ?path, size = content.len()), name = "vault_write_file")]
    pub async fn write_file(
        &self,
        path: &Path,
        content: &str,
        precondition: Precondition,
        message: &str,
    ) -> Result<()> {
        let vault_path = self.resolve_path(path)?;
        let rel_path = self.relative_path(&vault_path);
        let size = content.len() as u64;
        if size > self.config.max_file_size {
            return Err(Error::file_too_large(
                &vault_path,
                size,
                self.config.max_file_size,
            ));
        }

        let plan = ChangePlan::new(message)
            .upsert(rel_path.clone(), content.as_bytes())
            .with_precondition(rel_path, precondition);

        self.apply_one(&plan).await
    }

    /// The single-op mutators' shared tail: apply through the substrate, then
    /// re-raise a best-effort apply's error so they keep their all-or-nothing
    /// `Result` contract. Their plans hold ONE change, so "what landed" is
    /// all-or-nothing anyway — the re-raise is what stops a failed single
    /// write from being reported as a success now that
    /// `DirectSubstrate::apply` returns `Ok` for a stopped apply loop.
    ///
    /// Claiming the paths, indexing, and firing the change-listener all live
    /// in [`Self::apply_through_substrate`], which every mutation shares.
    pub(super) async fn apply_one(&self, plan: &ChangePlan) -> Result<()> {
        let outcome = self.apply_through_substrate(plan).await?;
        outcome.into_result().map(|_| ())
    }

    /// Apply an arbitrary multi-change [`ChangePlan`] — the R3 multi-change
    /// entry point the four single-op mutators above are thin builders over.
    /// Every path the plan touches is resolved through the same traversal
    /// guard (`resolve_path`) the single-op mutators run before ever
    /// building a plan, so this entry point can't become a chokepoint
    /// bypass for callers (batch, rollback, …) that build their own plans.
    /// Delegates to the substrate and runs the same post-apply link-
    /// graph/cache sync (R7) as the single-op mutators.
    ///
    /// A best-effort direct apply that stopped mid-plan (M5 S2) comes back as
    /// `Ok(outcome)` with `atomic: false`, `failed_at: Some(i)` and
    /// `error: Some(_)` — the sync above still runs over the prefix that
    /// landed, so a partially-applied plan is never invisible to the link
    /// graph, file cache and search index. Callers that want all-or-nothing
    /// `Result` semantics re-raise with [`ApplyOutcome::into_result`]; the
    /// batch reports the partial per-operation instead. A whole-plan abort
    /// (a stale precondition, or any git failure) is still a hard `Err` with
    /// nothing written.
    #[instrument(skip(self, plan), fields(changes = plan.changes.len()), name = "vault_apply_changes")]
    pub async fn apply_changes(&self, plan: &ChangePlan) -> Result<ApplyOutcome> {
        for touched in plan.touched_paths() {
            self.resolve_path(Path::new(&touched))?;
        }

        self.apply_through_substrate(plan).await
    }

    /// Edit file using SEARCH/REPLACE blocks (LLM-optimized)
    ///
    /// This method applies edits using the aider-inspired format that reduces
    /// LLM laziness by 3X. Uses cascading fuzzy matching to tolerate minor errors.
    ///
    /// # Arguments
    /// * `path` - Relative path to file in vault
    /// * `edits` - String containing SEARCH/REPLACE blocks
    /// * `precondition` - Guard checked against the pre-image the edit was
    ///   calculated from; only [`Precondition::ExpectBlob`] is meaningfully
    ///   checked here (a stale-hash rejection) — the M1 `for_in_place`
    ///   translation never produces the other variants for this call site
    /// * `dry_run` - If true, preview changes without applying
    /// * `message` - Commit/audit message for the write this edit produces
    ///
    /// # Returns
    /// EditResult with new hash, applied blocks count, and optional diff preview
    #[instrument(skip(self, edits), fields(file = ?path, dry_run), name = "vault_edit_file")]
    pub async fn edit_file(
        &self,
        path: &Path,
        edits: &str,
        precondition: Precondition,
        dry_run: bool,
        message: &str,
    ) -> Result<crate::edit::EditResult> {
        use crate::edit::EditEngine;

        let vault_path = self.resolve_path(path)?;

        // Read current content. No lock is held from here to the write: the
        // write is held to this exact pre-image (below), so an edit landing
        // in between is refused rather than overwritten, and the fuzzy match
        // doesn't stall every other reader and writer of the cache.
        let current_content = tokio::fs::read_to_string(&vault_path)
            .await
            .map_err(|e| Error::io_at(self.relative_path(&vault_path), e))?;

        // Preserve the exact pre-image that the edit was calculated from. The
        // write below revalidates this hash.
        // Mint the token via the active SUBSTRATE's convention (git blob-oid on
        // git, NFC-sha256 on direct) — the same token `edit`'s `expected`
        // precondition carries and the token the substrate re-checks at apply
        // time; using a hardcoded sha256 here would hand the git substrate an
        // unparseable ExpectBlob (write-substrate-layering M4d).
        let validated_hash = self.hash_bytes(current_content.as_bytes())?;

        if let Precondition::ExpectBlob(expected) = &precondition
            && &validated_hash != expected
        {
            return Err(Error::ConcurrencyError {
                reason: format!(
                    "File modified since read. Expected hash: {}, actual: {}. Re-read the file and try again.",
                    expected, validated_hash
                ),
            });
        }

        // Parse and apply edits. Fuzzy matching is CPU work bounded per block
        // but not in the number of blocks, so it runs on the blocking pool.
        let edits = edits.to_string();
        let (mut edit_result, new_content) = tokio::task::spawn_blocking(move || {
            let engine = EditEngine::new();
            let blocks = engine.parse_blocks(&edits)?;
            engine.apply_edits(&current_content, &blocks, dry_run)
        })
        .await
        .map_err(|e| Error::other(format!("edit task failed: {e}")))??;

        // Report substrate-native version tokens (git blob-oid on git, NFC-
        // sha256 on direct) so the caller can round-trip `new_hash` as a
        // subsequent `expected_hash` on either backend — `apply_edits` computes
        // sha256 unconditionally, which is wrong for git (matches the pre-M4d
        // GitFileTools blob-oid contract).
        edit_result.old_hash = validated_hash.clone();
        edit_result.new_hash = self.hash_bytes(new_content.as_bytes())?;

        // If dry run, return preview without writing
        if dry_run {
            return Ok(edit_result);
        }

        // Re-check at write time so an intervening in-process or external
        // change is rejected instead of silently overwritten.
        self.write_file(
            &vault_path,
            &new_content,
            Precondition::ExpectBlob(validated_hash),
            message,
        )
        .await?;

        Ok(edit_result)
    }

    /// Delete file from vault with audit trail, graph cleanup, and an
    /// explicit [`Precondition`] guard.
    ///
    /// write-substrate-layering M3b: the old hash-option signature is gone —
    /// callers translate via [`Precondition::for_in_place`] themselves (a
    /// bare delete with no hash
    /// requires the path to exist, matching the pre-M3a "attempt the remove
    /// and let it fail" behavior — both now surface as a loud error rather
    /// than an `Io` error specifically, an accepted refinement — see
    /// `DirectSubstrate::check_precondition`).
    #[instrument(skip(self), fields(file = ?path), name = "vault_delete_file")]
    pub async fn delete_file(
        &self,
        path: &Path,
        precondition: Precondition,
        message: &str,
    ) -> Result<()> {
        let vault_path = self.resolve_path(path)?;
        let rel_path = self.relative_path(&vault_path);

        let plan = ChangePlan::new(message)
            .remove(rel_path.clone())
            .with_precondition(rel_path, precondition);

        self.apply_one(&plan).await
    }

    /// Move file within vault with audit trail, graph update, and dual
    /// [`Precondition`] guards (design §6.3: one for the source, one for the
    /// destination).
    ///
    /// write-substrate-layering M3b: the old hash-option signature is gone —
    /// callers translate `from`'s guard via
    /// [`Precondition::for_in_place`] themselves; a caller preserving the
    /// pre-M3a behavior of clobbering an existing destination passes
    /// `dest_precondition = Precondition::Blind` (a real destination guard is
    /// `ChangePlan::rename`'s semantic-builder territory, not used here).
    #[instrument(skip(self), fields(from = ?from, to = ?to), name = "vault_move_file")]
    pub async fn move_file(
        &self,
        from: &Path,
        to: &Path,
        src_precondition: Precondition,
        dest_precondition: Precondition,
        message: &str,
    ) -> Result<()> {
        let from_path = self.resolve_path(from)?;
        let to_path = self.resolve_path(to)?;
        let rel_from = self.relative_path(&from_path);
        let rel_to = self.relative_path(&to_path);

        let plan = ChangePlan::new(message)
            .with_change(Change::Rename {
                from: rel_from.clone(),
                to: rel_to.clone(),
            })
            .with_precondition(rel_from, src_precondition)
            .with_precondition(rel_to, dest_precondition);

        self.apply_one(&plan).await
    }

    /// Copy a file within the vault, guarded by `dest_precondition` on the
    /// destination ([`Precondition::ExpectAbsent`] is the no-clobber guard).
    ///
    /// The copy is an ordinary one-change plan through the substrate, so it
    /// gets what every other write gets: the precondition, an audit entry on
    /// Direct, a commit on Git, and the destination indexed. The source is read
    /// as bytes, so an attachment copies as well as a note, and is held to the
    /// same `max_file_size` as any read.
    #[instrument(skip(self), fields(from = ?from, to = ?to), name = "vault_copy_file")]
    pub async fn copy_file(
        &self,
        from: &Path,
        to: &Path,
        dest_precondition: Precondition,
        message: &str,
    ) -> Result<()> {
        let from_path = self.resolve_path(from)?;
        let to_path = self.resolve_path(to)?;
        self.ensure_size_within_limit(&from_path).await?;
        let content = tokio::fs::read(&from_path)
            .await
            .map_err(|e| Error::io_at(self.relative_path(&from_path), e))?;
        let rel_to = self.relative_path(&to_path);

        let plan = ChangePlan::new(message)
            .upsert(rel_to.clone(), content)
            .with_precondition(rel_to, dest_precondition);

        self.apply_one(&plan).await
    }
}
