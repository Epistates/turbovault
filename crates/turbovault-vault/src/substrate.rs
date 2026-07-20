//! `WriteSubstrate` — the dispatch point behind `VaultManager` (write-
//! substrate-layering M3a, `turbovault-qae.4.1`).
//!
//! `VaultManager`'s mutators translate their `expected_hash` parameter into a
//! [`Precondition`], fold it into a one-change [`ChangePlan`], and hand it to
//! `self.substrate.apply(&plan)`. This module is where that plan actually
//! lands.
//!
//! **M3a scope:** only the [`DirectSubstrate`] arm sits on a production path
//! (VaultManager's mutators route through it). [`GitSubstrate`] is built and
//! unit-tested here but stays DORMANT — the server's `WriteTools::Git` /
//! `GitFileTools` still own the live git write path until M4 deletes them.
//! Temporary duplication of `GitFileTools::apply_txn`'s commit logic here is
//! deliberate (design §6.5 / migration §M3a).

use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use uuid::Uuid;

use turbovault_audit::{AuditEntry, AuditLog, OperationType, SnapshotStore};
use turbovault_core::{Change, ChangePlan, Error, Precondition, Result, bytes_to_lower_hex};
use turbovault_git::{CommitHook, CommitLocks, VaultRepo};

use crate::edit::compute_hash;

/// Outcome of applying one [`ChangePlan`] to a substrate (design §6.3) — the
/// substrate-level analogue of `BatchResult`.
#[derive(Debug, Clone, Default)]
pub struct ApplyOutcome {
    /// `(vault-relative path, present-after-apply)` for every path the plan
    /// touched — feeds the manager's link-graph + file-cache update (R7).
    pub changed: Vec<(String, bool)>,
    /// git: the commit oid as hex. direct: `None`.
    pub commit: Option<String>,
    /// `true` when every change in the plan landed atomically (git: always,
    /// one commit; direct: single-change plans only — M3a's manager never
    /// builds multi-change plans, so this is filled trivially here; true
    /// best-effort semantics for direct batches land in M4/M5).
    pub atomic: bool,
    /// Direct best-effort only: the change index that stopped a multi-change
    /// plan. Always `None` in M3a.
    pub failed_at: Option<usize>,
}

/// Dispatches a [`ChangePlan`] to whichever backend a vault is configured
/// for (design §6.3's `WriteSubstrate` enum).
pub enum WriteSubstrate {
    Direct(DirectSubstrate),
    Git(GitSubstrate),
}

impl WriteSubstrate {
    pub async fn apply(&self, plan: &ChangePlan) -> Result<ApplyOutcome> {
        match self {
            WriteSubstrate::Direct(direct) => direct.apply(plan).await,
            WriteSubstrate::Git(git) => git.apply(plan).await,
        }
    }

    /// The version token THIS substrate would compute for `bytes` if they
    /// were a path's current content — Direct: NFC-normalized sha256 hex (or
    /// raw sha256 for non-UTF-8); Git: git blob oid hex. For a caller that
    /// already has content in hand and wants to mint its own
    /// `Precondition::ExpectBlob` (e.g. a batch fold hashing a backlink
    /// source it just read), asking the manager's own substrate for the
    /// token — rather than assuming one hash convention — keeps the token
    /// valid whichever backend eventually applies the plan.
    pub fn hash_bytes(&self, bytes: &[u8]) -> Result<String> {
        match self {
            WriteSubstrate::Direct(_) => Ok(hash_bytes(bytes)),
            WriteSubstrate::Git(_) => VaultRepo::blob_oid_of(bytes)
                .map(|oid| oid.to_string())
                .map_err(git_err_to_core),
        }
    }
}

/// Derive `ApplyOutcome.changed` from a plan's changes alone (design §6.3):
/// `Upsert`/`Remove` map straight through; `Rename` reports both endpoints
/// (`from` absent, `to` present) — matching the git substrate's materialize
/// call. Shared by both substrates so "which paths does this plan touch, and
/// are they present afterwards" has one definition.
fn changes_to_outcome(changes: &[Change]) -> Vec<(String, bool)> {
    let mut out = Vec::with_capacity(changes.len());
    for change in changes {
        match change {
            Change::Upsert { path, .. } => out.push((path.clone(), true)),
            Change::Remove { path } => out.push((path.clone(), false)),
            Change::Rename { from, to } => {
                out.push((from.clone(), false));
                out.push((to.clone(), true));
            }
        }
    }
    out
}

