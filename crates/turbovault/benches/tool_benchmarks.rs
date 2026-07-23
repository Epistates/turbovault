//! Performance benchmarks for MCP tools

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::runtime::Runtime;
use turbovault_core::{ConfigProfile, VaultConfig};
use turbovault_tools::*;
use turbovault_vault::VaultManager;

/// Setup a test vault with various files
async fn setup_bench_vault(num_files: usize) -> (TempDir, Arc<VaultManager>) {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let vault_path = temp_dir.path();

    // Create interconnected notes
    for i in 0..num_files {
        let content = format!(
            "# Note {}\n\nThis is note {} linking to [[note{}]] and [[note{}]]",
            i,
            i,
            (i + 1) % num_files,
            (i + 2) % num_files
        );
        tokio::fs::write(vault_path.join(format!("note{}.md", i)), content)
            .await
            .expect("Failed to write file");
    }

    let mut config = ConfigProfile::Development.create_config();
    let vault_config = VaultConfig::builder("bench", vault_path)
        .build()
        .expect("Failed to create vault config");
    config.vaults.push(vault_config);

    let manager = VaultManager::new(config).expect("Failed to create vault manager");
    manager
        .initialize()
        .await
        .expect("Failed to initialize vault");

    (temp_dir, Arc::new(manager))
}

/// Setup a test vault with frontmatter-bearing notes for metadata benchmarks
async fn setup_metadata_vault(num_files: usize) -> (TempDir, Arc<VaultManager>) {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let vault_path = temp_dir.path();

    for i in 0..num_files {
        let content = format!(
            "---\ntitle: \"Note {}\"\npriority: {}\nstatus: \"active\"\n---\n# Note {}\nContent",
            i,
            i % 10,
            i
        );
        tokio::fs::write(vault_path.join(format!("meta{}.md", i)), content)
            .await
            .expect("Failed to write file");
    }

    let mut config = ConfigProfile::Development.create_config();
    let vault_config = VaultConfig::builder("bench", vault_path)
        .build()
        .expect("Failed to create vault config");
    config.vaults.push(vault_config);

    let manager = VaultManager::new(config).expect("Failed to create vault manager");
    manager
        .initialize()
        .await
        .expect("Failed to initialize vault");

    (temp_dir, Arc::new(manager))
}

/// Benchmark file read operations
fn bench_file_read(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (_temp_dir, manager) = rt.block_on(setup_bench_vault(100));
    let tools = FileTools::new(manager);

    c.bench_function("file_read_small", |b| {
        b.to_async(&rt)
            .iter(|| async { tools.read_file(black_box("note0.md")).await.unwrap() })
    });
}

/// Benchmark file write operations
fn bench_file_write(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (_temp_dir, manager) = rt.block_on(setup_bench_vault(10));
    let tools = FileTools::new(manager);

    c.bench_function("file_write_small", |b| {
        b.to_async(&rt).iter(|| async {
            tools
                .write_file(black_box("bench.md"), black_box("# Benchmark\nContent"))
                .await
                .unwrap()
        })
    });
}

/// Benchmark backlink queries
fn bench_backlinks(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("backlinks");

    for size in [10, 50, 100, 500].iter() {
        let (_temp_dir, manager) = rt.block_on(setup_bench_vault(*size));
        let tools = SearchTools::new(manager);

        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &_size| {
            b.to_async(&rt)
                .iter(|| async { tools.find_backlinks(black_box("note0.md")).await.unwrap() })
        });
    }
    group.finish();
}

/// Benchmark graph health checks
fn bench_health_check(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("health_check");

    for size in [10, 50, 100, 500].iter() {
        let (_temp_dir, manager) = rt.block_on(setup_bench_vault(*size));
        let tools = GraphTools::new(manager);

        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &_size| {
            b.to_async(&rt)
                .iter(|| async { tools.quick_health_check().await.unwrap() })
        });
    }
    group.finish();
}

/// Benchmark search operations
fn bench_search(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (_temp_dir, manager) = rt.block_on(setup_bench_vault(100));

    c.bench_function("search_engine_creation", |b| {
        b.to_async(&rt)
            .iter(|| async { SearchEngine::new(black_box(manager.clone())).await.unwrap() })
    });

    let engine = rt.block_on(SearchEngine::new(manager)).unwrap();

    c.bench_function("search_query", |b| {
        b.to_async(&rt)
            .iter(|| async { engine.search(black_box("test")).await.unwrap() })
    });
}

/// Benchmark batch operations
fn bench_batch_operations(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("batch_operations");

    for batch_size in [1, 5, 10, 20].iter() {
        let (_temp_dir, manager) = rt.block_on(setup_bench_vault(10));
        let tools = BatchTools::new(manager);

        group.bench_with_input(
            BenchmarkId::from_parameter(batch_size),
            batch_size,
            |b, &size| {
                b.to_async(&rt).iter(|| async {
                    let ops: Vec<_> = (0..size)
                        .map(|i| BatchOperation::WriteNote {
                            path: format!("batch_{}.md", i),
                            content: format!("# Batch {}", i),
                            expected_hash: None,
                        })
                        .collect();
                    tools
                        .batch_execute(black_box(ops), "bench batch")
                        .await
                        .unwrap()
                })
            },
        );
    }
    group.finish();
}

