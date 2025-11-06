//! Performance benchmarks for MCP tools

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
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
                        })
                        .collect();
                    tools.batch_execute(black_box(ops)).await.unwrap()
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
    let temp_dir = TempDir::new().unwrap();
    let vault_path = temp_dir.path();

    // Create files with metadata
    rt.block_on(async {
        for i in 0..100 {
            let content = format!(
                r#"---
title: "Note {}"
priority: {}
status: "active"
---
# Note {}
Content"#,
                i,
                i % 10,
                i
            );
            tokio::fs::write(vault_path.join(format!("meta{}.md", i)), content)
                .await
                .unwrap();
        }
    });

    let mut config = ConfigProfile::Development.create_config();
    let vault_config = VaultConfig::builder("bench", vault_path).build().unwrap();
    config.vaults.push(vault_config);

    let manager = rt.block_on(async {
        let manager = VaultManager::new(config).unwrap();
        manager.initialize().await.unwrap();
        Arc::new(manager)
    });

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

criterion_group!(
    benches,
    bench_file_read,
    bench_file_write,
    bench_backlinks,
    bench_health_check,
    bench_search,
    bench_batch_operations,
    bench_vault_scan,
    bench_concurrent_reads,
    bench_metadata_queries
);

criterion_main!(benches);