/// sha256 hex of `bytes` — NFC-normalized (via [`compute_hash`]) when the
/// bytes are valid UTF-8 text (matching every text CAS token in the
/// codebase), raw sha256 otherwise (matching `move_file`'s pre-M3a fallback
/// for non-UTF-8 attachments).
fn hash_bytes(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => compute_hash(text),
        Err(_) => bytes_to_lower_hex(Sha256::digest(bytes)),
    }
}

// ---------------------------------------------------------------------------
// DirectSubstrate
// ---------------------------------------------------------------------------

/// The non-git write path (design §6.4) — the pre-M3a `VaultManager` mutator
/// bodies (`write_file`/`delete_file`/`move_file`), extracted behind the
/// substrate boundary.
///
/// Does fs-write + precondition + audit ONLY (deliverable decision 3 of the
/// M3a DoD): it returns the changed paths and the manager updates the link
/// graph + file cache after `apply` returns (R7).
pub struct DirectSubstrate {
    vault_path: PathBuf,
    /// R6: serializes the check-preconditions -> apply window in-process,
    /// closing the TOCTOU window the pre-M3a per-op mutators left open
    /// between their hash check and their write. Cross-process safety stays
    /// a git-backend capability (ref CAS) — not simulated here.
    ///
    /// ponytail: substrate-wide, not per-path — every Direct-backend write
    /// serializes behind this one mutex for the duration of its fs I/O,
    /// even across unrelated files. Upgrade to a per-path lock table if
    /// write concurrency on a large vault ever proves observably slow.
    write_lock: Mutex<()>,
    audit_log: Option<Arc<AuditLog>>,
    snapshot_store: Option<Arc<SnapshotStore>>,
}

impl DirectSubstrate {
    pub fn new(vault_path: PathBuf) -> Self {
        Self {
            vault_path,
            write_lock: Mutex::new(()),
            audit_log: None,
            snapshot_store: None,
        }
    }

    /// Wired by `VaultManager::set_audit_log` — the Direct arm owns the
    /// audit/snapshot recording responsibility (deliverable decision 3).
    pub fn set_audit_log(&mut self, audit_log: Arc<AuditLog>, snapshot_store: Arc<SnapshotStore>) {
        self.audit_log = Some(audit_log);
        self.snapshot_store = Some(snapshot_store);
    }

    fn full_path(&self, rel: &str) -> PathBuf {
        self.vault_path.join(rel)
    }

    pub async fn apply(&self, plan: &ChangePlan) -> Result<ApplyOutcome> {
        let _guard = self.write_lock.lock().await;

        // Every precondition is checked against current on-disk bytes before
        // any change is applied — a stale read aborts the whole plan with
        // nothing written (mirrors the git substrate's reconsideration
        // domino, design §6.1/§6.2).
        for (path, precondition) in &plan.preconditions {
            let before = match tokio::fs::read(self.full_path(path)).await {
                Ok(bytes) => Some(bytes),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                // Anything other than "absent" (e.g. EACCES) is a real I/O
                // failure, not a precondition mismatch — propagate it as
                // such instead of masking it as "file does not exist".
                Err(e) => return Err(Error::io(e)),
            };
            Self::check_precondition(path, precondition, before.as_deref())?;
        }

        for change in &plan.changes {
            match change {
                Change::Upsert { path, content } => self.upsert(path, content).await?,
                Change::Remove { path } => self.remove(path).await?,
                Change::Rename { from, to } => self.rename(from, to).await?,
            }
        }

        Ok(ApplyOutcome {
            changed: changes_to_outcome(&plan.changes),
            commit: None,
            atomic: true,
            failed_at: None,
        })
    }

