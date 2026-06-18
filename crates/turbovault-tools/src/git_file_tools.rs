//! Git-backed write tools (GWS.12).
//!
//! Mirrors the mutating surface of [`crate::FileTools`] + [`crate::BatchTools`]
//! but routes every change through the git substrate's
//! [`turbovault_git::VaultRepo::apply_transaction`]. Reads still go through the
//! shared [`VaultManager`] — working tree == HEAD, so working-tree reads agree
//! with the git tip.
//!
//! Selected per vault by [`turbovault_core::config::WriteBackend::Git`]. Lives
//! beside `FileTools` only until the cutover (GWS.15), then this becomes the
//! sole write surface.
//!
//! **Discipline (do not violate):** `GitFileTools` reads from `VaultManager`
//! but must **never** call its mutators (`write_file`/`edit_file`/`delete_file`
//! /`move_file`). All mutations route through [`VaultRepo::apply_transaction`].

use crate::file_tools::{NoteInfo, WriteMode};
use crate::read_set::{ReadSet, apply_read_set_to_transaction};
use futures::future::BoxFuture;
use std::path::PathBuf;
use std::sync::Arc;
use turbovault_batch::{BatchOperation, BatchResult, OperationRecord};
use turbovault_core::prelude::*;
use turbovault_git::{CommitHook, CommitLocks, Oid, Transaction, VaultRepo};
use turbovault_vault::{EditEngine, EditResult, VaultManager};

/// turbovault-lqr: result of an atomic `move_file_with_link_updates`. The
/// rename + every link-source rewrite landed as ONE commit. Reports which
/// sources were rewritten so the caller can surface the diff to the user.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MoveWithLinksResult {
    pub from: String,
    pub to: String,
    /// Vault-relative paths of source files whose inbound wikilinks were
    /// rewritten in the same commit.
    pub link_sources_updated: Vec<String>,
}

/// Callback invoked **before** returning a `ConcurrencyError` from
/// [`GitFileTools::apply_txn`] (GWS.14b). The MCP server installs one that
/// drains the per-vault reindex queue — so the agent's re-read (which the
/// error tells it to do) sees a coherent graph + search state, not the
/// pre-conflict snapshot.
///
/// Boxed-future shape rather than a plain `async fn` so the type can be
/// stored on the `GitFileTools` struct without each call site naming an
/// `impl Future`.
pub type CasCollisionFlush = Arc<dyn Fn() -> BoxFuture<'static, Result<()>> + Send + Sync>;

/// turbovault-a0l (PERF-1): a per-vault cached substrate handle. `VaultRepo`
/// wraps a `git2::Repository` which is `Send + !Sync` (libgit2 raw pointers),
/// so it lives behind a `std::sync::Mutex`; the `Arc` lets the MCP server cache
/// one handle per vault and hand a clone to each `GitFileTools`. Reusing it
/// elides the ~140µs `Repository::open` (config re-parse + odb/strmap setup)
/// that otherwise fired on every write. The `Mutex` serializes commit sections
/// exactly where `CommitLocks` already does, so net concurrency is unchanged,
/// and cross-process CAS stays safe (libgit2 re-reads refs under `lock_ref` —
/// guarded by `cas::tests::reused_handle_detects_external_ref_advance_no_lost_update`).
pub type CachedRepo = Arc<std::sync::Mutex<VaultRepo>>;

/// Write-side tools backed by the git substrate.
///
/// Holds the vault path + a shared `CommitLocks` registry rather than an
/// `Arc<VaultRepo>` — `VaultRepo` wraps a `git2::Repository` which is `!Sync`
/// (raw pointer), so it cannot live inside an `async fn` future that needs
/// to be `Send`. The substrate handle is opened fresh inside a
/// `spawn_blocking` task per call (open is ~µs); the shared `CommitLocks`
/// keeps cross-call commit-section serialization intact.
#[derive(Clone)]
pub struct GitFileTools {
    pub manager: Arc<VaultManager>,
    pub vault_path: PathBuf,
    pub commit_locks: Arc<CommitLocks>,
    /// Optional post-commit hook installed on every `VaultRepo` opened
    /// inside `apply_txn`. Plumbed for GWS.14 lazy GSU: the MCP server
    /// passes a closure that pushes the new commit onto a per-vault
    /// `ReindexQueue`. `None` = no reindex wiring (acceptable for tests
    /// that don't care about derived state).
    pub commit_hook: Option<CommitHook>,
    /// Optional flush callback fired BEFORE returning a `ConcurrencyError`
    /// (GWS.14b). Drains the reindex queue so the agent's re-read sees
    /// coherent derived state. `None` = skip flush; callers see the raw
    /// concurrency error and the graph stays as stale as the last
    /// flush-on-query did.
    pub flush_on_collision: Option<CasCollisionFlush>,
    /// turbovault-lri: when `false`, every mutation pre-checks each
    /// touched path against the worktree's `.gitignore` matcher and
    /// refuses the transaction if any path would be ignored. Default
    /// `true` preserves pre-lri "always-write" behavior. Wired from
    /// `VaultGitConfig::include_ignored` by the MCP server.
    pub include_ignored: bool,
    /// turbovault-a0l (PERF-1): optional cached per-vault `VaultRepo` handle.
    /// When `Some`, `apply_txn` reuses it instead of opening a fresh repo per
    /// call (saving the ~140µs `Repository::open`). The MCP server installs one
    /// shared across all in-process writes to the vault. Bare `Self::new*`
    /// leaves it `None`, falling back to per-call open (tests / migrations that
    /// don't run the server-side cache).
    pub cached_repo: Option<CachedRepo>,
}

impl GitFileTools {
    /// Construct without a reindex hook (graph + search stay stale until
    /// another path triggers their rebuild). Tests use this; the MCP
    /// server uses [`Self::new_with_hook`].
    pub fn new(
        manager: Arc<VaultManager>,
        vault_path: PathBuf,
        commit_locks: Arc<CommitLocks>,
    ) -> Self {
        Self {
            manager,
            vault_path,
            commit_locks,
            commit_hook: None,
            flush_on_collision: None,
            include_ignored: true,
            cached_repo: None,
        }
    }

    /// Construct with a reindex hook fired post-commit. The MCP server
    /// installs one that pushes onto a per-vault [`crate::ReindexQueue`].
    pub fn new_with_hook(
        manager: Arc<VaultManager>,
        vault_path: PathBuf,
        commit_locks: Arc<CommitLocks>,
        commit_hook: CommitHook,
    ) -> Self {
        Self {
            manager,
            vault_path,
            commit_locks,
            commit_hook: Some(commit_hook),
            flush_on_collision: None,
            include_ignored: true,
            cached_repo: None,
        }
    }

    /// Construct with both a reindex hook AND a CAS-collision flush callback
    /// (GWS.14b). The flush callback runs BEFORE the `ConcurrencyError` is
    /// returned to the caller, so the agent's re-read sees coherent derived
    /// state.
    pub fn new_with_hook_and_flush(
        manager: Arc<VaultManager>,
        vault_path: PathBuf,
        commit_locks: Arc<CommitLocks>,
        commit_hook: CommitHook,
        flush_on_collision: CasCollisionFlush,
    ) -> Self {
        Self {
            manager,
            vault_path,
            commit_locks,
            commit_hook: Some(commit_hook),
            flush_on_collision: Some(flush_on_collision),
            include_ignored: true,
            cached_repo: None,
        }
    }

    /// turbovault-lri: builder-style override for `include_ignored`.
    /// `false` makes every subsequent mutation pre-check each touched
    /// path against the worktree's `.gitignore` matcher and refuse the
    /// transaction if any path would be ignored. Default `true`.
    pub fn with_include_ignored(mut self, include_ignored: bool) -> Self {
        self.include_ignored = include_ignored;
        self
    }

    /// turbovault-a0l (PERF-1): install a cached per-vault `VaultRepo` handle so
    /// writes reuse it instead of opening a fresh repo per call. The handle must
    /// already carry the shared `CommitLocks` + reindex `CommitHook` (the MCP
    /// server opens it that way via `get_or_init_git_repo`). When set, `apply_txn`
    /// ignores `commit_locks`/`commit_hook` on `self` — the cached handle owns
    /// both.
    pub fn with_cached_repo(mut self, cached_repo: CachedRepo) -> Self {
        self.cached_repo = Some(cached_repo);
        self
    }

    // -------- Reads (forwarded to VaultManager / fs) --------

    /// Read a file from the vault (working tree == HEAD, so this is the
    /// committed bytes).
    pub async fn read_file(&self, path: &str) -> Result<String> {
        self.manager.read_file(&PathBuf::from(path)).await
    }

    /// Lightweight metadata for multiple files — same shape as
    /// [`crate::FileTools::get_notes_info`].
    pub async fn get_notes_info(&self, paths: &[String]) -> Result<Vec<NoteInfo>> {
        let files = crate::FileTools::new(Arc::clone(&self.manager));
        files.get_notes_info(paths).await
    }

    // -------- Writes (route through VaultRepo) --------

    /// Write a file — overwrite by default, append/prepend for the other
    /// modes (mirrors [`crate::FileTools::write_file_with_mode`]).
    ///
    /// `expected_hash`, when present, must be a **git blob oid hex string**
    /// (40 hex chars). The substrate's version token is the blob oid (not a
    /// SHA-256 content hash). A non-Oid string is rejected loudly rather than
    /// silently dropping CAS protection.
    pub async fn write_file_with_mode(
        &self,
        path: &str,
        content: &str,
        mode: WriteMode,
        expected_hash: Option<&str>,
    ) -> Result<()> {
        self.write_file_with_read_set(path, content, mode, expected_hash, None)
            .await
    }

    /// turbovault-0bh: caller-supplied commit message variant of
    /// [`Self::write_file_with_mode`]. Substrate auto-derives the message
    /// otherwise (`write_file <path>`); this override lets the MCP layer
    /// pass a richer message (caller's text + verb=tool_name per TV-008).
    pub async fn write_file_with_mode_and_message(
        &self,
        path: &str,
        content: &str,
        mode: WriteMode,
        expected_hash: Option<&str>,
        message: &str,
    ) -> Result<()> {
        let final_content = self.resolve_write_content(path, content, mode).await?;
        let expected = parse_blob_oid(expected_hash)?;
        let txn = build_upsert_txn(message.to_string(), path, &final_content, expected);
        self.apply_txn(&txn).await
    }

    /// Overwrite shortcut — equivalent to `write_file_with_mode(.., Overwrite, None)`.
    pub async fn write_file(&self, path: &str, content: &str) -> Result<()> {
        self.write_file_with_mode(path, content, WriteMode::Overwrite, None)
            .await
    }

    /// Strict create: write a NEW file with an `expect_absent` precondition.
    /// If the path becomes occupied between the caller's check and the
    /// substrate's CAS, `apply_txn` returns `ConcurrencyError` — the create
    /// race the MCP layer's pre-check cannot close on its own.
    ///
    /// This is the substrate-side guarantee for turbovault-947 / write-note
    /// CAS-by-default: even with parallel subagents racing to create the
    /// same absent path, exactly one commit lands; the loser sees a loud
    /// ConcurrencyError and re-decides.
    pub async fn create_file(&self, path: &str, content: &str) -> Result<()> {
        self.create_file_with_message(path, content, &format!("create_file {}", path))
            .await
    }

