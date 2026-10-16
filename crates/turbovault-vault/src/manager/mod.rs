//! The vault manager: file operations, the note cache and link graph, and
//! the freshness gate that keeps them in step with the vault on disk.
//!
//! - `state`: what each cached note was last seen as, and the sweep schedule
//! - `scan`: reading and parsing the vault, and syncing the cache and graph
//! - `freshness`: draining Git commits and sweeping for external changes
//! - `write`: every change, through the write substrate
//! - `paths`: path resolution and the protected-directory policy

use crate::reindex::CommitOrigin;
use futures::StreamExt;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::instrument;
use turbovault_audit::{AuditLog, SnapshotStore};
use turbovault_core::prelude::*;
// Markdown only, which is narrower than `allowed_extensions`: that also
// admits `.txt` and `.canvas`, because those are worth *discovering*, not
// worth parsing as notes.
use turbovault_core::is_note;
use turbovault_core::{
    Change, ChangePlan, PathValidator, Precondition, VaultGitConfig, WriteBackend, path_to_slash,
};
use turbovault_git::{CommitHook, CommitLocks, Oid, VaultRepo};
use turbovault_graph::LinkGraph;
use turbovault_parser::Parser;

use crate::reindex::{ReindexQueue, watch_ref_changes};
use crate::substrate::{
    ApplyOutcome, CachedRepo, CasCollisionFlush, DirectSubstrate, GitSubstrate, WriteSubstrate,
};

/// One drained commit: its oid, how this process learned of it, and the
/// `(path, present)` set its diff produced.
type DrainedCommit = (Oid, CommitOrigin, Vec<(String, bool)>);

/// Tag a substrate outcome's `(path, present)` set with how this process
/// learned about it. Writes made here are `Local` by definition; only the
/// reindex drain can see anything else.
fn with_origin(
    changed: Vec<(String, bool)>,
    origin: CommitOrigin,
) -> Vec<(String, bool, CommitOrigin)> {
    changed
        .into_iter()
        .map(|(path, present)| (path, present, origin))
        .collect()
}

/// The work a [`ChangeListener`] hands back to be awaited.
pub type ChangeListenerFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// R2 change-listener (design §6.3): a callback fired with the collapsed
/// `(path, present, origin)` set after every `sync_index` (both backends),
/// every reindex drain pass, and every freshness sweep.
///
/// `origin` says whether this process made the change or merely observed it
/// afterwards. A consumer that also reports writes at the point of the write
/// needs that distinction, or it announces its own mutations twice: once when
/// it makes them and again when the drain pass or sweep sees the result. The
/// server registers one that feeds its full-text search + similarity indexes —
/// those engines stay ABOVE the vault layer; `VaultManager` only invokes the
/// callback (the R2 dependency inversion, design §6.6/§6.7).
///
/// The listener returns a future that the manager **awaits** before its
/// freshness gate returns. That is what makes [`VaultManager::ensure_fresh`] a
/// guarantee rather than a hint: without it the manager could report the link
/// graph as current while the search index was still catching up in a spawned
/// task, and a caller gated on one would read the other stale. The cost is a
/// re-entrancy rule, enforced structurally by the no-manager-capture rule in
/// [`VaultManager::set_change_listener`]: a listener must not call back into
/// the gate.
pub type ChangeListener =
    Arc<dyn Fn(Vec<(String, bool, CommitOrigin)>) -> ChangeListenerFuture + Send + Sync>;

/// Path components that the note APIs may never traverse, whatever the vault
/// configuration says.
///
/// `.turbovault/` holds the audit trail and snapshot store. A note write that
/// could reach it would let a caller rewrite the record of its own operations,
/// so unlike [`ServerConfig::excluded_paths`] this list is not configurable.
pub const PROTECTED_COMPONENTS: [&str; 1] = [".turbovault"];

/// One note found by a vault scan, with the metadata the scan already read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedNote {
    /// Absolute path to the note.
    pub path: PathBuf,
    /// Size in bytes at scan time.
    pub size_bytes: u64,
    /// Last-modified time exactly as the platform reports it.
    ///
    /// Kept as a `SystemTime` rather than a rounded integer because it doubles
    /// as half of the freshness fingerprint: two edits inside the same
    /// millisecond that land on the same byte length become indistinguishable
    /// once the timestamp is truncated. Use [`Self::modified_ms`] for the
    /// rounded form that the plugin-facing note listing publishes.
    pub modified: Option<SystemTime>,
}