    /// Behavior-preserving translation of the pre-M3a per-mutator
    /// `expected_hash` checks (write_file/delete_file/move_file, manager.rs
    /// ~L184/L406/L472) onto the four [`Precondition`] variants. Real
    /// working-tree CAS (vs. this advisory sha256 check) is M5 (R5).
    fn check_precondition(
        path: &str,
        precondition: &Precondition,
        before: Option<&[u8]>,
    ) -> Result<()> {
        match precondition {
            Precondition::Blind => Ok(()),
            Precondition::ExpectAbsent => match before {
                None => Ok(()),
                Some(_) => Err(Error::concurrency_error(format!(
                    "File already exists at '{path}'; expected it to be absent."
                ))),
            },
            Precondition::ExpectExists => match before {
                Some(_) => Ok(()),
                None => Err(Error::concurrency_error(format!(
                    "File does not exist at '{path}'."
                ))),
            },
            Precondition::ExpectBlob(expected) => match before {
                Some(bytes) => {
                    let actual = hash_bytes(bytes);
                    if &actual == expected {
                        Ok(())
                    } else {
                        Err(Error::concurrency_error(format!(
                            "File modified since last read. Expected hash: {expected}, actual: {actual}. Re-read the file and retry."
                        )))
                    }
                }
                None => Err(Error::concurrency_error(format!(
                    "File does not exist but expected_hash '{expected}' was provided. The file may have been deleted."
                ))),
            },
        }
    }

    /// Write `content` to `path` via temp+rename — the pre-M3a
    /// `VaultManager::write_file` body.
    async fn upsert(&self, path: &str, content: &[u8]) -> Result<()> {
        let full = self.full_path(path);
        let before = tokio::fs::read(&full).await.ok();

        if let Some(parent) = full.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(Error::io)?;
        }
        let temp = full.with_extension(format!("tmp.{}", Uuid::new_v4()));
        tokio::fs::write(&temp, content).await.map_err(Error::io)?;
        if let Err(e) = tokio::fs::rename(&temp, &full).await {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(Error::io(e));
        }

        let operation = if before.is_some() {
            OperationType::Update
        } else {
            OperationType::Create
        };
        self.record_audit(path, operation, before.as_deref(), Some(content), None)
            .await;
        Ok(())
    }

    /// The pre-M3a `VaultManager::delete_file` body.
    async fn remove(&self, path: &str) -> Result<()> {
        let full = self.full_path(path);
        let before = tokio::fs::read(&full).await.ok();
        tokio::fs::remove_file(&full).await.map_err(Error::io)?;
        self.record_audit(path, OperationType::Delete, before.as_deref(), None, None)
            .await;
        Ok(())
    }

    /// The pre-M3a `VaultManager::move_file` body (rename with cross-device
    /// fallback; raw bytes preserved for non-UTF-8 attachments).
    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        let from_full = self.full_path(from);
        let to_full = self.full_path(to);
        let bytes = tokio::fs::read(&from_full).await.map_err(Error::io)?;

        if let Some(parent) = to_full.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(Error::io)?;
        }
        match tokio::fs::rename(&from_full, &to_full).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
                tokio::fs::copy(&from_full, &to_full)
                    .await
                    .map_err(Error::io)?;
                if let Err(del_err) = tokio::fs::remove_file(&from_full).await {
                    let _ = tokio::fs::remove_file(&to_full).await;
                    return Err(Error::io(del_err));
                }
            }
            Err(e) => return Err(Error::io(e)),
        }

        // A move changes location, not content: before == after.
        self.record_audit(
            from,
            OperationType::Move,
            Some(&bytes),
            Some(&bytes),
            Some(to),
        )
        .await;
        Ok(())
    }

    /// Fire-and-forget audit + snapshot recording (never blocks/fails the
    /// write) — unchanged from the pre-M3a mutator bodies. Snapshot storage
    /// only happens for UTF-8-decodable content (`SnapshotStore` is text-
    /// addressed); non-UTF-8 attachments still get an audit entry, just
    /// without a stored snapshot, matching pre-M3a `move_file`.
    async fn record_audit(
        &self,
        rel_path: &str,
        operation: OperationType,
        before: Option<&[u8]>,
        after: Option<&[u8]>,
        new_path: Option<&str>,
    ) {
        let (Some(audit_log), Some(snapshot_store)) = (&self.audit_log, &self.snapshot_store)
        else {
            return;
        };

        let mut entry = AuditEntry::new(operation, rel_path);
        if let Some(new_path) = new_path {
            entry = entry.with_new_path(new_path);
        }

        if let Some(before) = before.and_then(|b| std::str::from_utf8(b).ok()) {
            match snapshot_store.store(before).await {
                Ok(snap_id) => {
                    entry = entry.with_before(SnapshotStore::compute_hash(before), snap_id);
                }
                Err(e) => log::warn!("Failed to store before-snapshot: {}", e),
            }
        }
        if let Some(after) = after.and_then(|a| std::str::from_utf8(a).ok()) {
            match snapshot_store.store(after).await {
                Ok(snap_id) => {
                    entry = entry.with_after(SnapshotStore::compute_hash(after), snap_id);
                }
                Err(e) => log::warn!("Failed to store after-snapshot: {}", e),
            }
        }

        if let Err(e) = audit_log.record(&entry).await {
            log::warn!("Failed to record audit entry: {}", e);
        }
    }
}

