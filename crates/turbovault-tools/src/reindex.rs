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
//! - Within one process, after a successful `commit_changeset` + drain,
//!   the link graph reflects every committed change.
//! - The drainer is idempotent (replaying a commit is a no-op modulo the
//!   timing of working-tree reads).
//! - Out-of-band changes (manual `git pull`, direct Obsidian edits) are
//!   NOT captured — architecture §8.4, unchanged limitation.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use turbovault_core::prelude::*;
use turbovault_git::{Oid, VaultRepo};
use turbovault_vault::VaultManager;

/// Per-vault queue of commit oids awaiting graph/search reindex.
///
/// Thread-safety: std `Mutex` (not tokio) — the critical sections are
/// pure data manipulation, microseconds long; no `await` inside.
///
/// `notify` is woken on every `push` so a background drainer (GWS.14a)
/// can react immediately instead of polling. Idle pollers can ignore it.
#[derive(Debug, Default)]
pub struct ReindexQueue {
    pending: Mutex<VecDeque<Oid>>,
    /// Most recent commit fully applied to the derived indexes.
    /// `None` = nothing reindexed yet.
    cursor: Mutex<Option<Oid>>,
    /// Woken on every `push`. Background drainers (GWS.14a) await this to
    /// drain promptly without burning CPU on a tight poll loop. Flush-on-
    /// query callers don't need to await it (they just call `drain_through`
    /// directly).
    notify: Notify,
    /// Serializes whole flush passes (turbovault-9zr). Both the background
    /// drainer and a read-path flush drain + apply this queue; each pops
    /// commits BEFORE applying them to the derived indexes, so without this
    /// lock a concurrent flush could observe `pending_count() == 0` while the
    /// other flush has popped a commit but not yet applied it — and then read
    /// a stale graph. Held across an entire drain+apply pass.
    flush_lock: tokio::sync::Mutex<()>,
}

impl ReindexQueue {
    /// Construct an empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enqueue a commit. Called from the substrate's [`turbovault_git::CommitHook`].
    /// Wakes any background drainer (GWS.14a) currently parked on `notify`.
    pub fn push(&self, commit: Oid) {
        self.pending.lock().unwrap().push_back(commit);
        self.notify.notify_one();
    }

    /// Borrow the per-queue notifier. The background drainer awaits
    /// `notify.notified()` between drain passes; flush-on-query callers
    /// don't need it.
    pub fn notify(&self) -> &Notify {
        &self.notify
    }

