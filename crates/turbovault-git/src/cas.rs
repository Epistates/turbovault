//! Ref compare-and-swap + optimistic rebuild-on-conflict (GWS.3).
//!
//! `cas_ref` is the serialization primitive: it advances a branch ref from an
//! expected old value to a new commit **under git's ref lock**, mirroring
//! `git update-ref <new> <old>`. It is atomic and **cross-process** (the lock is
//! a lockfile in `.git`), which is the property in-process mutexes cannot give.
//!
//! `commit_with_retry` is the optimistic loop: build a commit on the current
//! tip, CAS the ref; if a concurrent writer advanced it first, re-read the tip,
//! rebuild on the new tip, and retry. The caller's builder re-runs its per-file
//! preconditions (GWS.4) on each rebuild, so a conflicting change to one of the
//! transaction's own paths surfaces as an abort rather than a silent overwrite.

use crate::error::{Error, Result};
use crate::repo::VaultRepo;
use git2::Oid;
use tracing::instrument;

/// How many times `commit_with_retry` rebuilds before giving up. Contention is
/// rare; this only guards against pathological live-lock.
const DEFAULT_MAX_RETRIES: u32 = 8;

impl VaultRepo {
    /// Atomically advance `refname` from `expected_old` to `new`, under git's
    /// ref lock (mirrors `update-ref <new> <old>`).
    ///
    /// `expected_old == None` means the ref must **not** yet exist (the
    /// initial-commit case). On any mismatch returns [`Error::CasConflict`] with
    /// **nothing applied** — the ref is untouched.
    #[instrument(
        skip(self),
        fields(refname = %refname, expected = ?expected_old, new = %new),
        name = "git_cas_ref"
    )]
    pub fn cas_ref(&self, refname: &str, expected_old: Option<Oid>, new: Oid) -> Result<()> {
        let repo = self.git();
        let mut tx = repo.transaction()?;
        tx.lock_ref(refname)?;
        // Read the current value *under the lock* — this is the CAS comparison.
        let current = repo.refname_to_id(refname).ok();
        if current != expected_old {
            // Dropping `tx` here releases the lock without committing.
            return Err(Error::CasConflict {
                refname: refname.to_string(),
                expected: expected_old,
                found: current,
            });
        }
        tx.set_target(refname, new, None, "turbovault-git: cas advance")?;
        tx.commit()?;
        Ok(())
    }

    /// Advance `refname` with optimistic retry (default retry budget).
    /// See [`Self::commit_with_retry_n`].
    pub fn commit_with_retry<F>(&self, refname: &str, build: F) -> Result<Option<Oid>>
    where
        F: FnMut(Option<Oid>) -> Result<Option<Oid>>,
    {
        self.commit_with_retry_n(refname, DEFAULT_MAX_RETRIES, build)
    }

    /// Advance `refname` with optimistic retry. `build` is called with the
    /// current tip (the parent to build on, `None` if the branch is unborn) and
    /// returns `Some(commit)` to CAS onto that tip, or `None` to signal a
    /// **no-op** — there is nothing to commit (e.g. the resulting tree is
    /// identical to the base), so the ref is left untouched and the method
    /// returns `Ok(None)`. If the CAS loses to a concurrent advance, the tip is
    /// re-read and `build` is called again on the new tip, up to `max_retries`
    /// rebuilds.
    ///
    /// The builder owns conflict policy: on a rebuild it re-validates its
    /// per-file preconditions against the new tip (GWS.4) and may itself return
    /// an error to abort (the reconsideration domino) instead of rebuilding.
    #[instrument(
        skip(self, build),
        fields(refname = %refname, max_retries),
        name = "git_commit_with_retry"
    )]
    pub fn commit_with_retry_n<F>(
        &self,
        refname: &str,
        max_retries: u32,
        mut build: F,
    ) -> Result<Option<Oid>>
    where
        F: FnMut(Option<Oid>) -> Result<Option<Oid>>,
    {
        for _ in 0..=max_retries {
            let tip = self.git().refname_to_id(refname).ok();
            // `None` from the builder = no-op (e.g. an identity tree): nothing
            // to commit, so skip the CAS and leave the ref where it is.
            let new = match build(tip)? {
                Some(oid) => oid,
                None => return Ok(None),
            };
            match self.cas_ref(refname, tip, new) {
                Ok(()) => return Ok(Some(new)),
                // Lost the race: the ref moved between our read and the lock.
                // Re-read the tip and rebuild on it.
                Err(Error::CasConflict { .. }) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(Error::Other(format!(
            "ref CAS exhausted {max_retries} retries on {refname} (excessive contention)"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plumbing::TreeChange;
    use git2::Repository;
    use std::cell::Cell;
    use tempfile::TempDir;

    const MAIN: &str = "refs/heads/main";

    fn open_unborn() -> (TempDir, VaultRepo) {
        let tmp = TempDir::new().unwrap();
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        Repository::init_opts(tmp.path(), &opts).unwrap();
        let vr = VaultRepo::open(tmp.path()).unwrap();
        (tmp, vr)
    }

    fn upsert(path: &str, content: &str) -> TreeChange {
        TreeChange::Upsert {
            path: path.to_string(),
            content: content.as_bytes().to_vec(),
        }
    }

    /// Build a commit on `parent` (or initial if None) carrying one upsert.
    fn build_on(vr: &VaultRepo, parent: Option<Oid>, path: &str, content: &str) -> Oid {
        let base = parent.map(|p| vr.git().find_commit(p).unwrap().tree_id());
        let tree = vr.build_tree(base, &[upsert(path, content)]).unwrap();
        let parents: Vec<Oid> = parent.into_iter().collect();
        vr.commit_tree(tree, &parents, "c").unwrap()
    }

    #[test]
    fn cas_ref_initial_then_advance() {
        let (_tmp, vr) = open_unborn();
        let c0 = build_on(&vr, None, "a.md", "a");
        vr.cas_ref(MAIN, None, c0)
            .expect("initial CAS (None -> c0)");
        assert_eq!(vr.head_oid(), Some(c0));

        let c1 = build_on(&vr, Some(c0), "b.md", "b");
        vr.cas_ref(MAIN, Some(c0), c1).expect("advance c0 -> c1");
        assert_eq!(vr.head_oid(), Some(c1));
    }

    #[test]
    fn cas_ref_rejects_stale_and_leaves_ref() {
        let (_tmp, vr) = open_unborn();
        let c0 = build_on(&vr, None, "a.md", "a");
        vr.cas_ref(MAIN, None, c0).unwrap();

        let bogus = Oid::from_str("0000000000000000000000000000000000000001").unwrap();
        let c1 = build_on(&vr, Some(c0), "b.md", "b");
        match vr.cas_ref(MAIN, Some(bogus), c1) {
            Err(Error::CasConflict { found, .. }) => assert_eq!(found, Some(c0)),
            other => panic!("expected CasConflict, got {other:?}"),
        }
        assert_eq!(vr.head_oid(), Some(c0), "ref unchanged on reject");
    }

    #[test]
    fn cas_ref_initial_rejects_when_ref_exists() {
        let (_tmp, vr) = open_unborn();
        let c0 = build_on(&vr, None, "a.md", "a");
        vr.cas_ref(MAIN, None, c0).unwrap();
        // expected_old = None means "must not exist", but it does now.
        let c1 = build_on(&vr, Some(c0), "b.md", "b");
        assert!(matches!(
            vr.cas_ref(MAIN, None, c1),
            Err(Error::CasConflict { .. })
        ));
    }

    /// turbovault-a0l (PERF-1 safety guard): a REUSED `VaultRepo` handle must
    /// still observe a ref advance made by a DIFFERENT handle (another process)
    /// under `lock_ref`. If libgit2's refdb served a stale cached tip, handle A
    /// would clobber B's commit — a cross-process lost update, the exact failure
    /// the substrate exists to prevent. This is the pivotal correctness question
    /// for caching the repo handle (PERF-1): if it fails, caching is unsafe.
    #[test]
    fn reused_handle_detects_external_ref_advance_no_lost_update() {
        let (tmp, vr_a) = open_unborn();
        // A makes the initial commit (populates A's refdb with c0).
        let c0 = build_on(&vr_a, None, "a.md", "v1");
        vr_a.cas_ref(MAIN, None, c0).unwrap();
        assert_eq!(vr_a.head_oid(), Some(c0));

        // B = a SEPARATE handle (mimics another process) advances main.
        let vr_b = VaultRepo::open(tmp.path()).unwrap();
        let c1 = build_on(&vr_b, Some(c0), "b.md", "from-B");
        vr_b.cas_ref(MAIN, Some(c0), c1).unwrap();

        // A, REUSING its handle, advances main. commit_with_retry reads the tip
        // and CAS-locks; correct behavior is to see c1 and commit on top of it,
        // never clobber it from a stale c0.
        let got = vr_a
            .commit_with_retry(MAIN, |tip| Ok(Some(build_on(&vr_a, tip, "c.md", "from-A"))))
            .unwrap()
            .expect("a commit was produced");
        let parent = vr_a.git().find_commit(got).unwrap().parent_id(0).unwrap();
        assert_eq!(
            parent, c1,
            "reused handle committed atop B's external advance (saw the ref change; no lost update)"
        );
        assert!(
            vr_a.git().find_commit(c1).is_ok(),
            "B's commit is still reachable, not clobbered"
        );
    }

    #[test]
    fn commit_with_retry_no_contention() {
        let (_tmp, vr) = open_unborn();
        let c0 = build_on(&vr, None, "a.md", "a");
        vr.cas_ref(MAIN, None, c0).unwrap();

        let got = vr
            .commit_with_retry(MAIN, |tip| Ok(Some(build_on(&vr, tip, "b.md", "b"))))
            .unwrap()
            .expect("a commit was produced");
        assert_eq!(vr.head_oid(), Some(got));
    }

    #[test]
    fn commit_with_retry_rebuilds_on_conflict() {
        let (_tmp, vr) = open_unborn();
        let c0 = build_on(&vr, None, "a.md", "a");
        vr.cas_ref(MAIN, None, c0).unwrap();

        let calls = Cell::new(0u32);
        let got = vr
            .commit_with_retry(MAIN, |tip| {
                calls.set(calls.get() + 1);
                let tip = tip.unwrap();
                // On the FIRST attempt only, a concurrent writer advances the ref
                // behind our back so our CAS must lose and rebuild.
                if calls.get() == 1 {
                    let concurrent = build_on(&vr, Some(tip), "concurrent.md", "x");
                    vr.cas_ref(MAIN, Some(tip), concurrent).unwrap();
                }
                Ok(Some(build_on(&vr, Some(tip), "mine.md", "m")))
            })
            .unwrap()
            .expect("a commit was produced");

        assert_eq!(calls.get(), 2, "exactly one rebuild after the conflict");
        assert_eq!(vr.head_oid(), Some(got));
        // We rebuilt on the concurrent tip, so the final tree carries BOTH files.
        let head_tree = vr.git().find_commit(got).unwrap().tree_id();
        assert!(
            vr.blob_oid_at(head_tree, "concurrent.md")
                .unwrap()
                .is_some()
        );
        assert!(vr.blob_oid_at(head_tree, "mine.md").unwrap().is_some());
    }

    /// turbovault-uag: relentless contention exhausts the retry budget and
    /// surfaces a loud error (the live-lock guard) rather than spinning forever
    /// or silently giving up. Every attempt loses the CAS because a concurrent
    /// writer advances the ref first.
    #[test]
    fn commit_with_retry_exhausts_under_relentless_contention() {
        let (_tmp, vr) = open_unborn();
        let c0 = build_on(&vr, None, "a.md", "a");
        vr.cas_ref(MAIN, None, c0).unwrap();

        let calls = Cell::new(0u32);
        let err = vr
            .commit_with_retry_n(MAIN, 2, |tip| {
                calls.set(calls.get() + 1);
                let tip = tip.unwrap();
                // Advance the ref behind our back BEFORE our CAS, every attempt.
                let concurrent = build_on(&vr, Some(tip), &format!("c{}.md", calls.get()), "x");
                vr.cas_ref(MAIN, Some(tip), concurrent).unwrap();
                Ok(Some(build_on(&vr, Some(tip), "mine.md", "m")))
            })
            .unwrap_err();

        // max_retries=2 -> the loop runs 0..=2 = 3 attempts, all lose.
        assert_eq!(
            calls.get(),
            3,
            "builder runs max_retries+1 times then gives up"
        );
        assert!(
            err.to_string().contains("exhausted") && err.to_string().contains("contention"),
            "loud exhaustion error: {err}"
        );
    }
}