// ---------------------------------------------------------------------------
// GitSubstrate — dormant in M3a (design §6.5)
// ---------------------------------------------------------------------------

/// turbovault-a0l (PERF-1): a per-vault cached substrate handle. `VaultRepo`
/// wraps a `git2::Repository`, which is `Send + !Sync`, so it lives behind a
/// `std::sync::Mutex`. Duplicated from `turbovault-tools::git_file_tools`
/// (deliberate temporary duplication — M4 deletes `GitFileTools`).
type CachedRepo = Arc<std::sync::Mutex<VaultRepo>>;

/// Callback invoked **before** returning a `ConcurrencyError` from
/// [`GitSubstrate::apply`] (GWS.14b). Duplicated from
/// `turbovault-tools::git_file_tools::CasCollisionFlush`.
type CasCollisionFlush = Arc<dyn Fn() -> BoxFuture<'static, Result<()>> + Send + Sync>;

/// The git write path (design §6.5) — `GitFileTools::apply_txn` +
/// `run_txn` (`git_file_tools.rs:1132`/`:1205`), relocated. **Dormant in
/// M3a**: nothing on the production path constructs or calls this yet — the
/// server's `GitFileTools` is still the live git write surface until M4.
/// Built + unit-tested here so M4's migration is a rewire, not a rewrite.
pub struct GitSubstrate {
    vault_path: PathBuf,
    commit_locks: Arc<CommitLocks>,
    commit_hook: Option<CommitHook>,
    flush_on_collision: Option<CasCollisionFlush>,
    include_ignored: bool,
    cached_repo: Option<CachedRepo>,
}

impl GitSubstrate {
    /// `include_ignored` mirrors `VaultGitConfig::include_ignored`
    /// (turbovault-lri) — `false` refuses a plan touching a gitignored path.
    pub fn new(vault_path: PathBuf, include_ignored: bool) -> Self {
        Self {
            vault_path,
            commit_locks: Arc::new(CommitLocks::new()),
            commit_hook: None,
            flush_on_collision: None,
            include_ignored,
            cached_repo: None,
        }
    }

    pub async fn apply(&self, plan: &ChangePlan) -> Result<ApplyOutcome> {
        let plan_owned = plan.clone();
        let include_ignored = self.include_ignored;
        // `VaultRepo` is `Send` but `!Sync`; the substrate work is blocking
        // libgit2, so it moves to the blocking pool (mirrors `apply_txn`).
        let result = match &self.cached_repo {
            Some(cached) => {
                let cached = Arc::clone(cached);
                tokio::task::spawn_blocking(move || {
                    let repo = cached
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    run_plan(&repo, &plan_owned, include_ignored)
                })
                .await
                .map_err(|e| Error::config_error(format!("git changeset task failed: {}", e)))?
            }
            None => {
                let vault_path = self.vault_path.clone();
                let locks = Arc::clone(&self.commit_locks);
                let hook = self.commit_hook.clone();
                tokio::task::spawn_blocking(move || {
                    let repo = match hook {
                        Some(h) => VaultRepo::open_with_locks_and_hook(&vault_path, locks, h),
                        None => VaultRepo::open_with_locks(&vault_path, locks),
                    }
                    .map_err(git_err_to_core)?;
                    run_plan(&repo, &plan_owned, include_ignored)
                })
                .await
                .map_err(|e| Error::config_error(format!("git changeset task failed: {}", e)))?
            }
        };

        // GWS.14b: on the reconsideration-domino abort, drain the reindex
        // queue BEFORE returning the error so a re-read sees coherent
        // derived state.
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

        let changeset = result?;
        // turbovault-4nc: an identity-tree write commits nothing — nothing
        // changed for the manager's graph/cache to sync either.
        let changed = if changeset.no_op {
            Vec::new()
        } else {
            changes_to_outcome(&plan.changes)
        };
        Ok(ApplyOutcome {
            changed,
            commit: Some(changeset.commit.to_string()),
            atomic: true,
            failed_at: None,
        })
    }
}