/// Benchmark vault scanning
fn bench_vault_scan(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("vault_scan");

    for size in [10, 50, 100, 500].iter() {
        let (_temp_dir, manager) = rt.block_on(setup_bench_vault(*size));

        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &_size| {
            b.to_async(&rt)
                .iter(|| async { manager.scan_vault().await.unwrap() })
        });
    }
    group.finish();
}

/// Benchmark concurrent operations
fn bench_concurrent_reads(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (_temp_dir, manager) = rt.block_on(setup_bench_vault(100));
    let tools = FileTools::new(manager);

    c.bench_function("concurrent_reads_10", |b| {
        b.to_async(&rt).iter(|| async {
            let handles: Vec<_> = (0..10)
                .map(|i| {
                    let tools = tools.clone();
                    tokio::spawn(
                        async move { tools.read_file(&format!("note{}.md", i % 100)).await },
                    )
                })
                .collect();

            for handle in handles {
                let _ = handle.await;
            }
        })
    });
}

/// Benchmark metadata queries
fn bench_metadata_queries(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (_temp_dir, manager) = rt.block_on(setup_metadata_vault(100));
    let tools = MetadataTools::new(manager);

    c.bench_function("metadata_query", |b| {
        b.to_async(&rt).iter(|| async {
            tools
                .query_metadata(black_box("priority > 5"))
                .await
                .unwrap()
        })
    });
}

// ── A/B benchmarks ────────────────────────────────────────────────────────────
//
// Each group compares the current implementation against a proposed alternative.
// Run with: cargo bench -- "vault_scan_ab|metadata_query_ab"

/// A/B: scan_files (path.is_dir + path.metadata) vs scan_files_dtype (DirEntry::file_type)
///
/// Hypothesis: DirEntry::file_type() reads d_type from the Linux dirent struct returned
/// by readdir(3) — no additional statx/readlink syscall per entry. The current
/// implementation calls path.is_dir() (statx) + path.metadata() (statx) per entry.
fn bench_vault_scan_ab(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("vault_scan_ab");

    for size in [10, 50, 100, 500].iter() {
        let (_temp_dir, manager) = rt.block_on(setup_bench_vault(*size));

        group.bench_with_input(
            BenchmarkId::new("current_path_is_dir", size),
            size,
            |b, _| {
                b.to_async(&rt)
                    .iter(|| async { manager.scan_vault().await.unwrap() })
            },
        );

        group.bench_with_input(
            BenchmarkId::new("proposed_entry_file_type", size),
            size,
            |b, _| {
                b.to_async(&rt)
                    .iter(|| async { manager.scan_vault_dtype().await.unwrap() })
            },
        );
    }
    group.finish();
}

/// A/B: query_metadata (scan_vault + parse_file per call) vs query_metadata_cached (file_cache)
///
/// Hypothesis: The current implementation issues a full filesystem scan and reads every
/// file on every invocation. The cache variant reads from the in-memory HashMap populated
/// during initialize() — zero filesystem I/O on the hot path.
fn bench_metadata_query_ab(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (_temp_dir, manager) = rt.block_on(setup_metadata_vault(100));
    let tools = MetadataTools::new(manager);
    let mut group = c.benchmark_group("metadata_query_ab");

    // "before" baseline: inline the old scan-on-every-call logic so we can measure
    // it alongside the new implementation without reverting the source change.
    group.bench_function("before_scan_per_call", |b| {
        b.to_async(&rt).iter(|| async {
            let files = tools.manager.scan_vault().await.unwrap();
            let mut out = Vec::new();
            for path in files {
                if let Ok(vf) = tools.manager.parse_file(&path).await {
                    out.push(vf);
                }
            }
            out
        })
    });

    group.bench_function("after_validated_cache", |b| {
        b.to_async(&rt).iter(|| async {
            tools
                .query_metadata(black_box("priority > 5"))
                .await
                .unwrap()
        })
    });

    group.finish();
}

/// A/B: all_cached_vault_files (zero I/O) vs vault_files_validated (N stat calls)
///
/// Hypothesis: all_cached_vault_files() acquires a read lock and clones the HashMap
/// values — no I/O at all. vault_files_validated() issues N synchronous stat calls
/// inside one spawn_blocking task and zero file reads on the hot path (nothing changed).
/// The gap quantifies the cost of the mtime-validation safety net callers pay for
/// choosing the validated path over the raw fast path.
fn bench_cache_read_ab(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (_temp_dir, manager) = rt.block_on(setup_metadata_vault(100));
    let mut group = c.benchmark_group("cache_read_ab");

    group.bench_function("all_cached_no_io", |b| {
        b.to_async(&rt)
            .iter(|| async { manager.all_cached_vault_files().await })
    });

    group.bench_function("validated_n_stats", |b| {
        b.to_async(&rt)
            .iter(|| async { manager.vault_files_validated().await })
    });

    group.finish();
}

