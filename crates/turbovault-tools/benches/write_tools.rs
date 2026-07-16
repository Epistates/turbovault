//! Baseline benchmarks comparing the legacy `VaultManager`-backed write path
//! against the new git substrate path, both routed through `WriteTools`.
//!
//! Goal: capture **before-cutover** numbers so the GWS.15 decision is
//! grounded in measured cost rather than vibes. Run with
//! `cargo bench -p turbovault-tools --bench write_tools` — criterion writes
//! HTML reports under `target/criterion/`.
//!
//! Bench layout (each scenario benchmarked once per backend):
//! - `write_file_1kb` / `write_file_100kb` — single-file writes at two sizes.
//! - `edit_file` — SEARCH/REPLACE on a 1KB file.
//! - `delete_file` — single delete from a pre-seeded file.
//! - `move_file` — single rename.
//! - `batch_5_creates` / `batch_50_creates` — atomic batch at two scales.
//!
//! `write_file_1kb`/`write_file_100kb`/`batch_*` also carry a `git_cached` arm
//! (turbovault-a0l / PERF-1): the cached `VaultRepo` handle vs a per-op
//! `Repository::open`. The `git` → `git_cached` delta isolates the PERF-1 win
//! (~−16% on a single 1KB write; batch is open-amortized so the delta shrinks).
//!
//! Caveats:
//! - Tempdir per iteration (high setup cost outside the measured region).
//!   Use criterion's `iter_batched` to keep the measured closure tight.
//! - Single-threaded tokio runtime; no MCP-server overhead.
//! - The legacy `batch_execute` is **not** transactional — partial state on
//!   failure is the known defect; happy-path numbers only.

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::runtime::Runtime;
use turbovault_batch::BatchOperation;
use turbovault_core::config::{ServerConfig, VaultConfig};
use turbovault_git::VaultRepo;
use turbovault_tools::{CachedRepo, CommitLocks, WriteTools};
use turbovault_vault::VaultManager;

/// tlx.10/[12]: one process-wide runtime, built once and reused, so
/// `rt().block_on(...)` inside the measured `iter_batched` closures no longer
/// pays runtime startup/teardown on every sample. Returning `&'static Runtime`
/// keeps all 21 call sites unchanged (`block_on` takes `&self`).
fn rt() -> &'static Runtime {
    static RT: std::sync::OnceLock<Runtime> = std::sync::OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    })
}

fn test_server_config(vault_dir: &std::path::Path) -> ServerConfig {
    let mut cfg = ServerConfig::new();
    cfg.vaults
        .push(VaultConfig::builder("b", vault_dir).build().unwrap());
    cfg
}

struct Fixture {
    _tmp: TempDir,
    tools: WriteTools,
}

fn legacy_fixture() -> Fixture {
    let tmp = TempDir::new().unwrap();
    let manager = Arc::new(VaultManager::new(test_server_config(tmp.path())).unwrap());
    let tools = WriteTools::legacy(manager);
    Fixture { _tmp: tmp, tools }
}

fn git_fixture() -> Fixture {
    let tmp = TempDir::new().unwrap();
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("main");
    git2::Repository::init_opts(tmp.path(), &opts).unwrap();
    let manager = Arc::new(VaultManager::new(test_server_config(tmp.path())).unwrap());
    let locks = Arc::new(CommitLocks::new());
    let tools = WriteTools::git(manager, tmp.path().to_path_buf(), locks);
    Fixture { _tmp: tmp, tools }
}

