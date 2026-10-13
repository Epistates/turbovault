//! Benchmarks at the vault size the performance targets are stated for.
//!
//! The synthetic vault is 10,000 notes by default (set `TV_BENCH_NOTES` for
//! another size) spread over folders, each with frontmatter, tags, prose and
//! eight wikilinks whose targets follow a power law, so a few hub notes
//! collect thousands of backlinks the way real vaults' index notes do. About
//! 3% of links point at notes that do not exist. The vault is generated once
//! per run from a fixed seed, so every run measures the same notes.
//!
//! ```sh
//! cargo bench -p turbovault --bench vault_10k
//! TV_BENCH_NOTES=2000 cargo bench -p turbovault --bench vault_10k
//! ```
//!
//! The scheduled workflow compares each night's numbers against the previous
//! night's with scripts/bench_compare.py.

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tempfile::TempDir;
use tokio::runtime::Runtime;
use turbovault_core::{ConfigProfile, VaultConfig};
use turbovault_tools::{FileTools, GraphTools, SearchEngine, SearchTools};
use turbovault_vault::VaultManager;

const FOLDERS: usize = 20;
const LINKS_PER_NOTE: usize = 8;
const WORDS: &[&str] = &[
    "project", "meeting", "design", "review", "graph", "vault", "search", "index", "note",
    "research", "draft", "idea", "summary", "decision", "release", "customer", "roadmap",
    "question", "answer", "reference",
];

fn note_count() -> usize {
    std::env::var("TV_BENCH_NOTES")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(10_000)
}

/// A small deterministic generator (xorshift64*), so the vault is the same on
/// every run without a dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// A note index skewed toward 0: note `i` is picked with weight about
    /// 1 / (i + 1), which makes the first few notes the vault's hubs.
    fn hub_biased(&mut self, n: usize) -> usize {
        let u = (self.next() >> 11) as f64 / (1u64 << 53) as f64;
        ((n as f64).powf(u) - 1.0) as usize % n
    }
}

fn note_path(i: usize) -> String {
    format!("folder{:02}/note{i:05}.md", i % FOLDERS)
}

fn note_name(i: usize) -> String {
    format!("note{i:05}")
}

fn write_vault(root: &Path, notes: usize) {
    let mut rng = Rng(0x5EED_CAFE_F00D_D00D);
    for folder in 0..FOLDERS {
        std::fs::create_dir_all(root.join(format!("folder{folder:02}"))).unwrap();
    }
    for i in 0..notes {
        let mut text = format!(
            "---\ntitle: \"Note {i}\"\nstatus: {}\npriority: {}\ntags: [{}, area/{}]\n---\n\n# Note {i}\n\n",
            ["active", "done", "draft"][i % 3],
            i % 5,
            WORDS[i % WORDS.len()],
            WORDS[(i / 7) % WORDS.len()],
        );
        for paragraph in 0..4 {
            for _ in 0..60 {
                text.push_str(WORDS[rng.below(WORDS.len())]);
                text.push(' ');
            }
            if paragraph < LINKS_PER_NOTE / 2 {
                for _ in 0..2 {
                    if rng.below(100) < 3 {
                        text.push_str(&format!("[[missing{:05}]] ", rng.below(notes)));
                    } else {
                        text.push_str(&format!("[[{}]] ", note_name(rng.hub_biased(notes))));
                    }
                }
            }
            text.push_str(&format!("#{}\n\n", WORDS[rng.below(WORDS.len())]));
        }
        text.push_str("- [ ] follow up\n- [x] done\n");
        std::fs::write(root.join(note_path(i)), text).unwrap();
    }
}

/// The generated vault, shared by every benchmark in the run.
fn vault() -> &'static TempDir {
    static VAULT: OnceLock<TempDir> = OnceLock::new();
    VAULT.get_or_init(|| {
        let dir = TempDir::new().unwrap();
        write_vault(dir.path(), note_count());
        dir
    })
}

fn manager(rt: &Runtime) -> Arc<VaultManager> {
    let mut config = ConfigProfile::Development.create_config();
    config
        .vaults
        .push(VaultConfig::builder("bench", vault().path()).build().unwrap());
    let manager = VaultManager::new(config).unwrap();
    rt.block_on(manager.initialize()).unwrap();
    Arc::new(manager)
}

fn bench_init(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    vault();
    let mut group = c.benchmark_group("vault_10k");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));
    group.bench_function("initialize", |b| {
        b.iter(|| black_box(manager(&rt)));
    });
    group.finish();
}

fn bench_reads(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let manager = manager(&rt);
    let files = FileTools::new(manager.clone());
    let search_tools = SearchTools::new(manager.clone());
    let graph = GraphTools::new(manager.clone());
    let hub = note_path(0);
    let leaf = note_path(note_count() - 1);

    let mut group = c.benchmark_group("vault_10k");
    group.bench_function("read_note", |b| {
        b.to_async(&rt)
            .iter(|| async { files.read_file(black_box(&leaf)).await.unwrap() })
    });
    group.bench_function("get_backlinks_hub", |b| {
        b.to_async(&rt)
            .iter(|| async { search_tools.find_backlinks(black_box(&hub)).await.unwrap() })
    });
    group.sample_size(10);
    group.bench_function("quick_health_check", |b| {
        b.to_async(&rt)
            .iter(|| async { graph.quick_health_check().await.unwrap() })
    });
    group.finish();
}

fn bench_search(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let manager = manager(&rt);

    let mut group = c.benchmark_group("vault_10k");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));
    group.bench_function("search_index_build", |b| {
        b.to_async(&rt)
            .iter(|| async { SearchEngine::new(manager.clone()).await.unwrap() })
    });

    let engine = rt.block_on(SearchEngine::new(manager.clone())).unwrap();
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(10));
    group.bench_function("search", |b| {
        b.to_async(&rt)
            .iter(|| async { engine.search(black_box("roadmap decision")).await.unwrap() })
    });
    group.finish();
}

criterion_group!(benches, bench_init, bench_reads, bench_search);
criterion_main!(benches);