    /// Acquire the flush serialization lock (turbovault-9zr). A whole flush
    /// pass (pop + apply) must hold this so two flushers — the background
    /// drainer and a read-path flush — cannot interleave the pop-before-apply
    /// window and let a reader observe a half-drained graph.
    pub async fn lock_flush(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.flush_lock.lock().await
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
        // turbovault-9zr: hold the flush lock for the whole pass so this drain
        // can't interleave with the server's read-path flush (which also pops
        // before applying).
        let _flush_guard = self.lock_flush().await;
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

/// turbovault-bou / architecture §8.4 + §8.5: HEAD-ref polling listener.
///
/// Detects **out-of-band git ref advances** — a `git pull`, `git checkout`,
/// a bare `git commit` by another process, or a sibling turbovault
/// instance writing through its own substrate — and pushes the new HEAD
/// oid onto the per-vault [`ReindexQueue`] so the next drain absorbs the
/// change. Without this listener, a multi-instance dogfood setup silently
/// desyncs the in-memory link graph + tantivy index from disk for every
/// commit the other instance lands.
///
/// **Does NOT detect uncommitted working-tree edits.** A direct Obsidian
/// edit (or a CC `Edit` outside MCP) changes bytes on disk without
/// advancing HEAD; the polling listener can't see it. Architecture §8.4
/// limitation; a working-tree inotify listener (resurrecting the dormant
/// `turbovault_vault::watcher::VaultWatcher`) is the documented Phase 2.
///
/// Initial HEAD is snapshotted at startup, so the listener won't re-push
/// commits that landed before it started. A commit advanced by THIS
/// process (CommitHook already pushed) will also be observed by the
/// listener and re-pushed — that's wasteful but idempotent in net effect
/// (the drainer's `apply_commit_diff` is a function of `(parent, commit)`
/// and produces the same delta on a replay).
///
/// Runs forever; cancellation is via task abort (the server stores the
/// `JoinHandle` and calls `abort()` on `remove_vault` / shutdown).
pub async fn watch_ref_changes(
    vault_path: std::path::PathBuf,
    queue: Arc<ReindexQueue>,
    interval: std::time::Duration,
) {
    let mut last_oid = read_head_oid(&vault_path).await;
    loop {
        tokio::time::sleep(interval).await;
        let current = read_head_oid(&vault_path).await;
        if current != last_oid {
            if let Some(new) = current {
                queue.push(new);
            }
            last_oid = current;
        }
    }
}

/// Read HEAD's oid via libgit2 inside `spawn_blocking` (libgit2 is
/// `!Sync`). Returns `None` for unborn branches or any error opening the
/// repo (transient errors are skipped — the next poll retries).
async fn read_head_oid(vault_path: &std::path::Path) -> Option<Oid> {
    let path = vault_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        VaultRepo::open(&path).ok().and_then(|repo| repo.head_oid())
    })
    .await
    .ok()
    .flatten()
}

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
    use turbovault_git::{Changeset, CommitLocks};

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
        repo.commit_changeset(&Changeset::new("c").create("hub.md", "# Hub\n\nsee [[other]]"))
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

        repo.commit_changeset(&Changeset::new("c").create("ghost.md", "# Ghost"))
            .unwrap();
        queue.drain_through(&repo, &manager).await.unwrap();
        assert_eq!(manager.link_graph().read().await.node_count(), 1);

        let ghost_blob = VaultRepo::blob_oid_of(b"# Ghost").unwrap();
        repo.commit_changeset(&Changeset::new("d").delete("ghost.md", ghost_blob))
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

        repo.commit_changeset(&Changeset::new("c1").create("a.md", "A"))
            .unwrap();
        repo.commit_changeset(&Changeset::new("c2").create("b.md", "B"))
            .unwrap();
        repo.commit_changeset(&Changeset::new("c3").create("c.md", "C"))
            .unwrap();
        assert_eq!(queue.pending_count(), 3);

        let n = queue.drain_through(&repo, &manager).await.unwrap();
        assert_eq!(n, 3);
        assert_eq!(manager.link_graph().read().await.node_count(), 3);
    }

    /// turbovault-9zr: a single commit that adds a file AND a file linking to it
    /// (batch_execute / move shape) must produce a resolved edge after drain.
    #[tokio::test]
    async fn drain_resolves_intra_commit_link_in_one_commit() {
        let (_tmp, manager, repo, queue) = setup();

        repo.commit_changeset(
            &Changeset::new("batch")
                .create("one.md", "# One\n\nlinks [[two]]\n")
                .create("two.md", "# Two\n"),
        )
        .unwrap();
        assert_eq!(queue.pending_count(), 1, "one commit for the whole batch");

        queue.drain_through(&repo, &manager).await.unwrap();

        let lg = manager.link_graph();
        let graph = lg.read().await;
        assert_eq!(graph.node_count(), 2, "both files in the graph");
        assert_eq!(
            graph.unresolved_link_count(),
            0,
            "[[two]] should resolve once both files land in one commit"
        );
        // edge_count is key-agnostic (nodes are keyed by absolute path).
        assert_eq!(graph.edge_count(), 1, "one.md -> two.md edge should exist");
    }

    /// turbovault-78w (TV-002 fail-case 1): a linker authored BEFORE its target
    /// in a SEPARATE commit must end with a resolved backlink edge once the
    /// target lands and drains — the 9zr promotion must fire across commits,
    /// not just intra-commit. This is what move_note's graph resolution relies
    /// on (an inbound link that was unresolved at author time must be a real
    /// edge by move time).
    #[tokio::test]
    async fn drain_promotes_link_authored_before_target_across_commits() {
        let (_tmp, manager, repo, queue) = setup();

        // Commit 1: linker references [[target]] — target does not exist yet.
        repo.commit_changeset(&Changeset::new("c1").create("linker.md", "see [[target]]\n"))
            .unwrap();
        queue.drain_through(&repo, &manager).await.unwrap();
        {
            let lg = manager.link_graph();
            let graph = lg.read().await;
            assert_eq!(
                graph.unresolved_link_count(),
                1,
                "[[target]] parked unresolved while target absent"
            );
            assert_eq!(graph.edge_count(), 0);
        }

        // Commit 2: target.md created in a SEPARATE commit.
        repo.commit_changeset(&Changeset::new("c2").create("target.md", "# Target\n"))
            .unwrap();
        queue.drain_through(&repo, &manager).await.unwrap();

        let lg = manager.link_graph();
        let graph = lg.read().await;
        assert_eq!(graph.node_count(), 2, "both files in the graph");
        assert_eq!(
            graph.unresolved_link_count(),
            0,
            "link promoted once target landed in a later commit"
        );
        assert_eq!(
            graph.edge_count(),
            1,
            "linker -> target edge after promotion"
        );
        let bl = graph
            .backlinks(&manager.vault_path().join("target.md"))
            .unwrap();
        assert_eq!(bl.len(), 1, "backlinks(target) must see the linker");
    }

    /// turbovault-78w (TV-002 fail-case 2): after a move (delete old + add new
    /// + rewrite the linker) drains, the graph must reflect the new path —
    /// backlinks(renamed) sees the linker, forward_links(linker) sees renamed,
    /// the old node is gone.
    #[tokio::test]
    async fn drain_move_keeps_backlinks_coherent() {
        let (_tmp, manager, repo, queue) = setup();

        // Establish a resolved linker -> target edge.
        repo.commit_changeset(
            &Changeset::new("c1")
                .create("linker.md", "see [[target]]\n")
                .create("target.md", "# T\n"),
        )
        .unwrap();
        queue.drain_through(&repo, &manager).await.unwrap();
        assert_eq!(
            manager.link_graph().read().await.edge_count(),
            1,
            "precondition: linker -> target edge exists"
        );

        // Move target.md -> target-renamed.md and rewrite the linker, one commit
        // (the move_file_with_link_updates shape).
        repo.commit_changeset(
            &Changeset::new("move")
                .remove("target.md")
                .upsert("target-renamed.md", b"# T\n".to_vec())
                .upsert("linker.md", b"see [[target-renamed]]\n".to_vec()),
        )
        .unwrap();
        queue.drain_through(&repo, &manager).await.unwrap();

        let lg = manager.link_graph();
        let graph = lg.read().await;
        assert_eq!(graph.node_count(), 2, "linker + renamed target");
        assert_eq!(graph.edge_count(), 1, "linker -> renamed edge");
        let bl = graph
            .backlinks(&manager.vault_path().join("target-renamed.md"))
            .unwrap();
        assert_eq!(bl.len(), 1, "backlinks(renamed) sees the linker");
        let fl = graph
            .forward_links(&manager.vault_path().join("linker.md"))
            .unwrap();
        assert_eq!(fl.len(), 1, "forward_links(linker) sees renamed");
        let old = graph
            .backlinks(&manager.vault_path().join("target.md"))
            .unwrap();
        assert!(old.is_empty(), "old target node gone");
    }

    #[tokio::test]
    async fn drain_advances_cursor_to_latest_applied_commit() {
        let (_tmp, manager, repo, queue) = setup();
        let r1 = repo
            .commit_changeset(&Changeset::new("c").create("a.md", "A"))
            .unwrap();
        queue.drain_through(&repo, &manager).await.unwrap();
        assert_eq!(queue.cursor(), Some(r1.commit));
    }

    // -------- GWS.14a: background drainer wiring --------

    #[tokio::test]
    async fn push_wakes_notify_so_background_drainer_does_not_poll() {
        // Smoke test the notify path on ReindexQueue. A real drainer is
        // started by ObsidianMcpServer; here we verify a parked notified()
        // future fires immediately when push() runs.
        let q = Arc::new(ReindexQueue::new());
        let q2 = Arc::clone(&q);
        let woken = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let woken_clone = Arc::clone(&woken);
        let waiter = tokio::spawn(async move {
            let notified = q2.notify().notified();
            tokio::pin!(notified);
            tokio::time::timeout(std::time::Duration::from_secs(2), &mut notified)
                .await
                .expect("notify should fire before timeout");
            woken_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        // Yield so the waiter has a chance to park BEFORE we push.
        tokio::task::yield_now().await;
        let oid = Oid::from_str("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee").unwrap();
        q.push(oid);
        waiter.await.unwrap();
        assert!(woken.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn apply_commit_diff_modify_clears_stale_links_then_adds_new() {
        let (_tmp, manager, repo, queue) = setup();

        // v1 links to "alpha"
        repo.commit_changeset(&Changeset::new("c").create("n.md", "see [[alpha]]"))
            .unwrap();
        queue.drain_through(&repo, &manager).await.unwrap();
        let unresolved_after_v1 = manager.link_graph().read().await.unresolved_link_count();
        assert!(unresolved_after_v1 >= 1);

        // v2 replaces link target with "beta"
        let v1_blob = VaultRepo::blob_oid_of(b"see [[alpha]]").unwrap();
        repo.commit_changeset(&Changeset::new("u").update("n.md", "see [[beta]]", v1_blob))
            .unwrap();
        queue.drain_through(&repo, &manager).await.unwrap();

        // turbovault-34p: assert the modify actually CLEARED the stale [[alpha]]
        // link and recorded [[beta]] — the original test asserted only
        // node_count and would have passed even if the modify reindex did
        // nothing at all.
        let lg = manager.link_graph();
        let graph = lg.read().await;
        assert_eq!(graph.node_count(), 1);
        let n_path = manager.vault_path().join("n.md");
        let targets: Vec<String> = graph
            .all_unresolved_links()
            .get(&n_path)
            .map(|links| links.iter().map(|l| l.target.clone()).collect())
            .unwrap_or_default();
        assert!(
            targets.iter().any(|t| t == "beta"),
            "[[beta]] must be recorded after the modify: {targets:?}"
        );
        assert!(
            !targets.iter().any(|t| t == "alpha"),
            "stale [[alpha]] must be cleared after the modify: {targets:?}"
        );
    }

    // -------- turbovault-bou: HEAD-ref polling listener --------

    /// Create a commit using bare git2, bypassing the substrate. Simulates
    /// an out-of-band ref advance (manual `git pull`, another process
    /// committing). Returns the new commit's oid.
    fn make_external_commit(repo_path: &StdPath, file_name: &str, content: &str) -> Oid {
        let repo = git2::Repository::open(repo_path).unwrap();
        std::fs::write(repo_path.join(file_name), content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(StdPath::new(file_name)).unwrap();
        let tree_oid = index.write_tree().unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = git2::Signature::now("Ext", "ext@example").unwrap();
        let parent = repo
            .head()
            .ok()
            .and_then(|h| h.target())
            .and_then(|oid| repo.find_commit(oid).ok());
        match parent {
            Some(parent) => repo
                .commit(Some("HEAD"), &sig, &sig, content, &tree, &[&parent])
                .unwrap(),
            None => repo
                .commit(Some("HEAD"), &sig, &sig, content, &tree, &[])
                .unwrap(),
        }
    }

    /// Wait up to `timeout` for `queue.pending_count()` to reach `target`.
    /// Returns `true` on success, `false` on timeout.
    async fn wait_for_pending(
        queue: &ReindexQueue,
        target: usize,
        timeout: std::time::Duration,
    ) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if queue.pending_count() >= target {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        false
    }

    /// turbovault-bou: an out-of-band commit (made directly via git2,
    /// bypassing the substrate) is detected by the listener and pushed
    /// onto the queue within the poll interval.
    #[tokio::test]
    async fn watch_ref_changes_detects_external_commit() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        // Seed an initial commit so the listener has a non-None baseline.
        make_external_commit(tmp.path(), "seed.md", "seed");
        let queue = Arc::new(ReindexQueue::new());

        let vault_path = tmp.path().to_path_buf();
        let queue_clone = Arc::clone(&queue);
        let listener = tokio::spawn(async move {
            watch_ref_changes(
                vault_path,
                queue_clone,
                std::time::Duration::from_millis(25),
            )
            .await;
        });

        // Give the listener a couple of polls to establish baseline.
        tokio::time::sleep(std::time::Duration::from_millis(75)).await;
        assert_eq!(queue.pending_count(), 0, "baseline shouldn't enqueue");

        // External commit — listener should detect on next poll.
        let new_oid = make_external_commit(tmp.path(), "ext.md", "ext-content");
        let detected = wait_for_pending(&queue, 1, std::time::Duration::from_millis(500)).await;
        assert!(detected, "listener should detect external commit");
        assert_eq!(queue.pop_front(), Some(new_oid));

        listener.abort();
    }

    /// turbovault-bou: when no out-of-band changes happen, the listener
    /// stays silent — the queue doesn't accumulate idle pushes.
    #[tokio::test]
    async fn watch_ref_changes_idle_no_pushes() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        make_external_commit(tmp.path(), "seed.md", "seed");
        let queue = Arc::new(ReindexQueue::new());

        let vault_path = tmp.path().to_path_buf();
        let queue_clone = Arc::clone(&queue);
        let listener = tokio::spawn(async move {
            watch_ref_changes(
                vault_path,
                queue_clone,
                std::time::Duration::from_millis(25),
            )
            .await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(queue.pending_count(), 0, "idle listener stays quiet");

        listener.abort();
    }

    /// turbovault-bou: an unborn-branch baseline (no commits yet) doesn't
    /// crash the listener. It picks up the first commit when it appears.
    #[tokio::test]
    async fn watch_ref_changes_handles_unborn_baseline() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        // No initial commit; HEAD is unborn at listener start.
        let queue = Arc::new(ReindexQueue::new());

        let vault_path = tmp.path().to_path_buf();
        let queue_clone = Arc::clone(&queue);
        let listener = tokio::spawn(async move {
            watch_ref_changes(
                vault_path,
                queue_clone,
                std::time::Duration::from_millis(25),
            )
            .await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(75)).await;
        let first_oid = make_external_commit(tmp.path(), "first.md", "first");

        let detected = wait_for_pending(&queue, 1, std::time::Duration::from_millis(500)).await;
        assert!(detected, "listener should detect the first commit");
        assert_eq!(queue.pop_front(), Some(first_oid));

        listener.abort();
    }
}
