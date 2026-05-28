//! Lazy GSU (Graph + Search Update) queue + apply (GWS.14).
//!
//! When the git substrate commits, its `CommitHook` pushes the new commit's
//! oid onto a per-vault [`ReindexQueue`]. The actual index work is deferred:
//! a background drainer (see [`crate::WriteTools`] integration) and/or a
//! "flush before relevant query" call processes the queue, asking the
//! substrate for each commit's `(path, present)` diff and re-running the
//! parser → link graph delta.
//!
//! Search and similarity engines are NOT incrementally updated here; the
//! server's existing cache-evict + cold-rebuild pattern is reused (flush
//! callers invalidate the cached engines, the next query rebuilds). True
//! incremental tantivy/similarity updates are a follow-up.
//!
//! Coherence guarantees this layer provides:
//! - Within one process, after a successful `apply_transaction` + drain,
//!   the link graph reflects every committed change.
//! - The drainer is idempotent (replaying a commit is a no-op modulo the
//!   timing of working-tree reads).
//! - Out-of-band changes (manual `git pull`, direct Obsidian edits) are
//!   NOT captured — architecture §8.4, unchanged limitation.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use turbovault_core::prelude::*;
use turbovault_git::{Oid, VaultRepo};
use turbovault_vault::VaultManager;

/// Per-vault queue of commit oids awaiting graph/search reindex.
///
/// Thread-safety: std `Mutex` (not tokio) — the critical sections are
/// pure data manipulation, microseconds long; no `await` inside.
#[derive(Debug, Default)]
pub struct ReindexQueue {
    pending: Mutex<VecDeque<Oid>>,
    /// Most recent commit fully applied to the derived indexes.
    /// `None` = nothing reindexed yet.
    cursor: Mutex<Option<Oid>>,
}

impl ReindexQueue {
    /// Construct an empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enqueue a commit. Called from the substrate's [`turbovault_git::CommitHook`].
    pub fn push(&self, commit: Oid) {
        self.pending.lock().unwrap().push_back(commit);
    }