    /// turbovault-0bh: caller-supplied commit message variant of
    /// [`Self::create_file`]. The `message` becomes the commit subject (and
    /// body, when newline-separated). All other semantics are identical.
    pub async fn create_file_with_message(
        &self,
        path: &str,
        content: &str,
        message: &str,
    ) -> Result<()> {
        let txn = Transaction::new(message.to_string()).create(path, content.as_bytes().to_vec());
        self.apply_txn(&txn).await
    }

    /// Edit a file via SEARCH/REPLACE blocks. Reads working-tree bytes,
    /// applies the blocks in memory, and commits the result as one
    /// transaction. `dry_run = true` returns the preview without committing.
    pub async fn edit_file(
        &self,
        path: &str,
        edits: &str,
        expected_hash: Option<&str>,
        dry_run: bool,
    ) -> Result<EditResult> {
        let expected = parse_blob_oid(expected_hash)?;

        // Read current bytes from the working tree.
        let current = self.read_file(path).await?;

        // Apply SEARCH/REPLACE blocks.
        let engine = EditEngine::new();
        let blocks = engine.parse_blocks(edits)?;
        let (mut result, new_content) = engine.apply_edits(&current, &blocks, dry_run)?;

        // turbovault-6sj / TV-011: overwrite the SHA-256s that EditEngine
        // computed with git blob OIDs so the returned `old_hash`/`new_hash`
        // round-trip via `expected_hash` against the substrate's
        // `expect_blob` precondition. Same in the dry-run path so callers
        // can use a preview's `new_hash` as the next `expected_hash`.
        result.old_hash = VaultRepo::blob_oid_of(current.as_bytes())
            .map_err(|e| Error::config_error(format!("blob_oid_of(current): {}", e)))?
            .to_string();
        result.new_hash = VaultRepo::blob_oid_of(new_content.as_bytes())
            .map_err(|e| Error::config_error(format!("blob_oid_of(new): {}", e)))?
            .to_string();

        if dry_run {
            return Ok(result);
        }

        let txn = build_upsert_txn(format!("edit_file {}", path), path, &new_content, expected);
        self.apply_txn(&txn).await?;
        Ok(result)
    }

    /// turbovault-0bh: caller-supplied commit message variant of
    /// [`Self::edit_file`]. Behaviorally identical except the commit
    /// subject is the caller's message instead of the auto-derived
    /// `edit_file <path>`.
    pub async fn edit_file_with_message(
        &self,
        path: &str,
        edits: &str,
        expected_hash: Option<&str>,
        dry_run: bool,
        message: &str,
    ) -> Result<EditResult> {
        let expected = parse_blob_oid(expected_hash)?;
        let current = self.read_file(path).await?;
        let engine = EditEngine::new();
        let blocks = engine.parse_blocks(edits)?;
        let (mut result, new_content) = engine.apply_edits(&current, &blocks, dry_run)?;
        // 6sj: blob-OID hashes; same as edit_file.
        result.old_hash = VaultRepo::blob_oid_of(current.as_bytes())
            .map_err(|e| Error::config_error(format!("blob_oid_of(current): {}", e)))?
            .to_string();
        result.new_hash = VaultRepo::blob_oid_of(new_content.as_bytes())
            .map_err(|e| Error::config_error(format!("blob_oid_of(new): {}", e)))?
            .to_string();
        if dry_run {
            return Ok(result);
        }
        let txn = build_upsert_txn(message.to_string(), path, &new_content, expected);
        self.apply_txn(&txn).await?;
        Ok(result)
    }

    /// Delete a file. `expected_hash` (blob oid hex) enforces a CAS
    /// precondition — pass `None` for a blind delete.
    pub async fn delete_file(&self, path: &str) -> Result<()> {
        self.delete_file_with_hash(path, None).await
    }

    /// Delete with optional blob-oid CAS.
    pub async fn delete_file_with_hash(
        &self,
        path: &str,
        expected_hash: Option<&str>,
    ) -> Result<()> {
        let expected = parse_blob_oid(expected_hash)?;
        let mut txn = Transaction::new(format!("delete_file {}", path)).remove(path);
        if let Some(oid) = expected {
            txn = txn.expect_blob(path, oid);
        }
        self.apply_txn(&txn).await
    }

    /// turbovault-0bh: caller-supplied commit message variant of
    /// [`Self::delete_file_with_hash`].
    pub async fn delete_file_with_hash_and_message(
        &self,
        path: &str,
        expected_hash: Option<&str>,
        message: &str,
    ) -> Result<()> {
        let expected = parse_blob_oid(expected_hash)?;
        let mut txn = Transaction::new(message.to_string()).remove(path);
        if let Some(oid) = expected {
            txn = txn.expect_blob(path, oid);
        }
        self.apply_txn(&txn).await
    }

    /// Move a file — `remove(from) + upsert(to, bytes)` in one commit.
    pub async fn move_file(&self, from: &str, to: &str) -> Result<()> {
        self.move_file_with_hash(from, to, None).await
    }

    /// Move with optional blob-oid CAS on the source path.
    pub async fn move_file_with_hash(
        &self,
        from: &str,
        to: &str,
        expected_hash: Option<&str>,
    ) -> Result<()> {
        self.move_file_with_hash_and_message(
            from,
            to,
            expected_hash,
            &format!("move_file {} -> {}", from, to),
        )
        .await
    }

    /// turbovault-0bh: caller-supplied commit message variant of
    /// [`Self::move_file_with_hash`].
    pub async fn move_file_with_hash_and_message(
        &self,
        from: &str,
        to: &str,
        expected_hash: Option<&str>,
        message: &str,
    ) -> Result<()> {
        let expected_from = parse_blob_oid(expected_hash)?;
        let content = self.read_file(from).await?;

        let mut txn = Transaction::new(message.to_string())
            .remove(from)
            .upsert(to, content.into_bytes());
        if let Some(oid) = expected_from {
            txn = txn.expect_blob(from, oid);
        }
        // Destination is always required to be absent — refuses to clobber.
        txn = txn.expect_absent(to);
        self.apply_txn(&txn).await
    }

    /// turbovault-oz6: atomic delete + inbound-wikilink wrap-as-stale.
    /// Removes `path` AND rewrites every backlinking source's wikilinks
    /// targeting it as `~~[[old]]~~` strikethrough (signaling a dead
    /// reference) — all in **one substrate transaction**.
    ///
    /// Each source carries an `expect_blob` precondition; a concurrent
    /// edit to ANY source aborts the whole delete with
    /// `ConcurrencyError`. `expected_hash` (optional, blob OID hex)
    /// guards the target page itself.
    ///
    /// Returns the list of source paths whose content was rewritten so
    /// the caller can surface what changed.
    pub async fn delete_file_with_link_rewrite_to_stale(
        &self,
        path: &str,
        expected_hash: Option<&str>,
        message: &str,
    ) -> Result<MoveWithLinksResult> {
        use crate::wikilink_rewriter::wrap_wikilinks_as_stale;

        let expected_target = parse_blob_oid(expected_hash)?;

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

        let mut link_updates: Vec<(String, String, Oid)> = Vec::new();
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
            let src_oid = VaultRepo::blob_oid_of(src_content.as_bytes())
                .map_err(|e| Error::config_error(format!("blob_oid_of: {}", e)))?;
            link_updates.push((rel_str, rewritten, src_oid));
        }

        let mut txn = Transaction::new(message.to_string()).remove(path);
        if let Some(oid) = expected_target {
            txn = txn.expect_blob(path, oid);
        }
        for (rel_path, rewritten, oid) in &link_updates {
            txn = txn
                .upsert(rel_path.clone(), rewritten.clone().into_bytes())
                .expect_blob(rel_path.clone(), *oid);
        }

        self.apply_txn(&txn).await?;

