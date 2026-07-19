//! Single-file write commit latency.
//!
//! Steady-state cost of `commit_changeset` for a one-file `create`: the full
//! pipeline (build_tree → commit_tree → ref CAS → materialize) under the commit
//! lock, on a born branch.

use criterion::{Criterion, criterion_group, criterion_main};
use std::sync::atomic::{AtomicU64, Ordering};
use tempfile::TempDir;
use turbovault_core::ChangePlan;
use turbovault_git::VaultRepo;

fn open_born_repo() -> (TempDir, VaultRepo) {
    let tmp = TempDir::new().unwrap();
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("main");
    git2::Repository::init_opts(tmp.path(), &opts).unwrap();
    let vr = VaultRepo::open(tmp.path()).unwrap();
    // Seed so HEAD is born; subsequent benches don't pay the initial-commit cost.
    vr.commit_changeset(&ChangePlan::new("seed").create("seed.md", "S"))
        .unwrap();
    (tmp, vr)
}

fn single_write_create(c: &mut Criterion) {
    let (_tmp, vr) = open_born_repo();
    let counter = AtomicU64::new(0);
    c.bench_function("commit_changeset/create_one", |b| {
        b.iter(|| {
            let n = counter.fetch_add(1, Ordering::Relaxed);
            let path = format!("file_{n}.md");
            vr.commit_changeset(&ChangePlan::new("c").create(path, "content"))
                .unwrap();
        });
    });
}

criterion_group!(benches, single_write_create);
criterion_main!(benches);
