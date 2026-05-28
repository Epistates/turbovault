//! Per-worktree commit lock (GWS.6).
//!
//! Git's index and HEAD are **shared mutable state per worktree**: two
//! concurrent commit+checkout sequences on the same worktree race on that
//! worktree's `index.lock`. This is the *only* lock the git substrate needs —
//! it replaces the legacy per-path lock registry, because the lost-update
//! problem the per-path locks solved is now caught by the ref CAS (GWS.3),
//! cross-process. What remains is purely intra-process serialization of the
//! commit critical section, and that is coarse (one mutex per worktree), not
//! per-path.
//!
//! Different worktrees (the main vault and fan-out scratch worktrees, GWS.9)
//! have independent index/HEAD, so they never contend — they get distinct
//! locks. A single shared [`CommitLocks`] registry, keyed by worktree, lets all
//! [`VaultRepo`](crate::VaultRepo) handles for the same worktree serialize on
//! one mutex.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// A process-wide registry of per-worktree commit mutexes. Share one `Arc`
/// across every `VaultRepo` so handles to the same worktree serialize.
#[derive(Default)]
pub struct CommitLocks {
    locks: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

impl CommitLocks {
    pub fn new() -> Self {
        Self::default()
    }

    /// The commit mutex for the worktree identified by `key` (its canonical
    /// workdir). Returns the same `Arc<Mutex>` for repeated calls with the same
    /// key, and distinct mutexes for distinct keys.
    pub(crate) fn mutex_for(&self, key: &Path) -> Arc<Mutex<()>> {
        // Canonicalize so different spellings of the same worktree share a lock;
        // fall back to the raw path if the dir can't be canonicalized.
        let key = key.canonicalize().unwrap_or_else(|_| key.to_path_buf());
        let mut map = self
            .locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        map.entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

/// Lock `mutex`, recovering from poisoning (a panic in a prior holder shouldn't
/// permanently wedge a worktree's commit path).
pub(crate) fn lock_recover(mutex: &Mutex<()>) -> MutexGuard<'_, ()> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn same_key_shares_mutex_distinct_keys_differ() {
        let a = TempDir::new().unwrap();
        let b = TempDir::new().unwrap();
        let locks = CommitLocks::new();

        let a1 = locks.mutex_for(a.path());
        let a2 = locks.mutex_for(a.path());
        let b1 = locks.mutex_for(b.path());

        assert!(Arc::ptr_eq(&a1, &a2), "same worktree -> same mutex");
        assert!(
            !Arc::ptr_eq(&a1, &b1),
            "distinct worktrees -> distinct mutexes"
        );
    }

    #[test]
    fn mutex_provides_mutual_exclusion() {
        let dir = TempDir::new().unwrap();
        let locks = CommitLocks::new();
        let m = locks.mutex_for(dir.path());

        let held = lock_recover(&m);
        assert!(m.try_lock().is_err(), "a held commit lock blocks re-entry");
        drop(held);
        assert!(m.try_lock().is_ok(), "released lock is re-acquirable");
    }

    #[test]
    fn lock_recover_survives_poison() {
        let dir = TempDir::new().unwrap();
        let locks = CommitLocks::new();
        let m = locks.mutex_for(dir.path());

        // Poison the mutex by panicking while holding it.
        let m2 = Arc::clone(&m);
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("poison");
        })
        .join();

        // lock_recover still yields the guard despite poisoning.
        let _g = lock_recover(&m);
    }
}