impl ScannedNote {
    /// Last-modified time as Unix epoch milliseconds, when the platform reports
    /// one.
    pub fn modified_ms(&self) -> Option<u64> {
        self.modified
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .and_then(|since| u64::try_from(since.as_millis()).ok())
    }

    fn fingerprint(&self) -> FileFingerprint {
        FileFingerprint {
            size_bytes: self.size_bytes,
            modified: self.modified,
        }
    }
}

mod freshness;
mod paths;
mod scan;
mod state;
mod write;

use scan::*;
use state::*;

/// Main vault manager: file operations, cached notes and the link graph
pub struct VaultManager {
    config: ServerConfig,
    vault_path: PathBuf,
    parser: Parser,
    link_graph: Arc<RwLock<LinkGraph>>,
    file_cache: Arc<RwLock<HashMap<PathBuf, CacheEntry>>>,
    audit_log: Option<Arc<AuditLog>>,
    snapshot_store: Option<Arc<SnapshotStore>>,
    /// write-substrate-layering M3a: the write dispatch chokepoint (design
    /// §6.3). Selected once, at construction, from `write_backend` — layers
    /// above `VaultManager` stay backend-agnostic (R2).
    substrate: WriteSubstrate,
    /// M4c (bite 3a, design §6.6): the git reindex queue the `GitSubstrate`'s
    /// commit hook pushes onto and the HEAD-ref listener enqueues out-of-band
    /// advances onto. `None` on a Direct vault (nothing queues).
    reindex_queue: Option<Arc<ReindexQueue>>,
    /// Weak self-handle published by [`Self::ensure_reindex_started`]. The
    /// CAS-collision flush closure is built in `new()` — before any
    /// `Arc<Self>` exists — so it reaches the manager at fire time through
    /// this set-once slot instead of an `Arc` capture (which would cycle and
    /// leak the manager). `OnceLock` = lock-free reads after the one publish.
    self_ref: Arc<OnceLock<Weak<VaultManager>>>,
    /// Drainer + HEAD-ref-listener `JoinHandle`s, aborted in `Drop`. std
    /// `Mutex` — the critical section is pure `Vec` pushes, no `await`.
    /// The spawned tasks hold `Weak<Self>` (not `Arc`), so this abort is the
    /// sole thing that stops them (there is no other shutdown path).
    reindex_tasks: StdMutex<Vec<tokio::task::JoinHandle<()>>>,
    /// R7 change-listener slot (design §6.3). Fired after every `sync_index`,
    /// drain pass, and freshness sweep; both backends. See [`ChangeListener`].
    change_listener: StdMutex<Option<ChangeListener>>,
    /// Scan configuration lifted out of `config` once, so a freshness sweep can
    /// move the walk to the blocking pool without borrowing the manager.
    scan_spec: Arc<ScanSpec>,
    /// Serializes freshness sweeps with each other AND with their fan-out, so
    /// two passes can never hand the layers above their diffs out of order. A
    /// `tokio` mutex because the critical section awaits disk I/O.
    reconcile: tokio::sync::Mutex<ReconcileState>,
    /// Vault-relative paths with a write in flight. See [`WriteClaim`].
    write_claims: StdMutex<HashMap<String, usize>>,
    /// `vault_path` as the filesystem resolves it, every symlink to it
    /// followed (on Windows, `\\?\C:\...`): what path resolution checks real
    /// locations against, and what paths that reach the manager already
    /// resolved start with. Worked out once, by the first use that succeeds.
    real_root: OnceLock<PathBuf>,
    /// Whether the vault's filesystem finds a name whatever its case, worked
    /// out once (see `on_disk_spelling`).
    folds_names: OnceLock<bool>,
    /// The protected directory names, folded the way `protected_component`
    /// compares them, once rather than on every path component.
    protected_names: Vec<String>,
}