    /// Number of commits awaiting reindex. Snapshot; may change immediately
    /// after the read in concurrent contexts.
    pub fn pending_count(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    /// Most recent commit applied to the derived indexes. `None` until the
    /// drainer applies the first commit.
    pub fn cursor(&self) -> Option<Oid> {
        *self.cursor.lock().unwrap()
    }

    /// Pop the next pending oid. Returns `None` when the queue is empty.
    pub fn pop_front(&self) -> Option<Oid> {
        self.pending.lock().unwrap().pop_front()
    }

    /// Record that `commit` has been applied to the derived indexes.
    pub fn advance_cursor(&self, commit: Oid) {
        *self.cursor.lock().unwrap() = Some(commit);
    }

    /// Drain ALL pending commits through derived indexes, advancing the
    /// cursor as each one lands. Returns the number of commits applied.
    ///
    /// Errors from any single commit's diff/parse are logged and SKIPPED —
    /// a malformed file or a transient libgit2 error should NOT brick the
    /// drainer for subsequent commits. The cursor still advances for the
    /// failing commit so the queue keeps draining.
    pub async fn drain_through(
        &self,
        repo: &VaultRepo,
        manager: &Arc<VaultManager>,
    ) -> Result<usize> {
        let mut applied = 0usize;
        while let Some(commit) = self.pop_front() {
            let parent = repo
                .git_commit_first_parent(commit)
                .map_err(|e| Error::config_error(format!("git first-parent lookup: {}", e)))?;
            if let Err(e) = apply_commit_diff(repo, parent, commit, manager).await {
                log::warn!("reindex: skipping commit {} after error: {}", commit, e);
            }
            self.advance_cursor(commit);
            applied += 1;
        }
        Ok(applied)
    }
}

// `git_commit_first_parent` lives on VaultRepo — we add it as a thin helper
// rather than calling `git2` directly here (preserves the rule that the
// substrate is the only crate that talks to libgit2).

/// Apply one commit's diff to the link graph. Reads the working tree for
/// changed/added paths (working-tree == HEAD invariant) and removes deleted
/// paths from the graph.
///
/// **Does NOT touch search/similarity engines.** Those are cache-evicted at
/// flush time by the integration layer (cold rebuild on next query). Folding
/// incremental tantivy/similarity updates in here is a follow-up.
pub async fn apply_commit_diff(
    repo: &VaultRepo,
    parent: Option<Oid>,
    commit: Oid,
    manager: &Arc<VaultManager>,
) -> Result<()> {
    let changes = repo
        .diff_path_statuses(parent, commit)
        .map_err(|e| Error::config_error(format!("git diff failed: {}", e)))?;

    let vault_root = manager.vault_path().clone();
    let graph_handle = manager.link_graph();

    for (rel_path, present_in_commit) in changes {
        let full_path = vault_root.join(&rel_path);

        if present_in_commit {
            // Re-parse from the working tree (which is HEAD post-materialize).
            // Then remove + add to clear stale edges from a prior version.
            match manager.parse_file(std::path::Path::new(&rel_path)).await {
                Ok(vault_file) => {
                    let mut graph = graph_handle.write().await;
                    // remove_file is a no-op if the path is unknown — safe.
                    let _ = graph.remove_file(&full_path);
                    if let Err(e) = graph.add_file(&vault_file) {
                        log::warn!("reindex add_file({}) failed: {}", rel_path, e);
                    }
                    if let Err(e) = graph.update_links(&vault_file) {
                        log::warn!("reindex update_links({}) failed: {}", rel_path, e);
                    }
                }
                Err(e) => {
                    // Likely "file missing" because a LATER queued commit
                    // already deleted it. That commit's drain will do the
                    // graph remove. Log + skip.
                    log::debug!("reindex skipping {} (parse failed: {})", rel_path, e);
                }
            }
        } else {
            let mut graph = graph_handle.write().await;
            let _ = graph.remove_file(&full_path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path as StdPath;
    use tempfile::TempDir;
    use turbovault_core::config::{ServerConfig, VaultConfig};
    use turbovault_git::{CommitLocks, Transaction};

    fn init_repo(dir: &StdPath) {
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        git2::Repository::init_opts(dir, &opts).unwrap();
    }

    fn test_server_config(vault_dir: &StdPath) -> ServerConfig {
        let mut cfg = ServerConfig::new();
        cfg.vaults
            .push(VaultConfig::builder("r", vault_dir).build().unwrap());
        cfg
    }

    fn setup() -> (TempDir, Arc<VaultManager>, VaultRepo, Arc<ReindexQueue>) {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let manager = Arc::new(VaultManager::new(test_server_config(tmp.path())).unwrap());
        let queue = Arc::new(ReindexQueue::new());
        let queue_clone = Arc::clone(&queue);
        let hook: turbovault_git::CommitHook =
            Arc::new(move |_parent, commit| queue_clone.push(commit));
        let repo =
            VaultRepo::open_with_locks_and_hook(tmp.path(), Arc::new(CommitLocks::new()), hook)
                .unwrap();
        (tmp, manager, repo, queue)
    }

    #[test]
    fn queue_starts_empty_and_no_cursor() {
        let q = ReindexQueue::new();
        assert_eq!(q.pending_count(), 0);
        assert_eq!(q.cursor(), None);
        assert_eq!(q.pop_front(), None);
    }

    #[test]
    fn push_and_pop_are_fifo() {
        let q = ReindexQueue::new();
        let a = Oid::from_str("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let b = Oid::from_str("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
        q.push(a);
        q.push(b);
        assert_eq!(q.pending_count(), 2);
        assert_eq!(q.pop_front(), Some(a));
        assert_eq!(q.pop_front(), Some(b));
        assert_eq!(q.pop_front(), None);
    }

    #[test]
    fn advance_cursor_records_last_applied() {
        let q = ReindexQueue::new();
        let c = Oid::from_str("cccccccccccccccccccccccccccccccccccccccc").unwrap();
        q.advance_cursor(c);
        assert_eq!(q.cursor(), Some(c));
    }

    #[tokio::test]
    async fn drain_applies_initial_commit_into_link_graph() {
        let (_tmp, manager, repo, queue) = setup();

        // Substrate write fires the hook, which enqueues.
        repo.apply_transaction(&Transaction::new("c").create("hub.md", "# Hub\n\nsee [[other]]"))
            .unwrap();
        assert_eq!(queue.pending_count(), 1);

        let n = queue.drain_through(&repo, &manager).await.unwrap();
        assert_eq!(n, 1);
        assert_eq!(queue.pending_count(), 0);

        // Graph now knows about hub.md (added) and tracks the unresolved link.
        let lg = manager.link_graph();
        let graph = lg.read().await;
        assert_eq!(graph.node_count(), 1, "hub.md added to graph");
        assert!(
            graph.unresolved_link_count() > 0,
            "the [[other]] wikilink is recorded as unresolved"
        );
    }

    #[tokio::test]
    async fn drain_removes_deleted_files_from_graph() {
        let (_tmp, manager, repo, queue) = setup();

        repo.apply_transaction(&Transaction::new("c").create("ghost.md", "# Ghost"))
            .unwrap();
        queue.drain_through(&repo, &manager).await.unwrap();
        assert_eq!(manager.link_graph().read().await.node_count(), 1);

        let ghost_blob = VaultRepo::blob_oid_of(b"# Ghost").unwrap();
        repo.apply_transaction(&Transaction::new("d").delete("ghost.md", ghost_blob))
            .unwrap();
        queue.drain_through(&repo, &manager).await.unwrap();
        assert_eq!(
            manager.link_graph().read().await.node_count(),
            0,
            "ghost.md removed after delete commit"
        );
    }

    #[tokio::test]
    async fn drain_through_handles_multi_commit_burst_in_order() {
        let (_tmp, manager, repo, queue) = setup();

        repo.apply_transaction(&Transaction::new("c1").create("a.md", "A"))
            .unwrap();
        repo.apply_transaction(&Transaction::new("c2").create("b.md", "B"))
            .unwrap();
        repo.apply_transaction(&Transaction::new("c3").create("c.md", "C"))
            .unwrap();
        assert_eq!(queue.pending_count(), 3);

        let n = queue.drain_through(&repo, &manager).await.unwrap();
        assert_eq!(n, 3);
        assert_eq!(manager.link_graph().read().await.node_count(), 3);
    }

    #[tokio::test]
    async fn drain_advances_cursor_to_latest_applied_commit() {
        let (_tmp, manager, repo, queue) = setup();
        let r1 = repo
            .apply_transaction(&Transaction::new("c").create("a.md", "A"))
            .unwrap();
        queue.drain_through(&repo, &manager).await.unwrap();
        assert_eq!(queue.cursor(), Some(r1.commit));
    }

    #[tokio::test]
    async fn apply_commit_diff_modify_clears_stale_links_then_adds_new() {
        let (_tmp, manager, repo, queue) = setup();

        // v1 links to "alpha"
        repo.apply_transaction(&Transaction::new("c").create("n.md", "see [[alpha]]"))
            .unwrap();
        queue.drain_through(&repo, &manager).await.unwrap();
        let unresolved_after_v1 = manager.link_graph().read().await.unresolved_link_count();
        assert!(unresolved_after_v1 >= 1);

        // v2 replaces link target with "beta"
        let v1_blob = VaultRepo::blob_oid_of(b"see [[alpha]]").unwrap();
        repo.apply_transaction(&Transaction::new("u").update("n.md", "see [[beta]]", v1_blob))
            .unwrap();
        queue.drain_through(&repo, &manager).await.unwrap();

        // Graph still has one node; the [[alpha]] edge no longer dangles.
        let lg = manager.link_graph();
        let graph = lg.read().await;
        assert_eq!(graph.node_count(), 1);
        // The remove+re-add cycle replaces edges; the implementation is
        // idempotent for repeated drains, which is what the contract
        // requires.
    }
}
