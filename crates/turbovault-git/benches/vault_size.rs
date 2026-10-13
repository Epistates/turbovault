//! Single-note write latency against the size of the vault around it.
//!
//! A write touches one note, so its cost should not grow with the number of
//! notes it leaves alone. This measures one `update` in vaults of a few
//! sizes, each note about 10 KB, after the vault is committed and written out.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::sync::atomic::{AtomicU64, Ordering};
use tempfile::TempDir;
use turbovault_core::ChangePlan;
use turbovault_git::VaultRepo;

fn vault_of(notes: usize) -> (TempDir, VaultRepo) {
    let tmp = TempDir::new().unwrap();
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("main");
    git2::Repository::init_opts(tmp.path(), &opts).unwrap();
    let vr = VaultRepo::open(tmp.path()).unwrap();
    let body = "lorem ipsum dolor sit amet ".repeat(380);
    let mut plan = ChangePlan::new("seed");
    for i in 0..notes {
        plan = plan.create(
            format!("notes/{}/n{i}.md", i % 50),
            format!("# {i}\n{body}"),
        );
    }
    vr.commit_changeset(&plan).unwrap();
    (tmp, vr)
}

fn write_one_note(c: &mut Criterion) {
    let mut group = c.benchmark_group("commit_changeset/update_one");
    group.sample_size(10);
    for notes in [200, 2000] {
        let (_tmp, vr) = vault_of(notes);
        let counter = AtomicU64::new(0);
        group.bench_with_input(BenchmarkId::from_parameter(notes), &notes, |b, _| {
            b.iter(|| {
                let n = counter.fetch_add(1, Ordering::Relaxed);
                vr.commit_changeset(
                    &ChangePlan::new("edit").upsert("notes/0/n0.md", format!("edit {n}")),
                )
                .unwrap();
            });
        });
    }
    group.finish();
}

criterion_group!(benches, write_one_note);
criterion_main!(benches);