/// Run one plan against an already-open `repo`: the turbovault-lri gitignore
/// gate (when `include_ignored == false`), then `commit_changeset`. Ports
/// `git_file_tools::run_txn` verbatim.
fn run_plan(
    repo: &VaultRepo,
    plan: &ChangePlan,
    include_ignored: bool,
) -> Result<turbovault_git::ChangesetResult> {
    if !include_ignored {
        for changed in plan.touched_paths() {
            if repo.is_path_ignored(&changed).map_err(git_err_to_core)? {
                return Err(Error::config_error(format!(
                    "path '{}' is gitignored and include_ignored=false (turbovault-lri); enable include_ignored or add an exclusion in .gitignore",
                    changed
                )));
            }
        }
    }
    repo.commit_changeset(plan).map_err(git_err_to_core)
}

/// Translate a substrate error into the core error space. Ports
/// `git_file_tools::git_err_to_core` verbatim: "changed underneath us"
/// conflicts pass through as `ConcurrencyError`; everything else becomes a
/// `ConfigError` describing the substrate failure.
fn git_err_to_core(e: turbovault_git::Error) -> Error {
    match e {
        turbovault_git::Error::Core(turbovault_core::Error::ConcurrencyError { reason }) => {
            Error::ConcurrencyError { reason }
        }
        other => Error::config_error(format!("git substrate error: {}", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn direct(tmp: &TempDir) -> DirectSubstrate {
        DirectSubstrate::new(tmp.path().to_path_buf())
    }

    // -------- DirectSubstrate::apply — Upsert/Remove/Rename --------

    #[tokio::test]
    async fn upsert_creates_file_and_reports_present() {
        let tmp = TempDir::new().unwrap();
        let sub = direct(&tmp);
        let plan = ChangePlan::new("create").create("a.md", "alpha");

        let outcome = sub.apply(&plan).await.unwrap();

        assert_eq!(outcome.changed, vec![("a.md".to_string(), true)]);
        assert_eq!(
            tokio::fs::read_to_string(tmp.path().join("a.md"))
                .await
                .unwrap(),
            "alpha"
        );
    }

    #[tokio::test]
    async fn remove_deletes_file_and_reports_absent() {
        let tmp = TempDir::new().unwrap();
        let sub = direct(&tmp);
        sub.apply(&ChangePlan::new("seed").create("a.md", "alpha"))
            .await
            .unwrap();
        let hash = compute_hash("alpha");

        let outcome = sub
            .apply(&ChangePlan::new("delete").delete("a.md", hash))
            .await
            .unwrap();

        assert_eq!(outcome.changed, vec![("a.md".to_string(), false)]);
        assert!(!tmp.path().join("a.md").exists());
    }

    #[tokio::test]
    async fn rename_moves_content_and_reports_both_endpoints() {
        let tmp = TempDir::new().unwrap();
        let sub = direct(&tmp);
        sub.apply(&ChangePlan::new("seed").create("old.md", "body"))
            .await
            .unwrap();
        let hash = compute_hash("body");
        let plan = ChangePlan::new("move")
            .with_change(Change::Rename {
                from: "old.md".into(),
                to: "new.md".into(),
            })
            .with_precondition("old.md", Precondition::ExpectBlob(hash));

        let outcome = sub.apply(&plan).await.unwrap();

        assert_eq!(
            outcome.changed,
            vec![("old.md".to_string(), false), ("new.md".to_string(), true)]
        );
        assert!(!tmp.path().join("old.md").exists());
        assert_eq!(
            tokio::fs::read_to_string(tmp.path().join("new.md"))
                .await
                .unwrap(),
            "body"
        );
    }

    // -------- DirectSubstrate::apply — every Precondition variant --------

    #[tokio::test]
    async fn expect_blob_matching_hash_succeeds() {
        let tmp = TempDir::new().unwrap();
        let sub = direct(&tmp);
        sub.apply(&ChangePlan::new("seed").create("a.md", "v1"))
            .await
            .unwrap();
        let hash = compute_hash("v1");

        let outcome = sub
            .apply(&ChangePlan::new("update").update("a.md", "v2", hash))
            .await
            .unwrap();
        assert_eq!(outcome.changed, vec![("a.md".to_string(), true)]);
    }

    #[tokio::test]
    async fn expect_blob_stale_hash_aborts_nothing_applied() {
        let tmp = TempDir::new().unwrap();
        let sub = direct(&tmp);
        sub.apply(&ChangePlan::new("seed").create("a.md", "v1"))
            .await
            .unwrap();

        let err = sub
            .apply(&ChangePlan::new("update").update("a.md", "v2", "deadbeef"))
            .await
            .unwrap_err();

        assert!(matches!(err, Error::ConcurrencyError { .. }));
        assert_eq!(
            tokio::fs::read_to_string(tmp.path().join("a.md"))
                .await
                .unwrap(),
            "v1",
            "stale precondition must not apply the change"
        );
    }

    #[tokio::test]
    async fn expect_blob_against_absent_file_errors() {
        let tmp = TempDir::new().unwrap();
        let sub = direct(&tmp);

        let err = sub
            .apply(&ChangePlan::new("update").update("missing.md", "v2", "deadbeef"))
            .await
            .unwrap_err();

        assert!(matches!(err, Error::ConcurrencyError { .. }));
    }

    #[tokio::test]
    async fn expect_absent_on_existing_path_errors() {
        let tmp = TempDir::new().unwrap();
        let sub = direct(&tmp);
        sub.apply(&ChangePlan::new("seed").create("a.md", "v1"))
            .await
            .unwrap();

        let err = sub
            .apply(&ChangePlan::new("create").create("a.md", "v2"))
            .await
            .unwrap_err();

        assert!(matches!(err, Error::ConcurrencyError { .. }));
        assert_eq!(
            tokio::fs::read_to_string(tmp.path().join("a.md"))
                .await
                .unwrap(),
            "v1"
        );
    }

    #[tokio::test]
    async fn expect_absent_on_missing_path_succeeds() {
        let tmp = TempDir::new().unwrap();
        let sub = direct(&tmp);

        let outcome = sub
            .apply(&ChangePlan::new("create").create("a.md", "v1"))
            .await
            .unwrap();
        assert_eq!(outcome.changed, vec![("a.md".to_string(), true)]);
    }

    #[tokio::test]
    async fn expect_exists_on_missing_path_errors() {
        let tmp = TempDir::new().unwrap();
        let sub = direct(&tmp);
        let plan = ChangePlan::new("del")
            .remove("ghost.md")
            .with_precondition("ghost.md", Precondition::ExpectExists);

        let err = sub.apply(&plan).await.unwrap_err();
        assert!(matches!(err, Error::ConcurrencyError { .. }));
    }

    #[tokio::test]
    async fn expect_exists_on_present_path_succeeds() {
        let tmp = TempDir::new().unwrap();
        let sub = direct(&tmp);
        sub.apply(&ChangePlan::new("seed").create("a.md", "v1"))
            .await
            .unwrap();
        let plan = ChangePlan::new("del")
            .remove("a.md")
            .with_precondition("a.md", Precondition::ExpectExists);

        let outcome = sub.apply(&plan).await.unwrap();
        assert_eq!(outcome.changed, vec![("a.md".to_string(), false)]);
    }

    #[tokio::test]
    async fn blind_write_ignores_current_state() {
        let tmp = TempDir::new().unwrap();
        let sub = direct(&tmp);
        sub.apply(&ChangePlan::new("seed").create("a.md", "v1"))
            .await
            .unwrap();
        let plan = ChangePlan::new("blind")
            .upsert("a.md", "v2")
            .with_precondition("a.md", Precondition::Blind);

        let outcome = sub.apply(&plan).await.unwrap();
        assert_eq!(outcome.changed, vec![("a.md".to_string(), true)]);
        assert_eq!(
            tokio::fs::read_to_string(tmp.path().join("a.md"))
                .await
                .unwrap(),
            "v2"
        );
    }

    // -------- R6: the write lock serializes check -> apply --------

    /// Two concurrent `create` plans racing on the SAME absent path: without
    /// the write lock, both could observe "absent" before either writes
    /// (classic TOCTOU) and both would succeed, silently corrupting the
    /// no-clobber guarantee. With the lock serializing check-then-write,
    /// exactly one must win and the other must see `ExpectAbsent` fail.
    #[tokio::test]
    async fn write_lock_serializes_concurrent_creates_on_the_same_path() {
        let tmp = TempDir::new().unwrap();
        let sub = Arc::new(direct(&tmp));

        let a = {
            let sub = Arc::clone(&sub);
            tokio::spawn(async move {
                sub.apply(&ChangePlan::new("a").create("race.md", "from-a"))
                    .await
            })
        };
        let b = {
            let sub = Arc::clone(&sub);
            tokio::spawn(async move {
                sub.apply(&ChangePlan::new("b").create("race.md", "from-b"))
                    .await
            })
        };

        let (a, b) = tokio::join!(a, b);
        let (a, b) = (a.unwrap(), b.unwrap());

        assert_ne!(
            a.is_ok(),
            b.is_ok(),
            "exactly one racing create must win, got a={a:?} b={b:?}"
        );
        let winner_content = tokio::fs::read_to_string(tmp.path().join("race.md"))
            .await
            .unwrap();
        assert!(winner_content == "from-a" || winner_content == "from-b");
    }

    /// A real I/O failure on the precondition read (not "absent") must
    /// surface as `Error::Io`, not get masked as a `ConcurrencyError`
    /// claiming the file doesn't exist — regression for the `.ok()` bug
    /// that collapsed every read failure into "absent".
    #[cfg(unix)]
    #[tokio::test]
    async fn precondition_read_permission_denied_surfaces_as_io_error() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let sub = direct(&tmp);
        sub.apply(&ChangePlan::new("seed").create("locked.md", "secret"))
            .await
            .unwrap();
        let path = tmp.path().join("locked.md");
        let mut perms = tokio::fs::metadata(&path).await.unwrap().permissions();
        perms.set_mode(0o000);
        tokio::fs::set_permissions(&path, perms).await.unwrap();

        let plan = ChangePlan::new("del")
            .remove("locked.md")
            .with_precondition("locked.md", Precondition::ExpectExists);
        let err = sub.apply(&plan).await.unwrap_err();

        // Restore permissions so TempDir can clean up.
        let mut perms = tokio::fs::metadata(&path).await.unwrap().permissions();
        perms.set_mode(0o644);
        let _ = tokio::fs::set_permissions(&path, perms).await;

        assert!(
            matches!(err, Error::Io(_)),
            "expected Error::Io from an unreadable-but-present file, got {err:?}"
        );
    }

    // -------- GitSubstrate::apply — create + stale-precondition abort --------

    fn init_repo(dir: &std::path::Path) {
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        git2::Repository::init_opts(dir, &opts).unwrap();
    }

    #[tokio::test]
    async fn git_substrate_create_lands_one_commit() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let sub = GitSubstrate::new(tmp.path().to_path_buf(), true);

        let outcome = sub
            .apply(&ChangePlan::new("create a").create("a.md", "alpha"))
            .await
            .unwrap();

        assert!(outcome.commit.is_some());
        assert_eq!(outcome.changed, vec![("a.md".to_string(), true)]);
        assert_eq!(
            tokio::fs::read_to_string(tmp.path().join("a.md"))
                .await
                .unwrap(),
            "alpha"
        );
    }

    #[tokio::test]
    async fn git_substrate_stale_precondition_aborts_nothing_applied() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let sub = GitSubstrate::new(tmp.path().to_path_buf(), true);
        sub.apply(&ChangePlan::new("seed").create("a.md", "v1"))
            .await
            .unwrap();

        let stale = VaultRepo::blob_oid_of(b"stale").unwrap();
        let err = sub
            .apply(&ChangePlan::new("update").update("a.md", "v2", stale.to_string()))
            .await
            .unwrap_err();

        assert!(matches!(err, Error::ConcurrencyError { .. }));
        assert_eq!(
            tokio::fs::read_to_string(tmp.path().join("a.md"))
                .await
                .unwrap(),
            "v1",
            "stale precondition must not apply the change"
        );
    }
}
