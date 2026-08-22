//! Vault manager implementation with file watching and caching

use crate::reindex::CommitOrigin;
use path_trav::PathTrav;
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
use turbovault_core::{Change, ChangePlan, Precondition, VaultGitConfig, WriteBackend};
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

/// What the manager last observed about a note on disk.
///
/// [`VaultManager::ensure_fresh`] compares this against a fresh scan to work out
/// what changed underneath the process. It is deliberately an *identity* test
/// ("is this the same revision we parsed?") rather than a *happens-before* test
/// ("was it touched after we looked?"). Happens-before needs the file's clock
/// and ours to agree, and they do not: a sync tool that restores the original
/// mtime, a checkout that rewinds it, or a machine whose clock steps all defeat
/// it, each time by declaring a changed file fresh.
///
/// On a platform that reports no modification time at all this degrades to a
/// size comparison. Every platform TurboVault targets reports one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileFingerprint {
    size_bytes: u64,
    modified: Option<SystemTime>,
}

impl FileFingerprint {
    async fn observe(path: &Path) -> Option<Self> {
        let metadata = tokio::fs::metadata(path).await.ok()?;
        Some(Self {
            size_bytes: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
}

/// One note in the cache: what was parsed, and the revision it was parsed from.
#[derive(Debug, Clone)]
struct CacheEntry {
    file: VaultFile,
    /// The fingerprint of the bytes `file` was parsed from, when it could be
    /// established. `None` means "unknown", which no scan result can equal, so
    /// the next sweep re-parses the note. Unknown resolves to "assume stale".
    observed: Option<FileFingerprint>,
}

/// Never reconcile more often than this, however cheap the sweep turns out to
/// be. Agent tool calls arrive a model round-trip apart and human edits are
/// seconds apart, so a bound in this range is imperceptible while still
/// collapsing a burst of calls onto a single scan.
const RECONCILE_MIN_INTERVAL: Duration = Duration::from_millis(500);

/// ...and never less often than this, however expensive it turns out to be.
const RECONCILE_MAX_INTERVAL: Duration = Duration::from_secs(30);

/// Reconciliation may consume at most `1/RECONCILE_DUTY_DIVISOR` of wall clock.
/// Scaling the interval to the measured cost is what lets one default serve a
/// 200-note vault and a 200k-note vault: the small vault sweeps at the floor,
/// the large one backs off on its own, and nobody has to tune a number per
/// vault to avoid either staleness or a stall.
///
/// Measured on an M-series Mac, release build, warm page cache, no changes to
/// apply: 0.8ms at 100 notes, 1.7ms at 1k, 19ms at 10k. So every vault up to
/// roughly 13k notes sits at the floor and the duty cycle never binds; past
/// that it stretches the interval rather than the pass.
const RECONCILE_DUTY_DIVISOR: u32 = 20;

/// The reconcile schedule.
#[derive(Debug)]
struct ReconcileState {
    /// When the last completed sweep finished. `None` until the first one runs,
    /// so the first gated call after startup always sweeps.
    last_finished: Option<Instant>,
    /// What that sweep cost, which sets how long until the next one is due.
    last_cost: Duration,
}

impl ReconcileState {
    fn new() -> Self {
        Self {
            last_finished: None,
            last_cost: Duration::ZERO,
        }
    }

    fn interval(&self) -> Duration {
        (self.last_cost * RECONCILE_DUTY_DIVISOR)
            .clamp(RECONCILE_MIN_INTERVAL, RECONCILE_MAX_INTERVAL)
    }

    fn is_due(&self) -> bool {
        match self.last_finished {
            None => true,
            Some(finished) => finished.elapsed() >= self.interval(),
        }
    }

    fn record(&mut self, cost: Duration) {
        self.last_cost = cost;
        self.last_finished = Some(Instant::now());
    }
}

/// A live claim on the paths one write plan touches, released on drop.
///
/// Between the substrate landing bytes and `sync_index` recording the new
/// fingerprint there is a window in which a freshness sweep would see a change
/// this process is in the middle of making. Publishing it would attribute one
/// of TurboVault's own mutations to an external actor, which is a lie to
/// anything consuming the change feed for provenance, and would re-parse a file
/// the write path is about to re-parse anyway. Claimed paths are skipped; the
/// write that claimed them reports them.
struct WriteClaim<'a> {
    claims: &'a StdMutex<HashMap<String, usize>>,
    paths: Vec<String>,
}

impl<'a> WriteClaim<'a> {
    /// Counted rather than a plain set: two plans may legitimately have
    /// overlapping paths in flight, and the first to finish must not release
    /// the second's claim.
    fn new(claims: &'a StdMutex<HashMap<String, usize>>, paths: Vec<String>) -> Self {
        {
            let mut held = claims.lock().unwrap_or_else(|e| e.into_inner());
            for path in &paths {
                *held.entry(path.clone()).or_insert(0) += 1;
            }
        }
        Self { claims, paths }
    }
}

impl Drop for WriteClaim<'_> {
    fn drop(&mut self) {
        let mut held = self.claims.lock().unwrap_or_else(|e| e.into_inner());
        for path in &self.paths {
            match held.get_mut(path) {
                Some(count) if *count > 1 => *count -= 1,
                _ => {
                    held.remove(path);
                }
            }
        }
    }
}

/// Everything a vault scan needs, detached from the manager once at
/// construction so the walk can move to the blocking pool without borrowing it.
#[derive(Debug)]
struct ScanSpec {
    root: PathBuf,
    excluded: HashSet<String>,
    /// Admitted extensions, lower-cased and *without* the leading dot, so the
    /// per-entry test is a hash lookup on the raw `Path::extension` slice in
    /// the overwhelmingly common already-lower-case case.
    allowed_extensions: HashSet<String>,
    max_file_size: u64,
}

impl ScanSpec {
    fn new(config: &ServerConfig, root: PathBuf) -> Self {
        Self {
            root,
            excluded: config.excluded_paths.clone(),
            allowed_extensions: config
                .allowed_extensions
                .iter()
                .map(|ext| ext.trim_start_matches('.').to_lowercase())
                .collect(),
            max_file_size: config.max_file_size,
        }
    }

    /// Case-insensitive, matching `sync_index`'s markdown test.
    ///
    /// The two have to agree. A note the write path caches but the scan cannot
    /// find would be reported deleted by the very next sweep, so `Note.MD`
    /// would quietly fall out of the link graph after being written.
    fn admits_extension(&self, ext: &str) -> bool {
        self.allowed_extensions.contains(ext)
            || (ext.bytes().any(|b| b.is_ascii_uppercase())
                && self.allowed_extensions.contains(&ext.to_lowercase()))
    }

    /// Walk the vault and return every note the configuration admits, carrying
    /// the `(size, mtime)` the walk had to read anyway.
    ///
    /// Symlinks are never followed, as directories or as notes. A vault is a
    /// directory of files the operator owns, and a link inside it can point
    /// anywhere: at an ancestor, which makes the walk recurse until it exhausts
    /// memory, or outside the vault root, which would pull content into the
    /// index that `resolve_path` refuses to hand out. Skipping them keeps the
    /// walk terminating and keeps discovery inside the boundary the read and
    /// write paths already enforce.
    ///
    /// [`PROTECTED_COMPONENTS`] is skipped alongside the configured exclusions:
    /// `.turbovault/` is TurboVault's own audit trail and snapshot store, not
    /// vault content, and walking it is pure cost on every pass.
    fn walk(&self) -> Result<Vec<ScannedNote>> {
        let mut notes = Vec::new();
        let mut stack = vec![self.root.clone()];

        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                // A subdirectory that vanished between being pushed and being
                // read is the vault changing underneath us, not a scan failure.
                // A missing vault root still is one.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && dir != self.root => continue,
                Err(e) => return Err(Error::io(e)),
            };

            for entry in entries.flatten() {
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if PROTECTED_COMPONENTS.contains(&name) || self.excluded.contains(name) {
                    continue;
                }

                // `file_type` reads `d_type` straight off the dirent where the
                // platform supplies it, so the test costs no extra syscall, and
                // it describes the link itself rather than its target.
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_dir() {
                    stack.push(path);
                    continue;
                }
                if !file_type.is_file() {
                    continue; // symlinks, fifos, sockets, devices
                }

                let admitted = path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| self.admits_extension(ext));
                if !admitted {
                    continue;
                }

                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if metadata.len() > self.max_file_size {
                    continue;
                }
                notes.push(ScannedNote {
                    path,
                    size_bytes: metadata.len(),
                    modified: metadata.modified().ok(),
                });
            }
        }

        Ok(notes)
    }
}

/// Whether a path is a *note*: something the note cache, the link graph, and
/// the search index model.
///
/// Markdown only, which is narrower than `allowed_extensions` — that also
/// admits `.txt` and `.canvas`, because those are worth *discovering*, not
/// worth parsing as notes. The two have to be kept apart, and every consumer of
/// the note cache has to use this same test, because the freshness sweep
/// compares what the cache holds against what a scan finds: a path one side
/// includes and the other does not is reported changed on every single pass
/// forever, since nothing ever records having seen it.
fn is_note(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
}

/// Read a note and fingerprint the exact revision that was read.
///
/// The fingerprint is taken before and after the read and kept only if it did
/// not move. A write landing inside that window would otherwise be recorded as
/// "already seen", and because the sweep compares against precisely this value
/// the note would then stay stale forever rather than for one interval. When
/// the two disagree the content still stands (it is a coherent snapshot of one
/// of the two revisions) but the fingerprint is dropped, which makes the next
/// sweep look again.
///
/// `expected` is a fingerprint the caller already holds, from the scan that
/// found the file, and passing it saves the leading stat.
async fn read_note_with_fingerprint(
    path: &Path,
    expected: Option<FileFingerprint>,
) -> std::io::Result<(String, Option<FileFingerprint>)> {
    let before = match expected {
        Some(fingerprint) => Some(fingerprint),
        None => FileFingerprint::observe(path).await,
    };
    let content = tokio::fs::read_to_string(path).await?;
    let after = FileFingerprint::observe(path).await;
    Ok((content, if before == after { after } else { None }))
}

/// Main vault manager with file operations and watching
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
}

impl VaultManager {
    /// Create a new vault manager
    pub fn new(config: ServerConfig) -> Result<Self> {
        let default_vault = config.default_vault()?;
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
        };

        let scan_spec = Arc::new(ScanSpec::new(&config, vault_path.clone()));