/// turbovault-a0l (PERF-1): like `git_fixture`, but installs the server-style
/// CACHED `VaultRepo` handle so writes reuse it instead of opening a fresh repo
/// per op. The `git` vs `git_cached` delta is exactly the PERF-1 win.
fn git_cached_fixture() -> Fixture {
    let tmp = TempDir::new().unwrap();
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("main");
    git2::Repository::init_opts(tmp.path(), &opts).unwrap();
    let manager = Arc::new(VaultManager::new(test_server_config(tmp.path())).unwrap());
    let locks = Arc::new(CommitLocks::new());
    let repo = VaultRepo::open_with_locks(tmp.path(), Arc::clone(&locks)).unwrap();
    let cached: CachedRepo = Arc::new(std::sync::Mutex::new(repo));
    let tools = WriteTools::git(manager, tmp.path().to_path_buf(), locks).with_cached_repo(cached);
    Fixture { _tmp: tmp, tools }
}

fn body(size: usize) -> String {
    "x".repeat(size)
}

// ---- write_file ----

fn bench_write_file_1kb(c: &mut Criterion) {
    let mut g = c.benchmark_group("write_file_1kb");
    let content = body(1024);
    g.bench_function("legacy", |b| {
        b.iter_batched(
            legacy_fixture,
            |f| {
                rt().block_on(async {
                    f.tools.write_file("a.md", &content).await.unwrap();
                });
            },
            BatchSize::SmallInput,
        );
    });
    g.bench_function("git", |b| {
        b.iter_batched(
            git_fixture,
            |f| {
                rt().block_on(async {
                    f.tools.write_file("a.md", &content).await.unwrap();
                });
            },
            BatchSize::SmallInput,
        );
    });
    g.bench_function("git_cached", |b| {
        b.iter_batched(
            git_cached_fixture,
            |f| {
                rt().block_on(async {
                    f.tools.write_file("a.md", &content).await.unwrap();
                });
            },
            BatchSize::SmallInput,
        );
    });
    g.finish();
}

fn bench_write_file_100kb(c: &mut Criterion) {
    let mut g = c.benchmark_group("write_file_100kb");
    let content = body(100 * 1024);
    g.bench_function("legacy", |b| {
        b.iter_batched(
            legacy_fixture,
            |f| {
                rt().block_on(async {
                    f.tools.write_file("a.md", &content).await.unwrap();
                });
            },
            BatchSize::SmallInput,
        );
    });
    g.bench_function("git", |b| {
        b.iter_batched(
            git_fixture,
            |f| {
                rt().block_on(async {
                    f.tools.write_file("a.md", &content).await.unwrap();
                });
            },
            BatchSize::SmallInput,
        );
    });
    g.bench_function("git_cached", |b| {
        b.iter_batched(
            git_cached_fixture,
            |f| {
                rt().block_on(async {
                    f.tools.write_file("a.md", &content).await.unwrap();
                });
            },
            BatchSize::SmallInput,
        );
    });
    g.finish();
}

// ---- edit_file ----

fn bench_edit_file(c: &mut Criterion) {
    let mut g = c.benchmark_group("edit_file");
    let edits = "<<<<<<< SEARCH\nhello world\n=======\nhi world\n>>>>>>> REPLACE\n";

    fn seeded_legacy() -> Fixture {
        let f = legacy_fixture();
        rt().block_on(async {
            f.tools.write_file("a.md", "hello world\n").await.unwrap();
        });
        f
    }
    fn seeded_git() -> Fixture {
        let f = git_fixture();
        rt().block_on(async {
            f.tools.write_file("a.md", "hello world\n").await.unwrap();
        });
        f
    }

    g.bench_function("legacy", |b| {
        b.iter_batched(
            seeded_legacy,
            |f| {
                rt().block_on(async {
                    f.tools.edit_file("a.md", edits, None, false).await.unwrap();
                });
            },
            BatchSize::SmallInput,
        );
    });
    g.bench_function("git", |b| {
        b.iter_batched(
            seeded_git,
            |f| {
                rt().block_on(async {
                    f.tools.edit_file("a.md", edits, None, false).await.unwrap();
                });
            },
            BatchSize::SmallInput,
        );
    });
    g.finish();
}

// ---- delete_file ----

