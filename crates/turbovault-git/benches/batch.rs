//! Multi-file batch commit throughput.
//!
//! Cost of `apply_transaction` carrying N `create` ops in one atomic commit —
//! the case the legacy batch was supposed to be transactional about.

use criterion::{Criterion, criterion_group, criterion_main};
use std::sync::atomic::{AtomicU64, Ordering};
use tempfile::TempDir;
use turbovault_git::{Transaction, VaultRepo};

fn open_born_repo() -> (TempDir, VaultRepo) {
    let tmp = TempDir::new().unwrap();
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("main");
    git2::Repository::init_opts(tmp.path(), &opts).unwrap();
    let vr = VaultRepo::open(tmp.path()).unwrap();
    vr.apply_transaction(&Transaction::new("seed").create("seed.md", "S"))
        .unwrap();
    (tmp, vr)
}

fn batch_create(c: &mut Criterion) {
    let mut group = c.benchmark_group("apply_transaction/batch_create");
    for &n in &[1u64, 5, 10, 50] {
        let (_tmp, vr) = open_born_repo();
        let counter = AtomicU64::new(0);
        group.bench_function(format!("n={n}"), |b| {
            b.iter(|| {
                let base = counter.fetch_add(n, Ordering::Relaxed);
                let mut txn = Transaction::new("batch");
                for i in 0..n {
                    let path = format!("file_{}.md", base + i);
                    txn = txn.create(path, "content");
                }
                vr.apply_transaction(&txn).unwrap();
            });
        });
    }
    group.finish();
}

criterion_group!(benches, batch_create);
criterion_main!(benches);