        Ok(MoveWithLinksResult {
            from: path.to_string(),
            to: String::new(), // No destination for a delete.
            link_sources_updated: link_updates.into_iter().map(|(p, _, _)| p).collect(),
        })
    }

    /// turbovault-oz6: return the list of vault-relative source paths
    /// that have inbound wikilinks targeting `path`. Used by the MCP
    /// layer's "refuse-if-backlinks" pre-check (option A) before
    /// committing to a delete.
    pub async fn list_inbound_backlinks(&self, path: &str) -> Result<Vec<String>> {
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
            if let Some(s) = rel.to_str() {
                out.push(s.to_string());
            }
        }
        Ok(out)
    }

    /// turbovault-lqr: atomic move + inbound-wikilink rewrite. Renames
    /// `from` -> `to` AND rewrites every backlinking source's
    /// `[[from-basename]]` / `[[from-path]]` (plus alias / section /
    /// block-anchor / embed forms) to point at the new target, all in
    /// **one substrate transaction**.
    ///
    /// Per-source CAS: each rewritten source carries an `expect_blob`
    /// precondition. If ANY source's blob OID changed between the
    /// read-modify and the substrate apply, the WHOLE transaction
    /// aborts (architecture §6.3 reconsideration domino). The
    /// destination always carries `expect_absent` (no clobber).
    ///
    /// `expected_hash` (optional, blob OID hex) protects the SOURCE
    /// against a concurrent edit between the caller's read and this
    /// call.
    ///
    /// Returns the list of source paths whose content was rewritten so
    /// the caller can surface what changed.
    pub async fn move_file_with_link_updates(
        &self,
        from: &str,
        to: &str,
        expected_hash: Option<&str>,
        message: &str,
    ) -> Result<MoveWithLinksResult> {
        use crate::wikilink_rewriter::rewrite_wikilinks;
        use std::path::Path as StdPath;

        let expected_from = parse_blob_oid(expected_hash)?;
        let content = self.read_file(from).await?;

        // Resolve backlinks via the in-memory link graph (kept coherent
        // by the substrate's CommitHook + drainer / external-listener).
        // Source paths are vault-relative PathBuf.
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

        // Read each source, rewrite, capture blob OID for the
        // precondition. Skip sources whose rewritten content equals
        // the original (no actual link change — e.g. the source's
        // `[[from]]` literal sits in a code fence the parser missed).
        let mut link_updates: Vec<(String, String, Oid)> = Vec::new();
        for full_src in &backlink_paths {
            // Strip the vault prefix to get the substrate-relative path.
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
            let src_oid = VaultRepo::blob_oid_of(src_content.as_bytes())
                .map_err(|e| Error::config_error(format!("blob_oid_of: {}", e)))?;
            link_updates.push((rel_str, rewritten, src_oid));
        }

        // Build the atomic transaction: source rename + each link
        // source's rewrite, with their preconditions.
        let mut txn = Transaction::new(message.to_string())
            .remove(from)
            .upsert(to, content.into_bytes());
        if let Some(oid) = expected_from {
            txn = txn.expect_blob(from, oid);
        }
        txn = txn.expect_absent(to);
        for (rel_path, rewritten, oid) in &link_updates {
            txn = txn
                .upsert(rel_path.clone(), rewritten.clone().into_bytes())
                .expect_blob(rel_path.clone(), *oid);
        }

        self.apply_txn(&txn).await?;

        // Mark the destination's directory as moved-from-known if we
        // need it later; for now we just report the diff.
        let _ = StdPath::new(to); // (no-op; reserved)
        Ok(MoveWithLinksResult {
            from: from.to_string(),
            to: to.to_string(),
            link_sources_updated: link_updates.into_iter().map(|(p, _, _)| p).collect(),
        })
    }

    /// Copy a file — read source, commit target (no source change). One
    /// commit. Same expect-absent guard on the destination as `move_file`.
    pub async fn copy_file(&self, from: &str, to: &str) -> Result<()> {
        let content = self.read_file(from).await?;
        let txn = Transaction::new(format!("copy_file {} -> {}", from, to))
            .upsert(to, content.into_bytes())
            .expect_absent(to);
        self.apply_txn(&txn).await
    }

    // -------- Batch (the atomicity win) --------

    /// Translate every [`BatchOperation`] to a single [`Transaction`] and
    /// commit as **one atomic commit** — either every op lands or none do.
    /// This is the spec-promised behavior the legacy [`BatchTools`] never
    /// actually delivered (the legacy path stopped at `failed_at` and left
    /// partial state on disk).
    pub async fn batch_execute(&self, operations: Vec<BatchOperation>) -> Result<BatchResult> {
        self.batch_execute_with_read_set(operations, None).await
    }

    /// turbovault-0bh: caller-supplied commit message variant of
    /// [`Self::batch_execute`]. Overrides the auto-derived
    /// `batch_execute (N ops)` subject with the caller's message.
    pub async fn batch_execute_with_message(
        &self,
        operations: Vec<BatchOperation>,
        message: &str,
    ) -> Result<BatchResult> {
        self.batch_execute_inner(operations, None, Some(message))
            .await
    }

    // -------- internals --------

    async fn translate_op(&self, txn: Transaction, op: &BatchOperation) -> Result<Transaction> {
        // Per-op preconditions (turbovault-c0e). Every variant that touches
        // an existing target accepts `expected_hash` (git blob OID hex on
        // git backend); `CreateNote` carries an implicit `expect_absent`
        // unless `force == Some(true)`. A mismatch on any single op aborts
        // the whole batch (architecture §6.3 reconsideration domino).
        Ok(match op {
            BatchOperation::CreateNote {
                path,
                content,
                force,
            } => {
                if force.unwrap_or(false) {
                    // Caller-acknowledged blind create/overwrite — drops
                    // expect_absent. Equivalent to a WriteNote with no
                    // expected_hash but kept under CreateNote semantics
                    // for the intent the caller declared.
                    txn.upsert(path, content.as_bytes())
                } else {
                    // Strict create — `txn.create` carries `expect_absent`.
                    txn.create(path, content.as_bytes())
                }
            }
            BatchOperation::WriteNote {
                path,
                content,
                expected_hash,
            } => {
                let mut t = txn.upsert(path, content.as_bytes());
                if let Some(oid) = parse_blob_oid(expected_hash.as_deref())? {
                    t = t.expect_blob(path, oid);
                }
                t
            }
            BatchOperation::DeleteNote {
                path,
                expected_hash,
            } => {
                let mut t = txn.remove(path);
                if let Some(oid) = parse_blob_oid(expected_hash.as_deref())? {
                    t = t.expect_blob(path, oid);
                }
                t
            }
            BatchOperation::MoveNote {
                from,
                to,
                expected_hash,
            } => {
                let content = self.read_file(from).await?;
                let mut t = txn.remove(from).upsert(to, content.into_bytes());
                if let Some(oid) = parse_blob_oid(expected_hash.as_deref())? {
                    t = t.expect_blob(from, oid);
                }
                // Destination always carries expect_absent — single-op
                // move_file does the same.
                t = t.expect_absent(to);
                t
            }
            BatchOperation::UpdateLinks {
                file,
                old_target,
                new_target,
                expected_hash,
            } => {
                let current = self.read_file(file).await?;
                let updated = current.replace(old_target, new_target);
                let mut t = txn.upsert(file, updated.into_bytes());
                if let Some(oid) = parse_blob_oid(expected_hash.as_deref())? {
                    t = t.expect_blob(file, oid);
                }
                t
            }
            BatchOperation::EditNote {
                path,
                edits,
                expected_hash,
            } => {
                // turbovault-0g4.1: SEARCH/REPLACE blocks folded into the batch
                // commit. Reads working-tree bytes, applies the blocks in
                // memory (multiple blocks → multiple edited locations), and
                // upserts the result — the same EditEngine path `edit_file`
                // uses, minus the dry-run/hash reporting a batch doesn't need.
                let current = self.read_file(path).await?;
                let engine = EditEngine::new();
                let blocks = engine.parse_blocks(edits)?;
                let (_result, new_content) = engine.apply_edits(&current, &blocks, false)?;
                let mut t = txn.upsert(path, new_content.into_bytes());
                if let Some(oid) = parse_blob_oid(expected_hash.as_deref())? {
                    t = t.expect_blob(path, oid);
                }
                t
            }
            BatchOperation::UpdateFrontmatter {
                path,
                frontmatter,
                merge,
                expected_hash,
            } => {
                // turbovault-0g4.2: reuse the pure compute helper the
                // `update_frontmatter` tool uses (read + merge in memory), then
                // fold the resulting full content into the batch commit. The
                // helper reads via VaultManager (a read — invariant-safe); the
                // write rides the substrate transaction.
                let mt = crate::MetadataTools::new(Arc::clone(&self.manager));
                let fm_map: serde_json::Map<String, serde_json::Value> =
                    frontmatter.clone().into_iter().collect();
                let (new_content, _info) = mt
                    .compute_update_frontmatter(path, fm_map, merge.unwrap_or(true))
                    .await?;
                let mut t = txn.upsert(path, new_content.into_bytes());
                if let Some(oid) = parse_blob_oid(expected_hash.as_deref())? {
                    t = t.expect_blob(path, oid);
                }
                t
            }
        })
    }

    /// GWS.5fm: write with an optional read-set precondition. Equivalent
    /// to [`Self::write_file_with_mode`] when `read_set` is `None`. When
    /// `Some`, every `(path, oid)` in the read-set becomes an additional
    /// `expect_blob` precondition on the same transaction — a concurrent
    /// change to any of those source files aborts the write loudly
    /// (`ConcurrencyError`) so the agent re-reads + re-decides.
    pub async fn write_file_with_read_set(
        &self,
        path: &str,
        content: &str,
        mode: WriteMode,
        expected_hash: Option<&str>,
        read_set: Option<&ReadSet>,
    ) -> Result<()> {
        let final_content = self.resolve_write_content(path, content, mode).await?;
        let expected = parse_blob_oid(expected_hash)?;
        let txn = build_upsert_txn(
            format!("write_file {}", path),
            path,
            &final_content,
            expected,
        );
        self.apply_txn_augmented(txn, read_set).await
    }

    /// GWS.5fm: batch with an optional read-set precondition. The read-set's
    /// preconditions ride alongside the per-op preconditions, so the whole
    /// batch aborts atomically if any read-set source file changed.
    pub async fn batch_execute_with_read_set(
        &self,
        operations: Vec<BatchOperation>,
        read_set: Option<&ReadSet>,
    ) -> Result<BatchResult> {
        self.batch_execute_inner(operations, read_set, None).await
    }

    /// turbovault-0bh internal: full batch implementation accepting an
    /// optional caller-supplied commit message. `None` falls back to the
    /// auto-derived `batch_execute (N ops)` subject (existing behavior).
    async fn batch_execute_inner(
        &self,
        operations: Vec<BatchOperation>,
        read_set: Option<&ReadSet>,
        message: Option<&str>,
    ) -> Result<BatchResult> {
        let started = std::time::Instant::now();
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

        let commit_msg = message
            .map(String::from)
            .unwrap_or_else(|| format!("batch_execute ({} ops)", total));
        let mut txn = Transaction::new(commit_msg);
        let mut changes = Vec::with_capacity(total);
        let mut records = Vec::with_capacity(total);
        // turbovault-0g4.5: intra-batch same-path conflict policy. The git
        // path skips the legacy `validate()`/`conflicts_with()` O(n²) check;
        // the substrate DOES reject a transaction with duplicate change paths
        // (`apply_transaction` → "duplicate change for path …"), but only at
        // apply time and with a message that names neither the offending op
        // index nor that the cause is a *batch* overlap. Detect the collision
        // here instead — as each op folds into the shared transaction, any path
        // it newly writes that an earlier op already wrote aborts the batch with
        // a clear, op-indexed error. Reject-overlap (not coalesce): a path may
        // be mutated by at most one op per batch.
        let mut seen_paths: std::collections::HashSet<String> = std::collections::HashSet::new();

        for (idx, op) in operations.iter().enumerate() {
            let operation_desc = format!("{:?}", op);
            let affected = op.affected_files();
            // Paths already folded into `txn` before this op runs; anything
            // appended past this index is what THIS op contributes.
            let before = txn.touched_paths().len();
            match self.translate_op(txn, op).await {
                Ok(next) => {
                    txn = next;
                    if let Some(dup) = txn
                        .touched_paths()
                        .into_iter()
                        .skip(before)
                        .find(|p| !seen_paths.insert(p.clone()))
                    {
                        let err_msg = format!(
                            "intra-batch path collision (turbovault-0g4.5): operation {} writes '{}', which an earlier operation in this batch already writes. A path may be mutated by at most one operation per batch — split the conflicting writes across separate batches.",
                            idx, dup
                        );
                        records.push(OperationRecord {
                            operation_index: idx,
                            operation: operation_desc,
                            success: false,
                            error: Some(err_msg.clone()),
                            affected_files: affected,
                        });
                        return Ok(BatchResult {
                            success: false,
                            executed: idx,
                            total,
                            failed_at: Some(idx),
                            changes,
                            errors: vec![err_msg],
                            records,
                            transaction_id,
                            duration_ms: started.elapsed().as_millis() as u64,
                        });
                    }
                    changes.push(describe_op(op));
                    records.push(OperationRecord {
                        operation_index: idx,
                        operation: operation_desc,
                        success: true,
                        error: None,
                        affected_files: affected,
                    });
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    records.push(OperationRecord {
                        operation_index: idx,
                        operation: operation_desc,
                        success: false,
                        error: Some(err_msg.clone()),
                        affected_files: affected,
                    });
                    return Ok(BatchResult {
                        success: false,
                        executed: idx,
                        total,
                        failed_at: Some(idx),
                        changes,
                        errors: vec![err_msg],
                        records,
                        transaction_id,
                        duration_ms: started.elapsed().as_millis() as u64,
                    });
                }
            }
        }

        match self.apply_txn_augmented(txn, read_set).await {
            Ok(()) => Ok(BatchResult {
                success: true,
                executed: total,
                total,
                failed_at: None,
                changes,
                errors: vec![],
                records,
                transaction_id,
                duration_ms: started.elapsed().as_millis() as u64,
            }),
            Err(e) => {
                let err_msg = e.to_string();
                // turbovault-jk6 (TV-013): the per-op records were built
                // `success: true` during the translate loop, but an apply-phase
                // abort (a stale CAS precondition rolls the whole batch back)
                // commits NOTHING (`executed: 0`). Re-mark every op not-applied
                // so a caller iterating `records[]` cannot conclude any op
                // committed. Point the error at the op whose path the failure
                // names; the rest are rolled back.
                for rec in records.iter_mut() {
                    rec.success = false;
                    rec.error = Some(
                        if rec
                            .affected_files
                            .iter()
                            .any(|f| err_msg.contains(f.as_str()))
                        {
                            err_msg.clone()
                        } else {
                            format!("rolled back (batch aborted): {err_msg}")
                        },
                    );
                }
                Ok(BatchResult {
                    success: false,
                    executed: 0,
                    total,
                    failed_at: None,
                    changes: vec![],
                    errors: vec![err_msg],
                    records,
                    transaction_id,
                    duration_ms: started.elapsed().as_millis() as u64,
                })
            }
        }
    }

    /// Internal helper used by every commit path. Applies `txn` after
    /// optionally augmenting it with `read_set` preconditions.
    async fn apply_txn_augmented(
        &self,
        txn: Transaction,
        read_set: Option<&ReadSet>,
    ) -> Result<()> {
        let txn = match read_set {
            Some(rs) => apply_read_set_to_transaction(txn, rs)?,
            None => txn,
        };
        self.apply_txn(&txn).await
    }

    /// Compute the bytes to write given mode + path. Extracted so
    /// `write_file_with_mode` and `write_file_with_read_set` share one
    /// implementation.
    async fn resolve_write_content(
        &self,
        path: &str,
        content: &str,
        mode: WriteMode,
    ) -> Result<String> {
        Ok(match mode {
            WriteMode::Overwrite => content.to_string(),
            WriteMode::Append => {
                let existing = self.read_file(path).await.unwrap_or_default();
                if existing.is_empty() {
                    content.to_string()
                } else {
                    format!("{}\n{}", existing, content)
                }
            }
            WriteMode::Prepend => {
                let existing = self.read_file(path).await.unwrap_or_default();
                if existing.is_empty() {
                    content.to_string()
                } else if existing.starts_with("---\n") || existing.starts_with("---\r\n") {
                    if let Some(end_idx) = find_frontmatter_end(&existing) {
                        let (fm, body) = existing.split_at(end_idx);
                        format!("{}\n{}\n{}", fm.trim_end(), content, body.trim_start())
                    } else {
                        format!("{}\n{}", content, existing)
                    }
                } else {
                    format!("{}\n{}", content, existing)
                }
            }
        })
    }

    async fn apply_txn(&self, txn: &Transaction) -> Result<()> {
        // `VaultRepo` is `Send` but `!Sync`; the substrate work is blocking
        // libgit2. Move it to the blocking pool. The `Arc<CommitLocks>` is
        // shared across calls so cross-call commit-section serialization
        // survives even though we open a fresh `VaultRepo` per call. The
        // optional commit hook is cloned in and installed on each per-call
        // open so the substrate fires it after a successful materialize.
        let txn = txn.clone();
        let include_ignored = self.include_ignored;
        let result = match &self.cached_repo {
            // turbovault-a0l (PERF-1): reuse the cached per-vault handle — no
            // per-op `Repository::open`. Lock it on the blocking thread (the
            // Mutex makes the `!Sync` `VaultRepo` workable and serializes the
            // commit section, matching the CommitLocks boundary writes already
            // pass through). Cross-process CAS stays safe (libgit2 re-reads refs
            // under `lock_ref`).
            Some(cached) => {
                let cached = Arc::clone(cached);
                tokio::task::spawn_blocking(move || -> Result<()> {
                    let repo = cached
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    run_txn(&repo, &txn, include_ignored)
                })
                .await
                .map_err(|e| Error::config_error(format!("git transaction task failed: {}", e)))?
            }
            // Fallback: open a fresh `VaultRepo` per call (the pre-PERF-1 path).
            // Used by bare `Self::new*` — tests / migrations without the
            // server-side cache. The `Arc<CommitLocks>` is shared across calls
            // so cross-call commit-section serialization survives; the optional
            // hook is installed on each per-call open.
            None => {
                let path = self.vault_path.clone();
                let locks = Arc::clone(&self.commit_locks);
                let hook = self.commit_hook.clone();
                tokio::task::spawn_blocking(move || -> Result<()> {
                    let repo = match hook {
                        Some(h) => VaultRepo::open_with_locks_and_hook(&path, locks, h),
                        None => VaultRepo::open_with_locks(&path, locks),
                    }
                    .map_err(git_err_to_core)?;
                    run_txn(&repo, &txn, include_ignored)
                })
                .await
                .map_err(|e| Error::config_error(format!("git transaction task failed: {}", e)))?
            }
        };

        // GWS.14b: on the reconsideration-domino abort, drain the reindex
        // queue BEFORE returning the error so the agent's re-read sees a
        // coherent graph/search state. In-process bursts where the conflict
        // is against THIS process's own earlier commit benefit directly;
        // cross-process conflicts (§8.4) still need a separate listener.
        if let Err(ref e) = result
            && matches!(e, Error::ConcurrencyError { .. })
            && let Some(flush) = &self.flush_on_collision
            && let Err(flush_err) = flush().await
        {
            log::warn!(
                "GWS.14b CAS-collision flush failed (returning original error): {}",
                flush_err
            );
        }

        result
    }
}