impl VaultManager {
    /// Create a new vault manager
    pub fn new(config: ServerConfig) -> Result<Self> {
        // The vault's own settings override the server's from here on, for
        // the scan, path protection, size limit and freshness schedule alike.
        let config = config.for_vault(config.default_vault()?);
        let default_vault = config.default_vault()?;
        default_vault.validate_operations()?;
        let vault_path = default_vault.path.clone();
        let parser = Parser::new(vault_path.clone());

        // Set-once weak self-handle. The git arm's CAS-collision flush closure
        // is built below — BEFORE any `Arc<Self>` exists — so it reaches the
        // manager at fire time through this slot (published by
        // `ensure_reindex_started`) instead of an `Arc` capture that would
        // cycle and leak the manager. No-op flush until published.
        let self_ref: Arc<OnceLock<Weak<VaultManager>>> = Arc::new(OnceLock::new());

        // write-substrate-layering M4c (bite 3a, turbovault-qae.5.3): the git
        // arm now OWNS its reindex machinery — a `ReindexQueue` fed by the
        // substrate's commit hook, a cached repo, and a CAS-collision flush.
        // `ensure_reindex_started` (called by the server once an `Arc<Self>`
        // exists) spawns the background drainer + HEAD-ref listener over this
        // queue. That closes two of the three M4b (bite 2) gaps:
        //   - gap #2 (search staleness): CLOSED. Every apply fires the R7
        //     change-listener (both backends), and each drain pass fires it
        //     too, so the server's search + similarity indexes see a
        //     manager-git write. The link graph was already correct (sync_index
        //     runs on every apply).
        //   - gap #3 (per-call open): CLOSED. The repo is opened ONCE here
        //     (hook + locks bound) and cached; writes reuse it.
        // The one gap that STAYS open until bite 3b+4 (qae.5.4):
        //   - gap #1 (two `CommitLocks` registries + two drainers on one
        //     worktree). This substrate still holds its OWN `CommitLocks`,
        //     independent of the server's still-live `GitFileTools`, and both
        //     run a drainer/ref-listener over the same worktree. Commits stay
        //     linearizable NOT via a shared `Arc` but via
        //     `VaultRepo::commit_changeset`'s cross-process `flock` on
        //     `<repo>/.git/turbovault-write.lock` (keyed by the physical `.git`
        //     path, so it serializes both registries); the double reindex is
        //     idempotent (`apply` is a function of `(path, present)`). Sharing
        //     one registry + one drainer — repointing the handlers onto this
        //     manager — is bite 3b+4's job. Do NOT add dedup/sharing here.
        let (substrate, reindex_queue) = match default_vault.write_backend {
            WriteBackend::Direct => (
                WriteSubstrate::Direct(DirectSubstrate::new(vault_path.clone())),
                None,
            ),
            WriteBackend::Git => {
                let include_ignored = default_vault
                    .git
                    .as_ref()
                    .map(|git| git.include_ignored)
                    .unwrap_or_else(|| VaultGitConfig::default().include_ignored);

                // The manager-owned queue: the commit hook enqueues every
                // commit; the drainer + read-path flush apply them (design §6.6).
                let queue = Arc::new(ReindexQueue::new());
                let queue_for_hook = Arc::clone(&queue);
                let commit_hook: CommitHook =
                    Arc::new(move |_parent, commit| queue_for_hook.push(commit));

                // This substrate's OWN lock registry (gap #1, still open).
                let commit_locks = Arc::new(CommitLocks::new());

                // gap #3 closed: open the repo ONCE with the hook + locks
                // bound, and cache it (git-openability validated here — a
                // non-git path fails at construction, design §11.12 fail-fast).
                let repo = VaultRepo::open_with_locks_and_hook(
                    &vault_path,
                    Arc::clone(&commit_locks),
                    Arc::clone(&commit_hook),
                )
                .map_err(|e| {
                    Error::config_error(format!(
                        "vault '{}' has write_backend=git but {:?} is not a usable git repo: {}",
                        default_vault.name, vault_path, e
                    ))
                })?;
                // Finish a write an earlier process landed but did not write
                // out. If that fails now, the next write tries again.
                if let Err(error) = repo.recover_interrupted_write() {
                    tracing::warn!(
                        "vault '{}': could not finish an interrupted write: {}",
                        default_vault.name,
                        error
                    );
                }
                let cached_repo: CachedRepo = Arc::new(std::sync::Mutex::new(repo));

                // GWS.14b: drain the queue before a ConcurrencyError surfaces
                // so a re-read sees coherent derived state. Reaches the manager
                // via the set-once weak handle; a no-op until it is published.
                let self_ref_for_flush = Arc::clone(&self_ref);
                let flush_on_collision: CasCollisionFlush = Arc::new(move || {
                    let self_ref = Arc::clone(&self_ref_for_flush);
                    Box::pin(async move {
                        if let Some(mgr) = self_ref.get().and_then(Weak::upgrade) {
                            mgr.flush_reindex().await;
                        }
                        Ok(())
                    })
                });

                let git = GitSubstrate::with_reindex(
                    vault_path.clone(),
                    include_ignored,
                    commit_locks,
                    commit_hook,
                    cached_repo,
                    flush_on_collision,
                );
                (WriteSubstrate::Git(git), Some(queue))
            }
            backend => {
                return Err(Error::config_error(format!(
                    "vault '{}' has write_backend={backend:?}, which this build can't write",
                    default_vault.name
                )));
            }
        };

        let scan_spec = Arc::new(ScanSpec::new(&config, vault_path.clone()));
        let protected_names = PROTECTED_COMPONENTS
            .iter()
            .copied()
            .chain(config.excluded_paths.iter().map(String::as_str))
            .map(str::to_lowercase)
            .collect();

        Ok(Self {
            real_root: OnceLock::new(),
            folds_names: OnceLock::new(),
            protected_names,
            config,
            vault_path,
            parser,
            link_graph: Arc::new(RwLock::new(LinkGraph::new())),
            file_cache: Arc::new(RwLock::new(HashMap::new())),
            audit_log: None,
            snapshot_store: None,
            substrate,
            reindex_queue,
            self_ref,
            reindex_tasks: StdMutex::new(Vec::new()),
            change_listener: StdMutex::new(None),
            scan_spec,
            reconcile: tokio::sync::Mutex::new(ReconcileState::new()),
            write_claims: StdMutex::new(HashMap::new()),
        })
    }

