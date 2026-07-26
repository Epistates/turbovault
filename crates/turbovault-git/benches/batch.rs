//! Multi-file batch commit throughput.
//!
//! Cost of `commit_changeset` carrying N `create` ops in one atomic commit —
//! the case the direct batch was supposed to be transactional about.

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
    vr.commit_changeset(&ChangePlan::new("seed").create("seed.md", "S"))
        .unwrap();
    (tmp, vr)
}

fn batch_create(c: &mut Criterion) {
    let mut group = c.benchmark_group("commit_changeset/batch_create");
    for &n in &[1u64, 5, 10, 50] {
        let (_tmp, vr) = open_born_repo();
        let counter = AtomicU64::new(0);
        group.bench_function(format!("n={n}"), |b| {
            b.iter(|| {
                let base = counter.fetch_add(n, Ordering::Relaxed);
                let mut txn = ChangePlan::new("batch");
                for i in 0..n {
                    let path = format!("file_{}.md", base + i);
                    txn = txn.create(path, "content");
                }
                vr.commit_changeset(&txn).unwrap();
            });
        });
    }
    group.finish();
}

criterion_group!(benches, batch_create);
criterion_main!(benches);