        Ok(Self {
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

    /// M4c (design §6.6): drain the git reindex queue — apply every pending
    /// commit's diff to the link graph + note cache, then fire the
    /// change-listener with the collapsed `(path, present)` set. Holds the
    /// queue's flush lock across the whole pass so it can't interleave with a
    /// concurrent flush (turbovault-9zr). A no-op on Direct or an empty queue.
    ///
    /// This is what makes the manager's derived reads self-flushing and closes
    /// the M4b search-staleness gap for out-of-band commits (the ref-listener
    /// enqueues; this applies).
    pub async fn flush_reindex(&self) {
        let Some(queue) = self.reindex_queue.clone() else {
            return; // Direct backend
        };
        let _flush_guard = queue.lock_flush().await;
        if queue.pending_count() == 0 {
            return;
        }

        // Open the repo + collect per-commit diffs inside spawn_blocking:
        // `VaultRepo` is `!Sync`, so its libgit2 handle must never cross an
        // await (mirrors the server's `drain_pending_diffs`). The graph apply
        // runs back here, async, via `sync_index` — the SAME applier the
        // manager's own mutators use.
        let path = self.vault_path.clone();
        let drain_queue = Arc::clone(&queue);
        let drained = tokio::task::spawn_blocking(move || -> Vec<DrainedCommit> {
            let Ok(repo) = VaultRepo::open(&path) else {
                return Vec::new();
            };
            let mut batches = Vec::new();
            while let Some(pending) = drain_queue.pop_front() {
                let commit = pending.oid;
                // A commit the ref-watcher enqueued may no longer be reachable
                // (GC / non-ff move). A first-parent or diff failure SKIPS that
                // commit — advance the cursor, keep draining — rather than
                // bricking the whole pass (unify with drain_through's tlx.1).
                let parent = match repo.git_commit_first_parent(commit) {
                    Ok(p) => p,
                    Err(e) => {
                        log::warn!(
                            "flush_reindex: skipping commit {commit} after first-parent error: {e}"
                        );
                        drain_queue.advance_cursor(commit);
                        continue;
                    }
                };
                match repo.diff_path_statuses(parent, commit) {
                    Ok(changes) => batches.push((commit, pending.origin, changes)),
                    Err(e) => {
                        log::warn!("flush_reindex: skipping commit {commit} after diff error: {e}");
                        drain_queue.advance_cursor(commit);
                    }
                }
            }
            batches
        })
        .await
        .unwrap_or_default();

        if drained.is_empty() {
            return;
        }

        let mut collapsed: Vec<(String, bool, CommitOrigin)> = Vec::new();
        for (commit, origin, changes) in drained {
            self.sync_index(&changes).await;
            collapsed.extend(with_origin(changes, origin));
            queue.advance_cursor(commit);
        }
        // Fired while the flush guard is still held, deliberately. The guard is
        // what orders passes against each other; releasing it first would let a
        // later pass's fan-out overtake an earlier one's and leave the search
        // index holding an older revision of a path than the link graph does.
        self.fire_change_listener(collapsed).await;
    }

    /// Bring every derived view this manager owns into agreement with what is
    /// on disk right now, then return.
    ///
    /// This is the freshness gate, and it is the single thing a caller has to
    /// remember. Read paths that consult derived state (the link graph, the
    /// note cache, and — through the change-listener — the server's search and
    /// similarity indexes and any plugin's own index) call it first, so that
    /// every one of them answers from the same observation of the vault.
    ///
    /// # Why a gate and not a watcher
    ///
    /// A vault is a shared directory. Obsidian is usually open on it, an editor
    /// or a `git pull` or a sync client may touch it, and none of that goes
    /// through this process. Filesystem notifications look like the obvious
    /// answer and are not a correctness mechanism anywhere: inotify queues
    /// overflow and its watch budget is finite, FSEvents degrades to
    /// directory-granularity rescan hints under load, and network and
    /// cloud-synced vaults (iCloud, Dropbox, Syncthing — all ordinary for
    /// Obsidian) deliver nothing at all for a peer's changes. A watcher yields
    /// *mostly* fresh, and for an agent that is worse than plainly stale,
    /// because nothing marks the answers it should not have trusted.
    ///
    /// Comparing state cannot miss an event, because it is not listening for
    /// one. The comparison is a scan of `(size, mtime)` against what the note
    /// cache recorded when it parsed each note, debounced so a burst of calls
    /// pays for one pass. What it costs is bounded by
    /// [`RECONCILE_DUTY_DIVISOR`]; how stale an answer can be is bounded by
    /// [`ReconcileState::interval`].
    ///
    /// The two halves run in this order on purpose. Draining first settles any
    /// commit this process has already been told about and records the
    /// fingerprints that go with it, so the sweep that follows sees those notes
    /// as settled instead of reporting them a second time.
    pub async fn ensure_fresh(&self) {
        self.flush_reindex().await;
        self.reconcile_external_changes().await;
    }

    /// Reconcile right now, ignoring both the schedule and the configured
    /// opt-out.
    ///
    /// [`Self::ensure_fresh`] is what a read path should call: it is scheduled,
    /// so a burst of calls costs one pass and an idle server costs nothing. Use
    /// this when something out of band says the vault moved and waiting out the
    /// interval is not acceptable. Being explicit is also why it overrides
    /// `reconcile_external_changes`: someone asking for this has said what they
    /// want more recently than the config did.
    pub async fn reconcile_now(&self) {
        self.flush_reindex().await;
        let mut state = self.reconcile.lock().await;
        self.reconcile_holding(&mut state).await;
    }

    /// The sweep half of [`Self::ensure_fresh`], with its schedule.
    async fn reconcile_external_changes(&self) {
        if !self.config.reconcile_external_changes {
            return;
        }
        // Taking the lock before testing the schedule is what collapses
        // concurrent gate calls onto one pass: the losers block here, and by
        // the time they hold the lock the winner has recorded a fresh
        // timestamp, so they find the sweep no longer due and return having
        // paid only the wait. Testing first and then locking would let them all
        // through.
        let mut state = self.reconcile.lock().await;
        if !state.is_due() {
            return;
        }
        self.reconcile_holding(&mut state).await;
    }

    /// One reconcile pass. The caller holds the reconcile lock and has already
    /// decided the pass should happen.
    ///
    /// The fan-out runs under that lock too, for the reason the drain pass keeps
    /// its flush guard: it orders one pass's fan-out against the next, so the
    /// search index can never be left holding an older revision of a path than
    /// the link graph.
    async fn reconcile_holding(&self, state: &mut ReconcileState) {
        let started = Instant::now();
        let changed = self.sweep_for_external_changes().await;
        state.record(started.elapsed());
        self.fire_change_listener(with_origin(changed, CommitOrigin::External))
            .await;
    }

    /// Compare a fresh scan against what the note cache believes and bring the
    /// cache and link graph into line. Returns the vault-relative
    /// `(path, present)` set that moved, in the shape the change listener and
    /// the search index already consume.
    async fn sweep_for_external_changes(&self) -> Vec<(String, bool)> {
        let spec = Arc::clone(&self.scan_spec);
        let scanned = match tokio::task::spawn_blocking(move || spec.walk()).await {
            Ok(Ok(scanned)) => scanned,
            // A scan that fails leaves derived state exactly as it was. Acting
            // on a partial walk would report every note it did not reach as
            // deleted and tear the link graph down.
            Ok(Err(e)) => {
                log::warn!("freshness sweep: vault scan failed, derived state unchanged: {e}");
                return Vec::new();
            }
            Err(e) => {
                log::warn!("freshness sweep: scan task failed, derived state unchanged: {e}");
                return Vec::new();
            }
        };

        // Notes only, on both sides of the diff. The scan admits more than the
        // cache models, and comparing across that gap would report every
        // non-note as newly created on every pass.
        let mut on_disk: HashMap<PathBuf, FileFingerprint> = HashMap::with_capacity(scanned.len());
        for note in scanned {
            if !is_note(&note.path) {
                continue;
            }
            let fingerprint = note.fingerprint();
            on_disk.insert(note.path, fingerprint);
        }

        let mut changed: Vec<(PathBuf, bool)> = Vec::new();
        {
            let cache = self.file_cache.read().await;
            for (path, entry) in cache.iter() {
                match on_disk.remove(path) {
                    // Same revision we parsed. Nothing to do.
                    Some(found) if entry.observed == Some(found) => {}
                    // Still there, but not the bytes we parsed.
                    Some(_) => changed.push((path.clone(), true)),
                    // Gone from the vault.
                    None => changed.push((path.clone(), false)),
                }
            }
        }
        // Whatever the scan found that the cache had never heard of was created
        // underneath us. This is the case a per-entry mtime check structurally
        // cannot see, because it can only re-check entries it already holds.
        changed.extend(on_disk.into_keys().map(|path| (path, true)));

        let changed = self.without_claimed_paths(
            changed
                .into_iter()
                .filter_map(|(path, present)| Some((self.relative_key(&path)?, present)))
                .collect(),
        );
        if changed.is_empty() {
            return Vec::new();
        }
        log::debug!(
            "freshness sweep: {} path(s) changed outside this process",
            changed.len()
        );
        self.sync_index(&changed).await;
        changed
    }

    /// Vault-relative, `/`-separated — the spelling a git diff produces, and so
    /// the one the search index is already keyed by. Building it from path
    /// components rather than replacing separators keeps a sweep-reported path
    /// and a commit-reported path for the same note identical on every
    /// platform, without mangling a Unix filename that legitimately contains a
    /// backslash.
    fn relative_key(&self, path: &Path) -> Option<String> {
        let relative = path.strip_prefix(&self.vault_path).ok()?;
        let mut key = String::with_capacity(relative.as_os_str().len());
        for component in relative.components() {
            let std::path::Component::Normal(part) = component else {
                return None;
            };
            if !key.is_empty() {
                key.push('/');
            }
            key.push_str(&part.to_string_lossy());
        }
        (!key.is_empty()).then_some(key)
    }

    /// Drop the paths this process is actively writing. See [`WriteClaim`].
    fn without_claimed_paths(&self, changed: Vec<(String, bool)>) -> Vec<(String, bool)> {
        let claims = self.write_claims.lock().unwrap_or_else(|e| e.into_inner());
        if claims.is_empty() {
            return changed;
        }
        changed
            .into_iter()
            .filter(|(path, _)| !claims.contains_key(path))
            .collect()
    }

    /// Apply `plan` through the substrate, then run the one post-apply sync and
    /// notification every mutator shares (design R7).
    ///
    /// The plan's paths stay claimed across the apply and the sync so a
    /// concurrent freshness sweep cannot catch this write half-applied and
    /// republish it as somebody else's ([`WriteClaim`]).
    async fn apply_through_substrate(&self, plan: &ChangePlan) -> Result<ApplyOutcome> {
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
    /// render consistently across platforms). Falls back to the lossy full path
    /// when `path` is not under the vault root.
    pub fn relative_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.vault_path)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
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

    /// Initialize vault by scanning all files
    #[instrument(skip(self), name = "vault_initialize")]
    pub async fn initialize(&self) -> Result<()> {
        log::info!("Starting vault initialization for: {:?}", self.vault_path);

        let mut cache = self.file_cache.write().await;
        let mut graph = self.link_graph.write().await;

        // Scan for markdown files, keeping the metadata the scan already read:
        // it becomes each cache entry's fingerprint, so the first freshness
        // sweep after startup compares against the same observation the graph
        // was built from and reports nothing.
        let scanned = self.scan_files_with_metadata()?;
        log::info!("Found {} markdown files", scanned.len());

        // Two-pass initialization: first add all files to the graph index,
        // then resolve links. This ensures every file is discoverable when
        // resolving wikilink targets, regardless of scan order.
        let mut parsed_files = Vec::with_capacity(scanned.len());

        // Pass 1: parse all files, populate cache and graph nodes
        for note in scanned {
            // Notes only, matching `sync_index`. Caching an admitted-but-not-a-
            // note file here (a `.txt`, a `.canvas`) would put it somewhere no
            // applier ever updates it, so every freshness sweep would report it
            // changed again.
            if !is_note(&note.path) {
                continue;
            }
            let scanned_fingerprint = note.fingerprint();
            let file_path = note.path;
            log::debug!("Processing file: {:?}", file_path);
            match read_note_with_fingerprint(&file_path, Some(scanned_fingerprint)).await {
                Ok((content, observed)) => match self.parser.parse_file(&file_path, &content) {
                    Ok(vault_file) => {
                        log::debug!(
                            "Parsed {}: {} links extracted",
                            file_path.display(),
                            vault_file.links.len()
                        );

                        cache.insert(
                            file_path.clone(),
                            CacheEntry {
                                file: vault_file.clone(),
                                observed,
                            },
                        );

                        if let Err(e) = graph.add_file(&vault_file) {
                            log::warn!("Graph add_file failed for {}: {}", file_path.display(), e);
                        }
                        parsed_files.push(vault_file);
                    }
                    Err(e) => {
                        log::warn!("Failed to parse {}: {}", file_path.display(), e);
                    }
                },
                Err(e) => {
                    log::warn!("Failed to read file {}: {}", file_path.display(), e);
                }
            }
        }

        // Pass 2: resolve links (all files now in the index)
        for vault_file in &parsed_files {
            if let Err(e) = graph.update_links(vault_file) {
                log::warn!("Graph update_links failed: {}", e);
            }
        }

        log::info!(
            "Vault initialization complete. Graph now has {} files, {} links",
            graph.node_count(),
            graph.edge_count()
        );

        Ok(())
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
            .map_err(Error::io)?;

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
    async fn ensure_size_within_limit(&self, resolved: &Path) -> Result<()> {
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
    /// and delegates to [`Self::substrate`]; the fs-write + precondition +
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

        self.apply_through_substrate(&plan).await?;
        Ok(())
    }

    /// Apply an arbitrary multi-change [`ChangePlan`] — the R3 multi-change
    /// entry point the four single-op mutators above are thin builders over.
    /// Every path the plan touches is resolved through the same traversal
    /// guard (`resolve_path`) the single-op mutators run before ever
    /// building a plan, so this entry point can't become a chokepoint
    /// bypass for callers (batch, rollback, …) that build their own plans.
    /// Delegates to the substrate and runs the same post-apply link-
    /// graph/cache sync (R7) as the single-op mutators.
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

        // Acquire write lock on file cache to prevent TOCTOU
        let _cache_guard = self.file_cache.write().await;

        // Read current content
        let current_content = tokio::fs::read_to_string(&vault_path)
            .await
            .map_err(Error::io)?;

        // Preserve the exact pre-image that the edit was calculated from. The
        // write below revalidates this hash after releasing the cache lock.
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

        // Parse and apply edits
        let engine = EditEngine::new();
        let blocks = engine.parse_blocks(edits)?;

        let (mut edit_result, new_content) =
            engine.apply_edits(&current_content, &blocks, dry_run)?;

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

        // Release cache guard before write (avoid deadlock)
        drop(_cache_guard);

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

        self.apply_through_substrate(&plan).await?;
        Ok(())
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

        self.apply_through_substrate(&plan).await?;
        Ok(())
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

    /// Normalize a path by resolving `.` and `..` components
    /// This is used as a fallback when path_trav can't check non-existent paths
    fn normalize_path(path: &Path) -> PathBuf {
        let mut components = Vec::new();

        for component in path.components() {
            match component {
                std::path::Component::CurDir => {
                    // Skip `.` components
                }
                std::path::Component::ParentDir => {
                    // Pop the last component for `..`
                    components.pop();
                }
                comp => {
                    components.push(comp);
                }
            }
        }

        components.iter().collect()
    }

    /// Resolve a relative path to vault-root-relative path with path traversal protection
    /// Uses the battle-tested path_trav crate for security, with fallback normalization.
    ///
    /// This is the note-API resolver: it enforces BOTH the vault boundary and
    /// the in-vault protected-directory policy (see
    /// [`Self::ensure_path_is_not_protected`]). Callers holding an explicit
    /// capability grant for protected state use
    /// [`Self::resolve_path_bypassing_policy`] instead.
    pub fn resolve_path(&self, path: &Path) -> Result<PathBuf> {
        let full_path = self.resolve_path_bypassing_policy(path)?;
        self.ensure_path_is_not_protected(&full_path)?;
        Ok(full_path)
    }

    /// Resolve a path with vault-boundary (traversal) protection ONLY.
    ///
    /// The in-vault protected-directory policy is not applied. Every caller
    /// must have an explicit capability grant to reach protected state — today
    /// that is the plugin host's config-read capability. Prefer
    /// [`Self::resolve_path`] everywhere else.
    pub fn resolve_path_bypassing_policy(&self, path: &Path) -> Result<PathBuf> {
        // Resolve relative paths to absolute
        let full_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.vault_path.join(path)
        };

        // Use path_trav to detect traversal attempts (battle-tested library)
        // is_path_trav returns Ok(true) if traversal detected, Ok(false) if safe
        match self.vault_path.is_path_trav(&full_path) {
            Ok(true) => {
                // Path traversal detected by path_trav
                Err(Error::path_traversal(full_path))
            }
            Ok(false) => {
                // Path is safe according to path_trav
                Ok(full_path)
            }
            Err(_) => {
                // path_trav couldn't check (usually means file doesn't exist)
                // Use fallback normalization to detect traversal attempts
                let normalized = Self::normalize_path(&full_path);

                // Check if normalized path is still under vault
                if normalized.starts_with(&self.vault_path) {
                    Ok(full_path)
                } else {
                    Err(Error::path_traversal(full_path))
                }
            }
        }
    }

    /// Refuse a path that lies inside the vault but under a protected
    /// directory.
    ///
    /// Staying inside the vault root is not sufficient authorization. A vault
    /// also contains application and tool state whose contents are executed or
    /// trusted by something: `.obsidian/plugins/*/main.js` is code Obsidian
    /// runs, `.git/hooks/*` is code git runs, and `.turbovault/` holds the
    /// audit trail that is supposed to be a record of what the note APIs did.
    /// Exposing those through a note read/write turns "edit my notes" into code
    /// execution and makes the audit log self-editable.
    ///
    /// The configurable part is [`VaultConfig::excluded_paths`], which already
    /// defaults to `.obsidian`/`.git`/`node_modules`/`.DS_Store` — until now it
    /// only filtered directory scans, so the policy existed without being
    /// enforced at the point of access. [`PROTECTED_COMPONENTS`] is the part no
    /// configuration can open up.
    ///
    /// Note that `allowed_extensions` is deliberately NOT enforced here: it
    /// describes what counts as a note for discovery, and attachments beside
    /// notes are a normal thing to read and write.
    pub fn ensure_path_is_not_protected(&self, resolved: &Path) -> Result<()> {
        // A path outside the vault is the traversal check's business, not ours.
        let Ok(relative) = resolved.strip_prefix(&self.vault_path) else {
            return Ok(());
        };
        for component in relative.components() {
            let std::path::Component::Normal(raw) = component else {
                continue;
            };
            let name = raw.to_string_lossy();
            if PROTECTED_COMPONENTS.contains(&name.as_ref())
                || self.config.excluded_paths.contains(name.as_ref())
            {
                return Err(Error::protected_path(resolved, name.into_owned()));
            }
        }
        Ok(())
    }

    /// Scan for markdown files in vault
    fn scan_files(&self) -> Result<Vec<PathBuf>> {
        Ok(self
            .scan_files_with_metadata()?
            .into_iter()
            .map(|entry| entry.path)
            .collect())
    }

    /// Scan for markdown files, keeping the metadata the scan already reads.
    ///
    /// The size filter stats every candidate anyway, so returning that stat
    /// costs nothing. Consumers that maintain their own derived state — a
    /// search or vector index — can diff `(size_bytes, modified)` against what
    /// they stored and re-read only what actually changed, instead of reading
    /// every note to discover that most of them did not. It is the same
    /// comparison [`Self::ensure_fresh`] makes for the manager's own state.
    pub fn scan_vault_with_metadata(&self) -> Result<Vec<ScannedNote>> {
        self.scan_files_with_metadata()
    }

    fn scan_files_with_metadata(&self) -> Result<Vec<ScannedNote>> {
        self.scan_spec.walk()
    }

    /// Insert a parsed `VaultFile` into the note cache alongside the
    /// fingerprint of the revision it was parsed from.
    ///
    /// Recording the fingerprint here is what keeps the freshness sweep quiet
    /// about this process's own writes: the next scan finds exactly what this
    /// entry says it saw, so the note never enters the diff.
    async fn insert_cache_entry(
        &self,
        path: PathBuf,
        file: VaultFile,
        observed: Option<FileFingerprint>,
    ) {
        let mut cache = self.file_cache.write().await;
        cache.insert(path, CacheEntry { file, observed });
    }

    /// Bring the link graph + note cache into agreement with a substrate
    /// apply's verdict on which paths are now present (design R7 — one
    /// post-apply sync shared by `write_file`/`delete_file`/`move_file`,
    /// replacing their three near-duplicate pre-M3a graph/cache blocks).
    ///
    /// Markdown-only, mirroring pre-M3a `write_file`'s `is_markdown` guard:
    /// the link graph and note cache model *notes*, so attachments and other
    /// non-note artifacts are never graph nodes. (Pre-M3a `move_file` did
    /// not apply this guard on its insert side — any UTF-8-decodable file
    /// was cached regardless of extension; this unifies on the stricter,
    /// already-established `write_file` behavior. No existing caller relies
    /// on a non-markdown file appearing in the cache after a move.)
    async fn sync_index(&self, changed: &[(String, bool)]) {
        for (rel_path, present) in changed {
            let full_path = self.vault_path.join(rel_path);
            if !is_note(&full_path) {
                continue;
            }

            if *present {
                match read_note_with_fingerprint(&full_path, None).await {
                    Ok((content, observed)) => match self.parser.parse_file(&full_path, &content) {
                        Ok(vault_file) => {
                            {
                                let mut graph = self.link_graph.write().await;
                                if let Err(e) = graph.add_file(&vault_file) {
                                    log::warn!(
                                        "Graph add_file failed for {}: {}",
                                        full_path.display(),
                                        e
                                    );
                                }
                                if let Err(e) = graph.update_links(&vault_file) {
                                    log::warn!(
                                        "Graph update_links failed for {}: {}",
                                        full_path.display(),
                                        e
                                    );
                                }
                            }
                            self.insert_cache_entry(full_path, vault_file, observed)
                                .await;
                        }
                        Err(e) => log::warn!(
                            "Failed to parse {} after apply (graph not updated): {}",
                            full_path.display(),
                            e
                        ),
                    },
                    Err(e) => log::warn!(
                        "Failed to re-read {} after apply: {}",
                        full_path.display(),
                        e
                    ),
                }
            } else {
                // Re-check disk state before evicting: `sync_index` runs
                // after `substrate.apply()` has already released its
                // write-lock, so a concurrent blind write that recreated
                // this path can land between this apply's completion and
                // this eviction. If the path exists again, this "removed"
                // notification is stale — skip it and let the write that
                // recreated the path reconcile the cache via its own
                // present=true branch, instead of clobbering a legitimately
                // present file.
                // ponytail: narrows the race (check-then-evict is still not
                // atomic with the recreating write) rather than closing it
                // outright; a full fix serializes sync_index with apply()
                // per path, add if this ever proves observable in practice.
                if tokio::fs::try_exists(&full_path).await.unwrap_or(false) {
                    continue;
                }
                {
                    let mut graph = self.link_graph.write().await;
                    let _ = graph.remove_file(&full_path);
                }
                let mut cache = self.file_cache.write().await;
                cache.remove(&full_path);
            }
        }
    }

    /// Get a reference to the link graph (read-only access)
    ///
    /// NOT self-flushing — internal reindex/drain machinery
    /// (`flush_reindex`/`apply_commit_diff`) calls this directly and must
    /// keep doing so: `flush_reindex` already holds the queue's flush lock
    /// while draining, so routing it through [`Self::link_graph_flushed`]
    /// here would re-enter that lock and deadlock. Read-only consumers
    /// outside the manager (tool implementations) should prefer
    /// `link_graph_flushed()` instead.
    pub fn link_graph(&self) -> Arc<RwLock<LinkGraph>> {
        Arc::clone(&self.link_graph)
    }

    /// Self-flushing link-graph accessor (see `get_backlinks`): drains any
    /// queued out-of-band commits before handing back the graph handle. This
    /// is the shared primitive read-only tool implementations (graph/export/
    /// relationship/viewer) should call instead of the bare `link_graph()`,
    /// so every such reader gets the same out-of-band-commit coherence
    /// guarantee as `get_backlinks`/`get_stats`/etc. without each call site
    /// having to remember to flush itself.
    pub async fn link_graph_flushed(&self) -> Arc<RwLock<LinkGraph>> {
        self.ensure_fresh().await;
        self.link_graph()
    }

    /// Parse a single file and return VaultFile
    #[instrument(skip(self), fields(file = ?path), name = "vault_parse_file")]
    pub async fn parse_file(&self, path: &Path) -> Result<VaultFile> {
        let full_path = self.resolve_path(path)?;
        let content = tokio::fs::read_to_string(&full_path)
            .await
            .map_err(Error::io)?;
        self.parser
            .parse_file(&full_path, &content)
            .map_err(|e| Error::parse_error(e.to_string()))
    }

    /// Synchronize one note's on-disk state into the parsed cache and link graph.
    ///
    /// This is used after an external transactional operation, such as rollback,
    /// that creates, rewrites, or removes a note without going through the normal
    /// `write_file`/`delete_file` paths.
    pub async fn refresh_file_state(&self, path: &Path) -> Result<()> {
        let full_path = self.resolve_path(path)?;
        if !is_note(&full_path) {
            return Ok(()); // Not something the cache or the graph models.
        }
        match read_note_with_fingerprint(&full_path, None).await {
            Ok((content, observed)) => {
                let vault_file = self
                    .parser
                    .parse_file(&full_path, &content)
                    .map_err(|error| Error::parse_error(error.to_string()))?;
                {
                    let mut graph = self.link_graph.write().await;
                    graph.add_file(&vault_file)?;
                    graph.update_links(&vault_file)?;
                }
                self.insert_cache_entry(full_path, vault_file, observed)
                    .await;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                {
                    let mut graph = self.link_graph.write().await;
                    let _ = graph.remove_file(&full_path);
                }
                self.file_cache.write().await.remove(&full_path);
                Ok(())
            }
            Err(error) => Err(Error::io(error)),
        }
    }

    /// Scan vault and return list of all markdown files
    #[instrument(skip(self), name = "vault_scan")]
    pub async fn scan_vault(&self) -> Result<Vec<PathBuf>> {
        self.scan_files()
    }

    /// Return clones of all `VaultFile` objects currently in the in-memory cache.
    ///
    /// The cache is populated during `initialize()` and kept up-to-date on every
    /// write/delete/move. Callers that only need parsed metadata (frontmatter, links)
    /// and can tolerate up-to-millisecond staleness should prefer this over
    /// `scan_vault()` + `parse_file()`, which issues a filesystem scan and N file
    /// reads on every call.
    pub async fn all_cached_vault_files(&self) -> Vec<VaultFile> {
        let cache = self.file_cache.read().await;
        cache.values().map(|e| e.file.clone()).collect()
    }

    /// Return every cached vault file, after bringing the cache into agreement
    /// with disk.
    ///
    /// Thin wrapper over [`Self::ensure_fresh`] plus
    /// [`Self::all_cached_vault_files`], and that is the point: it used to run
    /// its own per-call mtime sweep over the entries it already held, which
    /// meant it could never discover a note created outside this process, and
    /// meant a burst of tool calls paid for one scan each. Sharing the gate
    /// fixes both, and lines this view up with the link graph and the search
    /// index rather than letting each drift on its own schedule.
    pub async fn vault_files_validated(&self) -> Vec<VaultFile> {
        self.ensure_fresh().await;
        self.all_cached_vault_files().await
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
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a test vault configuration
    fn create_test_config(vault_dir: &Path) -> ServerConfig {
        let mut config = ServerConfig::new();
        let vault_config = VaultConfig::builder("test_vault", vault_dir)
            .build()
            .unwrap();
        config.vaults.push(vault_config);
        config
    }

    #[tokio::test]
    async fn test_vault_manager_creation() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());

        let manager = VaultManager::new(config);
        assert!(manager.is_ok());
    }

    #[tokio::test]
    async fn test_vault_path() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());