    /// M4c (bite 3a, design §6.3/§6.6): spawn the git reindex background tasks
    /// — the drainer (applies queued commits to the link graph + fires the
    /// change-listener) and the HEAD-ref listener (enqueues out-of-band
    /// advances). Idempotent; a no-op on a Direct vault or once already
    /// started. MUST be called through an `Arc` (the server does, right after
    /// it caches the manager) — the spawned tasks hold `Weak<Self>`, never an
    /// `Arc<Self>`, so they can't keep the manager alive; an `Arc` capture
    /// would cycle → the manager never drops → the `Drop` abort never runs →
    /// leak. `new()` has no `Arc<Self>` to downgrade, which is why this is
    /// separate.
    pub fn ensure_reindex_started(self: &Arc<Self>) {
        let Some(queue) = self.reindex_queue.clone() else {
            return; // Direct backend — nothing to drain.
        };
        let mut tasks = self.reindex_tasks.lock().unwrap();
        if !tasks.is_empty() {
            return; // already started
        }
        // Publish the weak self-handle for the CAS-collision flush closure.
        let _ = self.self_ref.set(Arc::downgrade(self));

        self.start_reindex_tasks(&queue, Duration::from_secs(5), &mut tasks);
    }

    /// Test-only: like [`Self::ensure_reindex_started`] but with a caller-
    /// chosen HEAD-ref poll interval, so tests don't wait the production 5s.
    #[cfg(test)]
    pub(crate) fn ensure_reindex_started_with_interval(self: &Arc<Self>, ref_poll: Duration) {
        let Some(queue) = self.reindex_queue.clone() else {
            return;
        };
        let mut tasks = self.reindex_tasks.lock().unwrap();
        if !tasks.is_empty() {
            return;
        }
        let _ = self.self_ref.set(Arc::downgrade(self));
        self.start_reindex_tasks(&queue, ref_poll, &mut tasks);
    }