// ── Backend swap: direct vs git at the tool layer (GX.13 add-on) ────────────
// A git-backed vault: init a real repo + seed committed files, then a
// Git-backend manager. Lets tool_benchmarks measure the SAME manager write op
// on both substrates — a true same-layer direct-vs-git comparison.
async fn setup_git_vault(num_files: usize) -> (TempDir, Arc<VaultManager>) {
    let temp_dir = TempDir::new().expect("tempdir");
    let vault_path = temp_dir.path().to_path_buf();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&vault_path)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-b", "main"]);
    git(&["config", "user.name", "bench"]);
    git(&["config", "user.email", "bench@bench.local"]);
    for i in 0..num_files {
        let content = format!(
            "# Note {}\n\nThis is note {} linking to [[note{}]] and [[note{}]]",
            i,
            i,
            (i + 1) % num_files,
            (i + 2) % num_files
        );
        std::fs::write(vault_path.join(format!("note{}.md", i)), content).expect("seed write");
    }
    git(&["add", "-A"]);
    git(&["commit", "-m", "seed"]);

    let mut config = ConfigProfile::Development.create_config();
    let vault_config = VaultConfig::builder("bench", &vault_path)
        .write_backend(turbovault_core::WriteBackend::Git)
        .build()
        .expect("git vault config");
    config.vaults.push(vault_config);
    let manager = VaultManager::new(config).expect("git vault manager");
    manager.initialize().await.expect("init git vault");
    (temp_dir, Arc::new(manager))
}

/// The same manager write op on the `direct` vs `git` substrate, across the
/// write shapes the suite measures (single write + batch at 1/5/10/20) — a
/// true same-layer backend comparison, since the tool/manager surface is
/// backend-agnostic. UNIQUE content per iteration (a counter) so the git
/// substrate can't no-op an identity write (identical re-saves build the same
/// tree and skip the commit — cheap, but not the write cost we want to time);
/// both backends do real work on every call.
fn bench_write_backends(c: &mut Criterion) {
    use std::sync::atomic::{AtomicU64, Ordering};
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("write_backends");

    // ── single-file write ──────────────────────────────────────────────────
    let (_d, dm) = rt.block_on(setup_bench_vault(10));
    let dt = FileTools::new(dm);
    let dc = AtomicU64::new(0);
    group.bench_function("direct/file_write", |b| {
        b.to_async(&rt).iter(|| async {
            let n = dc.fetch_add(1, Ordering::Relaxed);
            dt.write_file("bench.md", &format!("# Benchmark {n}\nContent"))
                .await
                .unwrap()
        })
    });
    let (_g, gm) = rt.block_on(setup_git_vault(10));
    let gt = FileTools::new(gm);
    let gc = AtomicU64::new(0);
    group.bench_function("git/file_write", |b| {
        b.to_async(&rt).iter(|| async {
            let n = gc.fetch_add(1, Ordering::Relaxed);
            gt.write_file("bench.md", &format!("# Benchmark {n}\nContent"))
                .await
                .unwrap()
        })
    });

    // ── batch write at increasing sizes ────────────────────────────────────
    for &size in &[1usize, 5, 10, 20] {
        let (_db, dbm) = rt.block_on(setup_bench_vault(10));
        let dbt = BatchTools::new(dbm);
        let dbc = AtomicU64::new(0);
        group.bench_with_input(BenchmarkId::new("direct/batch", size), &size, |b, &size| {
            b.to_async(&rt).iter(|| async {
                let rev = dbc.fetch_add(1, Ordering::Relaxed);
                let ops: Vec<_> = (0..size)
                    .map(|i| BatchOperation::WriteNote {
                        path: format!("batch_{i}.md"),
                        content: format!("# Batch {i} rev {rev}"),
                        expected_hash: None,
                    })
                    .collect();
                dbt.batch_execute(black_box(ops), "bench batch")
                    .await
                    .unwrap()
            })
        });
        let (_gb, gbm) = rt.block_on(setup_git_vault(10));
        let gbt = BatchTools::new(gbm);
        let gbc = AtomicU64::new(0);
        group.bench_with_input(BenchmarkId::new("git/batch", size), &size, |b, &size| {
            b.to_async(&rt).iter(|| async {
                let rev = gbc.fetch_add(1, Ordering::Relaxed);
                let ops: Vec<_> = (0..size)
                    .map(|i| BatchOperation::WriteNote {
                        path: format!("batch_{i}.md"),
                        content: format!("# Batch {i} rev {rev}"),
                        expected_hash: None,
                    })
                    .collect();
                gbt.batch_execute(black_box(ops), "bench batch")
                    .await
                    .unwrap()
            })
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_write_backends,
    bench_file_read,
    bench_file_write,
    bench_backlinks,
    bench_health_check,
    bench_search,
    bench_batch_operations,
    bench_vault_scan,
    bench_concurrent_reads,
    bench_metadata_queries,
    bench_vault_scan_ab,
    bench_metadata_query_ab,
    bench_cache_read_ab,
);

criterion_main!(benches);