/// Run one transaction against an already-open `repo`: the turbovault-lri
/// gitignore gate (when `include_ignored == false`), then `apply_transaction`.
/// Shared by `apply_txn`'s cached-handle and per-call-open paths so the policy
/// + commit logic stays in one place.
fn run_txn(repo: &VaultRepo, txn: &Transaction, include_ignored: bool) -> Result<()> {
    if !include_ignored {
        for changed in txn.touched_paths() {
            if repo.is_path_ignored(&changed).map_err(git_err_to_core)? {
                return Err(Error::config_error(format!(
                    "path '{}' is gitignored and include_ignored=false (turbovault-lri); enable include_ignored or add an exclusion in .gitignore",
                    changed
                )));
            }
        }
    }
    repo.apply_transaction(txn)
        .map(|_| ())
        .map_err(git_err_to_core)
}

fn build_upsert_txn(
    message: String,
    path: &str,
    content: &str,
    expected: Option<Oid>,
) -> Transaction {
    let mut txn = Transaction::new(message).upsert(path, content.as_bytes().to_vec());
    if let Some(oid) = expected {
        txn = txn.expect_blob(path, oid);
    }
    txn
}

fn parse_blob_oid(s: Option<&str>) -> Result<Option<Oid>> {
    match s {
        None => Ok(None),
        Some(hex) => Oid::from_str(hex).map(Some).map_err(|_| {
            // `ConcurrencyError` (not `ConfigError`) so callers can switch on
            // ONE error type for both backends. Cross-restart edge case
            // (server flipped `write_backend` between the client's read and
            // write) lands here for SHA-256→git, and as a hash-mismatch
            // `ConcurrencyError` for git→SHA-256 — same shape, same fix
            // (re-read + retry).
            Error::ConcurrencyError {
                reason: format!(
                    "expected_hash for git backend must be a 40-char git blob oid hex (got {:?}). Re-read the file and retry with the fresh token.",
                    hex
                ),
            }
        }),
    }
}

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
    }
}

/// Translate a substrate error into the core error space used by the tool
/// layer. Precondition failures (the OCC CAS abort) become `ConcurrencyError`
/// so callers and tests can switch on the same shape they get from the
/// legacy path.
fn git_err_to_core(e: turbovault_git::Error) -> Error {
    match e {
        turbovault_git::Error::PreconditionFailed {
            path,
            expected,
            found,
        } => Error::ConcurrencyError {
            reason: format!(
                "precondition failed for {}: expected {:?}, found {:?}",
                path, expected, found
            ),
        },
        other => Error::config_error(format!("git substrate error: {}", other)),
    }
}

// -------- frontmatter helper (mirrors file_tools, deliberately not re-exported) --------