    /// Spawn the drainer + ref-listener over `queue` and store their handles.
    /// `ref_poll` is the HEAD-ref listener's poll interval (5s in production;
    /// tests override it). Caller holds the `reindex_tasks` lock + has
    /// published `self_ref`.
    fn start_reindex_tasks(
        self: &Arc<Self>,
        queue: &Arc<ReindexQueue>,
        ref_poll: Duration,
        tasks: &mut Vec<tokio::task::JoinHandle<()>>,
    ) {
        // Background drainer (mirrors the server's GWS.14a loop): park on the
        // queue notifier / 100ms safety-net poll, drain when pending. Holds
        // `Weak<Self>` and exits the instant the manager drops.
        let weak = Arc::downgrade(self);
        let drain_queue = Arc::clone(queue);
        let drainer = tokio::spawn(async move {
            loop {
                let notified = drain_queue.notify().notified();
                tokio::pin!(notified);
                tokio::select! {
                    _ = &mut notified => {}
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                }
                let Some(mgr) = weak.upgrade() else {
                    return; // manager dropped — stop.
                };
                if drain_queue.pending_count() == 0 {
                    continue;
                }
                mgr.flush_reindex().await;
            }
        });

        // HEAD-ref listener (turbovault-bou): enqueue out-of-band advances.
        // It holds only the path + queue (never `Self`), so it can't keep the
        // manager alive; `Drop` aborts its handle.
        let path = self.vault_path.clone();
        let listen_queue = Arc::clone(queue);
        let listener = tokio::spawn(async move {
            watch_ref_changes(path, listen_queue, ref_poll).await;
        });

        tasks.push(drainer);
        tasks.push(listener);
    }

    /// Register the R7 change-listener (design §6.3). Fired after every
    /// `sync_index`, every drain pass, and every freshness sweep with the
    /// collapsed `(path, present, origin)` set. The server registers one that
    /// updates its full-text search index + invalidates similarity — those
    /// engines live ABOVE the vault layer; the manager only invokes this
    /// callback (the R2 dependency inversion). Both backends fire it.
    ///
    /// # Contract
    ///
    /// A listener must not capture an `Arc<VaultManager>`, for two reasons that
    /// happen to have the same remedy. It would cycle (`vault_managers` holds
    /// the manager, which holds the listener) and defeat the `Drop`-based
    /// teardown of the reindex tasks. And because the manager now *awaits* the
    /// returned future inside its freshness gate, a listener holding a manager
    /// could re-enter that gate and deadlock on the lock the caller already
    /// holds. Capturing only the state the listener actually maintains rules
    /// both out by construction.
    pub fn set_change_listener(&self, listener: ChangeListener) {
        *self.change_listener.lock().unwrap() = Some(listener);
    }

    /// Invoke the change-listener (if registered) with `changed` and wait for
    /// the work it hands back. Cheap no-op when nothing changed or none is set.
    ///
    /// Awaiting rather than spawning is what lets [`Self::ensure_fresh`] promise
    /// that every derived view agrees once it returns. See [`ChangeListener`]
    /// for the re-entrancy rule that buys.
    async fn fire_change_listener(&self, changed: Vec<(String, bool, CommitOrigin)>) {
        if changed.is_empty() {
            return;
        }
        let listener = self.change_listener.lock().unwrap().clone();
        if let Some(listener) = listener {
            listener(changed).await;
        }
    }

    /// Get vault path
    pub fn vault_path(&self) -> &PathBuf {
        &self.vault_path
    }

    /// The name of the (single) vault this manager was built for. Lets the tool
    /// layer take one active-vault snapshot instead of re-resolving the active
    /// vault separately for the name and the manager.
    pub fn vault_name(&self) -> &str {
        self.config
            .default_vault()
            .map(|v| v.name.as_str())
            .unwrap_or_default()
    }