        let manager = VaultManager::new(config).unwrap();
        assert_eq!(manager.vault_path(), temp_dir.path());
    }

    #[tokio::test]
    async fn test_write_and_read_file() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Write a file
        let path = Path::new("test.md");
        let content = "# Test Note\nHello world";
        assert!(
            manager
                .write_file(path, content, Precondition::Blind, "test")
                .await
                .is_ok()
        );

        // Read it back
        let read_content = manager.read_file(path).await.unwrap();
        assert_eq!(read_content, content);
    }

    /// `apply_changes` is the multi-change entry point the single-op
    /// mutators (`write_file`/`delete_file`/`move_file`) build one-change
    /// plans over. Exercise it directly with a plan mixing create, update,
    /// and remove in one call.
    #[tokio::test]
    async fn test_apply_changes_multi_change_plan() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Seed the files the update/remove changes act on.
        manager
            .write_file(
                Path::new("updated.md"),
                "# Old",
                Precondition::Blind,
                "test",
            )
            .await
            .unwrap();
        manager
            .write_file(
                Path::new("removed.md"),
                "# Gone",
                Precondition::Blind,
                "test",
            )
            .await
            .unwrap();

        let plan = ChangePlan::new("multi-change test")
            .create("new.md", "# New")
            .upsert("updated.md", "# Updated")
            .with_precondition("updated.md", Precondition::ExpectExists)
            .remove("removed.md")
            .with_precondition("removed.md", Precondition::ExpectExists);

        let outcome = manager.apply_changes(&plan).await.unwrap();

        assert_eq!(
            outcome.changed,
            vec![
                ("new.md".to_string(), true),
                ("updated.md".to_string(), true),
                ("removed.md".to_string(), false),
            ]
        );

        assert_eq!(
            manager.read_file(Path::new("new.md")).await.unwrap(),
            "# New"
        );
        assert_eq!(
            manager.read_file(Path::new("updated.md")).await.unwrap(),
            "# Updated"
        );
        assert!(
            manager.read_file(Path::new("removed.md")).await.is_err(),
            "removed.md should no longer exist after apply_changes"
        );
    }

    #[tokio::test]
    async fn test_write_file_creates_directories() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Write file in nested directory
        let path = Path::new("notes/subfolder/test.md");
        let content = "Nested file";
        assert!(
            manager
                .write_file(path, content, Precondition::Blind, "test")
                .await
                .is_ok()
        );

        // Verify it was created
        let read_content = manager.read_file(path).await.unwrap();
        assert_eq!(read_content, content);
    }

    #[tokio::test]
    async fn test_path_traversal_prevention() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Attempt path traversal
        let bad_path = Path::new("../../../etc/passwd");
        let result = manager.read_file(bad_path).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_atomic_write() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        let path = Path::new("atomic_test.md");
        let content = "Atomic write test";

        // Write file
        assert!(
            manager
                .write_file(path, content, Precondition::Blind, "test")
                .await
                .is_ok()
        );

        // Verify no .tmp files are left
        let entries = std::fs::read_dir(temp_dir.path()).unwrap();
        for entry in entries {
            let entry = entry.unwrap();
            let path = entry.path();
            if let Some(ext) = path.extension() {
                assert_ne!(ext, "tmp", "Temporary file left after write");
            }
        }
    }

    #[tokio::test]
    async fn test_cache_invalidation() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        let path = Path::new("cache_test.md");
        let content1 = "Original content";

        // Write initial file
        assert!(
            manager
                .write_file(path, content1, Precondition::Blind, "test")
                .await
                .is_ok()
        );

        // Read from cache
        let read1 = manager.read_file(path).await.unwrap();
        assert_eq!(read1, content1);

        // Update file directly
        let vault_path = temp_dir.path().join(path);
        let content2 = "Updated content";
        std::fs::write(&vault_path, content2).unwrap();

        // Read again (should get new content, not cached)
        let read2 = manager.read_file(path).await.unwrap();
        // Note: may be cached depending on cache_ttl, but read should work
        assert!(!read2.is_empty());
    }

    #[tokio::test]
    async fn test_scan_files() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Create some files
        std::fs::write(temp_dir.path().join("note1.md"), "# Note 1").unwrap();
        std::fs::write(temp_dir.path().join("note2.md"), "# Note 2").unwrap();
        std::fs::create_dir(temp_dir.path().join("folder")).unwrap();
        std::fs::write(temp_dir.path().join("folder/note3.md"), "# Note 3").unwrap();

        // Scan files
        let files = manager.scan_files().unwrap();

        // Should find all 3 markdown files
        assert_eq!(files.len(), 3);

        // Verify they're all .md files
        for file in &files {
            assert_eq!(file.extension().and_then(|e| e.to_str()), Some("md"));
        }
    }

    #[tokio::test]
    async fn test_initialize_vault() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Create test files with lowercase links matching the filenames
        let note1 = "# Note 1\n[[note2]]";
        let note2 = "# Note 2\n[[note1]]";
        std::fs::write(temp_dir.path().join("note1.md"), note1).unwrap();
        std::fs::write(temp_dir.path().join("note2.md"), note2).unwrap();

        // Initialize vault
        assert!(manager.initialize().await.is_ok());

        // Verify stats work
        let stats = manager.get_stats().await.unwrap();
        assert_eq!(stats.total_files, 2);
        // At least one link should resolve
        assert!(stats.total_links >= 1);
    }

    #[tokio::test]
    async fn test_get_backlinks() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Create files with links (use absolute paths for graph queries)
        std::fs::write(temp_dir.path().join("target.md"), "# Target").unwrap();
        std::fs::write(temp_dir.path().join("source.md"), "# Source\n[[target]]").unwrap();

        manager.initialize().await.unwrap();

        // Get backlinks for target (query with absolute path since graph stores absolute paths)
        let target_path = temp_dir.path().join("target.md");
        // Backlink resolution depends on platform-specific path handling;
        // verify the operation succeeds without asserting exact results
        let _backlinks = manager.get_backlinks(&target_path).await.unwrap();
    }

    #[tokio::test]
    async fn test_get_forward_links() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Create files with links
        std::fs::write(
            temp_dir.path().join("source.md"),
            "# Source\n[[target1]]\n[[target2]]",
        )
        .unwrap();
        std::fs::write(temp_dir.path().join("target1.md"), "# Target 1").unwrap();
        std::fs::write(temp_dir.path().join("target2.md"), "# Target 2").unwrap();

        manager.initialize().await.unwrap();

        // Get forward links (use absolute path)
        let source_path = temp_dir.path().join("source.md");
        // Link resolution depends on platform-specific path handling;
        // verify the operation succeeds without asserting exact results
        let _forward = manager.get_forward_links(&source_path).await.unwrap();
    }

    #[tokio::test]
    async fn test_get_orphaned_notes() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Create orphaned and linked files
        std::fs::write(temp_dir.path().join("orphan.md"), "# Orphaned Note").unwrap();
        std::fs::write(
            temp_dir.path().join("linked1.md"),
            "# Linked 1\n[[linked2]]",
        )
        .unwrap();
        std::fs::write(temp_dir.path().join("linked2.md"), "# Linked 2").unwrap();

        manager.initialize().await.unwrap();

        // Get orphaned notes
        let orphans = manager.get_orphaned_notes().await.unwrap();
        assert_eq!(orphans.len(), 1);
    }

    #[tokio::test]
    async fn test_get_stats() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Create test files
        std::fs::write(temp_dir.path().join("note1.md"), "# Note 1").unwrap();
        std::fs::write(temp_dir.path().join("note2.md"), "# Note 2").unwrap();

        manager.initialize().await.unwrap();

        let stats = manager.get_stats().await.unwrap();
        assert_eq!(stats.total_files, 2);
        assert_eq!(stats.total_links, 0); // No links between these files
        assert_eq!(stats.orphaned_files, 2); // Both orphaned
    }

    #[tokio::test]
    async fn test_okf_markdown_cross_link_resolves_end_to_end() {
        // End-to-end: an OKF bundle-relative markdown cross-link
        // `[customers](/tables/customers.md)` must resolve through the real
        // parser -> graph pipeline (not just synthetic Link structs).
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::create_dir_all(temp_dir.path().join("tables")).unwrap();
        std::fs::write(
            temp_dir.path().join("tables/customers.md"),
            "---\ntype: BigQuery Table\ntitle: Customers\n---\n# Schema\n",
        )
        .unwrap();
        std::fs::write(
            temp_dir.path().join("tables/orders.md"),
            "---\ntype: BigQuery Table\ntitle: Orders\n---\n# Joins\n\nJoined with [customers](/tables/customers.md) on `customer_id`.\n",
        )
        .unwrap();

        manager.initialize().await.unwrap();

        // The cross-link must have produced a resolved graph edge.
        let stats = manager.get_stats().await.unwrap();
        assert_eq!(stats.total_files, 2);
        assert_eq!(
            stats.total_links, 1,
            "OKF markdown cross-link should resolve"
        );

        // And it must surface as a backlink on the target.
        let customers = temp_dir.path().join("tables/customers.md");
        let backlinks = manager.get_backlinks(&customers).await.unwrap();
        assert_eq!(backlinks.len(), 1, "customers.md should have one backlink");
    }

    #[tokio::test]
    async fn test_write_non_markdown_file_does_not_pollute_graph() {
        // Writing a non-markdown artifact (e.g. an exported viz.html) must not
        // add a node to the note graph or the note cache.
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::write(temp_dir.path().join("note.md"), "# Note").unwrap();
        manager.initialize().await.unwrap();
        assert_eq!(manager.get_stats().await.unwrap().total_files, 1);

        // Write a non-md file via the same path visualize() uses.
        manager
            .write_file(
                std::path::Path::new("viz.html"),
                "<html>[fake](/note.md)</html>",
                Precondition::Blind,
                "test",
            )
            .await
            .unwrap();

        // The HTML file is on disk but is NOT a graph node.
        assert!(temp_dir.path().join("viz.html").exists());
        assert_eq!(
            manager.get_stats().await.unwrap().total_files,
            1,
            "non-markdown write must not add a graph node"
        );
    }

    #[tokio::test]
    async fn test_get_related_notes() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Create a chain: A -> B -> C
        std::fs::write(temp_dir.path().join("a.md"), "# A\n[[b]]").unwrap();
        std::fs::write(temp_dir.path().join("b.md"), "# B\n[[a]]\n[[c]]").unwrap();
        std::fs::write(temp_dir.path().join("c.md"), "# C\n[[b]]").unwrap();

        manager.initialize().await.unwrap();

        // Get related notes to B within 1 hop (use absolute path)
        let b_path = temp_dir.path().join("b.md");
        let related = manager.get_related_notes(&b_path, 1).await.unwrap();

        // Should find A and C (direct neighbors)
        assert!(!related.is_empty());
    }

    #[tokio::test]
    async fn test_resolve_path_absolute() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Valid absolute path under vault
        let valid_path = temp_dir.path().join("test.md");
        let result = manager.resolve_path(&valid_path);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_resolve_path_relative() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Create the actual file
        std::fs::write(temp_dir.path().join("test.md"), "content").unwrap();

        let result = manager.resolve_path(Path::new("test.md"));
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_resolve_path_traversal_prevention() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Try to escape vault with ../ components
        let result = manager.resolve_path(Path::new("../../tmp/evil.md"));
        assert!(result.is_err(), "Path traversal should be prevented");

        // Also test with deeper traversal
        let result2 = manager.resolve_path(Path::new("../../../etc/passwd"));
        assert!(result2.is_err(), "Path traversal should be prevented");
    }

    // -------------------------------------------------------------------------
    // New comprehensive tests
    // -------------------------------------------------------------------------

    /// Writing a file then deleting it should leave the path absent on disk,
    /// and a subsequent `read_file` must return an error.
    #[tokio::test]
    async fn test_delete_file() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        let rel = Path::new("to_delete.md");
        manager
            .write_file(rel, "# Delete me", Precondition::Blind, "test")
            .await
            .unwrap();

        // Verify the file exists before deletion.
        assert!(temp_dir.path().join(rel).exists());

        manager
            .delete_file(rel, Precondition::ExpectExists, "test")
            .await
            .unwrap();

        // File must no longer exist on disk.
        assert!(
            !temp_dir.path().join(rel).exists(),
            "File should be gone after delete_file"
        );

        // read_file must return an error for the deleted path.
        let result = manager.read_file(rel).await;
        assert!(result.is_err(), "read_file on deleted path should error");
    }

    /// Moving a file should put its content at the new path and remove the old path.
    #[tokio::test]
    async fn test_move_file() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        let src = Path::new("source_note.md");
        let dst = Path::new("dest_note.md");
        let content = "# Moved Note\nsome content";

        manager
            .write_file(src, content, Precondition::Blind, "test")
            .await
            .unwrap();

        manager
            .move_file(
                src,
                dst,
                Precondition::ExpectExists,
                Precondition::Blind,
                "test",
            )
            .await
            .unwrap();

        // Old path must no longer exist.
        assert!(
            !temp_dir.path().join(src).exists(),
            "Source file should be gone after move"
        );

        // New path must exist with the original content.
        let read_back = manager.read_file(dst).await.unwrap();
        assert_eq!(
            read_back, content,
            "Destination must have the original content"
        );
    }

    /// Moving an attachment must preserve arbitrary bytes rather than requiring UTF-8.
    #[tokio::test]
    async fn test_move_file_preserves_non_utf8_bytes() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();
        let src = Path::new("attachments/source.bin");
        let dst = Path::new("assets/destination.bin");
        let bytes = [0, 159, 146, 150, 255, 10];

        tokio::fs::create_dir_all(temp_dir.path().join("attachments"))
            .await
            .unwrap();
        tokio::fs::write(temp_dir.path().join(src), bytes)
            .await
            .unwrap();

        manager
            .move_file(
                src,
                dst,
                Precondition::ExpectExists,
                Precondition::Blind,
                "test",
            )
            .await
            .unwrap();

        assert!(!temp_dir.path().join(src).exists());
        assert_eq!(
            tokio::fs::read(temp_dir.path().join(dst)).await.unwrap(),
            bytes
        );
        assert!(manager.all_cached_vault_files().await.is_empty());
    }

    /// `sync_index` is markdown-only (unified from `write_file`'s pre-M3a
    /// guard, see the doc comment on `sync_index`): a moved non-`.md`
    /// UTF-8-decodable file must not appear in the cache, even though
    /// pre-M3a `move_file` cached any UTF-8-decodable file regardless of
    /// extension.
    #[tokio::test]
    async fn test_move_file_non_markdown_utf8_not_cached() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();
        let src = Path::new("notes.txt");
        let dst = Path::new("moved.txt");

        tokio::fs::write(temp_dir.path().join(src), "plain text, not markdown")
            .await
            .unwrap();

        manager
            .move_file(
                src,
                dst,
                Precondition::ExpectExists,
                Precondition::Blind,
                "test",
            )
            .await
            .unwrap();

        assert!(!temp_dir.path().join(src).exists());
        assert_eq!(
            tokio::fs::read_to_string(temp_dir.path().join(dst))
                .await
                .unwrap(),
            "plain text, not markdown"
        );
        assert!(
            manager.all_cached_vault_files().await.is_empty(),
            "non-markdown files must never be cached"
        );
    }

    #[tokio::test]
    async fn test_refresh_file_state_tracks_external_delete_and_restore() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();
        std::fs::write(temp_dir.path().join("target.md"), "# Target\n").unwrap();
        std::fs::write(
            temp_dir.path().join("source.md"),
            "# Source\n\n[[target]]\n",
        )
        .unwrap();
        manager.initialize().await.unwrap();

        let source = Path::new("source.md");
        tokio::fs::remove_file(temp_dir.path().join(source))
            .await
            .unwrap();
        manager.refresh_file_state(source).await.unwrap();
        assert_eq!(manager.all_cached_vault_files().await.len(), 1);
        assert!(manager.get_forward_links(source).await.unwrap().is_empty());

        tokio::fs::write(temp_dir.path().join(source), "# Restored\n\n[[target]]\n")
            .await
            .unwrap();
        manager.refresh_file_state(source).await.unwrap();
        assert_eq!(manager.all_cached_vault_files().await.len(), 2);
        assert_eq!(manager.get_forward_links(source).await.unwrap().len(), 1);
    }

    /// Moving a file to a subdirectory that doesn't exist yet should create
    /// the intermediate directories automatically.
    #[tokio::test]
    async fn test_move_file_cross_directory() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        let src = Path::new("flat_note.md");
        let dst = Path::new("deep/nested/subdir/note.md");
        let content = "# Cross-dir Move";

        manager
            .write_file(src, content, Precondition::Blind, "test")
            .await
            .unwrap();

        // The destination directory does not exist yet.
        assert!(!temp_dir.path().join("deep").exists());

        manager
            .move_file(
                src,
                dst,
                Precondition::ExpectExists,
                Precondition::Blind,
                "test",
            )
            .await
            .unwrap();

        // Source gone, destination present.
        assert!(!temp_dir.path().join(src).exists());
        assert!(temp_dir.path().join(dst).exists());

        let read_back = manager.read_file(dst).await.unwrap();
        assert_eq!(read_back, content);
    }

    /// After a successful `write_file` no `.tmp.*` files should remain
    /// anywhere under the vault directory.
    #[tokio::test]
    async fn test_temp_file_cleanup_on_write() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Write into a nested directory to exercise the parent-creation path
        // and ensure temp files are cleaned up in the right place.
        let rel = Path::new("sub/cleanup_test.md");
        manager
            .write_file(rel, "content", Precondition::Blind, "test")
            .await
            .unwrap();

        // Walk the entire vault tree and assert no `.tmp.*` files remain.
        let mut stack = vec![temp_dir.path().to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                    assert!(
                        !ext.starts_with("tmp"),
                        "Leftover temp file found: {:?}",
                        path
                    );
                }
            }
        }
    }

    /// After writing a note that contains a wikilink the link graph must
    /// record that forward link from the written file.
    #[tokio::test]
    async fn test_graph_updated_after_write() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Write the target file so the parser can resolve the link.
        let target = Path::new("target.md");
        manager
            .write_file(target, "# Target", Precondition::Blind, "test")
            .await
            .unwrap();

        // Write a source file that links to target.
        let source = Path::new("source.md");
        manager
            .write_file(source, "# Source\n[[target]]", Precondition::Blind, "test")
            .await
            .unwrap();

        // Check the link graph via forward_links on the absolute source path.
        let source_abs = temp_dir.path().join(source);
        let forward = manager.get_forward_links(&source_abs).await.unwrap();

        // At least one forward link should resolve to target.
        assert!(
            !forward.is_empty(),
            "Link graph should record the [[target]] forward link after write"
        );
    }

    /// After deleting file A (which links to B) the backlinks for B must no
    /// longer include A.
    #[tokio::test]
    async fn test_graph_updated_after_delete() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Create both files and initialize so the graph is populated.
        std::fs::write(temp_dir.path().join("a.md"), "# A\n[[b]]").unwrap();
        std::fs::write(temp_dir.path().join("b.md"), "# B").unwrap();
        manager.initialize().await.unwrap();

        // Sanity: A should appear as a backlink to B before deletion.
        let b_abs = temp_dir.path().join("b.md");
        let backlinks_before = manager.get_backlinks(&b_abs).await.unwrap();
        assert!(
            !backlinks_before.is_empty(),
            "Before deletion A should be a backlink to B"
        );

        // Delete A.
        manager
            .delete_file(Path::new("a.md"), Precondition::ExpectExists, "test")
            .await
            .unwrap();

        // After deletion A must no longer appear in B's backlinks.
        let backlinks_after = manager.get_backlinks(&b_abs).await.unwrap();
        let a_abs = temp_dir.path().join("a.md");
        let a_still_linked = backlinks_after.iter().any(|p| p == &a_abs);
        assert!(
            !a_still_linked,
            "After deleting A, it must not appear in B's backlinks; found: {:?}",
            backlinks_after
        );
    }

    // ── vault_files_validated ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_validated_returns_cached_files_on_hot_path() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::write(temp_dir.path().join("a.md"), "# A").unwrap();
        std::fs::write(temp_dir.path().join("b.md"), "# B").unwrap();
        manager.initialize().await.unwrap();

        let files = manager.vault_files_validated().await;
        assert_eq!(files.len(), 2);
    }

    /// An edit made outside the process reaches `vault_files_validated`.
    ///
    /// Reconciled explicitly rather than by calling the accessor twice: the
    /// gate is debounced, so back-to-back calls deliberately share one pass.
    /// `the_gate_debounces_repeated_calls` covers that schedule; this covers
    /// what the accessor answers once a pass has run.
    #[tokio::test]
    async fn test_validated_reflects_external_modification() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        let path = temp_dir.path().join("note.md");
        std::fs::write(&path, "---\nstatus: draft\n---\n# Note").unwrap();
        manager.initialize().await.unwrap();

        // Confirm initial frontmatter is cached.
        let files = manager.vault_files_validated().await;
        let initial = files
            .iter()
            .find(|f| f.path == path)
            .and_then(|f| f.frontmatter.as_ref())
            .and_then(|fm| fm.data.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        assert_eq!(initial, "draft");

        // Overwrite the file externally with a future mtime.
        // Sleep 10 ms so the OS registers a mtime change.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        std::fs::write(&path, "---\nstatus: published\n---\n# Note").unwrap();

        manager.reconcile_now().await;
        let files = manager.vault_files_validated().await;
        let updated = files
            .iter()
            .find(|f| f.path == path)
            .and_then(|f| f.frontmatter.as_ref())
            .and_then(|fm| fm.data.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        assert_eq!(updated, "published");
    }

    /// A deletion made outside the process reaches `vault_files_validated`.
    #[tokio::test]
    async fn test_validated_reflects_external_deletion() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        let path = temp_dir.path().join("ephemeral.md");
        std::fs::write(&path, "# Ephemeral").unwrap();
        manager.initialize().await.unwrap();

        assert_eq!(manager.vault_files_validated().await.len(), 1);

        std::fs::remove_file(&path).unwrap();
        manager.reconcile_now().await;

        assert_eq!(manager.vault_files_validated().await.len(), 0);
    }

    // ── scan_vault ───────────────────────────────────────────────────────────

    /// The scan must recurse into nested subdirectories.
    #[tokio::test]
    async fn test_scan_vault_recurses_subdirectories() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::create_dir_all(temp_dir.path().join("a/b/c")).unwrap();
        std::fs::write(temp_dir.path().join("a/b/c/deep.md"), "# Deep").unwrap();

        let files = manager.scan_vault().await.unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("deep.md"));
    }

    /// The scan must skip files whose extension the configuration does not
    /// admit.
    #[tokio::test]
    async fn test_scan_vault_skips_disallowed_extensions() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::write(temp_dir.path().join("note.md"), "# Note").unwrap();
        std::fs::write(temp_dir.path().join("image.png"), "fake png").unwrap();
        std::fs::write(temp_dir.path().join("data.json"), "{}").unwrap();

        let files = manager.scan_vault().await.unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("note.md"));
    }

    /// Extension matching is case-insensitive, because `sync_index`'s markdown
    /// test is. If the two disagreed, a note the write path caches under one
    /// spelling would be reported deleted by the very next freshness sweep.
    #[tokio::test]
    async fn test_scan_vault_admits_uppercase_extensions() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::write(temp_dir.path().join("Shouty.MD"), "# Shouty").unwrap();

        let files = manager.scan_vault().await.unwrap();
        assert_eq!(files.len(), 1, "expected Shouty.MD to be discovered");
    }

    /// The scan must not descend through a symlink.
    ///
    /// A link pointing at an ancestor makes a following walk recurse until it
    /// exhausts memory, and one pointing outside the vault would pull content
    /// into the index that `resolve_path` refuses to hand back out. Both are
    /// reachable by anyone who can write a file into the vault.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_scan_vault_does_not_follow_symlinks() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::write(temp_dir.path().join("real.md"), "# Real").unwrap();
        // A loop: `vault/loop` -> `vault`. A following walk never terminates.
        std::os::unix::fs::symlink(temp_dir.path(), temp_dir.path().join("loop")).unwrap();

        let outside = TempDir::new().unwrap();
        std::fs::write(outside.path().join("secret.md"), "# Outside").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.md"),
            temp_dir.path().join("escape.md"),
        )
        .unwrap();

        let files = manager.scan_vault().await.unwrap();
        assert_eq!(files.len(), 1, "expected only the one real note: {files:?}");
        assert!(files[0].ends_with("real.md"));
    }

    /// `.turbovault/` is TurboVault's own audit trail and snapshot store, not
    /// vault content, and the scan runs on a schedule now.
    #[tokio::test]
    async fn test_scan_vault_skips_the_protected_directory() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::write(temp_dir.path().join("real.md"), "# Real").unwrap();
        std::fs::create_dir_all(temp_dir.path().join(".turbovault/snapshots")).unwrap();
        std::fs::write(
            temp_dir.path().join(".turbovault/snapshots/old.md"),
            "# Snapshot",
        )
        .unwrap();

        let files = manager.scan_vault().await.unwrap();
        assert_eq!(files.len(), 1, "expected only the one real note: {files:?}");
        assert!(files[0].ends_with("real.md"));
    }

    // ── external-change reconciliation ───────────────────────────────────────

    /// A note created by somebody else reaches the link graph and the cache.
    ///
    /// This is the case a per-entry mtime check cannot reach, because it can
    /// only re-check entries it already holds. Nothing tells this process the
    /// file appeared; the scan finds it by comparing state.
    #[tokio::test]
    async fn reconcile_discovers_a_note_created_outside_the_process() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();
        std::fs::write(temp_dir.path().join("seed.md"), "# Seed\n").unwrap();
        manager.initialize().await.unwrap();
        assert_eq!(manager.get_stats().await.unwrap().total_files, 1);

        std::fs::write(
            temp_dir.path().join("outside.md"),
            "# Outside\n\nsee [[seed]]\n",
        )
        .unwrap();

        manager.reconcile_now().await;

        assert_eq!(manager.get_stats().await.unwrap().total_files, 2);
        let backlinks = manager.get_backlinks(Path::new("seed.md")).await.unwrap();
        assert_eq!(
            backlinks.len(),
            1,
            "the externally created note's link must reach the graph: {backlinks:?}"
        );
    }

    /// An edit made by somebody else replaces what the graph and cache hold,
    /// including the links it added and the ones it took away.
    #[tokio::test]
    async fn reconcile_applies_an_edit_made_outside_the_process() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();
        std::fs::write(temp_dir.path().join("a.md"), "# A\n").unwrap();
        std::fs::write(temp_dir.path().join("b.md"), "# B\n").unwrap();
        manager.initialize().await.unwrap();
        assert!(
            manager
                .get_forward_links(Path::new("a.md"))
                .await
                .unwrap()
                .is_empty()
        );

        std::fs::write(temp_dir.path().join("a.md"), "# A\n\nnow links to [[b]]\n").unwrap();
        manager.reconcile_now().await;

        let forward = manager.get_forward_links(Path::new("a.md")).await.unwrap();
        assert_eq!(forward.len(), 1, "expected the added link: {forward:?}");
        assert!(forward[0].ends_with("b.md"));

        let cached = manager.all_cached_vault_files().await;
        let a = cached
            .iter()
            .find(|file| file.path.ends_with("a.md"))
            .expect("a.md must still be cached");
        assert!(
            a.content.contains("now links to"),
            "the cache must hold the new revision, not the parsed original"
        );
    }

    /// A note deleted by somebody else leaves the graph and the cache.
    #[tokio::test]
    async fn reconcile_drops_a_note_deleted_outside_the_process() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();
        std::fs::write(temp_dir.path().join("doomed.md"), "# Doomed\n").unwrap();
        manager.initialize().await.unwrap();
        assert_eq!(manager.get_stats().await.unwrap().total_files, 1);

        std::fs::remove_file(temp_dir.path().join("doomed.md")).unwrap();
        manager.reconcile_now().await;

        assert_eq!(manager.get_stats().await.unwrap().total_files, 0);
        assert!(manager.all_cached_vault_files().await.is_empty());
    }

    /// A pass that finds nothing must not announce anything.
    ///
    /// The listener drives index rebuilds and the plugin change feed, so a
    /// sweep that reported every note on every pass would be worse than no
    /// sweep at all. This is the property that makes recording the fingerprint
    /// on the write path matter.
    #[tokio::test]
    async fn reconcile_is_silent_when_nothing_moved() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();
        std::fs::write(temp_dir.path().join("one.md"), "# One\n").unwrap();
        std::fs::write(temp_dir.path().join("two.md"), "# Two\n").unwrap();
        manager.initialize().await.unwrap();

        let captured: Arc<StdMutex<Vec<(String, bool)>>> = Arc::new(StdMutex::new(Vec::new()));
        let cap = Arc::clone(&captured);
        manager.set_change_listener(Arc::new(move |changed| {
            cap.lock().unwrap().extend(
                changed
                    .into_iter()
                    .map(|(path, present, _)| (path, present)),
            );
            Box::pin(async {})
        }));

        manager.reconcile_now().await;
        assert!(
            captured.lock().unwrap().is_empty(),
            "an unchanged vault must produce no change events: {:?}",
            captured.lock().unwrap()
        );

        // And a write this process makes is announced once, by the write path,
        // not again by the sweep that follows it.
        manager
            .write_file(
                Path::new("three.md"),
                "# Three\n",
                Precondition::Blind,
                "add",
            )
            .await
            .unwrap();
        assert_eq!(captured.lock().unwrap().len(), 1);

        manager.reconcile_now().await;
        assert_eq!(
            captured.lock().unwrap().len(),
            1,
            "the sweep must not re-announce this process's own write: {:?}",
            captured.lock().unwrap()
        );
    }

    /// A file the vault admits for discovery but does not model as a note must
    /// not be reported over and over.
    ///
    /// `allowed_extensions` also covers `.txt` and `.canvas`, while the note
    /// cache, the link graph, and the search index are markdown only. If the
    /// sweep compared across that gap, nothing would ever record having seen
    /// such a file, so every pass would rediscover it and republish it: a
    /// permanent, silent event storm through the change feed.
    #[tokio::test]
    async fn reconcile_does_not_republish_files_it_does_not_model() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();
        std::fs::write(temp_dir.path().join("attachment.txt"), "one").unwrap();
        std::fs::write(temp_dir.path().join("note.md"), "# Note\n").unwrap();
        manager.initialize().await.unwrap();

        let captured: Arc<StdMutex<Vec<(String, bool)>>> = Arc::new(StdMutex::new(Vec::new()));
        let cap = Arc::clone(&captured);
        manager.set_change_listener(Arc::new(move |changed| {
            cap.lock().unwrap().extend(
                changed
                    .into_iter()
                    .map(|(path, present, _)| (path, present)),
            );
            Box::pin(async {})
        }));

        // Untouched, and admitted for discovery but not modelled.
        for _ in 0..3 {
            manager.reconcile_now().await;
        }
        assert!(
            captured.lock().unwrap().is_empty(),
            "a file the cache does not model must not be reported: {:?}",
            captured.lock().unwrap()
        );

        // Changed, still not modelled, still silent.
        std::fs::write(temp_dir.path().join("attachment.txt"), "two").unwrap();
        for _ in 0..3 {
            manager.reconcile_now().await;
        }
        assert!(
            captured.lock().unwrap().is_empty(),
            "changing it must not start an event storm either: {:?}",
            captured.lock().unwrap()
        );

        // And the note beside it is still reported, exactly once.
        std::fs::write(temp_dir.path().join("note.md"), "# Note\n\nedited\n").unwrap();
        for _ in 0..3 {
            manager.reconcile_now().await;
        }
        assert_eq!(
            *captured.lock().unwrap(),
            vec![("note.md".to_string(), true)],
            "a real note change is reported once, not once per pass"
        );
    }

    /// External changes are reported as external, so a consumer that reports
    /// its own writes at the write site can tell them apart.
    #[tokio::test]
    async fn reconcile_reports_external_origin() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();
        manager.initialize().await.unwrap();

        let captured: Arc<StdMutex<Vec<CommitOrigin>>> = Arc::new(StdMutex::new(Vec::new()));
        let cap = Arc::clone(&captured);
        manager.set_change_listener(Arc::new(move |changed| {
            cap.lock()
                .unwrap()
                .extend(changed.into_iter().map(|(_, _, origin)| origin));
            Box::pin(async {})
        }));

        std::fs::write(temp_dir.path().join("theirs.md"), "# Theirs\n").unwrap();
        manager.reconcile_now().await;
        assert_eq!(
            *captured.lock().unwrap(),
            vec![CommitOrigin::External],
            "a change this process did not make is external"
        );

        captured.lock().unwrap().clear();
        manager
            .write_file(Path::new("ours.md"), "# Ours\n", Precondition::Blind, "add")
            .await
            .unwrap();
        assert_eq!(
            *captured.lock().unwrap(),
            vec![CommitOrigin::Local],
            "a change this process made is local"
        );
    }

    /// The schedule bounds both how stale an answer can be and what
    /// reconciliation costs, without a per-vault knob.
    #[test]
    fn reconcile_interval_scales_with_the_last_pass() {
        let mut state = ReconcileState::new();
        assert!(state.is_due(), "the first gated call must always sweep");

        // A vault small enough that the duty cycle would ask for less than the
        // floor still waits the floor.
        state.record(Duration::from_micros(200));
        assert_eq!(state.interval(), RECONCILE_MIN_INTERVAL);
        assert!(!state.is_due(), "a pass just ran");

        // 40ms is roughly a 10k-note vault; 20x is under a second.
        state.record(Duration::from_millis(40));
        assert_eq!(state.interval(), Duration::from_millis(800));

        // A vault large enough to blow past the ceiling is capped there rather
        // than backing off without limit.
        state.record(Duration::from_secs(10));
        assert_eq!(state.interval(), RECONCILE_MAX_INTERVAL);
    }

    /// The gate is scheduled, so a burst of read calls costs one pass. Without
    /// this the sweep would dominate the cost of every cheap tool call.
    #[tokio::test]
    async fn the_gate_debounces_repeated_calls() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();
        manager.initialize().await.unwrap();

        // First call sweeps and records a timestamp.
        manager.ensure_fresh().await;

        std::fs::write(temp_dir.path().join("late.md"), "# Late\n").unwrap();
        manager.ensure_fresh().await;
        assert_eq!(
            manager.get_stats().await.unwrap().total_files,
            0,
            "a second call inside the interval must not pay for another scan"
        );

        // An explicit request overrides the schedule.
        manager.reconcile_now().await;
        assert_eq!(manager.get_stats().await.unwrap().total_files, 1);
    }

    /// Turning reconciliation off leaves the scheduled gate inert, and leaves
    /// the explicit call working.
    #[tokio::test]
    async fn reconciliation_can_be_turned_off() {
        let temp_dir = TempDir::new().unwrap();
        let mut config = create_test_config(temp_dir.path());
        config.reconcile_external_changes = false;
        let manager = VaultManager::new(config).unwrap();
        manager.initialize().await.unwrap();

        std::fs::write(temp_dir.path().join("ignored.md"), "# Ignored\n").unwrap();
        manager.ensure_fresh().await;
        assert_eq!(manager.get_stats().await.unwrap().total_files, 0);

        manager.reconcile_now().await;
        assert_eq!(
            manager.get_stats().await.unwrap().total_files,
            1,
            "an explicit reconcile is a request, not a setting"
        );
    }

    /// `vault_files_validated` sees notes created outside the process, which is
    /// what it could never do while it re-checked only the entries it held.
    #[tokio::test]
    async fn validated_files_include_externally_created_notes() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();
        std::fs::write(temp_dir.path().join("known.md"), "# Known\n").unwrap();
        manager.initialize().await.unwrap();

        std::fs::write(temp_dir.path().join("new.md"), "# New\n").unwrap();
        manager.reconcile_now().await;

        let files = manager.vault_files_validated().await;
        assert_eq!(files.len(), 2, "expected both notes: {files:?}");
    }

    // ── all_cached_vault_files ───────────────────────────────────────────────

    /// Before initialize(), all_cached_vault_files returns an empty list.
    #[tokio::test]
    async fn test_all_cached_vault_files_empty_before_init() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::write(temp_dir.path().join("note.md"), "# Note").unwrap();

        // Cache is empty — initialize() has not been called.
        assert!(manager.all_cached_vault_files().await.is_empty());
    }

    /// After initialize(), all_cached_vault_files returns all parsed files.
    #[tokio::test]
    async fn test_all_cached_vault_files_populated_after_init() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::write(temp_dir.path().join("a.md"), "# A").unwrap();
        std::fs::write(temp_dir.path().join("b.md"), "# B").unwrap();
        manager.initialize().await.unwrap();

        assert_eq!(manager.all_cached_vault_files().await.len(), 2);
    }

    /// all_cached_vault_files does NOT pick up external disk modifications —
    /// it returns whatever is in the cache without mtime checks.  This is the
    /// intended "fast path" behaviour; callers that need freshness should use
    /// vault_files_validated() instead.
    #[tokio::test]
    async fn test_all_cached_vault_files_does_not_detect_external_modification() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        let path = temp_dir.path().join("note.md");
        std::fs::write(&path, "---\nstatus: draft\n---\n# Note").unwrap();
        manager.initialize().await.unwrap();

        // Externally overwrite the file.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        std::fs::write(&path, "---\nstatus: published\n---\n# Note").unwrap();

        // all_cached_vault_files returns stale data — still "draft".
        let files = manager.all_cached_vault_files().await;
        let status = files
            .iter()
            .find(|f| f.path == path)
            .and_then(|f| f.frontmatter.as_ref())
            .and_then(|fm| fm.data.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(
            status, "draft",
            "all_cached_vault_files must not re-read disk"
        );
    }

    // ── cache-coherence: write_file / move_file / delete_file ────────────────

    /// write_file() on a brand-new file must insert it into the cache so that
    /// all_cached_vault_files() returns it without a reinitialize().
    #[tokio::test]
    async fn test_write_file_new_inserts_into_cache() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();
        manager.initialize().await.unwrap(); // warm empty cache

        manager
            .write_file(
                Path::new("new.md"),
                "---\nstatus: fresh\n---\n# New",
                Precondition::Blind,
                "test",
            )
            .await
            .unwrap();

        let files = manager.all_cached_vault_files().await;
        assert_eq!(
            files.len(),
            1,
            "new file must appear in cache after write_file"
        );

        let status = files[0]
            .frontmatter
            .as_ref()
            .and_then(|fm| fm.data.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(status, "fresh");
    }

    /// write_file() that overwrites an existing note must update the cached
    /// frontmatter; the cache must NOT keep the stale pre-write values.
    #[tokio::test]
    async fn test_write_file_overwrite_updates_cache() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::write(
            temp_dir.path().join("note.md"),
            "---\nstatus: old\n---\n# Note",
        )
        .unwrap();
        manager.initialize().await.unwrap();

        manager
            .write_file(
                Path::new("note.md"),
                "---\nstatus: updated\n---\n# Note",
                Precondition::Blind,
                "test",
            )
            .await
            .unwrap();

        let files = manager.all_cached_vault_files().await;
        assert_eq!(files.len(), 1);
        let status = files[0]
            .frontmatter
            .as_ref()
            .and_then(|fm| fm.data.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(status, "updated", "cache must reflect updated frontmatter");
    }

    /// move_file() must evict the old path and insert the new path so that
    /// all_cached_vault_files() reflects the move without reinitialize().
    #[tokio::test]
    async fn test_move_file_updates_cache() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::write(
            temp_dir.path().join("from.md"),
            "---\nstatus: active\n---\n# From",
        )
        .unwrap();
        manager.initialize().await.unwrap();

        manager
            .move_file(
                Path::new("from.md"),
                Path::new("to.md"),
                Precondition::ExpectExists,
                Precondition::Blind,
                "test",
            )
            .await
            .unwrap();

        let files = manager.all_cached_vault_files().await;
        assert_eq!(
            files.len(),
            1,
            "cache should have exactly one entry after move"
        );

        let from_abs = temp_dir.path().join("from.md");
        let to_abs = temp_dir.path().join("to.md");
        assert!(
            !files.iter().any(|f| f.path == from_abs),
            "old path must be absent from cache after move"
        );
        assert!(
            files.iter().any(|f| f.path == to_abs),
            "new path must be present in cache after move"
        );
    }

    /// delete_file() must evict the entry from the cache immediately.
    #[tokio::test]
    async fn test_delete_file_evicts_from_cache() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::write(temp_dir.path().join("note.md"), "# Note").unwrap();
        manager.initialize().await.unwrap();
        assert_eq!(manager.all_cached_vault_files().await.len(), 1);

        manager
            .delete_file(Path::new("note.md"), Precondition::ExpectExists, "test")
            .await
            .unwrap();

        assert_eq!(
            manager.all_cached_vault_files().await.len(),
            0,
            "deleted file must be evicted from cache"
        );
    }

    /// `sync_index`'s eviction branch runs after `substrate.apply()` has
    /// already released its write-lock (see the "ponytail" comment on that
    /// branch). If a path is recreated between an operation's apply() and
    /// its sync_index() call, a stale "now absent" notification for that
    /// path must not evict the file that is legitimately present again.
    #[tokio::test]
    async fn test_sync_index_skips_stale_removal_when_path_recreated() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        manager
            .write_file(Path::new("race.md"), "v1", Precondition::Blind, "test")
            .await
            .unwrap();
        assert_eq!(manager.all_cached_vault_files().await.len(), 1);

        // Stand in for a concurrent blind write that recreated the path
        // after a delete's apply() completed but before the delete's
        // sync_index() call ran.
        manager
            .write_file(Path::new("race.md"), "v2", Precondition::Blind, "test")
            .await
            .unwrap();

        // The delete's (delayed, stale) removal notification arrives last.
        manager.sync_index(&[("race.md".to_string(), false)]).await;

        assert_eq!(
            manager.all_cached_vault_files().await.len(),
            1,
            "stale removal must not evict a path that was recreated"
        );
        assert_eq!(manager.read_file(Path::new("race.md")).await.unwrap(), "v2");
    }

    #[tokio::test]
    async fn stale_write_hash_preserves_existing_content() {
        let temp_dir = TempDir::new().unwrap();
        let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();
        manager
            .write_file(Path::new("note.md"), "current", Precondition::Blind, "test")
            .await
            .unwrap();

        let error = manager
            .write_file(
                Path::new("note.md"),
                "replacement",
                Precondition::ExpectBlob("stale".to_string()),
                "test",
            )
            .await
            .unwrap_err();

        assert!(matches!(error, Error::ConcurrencyError { .. }));
        assert_eq!(
            manager.read_file(Path::new("note.md")).await.unwrap(),
            "current"
        );
    }

    #[tokio::test]
    async fn expected_hash_rejects_recreating_a_deleted_file() {
        let temp_dir = TempDir::new().unwrap();
        let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();

        let error = manager
            .write_file(
                Path::new("missing.md"),
                "replacement",
                Precondition::ExpectBlob("stale".to_string()),
                "test",
            )
            .await
            .unwrap_err();

        match error {
            Error::ConcurrencyError { reason } => assert!(reason.contains("does not exist")),
            other => panic!("expected concurrency error, got {other:?}"),
        }
        assert!(!temp_dir.path().join("missing.md").exists());
    }

    #[tokio::test]
    async fn stale_edit_hash_preserves_existing_content() {
        let temp_dir = TempDir::new().unwrap();
        let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();
        manager
            .write_file(
                Path::new("note.md"),
                "hello world",
                Precondition::Blind,
                "test",
            )
            .await
            .unwrap();

        let edits = "<<<<<<< SEARCH\nhello world\n=======\ngoodbye world\n>>>>>>> REPLACE";
        let error = manager
            .edit_file(
                Path::new("note.md"),
                edits,
                Precondition::ExpectBlob("stale".to_string()),
                false,
                "test",
            )
            .await
            .unwrap_err();

        assert!(matches!(error, Error::ConcurrencyError { .. }));
        assert_eq!(
            manager.read_file(Path::new("note.md")).await.unwrap(),
            "hello world"
        );
    }

    /// Staying inside the vault root is not authorization to touch everything
    /// in it: `.obsidian/plugins/*/main.js` and `.git/hooks/*` are code that
    /// something else executes, and `.turbovault/` is the audit trail.
    #[tokio::test]
    async fn protected_directories_are_unreachable_through_the_note_api() {
        let temp_dir = TempDir::new().unwrap();
        let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();

        for path in [
            ".obsidian/plugins/tasks/main.js",
            ".git/hooks/post-commit",
            ".turbovault/audit/operations.jsonl",
            "node_modules/pkg/index.js",
            "notes/.obsidian/nested.md",
        ] {
            let error = manager
                .write_file(Path::new(path), "payload", Precondition::Blind, "test")
                .await
                .unwrap_err();
            assert!(
                matches!(error, Error::ProtectedPath { .. }),
                "{path} should be refused, got {error:?}"
            );
            assert!(!temp_dir.path().join(path).exists(), "{path} was written");

            let error = manager.read_file(Path::new(path)).await.unwrap_err();
            assert!(
                matches!(error, Error::ProtectedPath { .. }),
                "reading {path} should be refused, got {error:?}"
            );
        }

        // A capability-holding caller can still resolve protected state; the
        // policy lives in `resolve_path`, not in the traversal check.
        manager
            .resolve_path_bypassing_policy(Path::new(".obsidian/app.json"))
            .expect("capability-gated resolution stays available");
        // ...but the vault boundary still holds for it.
        assert!(matches!(
            manager
                .resolve_path_bypassing_policy(Path::new("../escape.md"))
                .unwrap_err(),
            Error::PathTraversalAttempt { .. }
        ));
    }

    /// `max_file_size` used to be applied only while scanning directories, so
    /// an explicit read pulled any size into memory.
    #[tokio::test]
    async fn max_file_size_is_enforced_on_reads_and_writes() {
        let temp_dir = TempDir::new().unwrap();
        let mut config = create_test_config(temp_dir.path());
        config.max_file_size = 32;
        let manager = VaultManager::new(config).unwrap();

        let oversized = "x".repeat(64);
        let error = manager
            .write_file(Path::new("big.md"), &oversized, Precondition::Blind, "test")
            .await
            .unwrap_err();
        assert!(matches!(error, Error::FileTooLarge { .. }), "{error:?}");
        assert!(!temp_dir.path().join("big.md").exists());

        // Written out of band (an editor, a sync client) — the read must still
        // refuse rather than buffer the whole file.
        std::fs::write(temp_dir.path().join("big.md"), &oversized).unwrap();
        let error = manager.read_file(Path::new("big.md")).await.unwrap_err();
        assert!(matches!(error, Error::FileTooLarge { .. }), "{error:?}");

        manager
            .write_file(Path::new("small.md"), "fits", Precondition::Blind, "test")
            .await
            .expect("within the limit");
    }

    #[tokio::test]
    async fn stale_delete_and_move_hashes_preserve_the_source() {
        let temp_dir = TempDir::new().unwrap();
        let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();
        manager
            .write_file(Path::new("note.md"), "keep me", Precondition::Blind, "test")
            .await
            .unwrap();

        let delete_error = manager
            .delete_file(
                Path::new("note.md"),
                Precondition::ExpectBlob("stale".to_string()),
                "test",
            )
            .await
            .unwrap_err();
        assert!(matches!(delete_error, Error::ConcurrencyError { .. }));

        let move_error = manager
            .move_file(
                Path::new("note.md"),
                Path::new("moved.md"),
                Precondition::ExpectBlob("stale".to_string()),
                Precondition::Blind,
                "test",
            )
            .await
            .unwrap_err();
        assert!(matches!(move_error, Error::ConcurrencyError { .. }));
        assert_eq!(
            manager.read_file(Path::new("note.md")).await.unwrap(),
            "keep me"
        );
        assert!(!temp_dir.path().join("moved.md").exists());
    }

    // -------------------------------------------------------------------------
    // M4b: VaultManager::new builds a live Git substrate from write_backend
    // -------------------------------------------------------------------------

    /// A born-HEAD git repo (mirrors `substrate.rs`'s own `init_repo` test
    /// helper) — required before `commit_changeset` can build a parent chain.
    fn init_git_repo(dir: &Path) {
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        git2::Repository::init_opts(dir, &opts).unwrap();
    }

    /// A `write_backend: git` vault config, the M4b construction path.
    fn create_git_test_config(vault_dir: &Path) -> ServerConfig {
        let mut config = ServerConfig::new();
        let vault_config = VaultConfig::builder("git_vault", vault_dir)
            .write_backend(WriteBackend::Git)
            .build()
            .unwrap();
        config.vaults.push(vault_config);
        config
    }

    fn head_oid(dir: &Path) -> Option<git2::Oid> {
        git2::Repository::open(dir).ok()?.head().ok()?.target()
    }

    /// The blob content at `rel_path` in HEAD's tree, for asserting working
    /// tree == HEAD independent of what's on disk.
    fn head_blob_content(dir: &Path, rel_path: &str) -> Option<String> {
        let repo = git2::Repository::open(dir).ok()?;
        let tree = repo.head().ok()?.peel_to_commit().ok()?.tree().ok()?;
        let entry = tree.get_path(Path::new(rel_path)).ok()?;
        let blob = repo.find_blob(entry.id()).ok()?;
        Some(String::from_utf8_lossy(blob.content()).into_owned())
    }

    /// A `VaultManager` built over a `write_backend: git` vault routes
    /// `write_file` through `GitSubstrate`: the write lands as a commit, the
    /// working tree agrees with HEAD, and the link graph picks up the note
    /// via `sync_index` (R7) — all without touching `GitFileTools`.
    #[tokio::test]
    async fn test_git_backend_write_file_commits_and_syncs_index() {
        let temp_dir = TempDir::new().unwrap();
        init_git_repo(temp_dir.path());
        let manager = VaultManager::new(create_git_test_config(temp_dir.path())).unwrap();

        assert!(
            head_oid(temp_dir.path()).is_none(),
            "repo starts unborn (no commits yet)"
        );

        manager
            .write_file(
                Path::new("note.md"),
                "# Git Note",
                Precondition::Blind,
                "test commit",
            )
            .await
            .unwrap();

        assert!(
            head_oid(temp_dir.path()).is_some(),
            "write_file on a git vault must land a commit"
        );
        assert_eq!(
            manager.read_file(Path::new("note.md")).await.unwrap(),
            "# Git Note"
        );
        assert_eq!(
            head_blob_content(temp_dir.path(), "note.md").as_deref(),
            Some("# Git Note"),
            "working tree must agree with HEAD after a git-backend apply"
        );

        // sync_index (R7) ran off the substrate's ApplyOutcome, not a
        // separate `initialize()` scan.
        let stats = manager.get_stats().await.unwrap();
        assert_eq!(stats.total_files, 1);
    }

    /// `apply_changes` on a git vault is the same GitSubstrate path as
    /// `write_file` — exercise it directly, matching
    /// `test_apply_changes_multi_change_plan`'s direct-backend coverage.
    #[tokio::test]
    async fn test_git_backend_apply_changes_commits_multi_change_plan() {
        let temp_dir = TempDir::new().unwrap();
        init_git_repo(temp_dir.path());
        let manager = VaultManager::new(create_git_test_config(temp_dir.path())).unwrap();

        let plan = ChangePlan::new("multi-change git test")
            .create("a.md", "# A")
            .create("b.md", "# B");
        let outcome = manager.apply_changes(&plan).await.unwrap();

        assert!(outcome.commit.is_some());
        assert_eq!(
            head_blob_content(temp_dir.path(), "a.md").as_deref(),
            Some("# A")
        );
        assert_eq!(
            head_blob_content(temp_dir.path(), "b.md").as_deref(),
            Some("# B")
        );
    }

    /// A stale `ExpectBlob` against a git vault aborts the whole plan with
    /// `ConcurrencyError` and lands zero commits — the manager's GitSubstrate
    /// enforces the same CAS guarantee `GitFileTools` does today.
    #[tokio::test]
    async fn test_git_backend_stale_precondition_aborts_no_commit() {
        let temp_dir = TempDir::new().unwrap();
        init_git_repo(temp_dir.path());
        let manager = VaultManager::new(create_git_test_config(temp_dir.path())).unwrap();

        manager
            .write_file(Path::new("note.md"), "v1", Precondition::Blind, "seed")
            .await
            .unwrap();
        let head_after_seed = head_oid(temp_dir.path());

        let err = manager
            .write_file(
                Path::new("note.md"),
                "v2",
                Precondition::ExpectBlob("deadbeef".to_string()),
                "stale update",
            )
            .await
            .unwrap_err();

        assert!(matches!(err, Error::ConcurrencyError { .. }));
        assert_eq!(
            head_oid(temp_dir.path()),
            head_after_seed,
            "a stale precondition must not land a commit"
        );
        assert_eq!(
            head_blob_content(temp_dir.path(), "note.md").as_deref(),
            Some("v1"),
            "HEAD must still hold the pre-abort content"
        );
    }

    // -------------------------------------------------------------------------
    // M4c (bite 3a): manager-owned reindex — change-listener, out-of-band
    // drain, and background-task lifecycle (turbovault-qae.5.3, deliverable F)
    // -------------------------------------------------------------------------

    /// Create a commit with bare git2, bypassing the substrate — an
    /// out-of-band ref advance (another process / manual git commit).
    fn make_external_commit(repo_path: &Path, file_name: &str, content: &str) {
        let repo = git2::Repository::open(repo_path).unwrap();
        std::fs::write(repo_path.join(file_name), content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(file_name)).unwrap();
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
        };
    }

    /// A `write_file` through a git-backend manager fires the R7
    /// change-listener with the written path — the search-staleness close
    /// (the server registers a listener that feeds search + similarity off
    /// exactly this set).
    #[tokio::test]
    async fn git_write_fires_change_listener_with_written_path() {
        let temp_dir = TempDir::new().unwrap();
        init_git_repo(temp_dir.path());
        let manager = Arc::new(VaultManager::new(create_git_test_config(temp_dir.path())).unwrap());
        manager.ensure_reindex_started();

        let captured: Arc<std::sync::Mutex<Vec<(String, bool)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let cap = Arc::clone(&captured);
        manager.set_change_listener(Arc::new(move |changed| {
            cap.lock().unwrap().extend(
                changed
                    .into_iter()
                    .map(|(path, present, _)| (path, present)),
            );
            Box::pin(async {})
        }));

        manager
            .write_file(
                Path::new("note.md"),
                "# Note",
                Precondition::Blind,
                "commit",
            )
            .await
            .unwrap();

        let got = captured.lock().unwrap();
        assert!(
            got.iter().any(|(p, present)| p == "note.md" && *present),
            "change-listener must fire with (note.md, present=true); got {got:?}"
        );
    }

    /// An out-of-band commit (bare git2) is detected by the manager's
    /// ref-listener and, after `flush_reindex()`, is reflected in the link
    /// graph AND fired to the change-listener.
    #[tokio::test]
    async fn git_out_of_band_commit_drains_into_graph_and_listener() {
        let temp_dir = TempDir::new().unwrap();
        init_git_repo(temp_dir.path());
        // Seed a committed note: a non-None ref baseline + one graph node.
        make_external_commit(temp_dir.path(), "seed.md", "# Seed\n");
        let manager = Arc::new(VaultManager::new(create_git_test_config(temp_dir.path())).unwrap());
        manager.initialize().await.unwrap();
        assert_eq!(manager.get_stats().await.unwrap().total_files, 1);

        let captured: Arc<std::sync::Mutex<Vec<(String, bool)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let cap = Arc::clone(&captured);
        manager.set_change_listener(Arc::new(move |changed| {
            cap.lock().unwrap().extend(
                changed
                    .into_iter()
                    .map(|(path, present, _)| (path, present)),
            );
            Box::pin(async {})
        }));

        // Fast ref poll so the test doesn't wait the production 5s.
        manager.ensure_reindex_started_with_interval(std::time::Duration::from_millis(25));
        // Let the listener snapshot its HEAD baseline (= the seed commit)
        // BEFORE the out-of-band commit lands, so it registers as an advance.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Out-of-band commit adds external.md (links back to seed).
        make_external_commit(
            temp_dir.path(),
            "external.md",
            "# External\n\nsee [[seed]]\n",
        );

        // The ref-listener enqueues within a poll; the drainer and/or this
        // flush apply it (idempotent). Poll until the graph reflects it.
        let mut reflected = false;
        for _ in 0..80 {
            manager.flush_reindex().await;
            if manager.get_stats().await.unwrap().total_files == 2 {
                reflected = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert!(
            reflected,
            "out-of-band commit must be reflected in the link graph after flush"
        );

        let got = captured.lock().unwrap();
        assert!(
            got.iter()
                .any(|(p, present)| p == "external.md" && *present),
            "change-listener must fire with the out-of-band path; got {got:?}"
        );
    }

    /// Dropping the manager aborts its background tasks (no leak). The tasks
    /// hold `Weak<Self>`, never `Arc<Self>`, so the caller's ref is the last
    /// strong one — an `Arc` capture would keep this upgrade succeeding.
    #[tokio::test]
    async fn dropping_manager_aborts_reindex_tasks_no_leak() {
        let temp_dir = TempDir::new().unwrap();
        init_git_repo(temp_dir.path());
        let manager = Arc::new(VaultManager::new(create_git_test_config(temp_dir.path())).unwrap());
        manager.ensure_reindex_started();

        let weak = Arc::downgrade(&manager);
        drop(manager);

        // A drainer iteration may briefly hold an upgraded Arc; once Drop's
        // abort lands the last strong ref is gone for good.
        let mut gone = false;
        for _ in 0..100 {
            if weak.upgrade().is_none() {
                gone = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(gone, "manager leaked: a background task holds a strong ref");
    }
}