fn find_frontmatter_end(content: &str) -> Option<usize> {
    let start = if content.starts_with("---\r\n") {
        5
    } else if content.starts_with("---\n") {
        4
    } else {
        return None;
    };
    let bytes = content.as_bytes();
    let check_closing = |pos: usize| -> Option<usize> {
        if !bytes[pos..].starts_with(b"---") {
            return None;
        }
        let after = pos + 3;
        if after >= bytes.len() {
            return Some(after);
        }
        match bytes[after] {
            b'\n' => Some(after + 1),
            b'\r' if after + 1 < bytes.len() && bytes[after + 1] == b'\n' => Some(after + 2),
            _ => None,
        }
    };
    if let Some(end) = check_closing(start) {
        return Some(end);
    }
    let mut i = start;
    while i < bytes.len() {
        let nl = bytes[i..]
            .iter()
            .position(|&b| b == b'\n' || b == b'\r')
            .map(|p| i + p)?;
        let line_start = if bytes[nl] == b'\r' && nl + 1 < bytes.len() && bytes[nl + 1] == b'\n' {
            nl + 2
        } else {
            nl + 1
        };
        if line_start >= bytes.len() {
            break;
        }
        if let Some(end) = check_closing(line_start) {
            return Some(end);
        }
        i = line_start;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path as StdPath;
    use tempfile::TempDir;
    use turbovault_core::config::{ServerConfig, VaultConfig};
    use turbovault_vault::VaultManager;

    fn init_repo(dir: &StdPath) {
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        git2::Repository::init_opts(dir, &opts).unwrap();
    }

    fn test_server_config(vault_dir: &StdPath) -> ServerConfig {
        let mut cfg = ServerConfig::new();
        cfg.vaults
            .push(VaultConfig::builder("t", vault_dir).build().unwrap());
        cfg
    }

    async fn setup() -> (TempDir, GitFileTools) {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let manager = Arc::new(VaultManager::new(test_server_config(tmp.path())).unwrap());
        let locks = Arc::new(CommitLocks::new());
        let tools = GitFileTools::new(manager, tmp.path().to_path_buf(), locks);
        (tmp, tools)
    }

    /// turbovault-a0l: like `setup`, but installs a server-style CACHED
    /// `VaultRepo` handle (the PERF-1 path) so writes reuse it instead of
    /// opening a fresh repo per call.
    async fn setup_cached() -> (TempDir, GitFileTools) {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let manager = Arc::new(VaultManager::new(test_server_config(tmp.path())).unwrap());
        let locks = Arc::new(CommitLocks::new());
        let repo = VaultRepo::open_with_locks(tmp.path(), Arc::clone(&locks)).unwrap();
        let cached: CachedRepo = Arc::new(std::sync::Mutex::new(repo));
        let tools =
            GitFileTools::new(manager, tmp.path().to_path_buf(), locks).with_cached_repo(cached);
        (tmp, tools)
    }

    fn head_oid(tools: &GitFileTools) -> Option<git2::Oid> {
        VaultRepo::open(&tools.vault_path).unwrap().head_oid()
    }

    fn head_commit_message(tools: &GitFileTools) -> String {
        let repo = git2::Repository::open(&tools.vault_path).unwrap();
        let oid = head_oid(tools).unwrap();
        repo.find_commit(oid)
            .unwrap()
            .message()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn write_file_creates_commit_and_materializes() {
        let (tmp, tools) = setup().await;
        tools.write_file("a.md", "alpha").await.unwrap();
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("a.md")).unwrap(),
            "alpha"
        );
        assert!(head_oid(&tools).is_some(), "commit landed on HEAD");
    }

    #[tokio::test]
    async fn write_file_overwrites_existing() {
        let (_tmp, tools) = setup().await;
        tools.write_file("a.md", "v1").await.unwrap();
        tools.write_file("a.md", "v2").await.unwrap();
        assert_eq!(tools.read_file("a.md").await.unwrap(), "v2");
    }

    /// turbovault-a0l (PERF-1): the cached-handle path writes, reuses the
    /// handle across calls (the cached repo sees its own prior commits, so the
    /// parent chain advances correctly), and materializes — same observable
    /// behavior as the per-call-open path.
    #[tokio::test]
    async fn cached_repo_path_writes_reuses_and_reads_back() {
        let (tmp, tools) = setup_cached().await;
        tools.write_file("a.md", "v1").await.unwrap();
        tools.write_file("a.md", "v2").await.unwrap();
        assert_eq!(tools.read_file("a.md").await.unwrap(), "v2");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("a.md")).unwrap(),
            "v2"
        );
        assert!(
            head_oid(&tools).is_some(),
            "commit landed via the cached handle"
        );
        // A second distinct file through the same handle also lands.
        tools.write_file("b.md", "B").await.unwrap();
        assert_eq!(tools.read_file("b.md").await.unwrap(), "B");
    }

    /// turbovault-a0l (PERF-1): the cached path must still enforce the blob-oid
    /// CAS precondition — caching the handle changes nothing about correctness.
    #[tokio::test]
    async fn cached_repo_path_still_enforces_cas() {
        let (_tmp, tools) = setup_cached().await;
        tools.write_file("a.md", "v1").await.unwrap();
        let bogus = VaultRepo::blob_oid_of(b"NOPE").unwrap();
        let err = tools
            .write_file_with_mode("a.md", "v2", WriteMode::Overwrite, Some(&bogus.to_string()))
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::ConcurrencyError { .. }),
            "got: {err:?}"
        );
        assert_eq!(
            tools.read_file("a.md").await.unwrap(),
            "v1",
            "stale CAS did not apply"
        );
    }

    /// turbovault-jk6 (TV-013): on an apply-phase abort (a stale CAS
    /// precondition on one op rolls the whole batch back, `executed: 0`),
    /// the per-op `records[]` must NOT report `success: true` — nothing
    /// committed. The failing op carries the error; the rest are rolled back.
    #[tokio::test]
    async fn batch_abort_marks_records_not_applied() {
        let (tmp, tools) = setup().await;
        // Seed an existing file so a stale `expected_hash` forces a CAS abort.
        tools.write_file("s1.md", "v1").await.unwrap();
        let stale = VaultRepo::blob_oid_of(b"STALE").unwrap().to_string();
        let ops = vec![
            BatchOperation::CreateNote {
                path: "ghost.md".to_string(),
                content: "x".to_string(),
                force: None,
            },
            BatchOperation::WriteNote {
                path: "s1.md".to_string(),
                content: "v2".to_string(),
                expected_hash: Some(stale),
            },
        ];
        let res = tools.batch_execute(ops).await.unwrap();

        // Top-level: aborted, nothing executed.
        assert!(!res.success, "batch must report failure");
        assert_eq!(res.executed, 0, "nothing committed");
        assert!(res.changes.is_empty());
        assert!(!res.errors.is_empty(), "top-level error populated");

        // Per-op records reflect the abort — the TV-013 bug was success:true here.
        assert_eq!(res.records.len(), 2);
        assert!(
            res.records.iter().all(|r| !r.success),
            "no op may claim success on an aborted batch: {:?}",
            res.records
        );
        let s1 = res
            .records
            .iter()
            .find(|r| r.affected_files.iter().any(|f| f == "s1.md"))
            .expect("s1 op record present");
        assert!(
            s1.error.as_deref().is_some_and(|e| !e.is_empty()),
            "failing op carries an error: {s1:?}"
        );

        // Disk: atomicity intact — ghost not created, s1 unchanged.
        assert!(!tmp.path().join("ghost.md").exists(), "ghost not created");
        assert_eq!(tools.read_file("s1.md").await.unwrap(), "v1");
    }

    /// turbovault-0g4.5: two ops in one batch writing the SAME path are
    /// rejected with a clear, op-indexed collision error (not the substrate's
    /// cryptic apply-time "duplicate change for path" abort), and NOTHING
    /// commits.
    #[tokio::test]
    async fn batch_same_path_collision_is_loud_and_atomic() {
        let (tmp, tools) = setup().await;
        let ops = vec![
            BatchOperation::WriteNote {
                path: "dup.md".to_string(),
                content: "first".to_string(),
                expected_hash: None,
            },
            BatchOperation::WriteNote {
                path: "dup.md".to_string(),
                content: "second".to_string(),
                expected_hash: None,
            },
        ];
        let res = tools.batch_execute(ops).await.unwrap();
        assert!(!res.success, "same-path collision must fail the batch");
        assert_eq!(res.failed_at, Some(1), "the second op is the collision");
        assert!(
            res.errors
                .iter()
                .any(|e| e.contains("dup.md") && e.to_lowercase().contains("collision")),
            "error names the colliding path: {:?}",
            res.errors
        );
        assert!(
            !tmp.path().join("dup.md").exists(),
            "atomic: nothing committed on collision"
        );
    }

    /// turbovault-0g4.5: a MoveNote's two endpoints colliding with a sibling
    /// write is caught (the `to` path is already written by an earlier op).
    #[tokio::test]
    async fn batch_move_dest_collision_with_prior_write_is_caught() {
        let (tmp, tools) = setup().await;
        tools.write_file("src.md", "body").await.unwrap();
        let ops = vec![
            BatchOperation::WriteNote {
                path: "dest.md".to_string(),
                content: "occupant".to_string(),
                expected_hash: None,
            },
            BatchOperation::MoveNote {
                from: "src.md".to_string(),
                to: "dest.md".to_string(),
                expected_hash: None,
            },
        ];
        let res = tools.batch_execute(ops).await.unwrap();
        assert!(!res.success);
        assert_eq!(res.failed_at, Some(1));
        assert!(res.errors.iter().any(|e| e.contains("dest.md")));
        // src untouched, dest never created.
        assert_eq!(tools.read_file("src.md").await.unwrap(), "body");
        assert!(!tmp.path().join("dest.md").exists());
    }

    /// turbovault-0g4.5: a disjoint multi-op batch still succeeds — the guard
    /// only fires on genuine same-path overlap.
    #[tokio::test]
    async fn batch_disjoint_paths_still_succeed() {
        let (tmp, tools) = setup().await;
        let ops = vec![
            BatchOperation::WriteNote {
                path: "a.md".to_string(),
                content: "A".to_string(),
                expected_hash: None,
            },
            BatchOperation::WriteNote {
                path: "b.md".to_string(),
                content: "B".to_string(),
                expected_hash: None,
            },
        ];
        let res = tools.batch_execute(ops).await.unwrap();
        assert!(res.success);
        assert_eq!(res.executed, 2);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("a.md")).unwrap(),
            "A"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("b.md")).unwrap(),
            "B"
        );
    }

    /// turbovault-0g4.1: EditNote folds SEARCH/REPLACE blocks into the batch
    /// commit; multiple blocks edit multiple locations in the one file, and a
    /// sibling op rides the same atomic commit.
    #[tokio::test]
    async fn batch_edit_note_multi_block_in_one_commit() {
        let (_tmp, tools) = setup().await;
        tools
            .write_file("doc.md", "alpha\nbeta\ngamma\n")
            .await
            .unwrap();
        let before = head_oid(&tools);
        let edits = "<<<<<<< SEARCH\nalpha\n=======\nALPHA\n>>>>>>> REPLACE\n\
                     <<<<<<< SEARCH\ngamma\n=======\nGAMMA\n>>>>>>> REPLACE";
        let ops = vec![
            BatchOperation::EditNote {
                path: "doc.md".to_string(),
                edits: edits.to_string(),
                expected_hash: None,
            },
            BatchOperation::WriteNote {
                path: "sibling.md".to_string(),
                content: "S".to_string(),
                expected_hash: None,
            },
        ];
        let res = tools.batch_execute(ops).await.unwrap();
        assert!(res.success, "edit batch failed: {:?}", res.errors);
        assert_eq!(res.executed, 2);
        assert_eq!(
            tools.read_file("doc.md").await.unwrap(),
            "ALPHA\nbeta\nGAMMA\n"
        );
        assert_eq!(tools.read_file("sibling.md").await.unwrap(), "S");
        assert_ne!(head_oid(&tools), before, "one new commit for the batch");
    }

    /// turbovault-0g4.1: a stale `expected_hash` on an EditNote aborts the
    /// whole batch atomically — the file is untouched.
    #[tokio::test]
    async fn batch_edit_note_stale_hash_aborts() {
        let (_tmp, tools) = setup().await;
        tools.write_file("doc.md", "x\n").await.unwrap();
        let stale = VaultRepo::blob_oid_of(b"STALE").unwrap().to_string();
        let ops = vec![BatchOperation::EditNote {
            path: "doc.md".to_string(),
            edits: "<<<<<<< SEARCH\nx\n=======\ny\n>>>>>>> REPLACE".to_string(),
            expected_hash: Some(stale),
        }];
        let res = tools.batch_execute(ops).await.unwrap();
        assert!(!res.success);
        assert_eq!(tools.read_file("doc.md").await.unwrap(), "x\n", "unchanged");
    }

    /// turbovault-0g4.2: UpdateFrontmatter merges keys into an existing note's
    /// frontmatter as part of the batch commit (existing keys + body preserved).
    #[tokio::test]
    async fn batch_update_frontmatter_merges_in_one_commit() {
        let (_tmp, tools) = setup().await;
        tools
            .write_file("n.md", "---\ntitle: T\n---\nbody\n")
            .await
            .unwrap();
        let mut fm = std::collections::HashMap::new();
        fm.insert("status".to_string(), serde_json::json!("active"));
        let ops = vec![BatchOperation::UpdateFrontmatter {
            path: "n.md".to_string(),
            frontmatter: fm,
            merge: Some(true),
            expected_hash: None,
        }];
        let res = tools.batch_execute(ops).await.unwrap();
        assert!(res.success, "frontmatter batch failed: {:?}", res.errors);
        let content = tools.read_file("n.md").await.unwrap();
        assert!(
            content.contains("title: T"),
            "existing key preserved: {content}"
        );
        assert!(
            content.contains("status: active"),
            "new key merged: {content}"
        );
        assert!(content.contains("body"), "body preserved: {content}");
    }

    #[tokio::test]
    async fn write_file_with_stale_blob_oid_aborts_concurrency_error() {
        let (_tmp, tools) = setup().await;
        tools.write_file("a.md", "v1").await.unwrap();
        // Use a deliberately wrong blob oid.
        let bogus = VaultRepo::blob_oid_of(b"NOPE").unwrap();
        let err = tools
            .write_file_with_mode("a.md", "v2", WriteMode::Overwrite, Some(&bogus.to_string()))
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::ConcurrencyError { .. }),
            "got: {err:?}"
        );
        assert_eq!(tools.read_file("a.md").await.unwrap(), "v1");
    }

    #[tokio::test]
    async fn write_file_with_garbage_hash_is_loud_concurrency_error() {
        // A malformed hash from the caller (e.g. cross-restart edge case
        // where the legacy SHA-256 hex still lives in the client) lands as
        // ConcurrencyError, NOT ConfigError — same shape callers handle for
        // any other stale-token failure, single switch arm fixes both
        // backends.
        let (_tmp, tools) = setup().await;
        let err = tools
            .write_file_with_mode("a.md", "v1", WriteMode::Overwrite, Some("not-a-hash"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::ConcurrencyError { .. }),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn delete_file_removes_and_commits() {
        let (tmp, tools) = setup().await;
        tools.write_file("a.md", "x").await.unwrap();
        tools.delete_file("a.md").await.unwrap();
        assert!(!tmp.path().join("a.md").exists());
    }

    #[tokio::test]
    async fn move_file_atomic_remove_plus_add_one_commit() {
        let (tmp, tools) = setup().await;
        tools.write_file("old.md", "body").await.unwrap();
        let before = head_oid(&tools);
        tools.move_file("old.md", "new.md").await.unwrap();
        assert!(!tmp.path().join("old.md").exists());
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("new.md")).unwrap(),
            "body"
        );
        assert_ne!(head_oid(&tools), before, "new commit");
    }

    #[tokio::test]
    async fn move_file_refuses_to_clobber_existing_destination() {
        let (_tmp, tools) = setup().await;
        tools.write_file("a.md", "A").await.unwrap();
        tools.write_file("b.md", "B").await.unwrap();
        let err = tools.move_file("a.md", "b.md").await.unwrap_err();
        assert!(
            matches!(err, Error::ConcurrencyError { .. }),
            "got: {err:?}"
        );
        // Both files still present, untouched.
        assert_eq!(tools.read_file("a.md").await.unwrap(), "A");
        assert_eq!(tools.read_file("b.md").await.unwrap(), "B");
    }

    #[tokio::test]
    async fn copy_file_writes_destination_only() {
        let (_tmp, tools) = setup().await;
        tools.write_file("a.md", "alpha").await.unwrap();
        tools.copy_file("a.md", "b.md").await.unwrap();
        assert_eq!(tools.read_file("a.md").await.unwrap(), "alpha");
        assert_eq!(tools.read_file("b.md").await.unwrap(), "alpha");
    }

    #[tokio::test]
    async fn edit_file_search_replace_commits() {
        let (_tmp, tools) = setup().await;
        tools.write_file("a.md", "hello world\n").await.unwrap();
        let edits = "<<<<<<< SEARCH\nhello world\n=======\nhi world\n>>>>>>> REPLACE\n";
        tools.edit_file("a.md", edits, None, false).await.unwrap();
        assert_eq!(tools.read_file("a.md").await.unwrap(), "hi world\n");
    }

    #[tokio::test]
    async fn edit_file_dry_run_does_not_commit() {
        let (_tmp, tools) = setup().await;
        tools.write_file("a.md", "hello\n").await.unwrap();
        let head_before = head_oid(&tools);
        let edits = "<<<<<<< SEARCH\nhello\n=======\nbye\n>>>>>>> REPLACE\n";
        let _ = tools.edit_file("a.md", edits, None, true).await.unwrap();
        assert_eq!(head_oid(&tools), head_before, "no commit on dry_run");
        assert_eq!(tools.read_file("a.md").await.unwrap(), "hello\n");
    }

    /// turbovault-6sj / TV-011: edit_file's returned `old_hash`/`new_hash`
    /// must be 40-char git blob OIDs on the git backend (NOT 64-char SHA-256).
    /// Without this, callers cannot use them as `expected_hash` on a follow-up
    /// call — the CAS round-trip breaks.
    #[tokio::test]
    async fn edit_file_returns_blob_oid_hashes_not_sha256() {
        let (_tmp, tools) = setup().await;
        tools.write_file("a.md", "hello\n").await.unwrap();
        let edits = "<<<<<<< SEARCH\nhello\n=======\nbye\n>>>>>>> REPLACE\n";
        let result = tools.edit_file("a.md", edits, None, false).await.unwrap();
        assert_eq!(
            result.old_hash.len(),
            40,
            "old_hash must be 40-char blob OID hex, got {:?}",
            result.old_hash
        );
        assert_eq!(
            result.new_hash.len(),
            40,
            "new_hash must be 40-char blob OID hex, got {:?}",
            result.new_hash
        );
        // Sanity: each hash equals the blob OID of the actual content.
        let expected_old = VaultRepo::blob_oid_of(b"hello\n").unwrap().to_string();
        let expected_new = VaultRepo::blob_oid_of(b"bye\n").unwrap().to_string();
        assert_eq!(result.old_hash, expected_old);
        assert_eq!(result.new_hash, expected_new);
    }

    /// turbovault-6sj: the `new_hash` an edit returns must round-trip as
    /// `expected_hash` on the next call. This is the CAS contract the legacy
    /// path delivered; the git backend must do the same.
    #[tokio::test]
    async fn edit_file_new_hash_round_trips_as_expected_hash() {
        let (_tmp, tools) = setup().await;
        tools.write_file("a.md", "v1\n").await.unwrap();
        let edits1 = "<<<<<<< SEARCH\nv1\n=======\nv2\n>>>>>>> REPLACE\n";
        let r1 = tools.edit_file("a.md", edits1, None, false).await.unwrap();
        // Use r1.new_hash as the next expected_hash — must succeed because
        // no concurrent change has touched the file.
        let edits2 = "<<<<<<< SEARCH\nv2\n=======\nv3\n>>>>>>> REPLACE\n";
        let r2 = tools
            .edit_file("a.md", edits2, Some(&r1.new_hash), false)
            .await
            .unwrap();
        assert_eq!(
            r2.old_hash, r1.new_hash,
            "old_hash chains to prior new_hash"
        );
        assert_eq!(tools.read_file("a.md").await.unwrap(), "v3\n");
    }

    /// turbovault-6sj: dry-run hashes must match what an actual apply would
    /// produce, so callers can use a preview's `new_hash` to plan the next
    /// `expected_hash`.
    #[tokio::test]
    async fn edit_file_dry_run_hashes_match_real_apply() {
        let (_tmp, tools) = setup().await;
        tools.write_file("a.md", "hello\n").await.unwrap();
        let edits = "<<<<<<< SEARCH\nhello\n=======\nbye\n>>>>>>> REPLACE\n";
        let dry = tools.edit_file("a.md", edits, None, true).await.unwrap();
        let live = tools.edit_file("a.md", edits, None, false).await.unwrap();
        assert_eq!(dry.old_hash, live.old_hash);
        assert_eq!(dry.new_hash, live.new_hash);
    }

    /// turbovault-947: create_file on an absent path lands one commit.
    #[tokio::test]
    async fn create_file_writes_absent_path() {
        let (tmp, tools) = setup().await;
        let head_before = head_oid(&tools);
        tools.create_file("new.md", "fresh\n").await.unwrap();
        assert_eq!(tools.read_file("new.md").await.unwrap(), "fresh\n");
        let head_after = head_oid(&tools).unwrap();
        assert_ne!(Some(head_after), head_before, "create advanced HEAD");
        // Existence side-effect lands in the working tree.
        assert!(tmp.path().join("new.md").exists());
    }

    /// turbovault-c0e: `WriteNote.expected_hash` carries an `expect_blob`
    /// precondition. A stale hash aborts the WHOLE batch (atomicity §6.3),
    /// not just that op — zero files land, HEAD unchanged. The substrate
    /// folds the apply-time ConcurrencyError into the returned `BatchResult`
    /// (matching the existing batch-failure shape) rather than surfacing
    /// it as `Err`.
    #[tokio::test]
    async fn batch_write_note_with_stale_expected_hash_aborts_atomically() {
        let (tmp, tools) = setup().await;
        tools.write_file("a.md", "v1\n").await.unwrap();
        let bogus = VaultRepo::blob_oid_of(b"NEVER_HERE").unwrap().to_string();
        let head_before = head_oid(&tools).unwrap();

        let ops = vec![
            BatchOperation::CreateNote {
                path: "fresh.md".into(),
                content: "ok".into(),
                force: None,
            },
            BatchOperation::WriteNote {
                path: "a.md".into(),
                content: "v2\n".into(),
                expected_hash: Some(bogus),
            },
        ];
        let res = tools.batch_execute(ops).await.unwrap();
        assert!(!res.success, "batch reports failure");
        assert_eq!(res.executed, 0, "no op committed on abort");
        let any_concurrency = res.errors.iter().any(|e| e.contains("precondition failed"));
        assert!(
            any_concurrency,
            "expected precondition-failed error in result: {:?}",
            res.errors
        );
        // Atomic abort: fresh.md was NOT created; a.md is unchanged.
        assert!(!tmp.path().join("fresh.md").exists());
        assert_eq!(tools.read_file("a.md").await.unwrap(), "v1\n");
        assert_eq!(head_oid(&tools), Some(head_before), "no commit on abort");
    }

    /// turbovault-c0e: matching `expected_hash` succeeds — the whole batch
    /// lands as one commit.
    #[tokio::test]
    async fn batch_write_note_with_matching_expected_hash_lands() {
        let (_tmp, tools) = setup().await;
        tools.write_file("a.md", "v1\n").await.unwrap();
        let current = VaultRepo::blob_oid_of(b"v1\n").unwrap().to_string();
        let head_before = head_oid(&tools);

        let ops = vec![BatchOperation::WriteNote {
            path: "a.md".into(),
            content: "v2\n".into(),
            expected_hash: Some(current),
        }];
        let res = tools.batch_execute(ops).await.unwrap();
        assert!(res.success);
        assert_eq!(tools.read_file("a.md").await.unwrap(), "v2\n");
        assert_ne!(head_oid(&tools), head_before, "commit advanced HEAD");
    }

    /// turbovault-c0e: `CreateNote { force: true }` drops `expect_absent`,
    /// behaving as a blind upsert. Existing content is replaced; the batch
    /// lands.
    #[tokio::test]
    async fn batch_create_note_force_true_is_blind_upsert() {
        let (_tmp, tools) = setup().await;
        tools.write_file("dup.md", "v1\n").await.unwrap();
        let ops = vec![BatchOperation::CreateNote {
            path: "dup.md".into(),
            content: "v2\n".into(),
            force: Some(true),
        }];
        let res = tools.batch_execute(ops).await.unwrap();
        assert!(res.success);
        assert_eq!(tools.read_file("dup.md").await.unwrap(), "v2\n");
    }

    /// turbovault-947: create_file on an existing path fails its `expect_absent`
    /// precondition. ZERO commits land; the working tree is unchanged. This is
    /// the substrate guarantee for the concurrent-create race the MCP layer
    /// pre-check cannot close on its own.
    #[tokio::test]
    async fn create_file_aborts_on_existing_path() {
        let (tmp, tools) = setup().await;
        tools.write_file("dup.md", "v1\n").await.unwrap();
        let head_before = head_oid(&tools).unwrap();

        let err = tools.create_file("dup.md", "v2\n").await.unwrap_err();
        assert!(
            matches!(err, Error::ConcurrencyError { .. }),
            "expected ConcurrencyError, got: {err:?}"
        );
        assert_eq!(
            tools.read_file("dup.md").await.unwrap(),
            "v1\n",
            "original content untouched on aborted create"
        );
        assert_eq!(head_oid(&tools), Some(head_before), "no commit on abort");
        // Working tree file count unchanged: only `dup.md` exists.
        assert!(tmp.path().join("dup.md").exists());
    }

    #[tokio::test]
    async fn batch_execute_one_atomic_commit_all_op_types() {
        let (tmp, tools) = setup().await;
        // Seed for delete + move + update-links.
        tools.write_file("seed_del.md", "gone").await.unwrap();
        tools.write_file("seed_mv.md", "moveme").await.unwrap();
        tools
            .write_file("links.md", "see [[old-target]]")
            .await
            .unwrap();
        let head_before = head_oid(&tools);

        let ops = vec![
            BatchOperation::CreateNote {
                path: "new1.md".into(),
                content: "C1".into(),
                force: None,
            },
            BatchOperation::WriteNote {
                path: "new2.md".into(),
                content: "W2".into(),
                expected_hash: None,
            },
            BatchOperation::DeleteNote {
                path: "seed_del.md".into(),
                expected_hash: None,
            },
            BatchOperation::MoveNote {
                from: "seed_mv.md".into(),
                to: "moved.md".into(),
                expected_hash: None,
            },
            BatchOperation::UpdateLinks {
                file: "links.md".into(),
                old_target: "old-target".into(),
                new_target: "new-target".into(),
                expected_hash: None,
            },
        ];

        let res = tools.batch_execute(ops).await.unwrap();
        assert!(res.success, "batch should succeed: {:?}", res.errors);
        assert_eq!(res.executed, 5);

        // HEAD advanced by exactly one commit (the batch landed as one txn).
        let head_after = head_oid(&tools).unwrap();
        assert_ne!(Some(head_after), head_before);
        let repo = git2::Repository::open(tmp.path()).unwrap();
        let commit = repo.find_commit(head_after).unwrap();
        assert_eq!(commit.parent_count(), 1, "exactly one new commit");

        // Filesystem state matches.
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("new1.md")).unwrap(),
            "C1"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("new2.md")).unwrap(),
            "W2"
        );
        assert!(!tmp.path().join("seed_del.md").exists());
        assert!(!tmp.path().join("seed_mv.md").exists());
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("moved.md")).unwrap(),
            "moveme"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("links.md")).unwrap(),
            "see [[new-target]]"
        );
    }

    #[tokio::test]
    async fn batch_execute_failure_leaves_no_partial_state() {
        // CreateNote on a path that already exists -> precondition abort.
        // Atomicity contract: zero files from the batch should land.
        let (tmp, tools) = setup().await;
        tools.write_file("exists.md", "already").await.unwrap();
        let head_before = head_oid(&tools);

        let ops = vec![
            BatchOperation::WriteNote {
                path: "untouched1.md".into(),
                content: "X".into(),
                expected_hash: None,
            },
            BatchOperation::CreateNote {
                path: "exists.md".into(),
                content: "boom".into(),
                force: None,
            },
            BatchOperation::WriteNote {
                path: "untouched2.md".into(),
                content: "Y".into(),
                expected_hash: None,
            },
        ];

        let res = tools.batch_execute(ops).await.unwrap();
        assert!(!res.success);
        // Neither untouched file was written; the existing file is unchanged.
        assert!(!tmp.path().join("untouched1.md").exists());
        assert!(!tmp.path().join("untouched2.md").exists());
        assert_eq!(tools.read_file("exists.md").await.unwrap(), "already");
        assert_eq!(head_oid(&tools), head_before, "no commit on abort");
    }

    #[tokio::test]
    async fn batch_execute_empty_is_a_loud_failure() {
        let (_tmp, tools) = setup().await;
        let res = tools.batch_execute(vec![]).await.unwrap();
        assert!(!res.success);
        assert_eq!(res.total, 0);
    }

    // -------- GWS.14b: CAS-collision flush --------

    #[tokio::test]
    async fn cas_collision_flush_fires_before_concurrency_error_returns() {
        // Wire a sentinel flush callback that flips an Arc<AtomicBool> so we
        // can prove flush ran BEFORE the error reached the caller.
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let manager = Arc::new(VaultManager::new(test_server_config(tmp.path())).unwrap());
        let locks = Arc::new(CommitLocks::new());
        let commit_hook: CommitHook = Arc::new(|_p, _c| {});

        let flushed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flushed_clone = Arc::clone(&flushed);
        let flush: CasCollisionFlush = Arc::new(move || {
            let f = Arc::clone(&flushed_clone);
            Box::pin(async move {
                f.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
        });

        let tools = GitFileTools::new_with_hook_and_flush(
            manager,
            tmp.path().to_path_buf(),
            locks,
            commit_hook,
            flush,
        );

        // Trigger a guaranteed precondition failure: write v1, then update
        // with a stale expected blob.
        tools.write_file("a.md", "v1").await.unwrap();
        let stale_oid = VaultRepo::blob_oid_of(b"WAS_NEVER_HERE").unwrap();
        let err = tools
            .write_file_with_mode(
                "a.md",
                "v2",
                WriteMode::Overwrite,
                Some(&stale_oid.to_string()),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::ConcurrencyError { .. }),
            "got: {err:?}"
        );
        assert!(
            flushed.load(std::sync::atomic::Ordering::SeqCst),
            "flush callback must fire before the ConcurrencyError surfaces to the caller"
        );
    }

    /// turbovault-9zr: the full batch path. `batch_execute` must fire the
    /// commit hook (enqueue exactly one commit), and draining that commit must
    /// produce the one -> two link edge in the graph.
    #[tokio::test]
    async fn batch_execute_enqueues_and_reindexes_intra_commit_edge() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let manager = Arc::new(VaultManager::new(test_server_config(tmp.path())).unwrap());
        let locks = Arc::new(CommitLocks::new());

        let queue = Arc::new(crate::ReindexQueue::new());
        let q = Arc::clone(&queue);
        let commit_hook: CommitHook = Arc::new(move |_p, c| q.push(c));
        let flush: CasCollisionFlush = Arc::new(|| Box::pin(async { Ok(()) }));
        let tools = GitFileTools::new_with_hook_and_flush(
            Arc::clone(&manager),
            tmp.path().to_path_buf(),
            locks,
            commit_hook,
            flush,
        );

        tools
            .batch_execute(vec![
                BatchOperation::CreateNote {
                    path: "one.md".to_string(),
                    content: "# One\n\nlinks [[two]]\n".to_string(),
                    force: None,
                },
                BatchOperation::CreateNote {
                    path: "two.md".to_string(),
                    content: "# Two\n".to_string(),
                    force: None,
                },
            ])
            .await
            .unwrap();

        assert_eq!(
            queue.pending_count(),
            1,
            "batch_execute should enqueue exactly one commit"
        );

        let repo = VaultRepo::open(tmp.path()).unwrap();
        queue.drain_through(&repo, &manager).await.unwrap();

        assert_eq!(
            manager.link_graph().read().await.edge_count(),
            1,
            "drained batch commit should produce the one -> two edge"
        );
    }

    #[tokio::test]
    async fn cas_collision_flush_skipped_on_successful_write() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let manager = Arc::new(VaultManager::new(test_server_config(tmp.path())).unwrap());
        let locks = Arc::new(CommitLocks::new());
        let commit_hook: CommitHook = Arc::new(|_p, _c| {});

        let flush_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let flush_calls_clone = Arc::clone(&flush_calls);
        let flush: CasCollisionFlush = Arc::new(move || {
            let c = Arc::clone(&flush_calls_clone);
            Box::pin(async move {
                c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
        });

        let tools = GitFileTools::new_with_hook_and_flush(
            manager,
            tmp.path().to_path_buf(),
            locks,
            commit_hook,
            flush,
        );

        tools.write_file("a.md", "alpha").await.unwrap();
        tools.write_file("b.md", "beta").await.unwrap();
        assert_eq!(
            flush_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "flush only fires on ConcurrencyError, never on successful writes"
        );
    }

    #[tokio::test]
    async fn cas_collision_flush_error_does_not_mask_original_concurrency_error() {
        // Even when the flush callback itself errors, the caller still sees
        // the original ConcurrencyError — flush failures are logged + dropped
        // (correctness contract).
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let manager = Arc::new(VaultManager::new(test_server_config(tmp.path())).unwrap());
        let locks = Arc::new(CommitLocks::new());
        let commit_hook: CommitHook = Arc::new(|_p, _c| {});

        let flush: CasCollisionFlush =
            Arc::new(|| Box::pin(async { Err(Error::config_error("simulated flush failure")) }));

        let tools = GitFileTools::new_with_hook_and_flush(
            manager,
            tmp.path().to_path_buf(),
            locks,
            commit_hook,
            flush,
        );

        tools.write_file("a.md", "v1").await.unwrap();
        let stale_oid = VaultRepo::blob_oid_of(b"WAS_NEVER_HERE").unwrap();
        let err = tools
            .write_file_with_mode(
                "a.md",
                "v2",
                WriteMode::Overwrite,
                Some(&stale_oid.to_string()),
            )
            .await
            .unwrap_err();
        // Caller sees the original ConcurrencyError, NOT the flush's
        // ConfigError. Flush failures are best-effort.
        assert!(
            matches!(err, Error::ConcurrencyError { .. }),
            "got: {err:?}"
        );
    }

    // -------- turbovault-0bh: caller-supplied commit messages --------

    #[tokio::test]
    async fn write_file_with_mode_and_message_uses_caller_subject() {
        let (_tmp, tools) = setup().await;
        tools
            .write_file_with_mode_and_message(
                "a.md",
                "alpha",
                WriteMode::Overwrite,
                None,
                "add concept page for Alpha",
            )
            .await
            .unwrap();
        let msg = head_commit_message(&tools);
        assert!(msg.contains("add concept page for Alpha"), "got: {msg:?}");
    }

    #[tokio::test]
    async fn create_file_with_message_uses_caller_subject() {
        let (_tmp, tools) = setup().await;
        tools
            .create_file_with_message("new.md", "fresh", "create stub page")
            .await
            .unwrap();
        let msg = head_commit_message(&tools);
        assert!(msg.contains("create stub page"), "got: {msg:?}");
    }

    #[tokio::test]
    async fn edit_file_with_message_uses_caller_subject() {
        let (_tmp, tools) = setup().await;
        tools.write_file("a.md", "hello\n").await.unwrap();
        let edits = "<<<<<<< SEARCH\nhello\n=======\nbye\n>>>>>>> REPLACE\n";
        let _ = tools
            .edit_file_with_message("a.md", edits, None, false, "fix greeting")
            .await
            .unwrap();
        let msg = head_commit_message(&tools);
        assert!(msg.contains("fix greeting"), "got: {msg:?}");
    }

    #[tokio::test]
    async fn delete_file_with_hash_and_message_uses_caller_subject() {
        let (_tmp, tools) = setup().await;
        tools.write_file("a.md", "v").await.unwrap();
        tools
            .delete_file_with_hash_and_message("a.md", None, "remove superseded page")
            .await
            .unwrap();
        let msg = head_commit_message(&tools);
        assert!(msg.contains("remove superseded page"), "got: {msg:?}");
    }

    #[tokio::test]
    async fn move_file_with_hash_and_message_uses_caller_subject() {
        let (_tmp, tools) = setup().await;
        tools.write_file("a.md", "v").await.unwrap();
        tools
            .move_file_with_hash_and_message("a.md", "b.md", None, "rename to canonical slug")
            .await
            .unwrap();
        let msg = head_commit_message(&tools);
        assert!(msg.contains("rename to canonical slug"), "got: {msg:?}");
    }

    #[tokio::test]
    async fn batch_execute_with_message_uses_caller_subject() {
        let (_tmp, tools) = setup().await;
        let ops = vec![
            BatchOperation::CreateNote {
                path: "x.md".into(),
                content: "x".into(),
                force: None,
            },
            BatchOperation::CreateNote {
                path: "y.md".into(),
                content: "y".into(),
                force: None,
            },
        ];
        tools
            .batch_execute_with_message(ops, "ingest source S: 2 concept pages")
            .await
            .unwrap();
        let msg = head_commit_message(&tools);
        assert!(
            msg.contains("ingest source S: 2 concept pages"),
            "got: {msg:?}"
        );
    }

    /// Auto-derived fallback still says `batch_execute (N ops)` when no
    /// message is supplied (legacy behavior preserved).
    #[tokio::test]
    async fn batch_execute_auto_derive_unchanged() {
        let (_tmp, tools) = setup().await;
        let ops = vec![BatchOperation::CreateNote {
            path: "x.md".into(),
            content: "x".into(),
            force: None,
        }];
        tools.batch_execute(ops).await.unwrap();
        let msg = head_commit_message(&tools);
        assert!(msg.contains("batch_execute (1 ops)"), "got: {msg:?}");
    }

    // -------- turbovault-lqr: atomic move + wikilink rewrite --------

    /// turbovault-lqr: move with one backlinking source. The rename AND
    /// the link rewrite land as ONE commit. HEAD advances by exactly one
    /// commit touching both paths.
    #[tokio::test]
    async fn move_with_link_updates_atomic_one_commit() {
        let (tmp, tools) = setup().await;
        tools.write_file("old.md", "# Old\n").await.unwrap();
        tools
            .write_file("linker.md", "I link to [[old]] here.\n")
            .await
            .unwrap();
        // Initialize link graph from the seeded files so backlinks resolve.
        tools.manager.initialize().await.unwrap();
        let head_before = head_oid(&tools).unwrap();

        let result = tools
            .move_file_with_link_updates("old.md", "new.md", None, "rename old -> new")
            .await
            .unwrap();
        assert_eq!(result.link_sources_updated, vec!["linker.md".to_string()]);
        // Working tree state.
        assert!(!tmp.path().join("old.md").exists());
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("new.md")).unwrap(),
            "# Old\n"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("linker.md")).unwrap(),
            "I link to [[new]] here.\n"
        );
        // Exactly one new commit touched both files.
        let head_after = head_oid(&tools).unwrap();
        assert_ne!(head_after, head_before);
        let repo = git2::Repository::open(&tools.vault_path).unwrap();
        let commit = repo.find_commit(head_after).unwrap();
        assert_eq!(commit.parent_count(), 1, "single parent");
    }

    /// turbovault-lqr: move with multiple backlinking sources. All link
    /// rewrites + the rename land in one commit.
    #[tokio::test]
    async fn move_with_link_updates_handles_multiple_sources() {
        let (tmp, tools) = setup().await;
        tools.write_file("old.md", "# Old\n").await.unwrap();
        tools
            .write_file("a.md", "see [[old|the page]]\n")
            .await
            .unwrap();
        tools
            .write_file("b.md", "embed: ![[old]]\nsection: [[old#Header]]\n")
            .await
            .unwrap();
        // turbovault-34p: a source where "old" is a SUBSTRING of unrelated words
        // (golden, oldie) AND that also links to a page we must NOT touch
        // ([[keeper]]). A substring/too-greedy rewrite would corrupt these; only
        // the [[old]] wikilink may change.
        tools
            .write_file("c.md", "golden oldie [[old]] keep [[keeper]]\n")
            .await
            .unwrap();
        tools.manager.initialize().await.unwrap();

        let result = tools
            .move_file_with_link_updates("old.md", "new.md", None, "rename")
            .await
            .unwrap();
        let mut updated = result.link_sources_updated.clone();
        updated.sort();
        assert_eq!(
            updated,
            vec!["a.md".to_string(), "b.md".to_string(), "c.md".to_string()]
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("a.md")).unwrap(),
            "see [[new|the page]]\n"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("b.md")).unwrap(),
            "embed: ![[new]]\nsection: [[new#Header]]\n"
        );
        // Only the [[old]] wikilink changed: "golden"/"oldie" + [[keeper]] intact.
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("c.md")).unwrap(),
            "golden oldie [[new]] keep [[keeper]]\n"
        );
    }

    /// turbovault-uag: git-backend Prepend into a note WITH frontmatter inserts
    /// AFTER the `---` block (never above it); Append goes to the end. The legacy
    /// `test_file_tools` suite covered only the legacy path; the git-backend
    /// resolve_write_content + find_frontmatter_end path was uncovered.
    #[tokio::test]
    async fn git_prepend_after_frontmatter_and_append_at_end() {
        let (tmp, tools) = setup().await;
        tools
            .write_file("n.md", "---\ntitle: T\n---\n\nbody line\n")
            .await
            .unwrap();

        // Prepend lands below the closing `---`, above the body.
        tools
            .write_file_with_mode("n.md", "PRE", WriteMode::Prepend, None)
            .await
            .unwrap();
        assert_eq!(
            tools.read_file("n.md").await.unwrap(),
            "---\ntitle: T\n---\nPRE\nbody line\n",
            "prepend must not push above the frontmatter"
        );

        // Append lands at the very end.
        tools
            .write_file_with_mode("n.md", "POST", WriteMode::Append, None)
            .await
            .unwrap();
        let after = tools.read_file("n.md").await.unwrap();
        assert_eq!(after, "---\ntitle: T\n---\nPRE\nbody line\n\nPOST");
        // Working tree == HEAD.
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("n.md")).unwrap(),
            after
        );
    }

    /// Advance the branch ref + change `file`'s blob via a bare git2 commit,
    /// WITHOUT touching the working tree — simulates another process committing.
    fn external_commit_change(repo_path: &StdPath, file: &str, content: &str) {
        let repo = git2::Repository::open(repo_path).unwrap();
        let head = repo.head().unwrap();
        let branch = head.shorthand().unwrap().to_string();
        let parent = head.peel_to_commit().unwrap();
        let mut tb = repo.treebuilder(Some(&parent.tree().unwrap())).unwrap();
        let blob = repo.blob(content.as_bytes()).unwrap();
        tb.insert(file, blob, 0o100644).unwrap();
        let tree = repo.find_tree(tb.write().unwrap()).unwrap();
        let sig = git2::Signature::now("Ext", "ext@x").unwrap();
        repo.commit(
            Some(&format!("refs/heads/{branch}")),
            &sig,
            &sig,
            "external",
            &tree,
            &[&parent],
        )
        .unwrap();
    }

    /// turbovault-xw4: the CACHED `VaultRepo` handle (PERF-1) must still detect
    /// a SEPARATE-PROCESS ref advance and reject a stale-precondition write — no
    /// lost update. Prior coverage proved this for a raw handle (cas.rs) but not
    /// at the cached GitFileTools seam, which is the exact safety question PERF-1
    /// raised.
    #[tokio::test]
    async fn cached_handle_detects_external_ref_advance() {
        let (tmp, tools) = setup_cached().await;
        tools.write_file("a.md", "v1").await.unwrap();
        let v1 = VaultRepo::blob_oid_of(b"v1").unwrap().to_string();

        // Another process advances the ref + rewrites a.md's blob.
        external_commit_change(tmp.path(), "a.md", "EXTERNAL");

        // The cached handle must re-read the ref under lock and REJECT the write
        // carrying the now-stale precondition.
        let err = tools
            .write_file_with_mode("a.md", "v2", WriteMode::Overwrite, Some(&v1))
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::ConcurrencyError { .. }),
            "stale precondition must surface ConcurrencyError, got: {err:?}"
        );
        // The external commit is still HEAD — our stale write did not clobber it.
        let repo = git2::Repository::open(tmp.path()).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(
            head.message().unwrap(),
            "external",
            "external commit survived; no lost update"
        );
    }

    /// turbovault-oz6: atomic delete + wrap-as-stale across multiple
    /// linkers. One commit; target gone; sources strikethrough-wrapped.
    #[tokio::test]
    async fn delete_with_link_rewrite_to_stale_wraps_all_linkers() {
        let (tmp, tools) = setup().await;
        tools.write_file("doomed.md", "# Doomed").await.unwrap();
        tools
            .write_file("a.md", "see [[doomed]] for details\n")
            .await
            .unwrap();
        tools
            .write_file("b.md", "another ref ![[doomed#Sec]]\n")
            .await
            .unwrap();
        tools.manager.initialize().await.unwrap();
        let head_before = head_oid(&tools).unwrap();

        let result = tools
            .delete_file_with_link_rewrite_to_stale("doomed.md", None, "kill doomed")
            .await
            .unwrap();
        let mut updated = result.link_sources_updated.clone();
        updated.sort();
        assert_eq!(updated, vec!["a.md".to_string(), "b.md".to_string()]);
        // Target gone.
        assert!(!tmp.path().join("doomed.md").exists());
        // Sources wrapped.
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("a.md")).unwrap(),
            "see ~~[[doomed]]~~ for details\n"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("b.md")).unwrap(),
            "another ref ~~![[doomed#Sec]]~~\n"
        );
        // Single commit.
        let head_after = head_oid(&tools).unwrap();
        assert_ne!(head_after, head_before);
        let repo = git2::Repository::open(&tools.vault_path).unwrap();
        let commit = repo.find_commit(head_after).unwrap();
        assert_eq!(commit.parent_count(), 1);
    }

    /// turbovault-oz6: list_inbound_backlinks returns the linkers the
    /// MCP layer uses to decide whether to refuse, rewrite-stale, or
    /// force-delete.
    #[tokio::test]
    async fn list_inbound_backlinks_returns_linkers() {
        let (_tmp, tools) = setup().await;
        tools.write_file("doomed.md", "# Doomed").await.unwrap();
        tools
            .write_file("linker.md", "see [[doomed]]")
            .await
            .unwrap();
        tools
            .write_file("unrelated.md", "no links here")
            .await
            .unwrap();
        tools.manager.initialize().await.unwrap();

        let mut bls = tools.list_inbound_backlinks("doomed.md").await.unwrap();
        bls.sort();
        assert_eq!(bls, vec!["linker.md".to_string()]);
    }

    /// turbovault-lqr: a source modified between the read and the apply
    /// fails its expect_blob precondition, aborting the entire move —
    /// zero files change.
    #[tokio::test]
    async fn move_with_link_updates_aborts_on_stale_source() {
        let (tmp, tools) = setup().await;
        tools.write_file("old.md", "# Old\n").await.unwrap();
        tools
            .write_file("linker.md", "see [[old]]\n")
            .await
            .unwrap();
        tools.manager.initialize().await.unwrap();

        // Simulate stale read: an external commit mutates linker.md
        // between the link-graph lookup and the substrate apply. We
        // approximate that by using a stale expected_hash on the
        // source — substrate aborts identically.
        // Compute a bogus oid to feed as expected_hash.
        let bogus_oid = VaultRepo::blob_oid_of(b"NEVER_HERE_LQR")
            .unwrap()
            .to_string();
        let head_before = head_oid(&tools).unwrap();
        let res = tools
            .move_file_with_link_updates("old.md", "new.md", Some(&bogus_oid), "should abort")
            .await;
        let err = res.unwrap_err();
        assert!(
            matches!(err, Error::ConcurrencyError { .. }),
            "expected ConcurrencyError, got: {err:?}"
        );
        // Working tree unchanged.
        assert!(tmp.path().join("old.md").exists());
        assert!(!tmp.path().join("new.md").exists());
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("linker.md")).unwrap(),
            "see [[old]]\n"
        );
        assert_eq!(head_oid(&tools), Some(head_before), "no commit on abort");
    }
}
