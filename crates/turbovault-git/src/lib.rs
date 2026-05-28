//! # turbovault-git — git-native write substrate
//!
//! Every vault mutation is a git commit built from plumbing — blobs written to
//! the object DB, a tree assembled in an **isolated/ephemeral index** (never the
//! shared `.git/index`), a `commit-tree`, and a **compare-and-swap ref advance**
//! — then materialized into the working tree. Git history is the rollback/audit
//! log; `update-ref` CAS is the cross-process serialization primitive.
//!
//! Design: `git-write-substrate-architecture.md`. This crate replaces the
//! legacy `VaultManager` write path (mutators, batch executor, path-lock
//! registry, audit/snapshot rollback).
//!
//! Status: scaffolding. The GWS.0 spike below validates that `git2` covers the
//! primitives the substrate needs (in-memory index tree-building + ref CAS); the
//! real abstractions land in GWS.1–GWS.10.

#![forbid(unsafe_code)]

#[cfg(test)]
mod spike {
    //! GWS.0 spike: prove `git2` covers the substrate's core primitives, against
    //! throwaway temp repos. Not part of the public surface — validation only.

    use git2::{Index, IndexEntry, IndexTime, Oid, Repository, Signature};
    use std::path::Path;
    use tempfile::TempDir;

    /// Build a tree in an **isolated in-memory index** (not bound to `.git/index`):
    /// seed from `base` (an optional parent tree), stage `(path, content)` blobs,
    /// and write the tree to the repo's object DB. This is the plumbing the
    /// substrate uses to stage from the batch's own bytes, never the working tree.
    fn build_tree(repo: &Repository, base: Option<&git2::Tree>, files: &[(&str, &[u8])]) -> Oid {
        let mut index = Index::new().expect("in-memory index");
        if let Some(tree) = base {
            index.read_tree(tree).expect("seed index from base tree");
        }
        for (path, content) in files {
            let blob_oid = repo.blob(content).expect("write blob to ODB");
            let entry = IndexEntry {
                ctime: IndexTime::new(0, 0),
                mtime: IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode: 0o100_644,
                uid: 0,
                gid: 0,
                file_size: content.len() as u32,
                id: blob_oid,
                flags: 0,
                flags_extended: 0,
                path: path.as_bytes().to_vec(),
            };
            index.add(&entry).expect("stage entry into in-memory index");
        }
        index.write_tree_to(repo).expect("write tree to ODB")
    }

    /// Compare-and-swap a branch ref: advance `refname` from `expected_old` to
    /// `new` atomically, under git's ref lock. Mirrors `update-ref <new> <old>`.
    /// `expected_old == None` means the ref must not yet exist.
    fn cas_ref(
        repo: &Repository,
        refname: &str,
        expected_old: Option<Oid>,
        new: Oid,
    ) -> Result<(), String> {
        let mut tx = repo.transaction().expect("begin ref transaction");
        tx.lock_ref(refname).expect("lock ref");
        // Read the current value *under the lock* — this is the CAS comparison.
        let current = repo.refname_to_id(refname).ok();
        if current != expected_old {
            // Drop the transaction (releases the lock) without committing.
            return Err(format!(
                "CAS conflict on {refname}: expected {expected_old:?}, found {current:?}"
            ));
        }
        tx.set_target(refname, new, None, "turbovault-git: cas advance")
            .expect("set ref target");
        tx.commit().expect("commit ref transaction");
        Ok(())
    }

    fn init_repo(dir: &Path) -> Repository {
        Repository::init(dir).expect("init repo")
    }

    fn sig() -> Signature<'static> {
        Signature::now("TurboVault", "turbovault@localhost").expect("signature")
    }

    #[test]
    fn spike_in_memory_index_commit_and_ref_cas() {
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(tmp.path());
        let refname = "refs/heads/main";

        // Initial commit: one file, no parents. Points the branch ref.
        let t1 = build_tree(&repo, None, &[("a.md", b"alpha")]);
        let tree1 = repo.find_tree(t1).unwrap();
        let c1 = repo
            .commit(Some(refname), &sig(), &sig(), "init", &tree1, &[])
            .expect("initial commit");
        assert_eq!(repo.refname_to_id(refname).unwrap(), c1);

        // Second commit: seed the in-memory index from c1's tree, add a new file
        // (a.md preserved, b.md added). Build the commit object WITHOUT moving the
        // ref, then advance via CAS (expected old = c1).
        let parent = repo.find_commit(c1).unwrap();
        let t2 = build_tree(&repo, Some(&parent.tree().unwrap()), &[("b.md", b"beta")]);
        let tree2 = repo.find_tree(t2).unwrap();
        let c2 = repo
            .commit(None, &sig(), &sig(), "add b", &tree2, &[&parent])
            .expect("second commit object (no ref update)");

        cas_ref(&repo, refname, Some(c1), c2).expect("CAS c1 -> c2 must succeed");
        assert_eq!(repo.refname_to_id(refname).unwrap(), c2);

        // The committed tree carries BOTH files (seed-from-parent worked).
        let head_tree = repo.find_commit(c2).unwrap().tree().unwrap();
        assert!(
            head_tree.get_path(Path::new("a.md")).is_ok(),
            "a.md preserved"
        );
        assert!(head_tree.get_path(Path::new("b.md")).is_ok(), "b.md added");

        // Blob content addressable + retrievable from the tree.
        let b_entry = head_tree.get_path(Path::new("b.md")).unwrap();
        let blob = repo.find_blob(b_entry.id()).unwrap();
        assert_eq!(blob.content(), b"beta");
    }

    #[test]
    fn spike_ref_cas_rejects_stale_expected() {
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(tmp.path());
        let refname = "refs/heads/main";

        let t1 = build_tree(&repo, None, &[("a.md", b"alpha")]);
        let tree1 = repo.find_tree(t1).unwrap();
        let c1 = repo
            .commit(Some(refname), &sig(), &sig(), "init", &tree1, &[])
            .unwrap();

        // Build a competing commit but attempt to CAS with a WRONG expected-old
        // (a bogus oid, as if another writer had advanced the ref). Must reject,
        // and the ref must stay at c1 (nothing applied).
        let parent = repo.find_commit(c1).unwrap();
        let t2 = build_tree(&repo, Some(&parent.tree().unwrap()), &[("b.md", b"beta")]);
        let tree2 = repo.find_tree(t2).unwrap();
        let c2 = repo
            .commit(None, &sig(), &sig(), "add b", &tree2, &[&parent])
            .unwrap();

        let bogus_expected = Oid::from_str("0000000000000000000000000000000000000001").unwrap();
        let result = cas_ref(&repo, refname, Some(bogus_expected), c2);
        assert!(result.is_err(), "stale expected-old must reject");
        assert_eq!(
            repo.refname_to_id(refname).unwrap(),
            c1,
            "ref must stay at c1 — nothing applied on CAS reject"
        );
    }
}