fn bench_delete_file(c: &mut Criterion) {
    let mut g = c.benchmark_group("delete_file");

    fn seeded_legacy() -> Fixture {
        let f = legacy_fixture();
        rt().block_on(async {
            f.tools.write_file("a.md", "body").await.unwrap();
        });
        f
    }
    fn seeded_git() -> Fixture {
        let f = git_fixture();
        rt().block_on(async {
            f.tools.write_file("a.md", "body").await.unwrap();
        });
        f
    }

    g.bench_function("legacy", |b| {
        b.iter_batched(
            seeded_legacy,
            |f| {
                rt().block_on(async {
                    f.tools.delete_file("a.md").await.unwrap();
                });
            },
            BatchSize::SmallInput,
        );
    });
    g.bench_function("git", |b| {
        b.iter_batched(
            seeded_git,
            |f| {
                rt().block_on(async {
                    f.tools.delete_file("a.md").await.unwrap();
                });
            },
            BatchSize::SmallInput,
        );
    });
    g.finish();
}

// ---- move_file ----

fn bench_move_file(c: &mut Criterion) {
    let mut g = c.benchmark_group("move_file");

    fn seeded_legacy() -> Fixture {
        let f = legacy_fixture();
        rt().block_on(async {
            f.tools.write_file("a.md", "body").await.unwrap();
        });
        f
    }
    fn seeded_git() -> Fixture {
        let f = git_fixture();
        rt().block_on(async {
            f.tools.write_file("a.md", "body").await.unwrap();
        });
        f
    }

    g.bench_function("legacy", |b| {
        b.iter_batched(
            seeded_legacy,
            |f| {
                rt().block_on(async {
                    f.tools.move_file("a.md", "b.md").await.unwrap();
                });
            },
            BatchSize::SmallInput,
        );
    });
    g.bench_function("git", |b| {
        b.iter_batched(
            seeded_git,
            |f| {
                rt().block_on(async {
                    f.tools.move_file("a.md", "b.md").await.unwrap();
                });
            },
            BatchSize::SmallInput,
        );
    });
    g.finish();
}

// ---- batch ----

fn batch_ops(n: usize) -> Vec<BatchOperation> {
    (0..n)
        .map(|i| BatchOperation::CreateNote {
            path: format!("note_{i}.md"),
            content: format!("body {i}"),
            force: None,
        })
        .collect()
}

fn bench_batch(c: &mut Criterion, name: &str, n: usize) {
    let mut g = c.benchmark_group(name);
    g.bench_function("legacy", |b| {
        b.iter_batched(
            legacy_fixture,
            |f| {
                rt().block_on(async {
                    let res = f.tools.batch_execute(batch_ops(n)).await.unwrap();
                    assert!(res.success);
                });
            },
            BatchSize::SmallInput,
        );
    });
    g.bench_function("git", |b| {
        b.iter_batched(
            git_fixture,
            |f| {
                rt().block_on(async {
                    let res = f.tools.batch_execute(batch_ops(n)).await.unwrap();
                    assert!(res.success);
                });
            },
            BatchSize::SmallInput,
        );
    });
    g.bench_function("git_cached", |b| {
        b.iter_batched(
            git_cached_fixture,
            |f| {
                rt().block_on(async {
                    let res = f.tools.batch_execute(batch_ops(n)).await.unwrap();
                    assert!(res.success);
                });
            },
            BatchSize::SmallInput,
        );
    });
    g.finish();
}

fn bench_batch_5(c: &mut Criterion) {
    bench_batch(c, "batch_5_creates", 5);
}
fn bench_batch_50(c: &mut Criterion) {
    bench_batch(c, "batch_50_creates", 50);
}

criterion_group!(
    benches,
    bench_write_file_1kb,
    bench_write_file_100kb,
    bench_edit_file,
    bench_delete_file,
    bench_move_file,
    bench_batch_5,
    bench_batch_50,
);
criterion_main!(benches);
