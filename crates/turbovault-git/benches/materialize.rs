//! Working-tree materialization cost.
//!
//! Cost of `materialize` (atomic temp+rename per file + index sync) for a
//! committed tip, varied across N paths. Isolates the checkout step from the
//! commit-object step.

use criterion::{Criterion, criterion_group, criterion_main};
use tempfile::TempDir;
use turbovault_git::{Transaction, VaultRepo};

fn setup_repo_with_n_files(n: usize) -> (TempDir, VaultRepo, git2::Oid, Vec<String>) {
    let tmp = TempDir::new().unwrap();
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("main");
    git2::Repository::init_opts(tmp.path(), &opts).unwrap();
    let vr = VaultRepo::open(tmp.path()).unwrap();
    let mut txn = Transaction::new("seed");
    let mut paths = Vec::with_capacity(n);
    for i in 0..n {
        let p = format!("file_{i}.md");
        txn = txn.create(p.clone(), format!("content_{i}"));
        paths.push(p);
    }
    let res = vr.apply_transaction(&txn).unwrap();
    (tmp, vr, res.commit, paths)
}

fn materialize_n(c: &mut Criterion) {
    let mut group = c.benchmark_group("materialize");
    for &n in &[1usize, 10, 50] {
        let (_tmp, vr, commit, paths) = setup_repo_with_n_files(n);
        // Already materialized once by apply_transaction; subsequent calls are
        // the steady-state idempotent rewrite path.
        group.bench_function(format!("n={n}"), |b| {
            b.iter(|| vr.materialize(commit, &paths).unwrap());
        });
    }
    group.finish();
}

criterion_group!(benches, materialize_n);
criterion_main!(benches);