    /// Convert a path to a `/`-separated vault-relative string.
    ///
    /// Strips the vault root prefix and normalizes separators to `/` (so paths
    /// render consistently across platforms). The root is matched as
    /// configured and as the filesystem resolves it, since on Windows a
    /// resolved path starts `\\?\C:\` where the configured root starts `C:\`.
    /// Both are compared as rendered with `/`, so a path that was already
    /// rendered that way (`//?/C:/...`, which Windows no longer reads as the
    /// same path) is stripped too. Falls back to the lossy full path when
    /// `path` is not under the vault root.
    pub fn relative_path(&self, path: &Path) -> String {
        let rendered = path_to_slash(path);
        let resolved = self.real_root().unwrap_or(&self.vault_path);
        for root in [&self.vault_path, resolved] {
            let root = path_to_slash(root);
            if let Some(relative) = rendered
                .strip_prefix(root.trim_end_matches('/'))
                .and_then(|rest| rest.strip_prefix('/'))
            {
                return relative.to_string();
            }
        }
        rendered
    }

    /// Set the audit log and snapshot store for operation tracking.
    ///
    /// Wires the Direct substrate's own audit/snapshot recording (M3a
    /// deliverable decision 3) in addition to the manager's own copies
    /// (kept for the `audit_log()`/`snapshot_store()` accessors below).
    pub fn set_audit_log(&mut self, audit_log: Arc<AuditLog>, snapshot_store: Arc<SnapshotStore>) {
        if let WriteSubstrate::Direct(direct) = &mut self.substrate {
            direct.set_audit_log(Arc::clone(&audit_log), Arc::clone(&snapshot_store));
        }
        self.audit_log = Some(audit_log);
        self.snapshot_store = Some(snapshot_store);
    }

    /// Get the audit log reference (if configured)
    pub fn audit_log(&self) -> Option<&Arc<AuditLog>> {
        self.audit_log.as_ref()
    }

    /// Get the snapshot store reference (if configured)
    pub fn snapshot_store(&self) -> Option<&Arc<SnapshotStore>> {
        self.snapshot_store.as_ref()
    }

    /// Get backlinks for a file
    ///
    /// M4c (design §6.3, deliverable E): self-flushing — drains any queued
    /// out-of-band commits into the link graph first, so the read reflects
    /// them without the server's `get_vault_pair_with_reindex` wrapper.
    /// A no-op on Direct / an empty queue.
    pub async fn get_backlinks(&self, path: &Path) -> Result<Vec<PathBuf>> {
        self.ensure_fresh().await;
        let vault_path = self.resolve_path(path)?;
        let graph = self.link_graph.read().await;
        let backlinks = graph.backlinks(&vault_path)?;
        Ok(backlinks.into_iter().map(|(p, _)| p).collect())
    }

    /// Get forward links for a file (self-flushing — see `get_backlinks`).
    pub async fn get_forward_links(&self, path: &Path) -> Result<Vec<PathBuf>> {
        self.ensure_fresh().await;
        let vault_path = self.resolve_path(path)?;
        let graph = self.link_graph.read().await;
        let forward_links = graph.forward_links(&vault_path)?;
        Ok(forward_links.into_iter().map(|(p, _)| p).collect())
    }

    /// Get orphaned notes (self-flushing — see `get_backlinks`).
    pub async fn get_orphaned_notes(&self) -> Result<Vec<PathBuf>> {
        self.ensure_fresh().await;
        let graph = self.link_graph.read().await;
        Ok(graph.orphaned_notes())
    }

    /// Get related notes (self-flushing — see `get_backlinks`).
    pub async fn get_related_notes(&self, path: &Path, max_hops: usize) -> Result<Vec<PathBuf>> {
        self.ensure_fresh().await;
        let vault_path = self.resolve_path(path)?;
        let graph = self.link_graph.read().await;
        graph.related_notes(&vault_path, max_hops)
    }

    /// Get graph statistics (self-flushing — see `get_backlinks`).
    pub async fn get_stats(&self) -> Result<turbovault_graph::GraphStats> {
        self.ensure_fresh().await;
        let graph = self.link_graph.read().await;
        Ok(graph.stats())
    }
}

impl Drop for VaultManager {
    /// M4c (design fork #3): abort the git reindex background tasks so they
    /// don't outlive the manager. The drainer holds `Weak<Self>` and self-exits,
    /// but the HEAD-ref listener holds only the queue and is stopped SOLELY by
    /// this abort — so recover the guard even if the lock was poisoned rather
    /// than skip the abort and leak the listener (mirrors `substrate.rs`).
    fn drop(&mut self) {
        let tasks = self
            .reindex_tasks
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for handle in tasks.iter() {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod tests;
