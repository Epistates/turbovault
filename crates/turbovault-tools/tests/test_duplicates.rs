//! `find_duplicates` scores each candidate pair directly.
//!
//! It used to get a pair's score by ranking the whole vault from one note and
//! reading the other off the list, once per candidate pair. The direct score
//! has to be the same number the ranking gives, or duplicates would come and
//! go with the change.

use std::sync::Arc;
use tempfile::TempDir;
use turbovault_core::{ConfigProfile, VaultConfig};
use turbovault_tools::{DuplicateTools, SimilarityEngine};
use turbovault_vault::VaultManager;

const NOTES: &[(&str, &str)] = &[
    (
        "rust.md",
        "# Rust ownership\n\nOwnership and borrowing rules keep memory safe without a garbage collector. The borrow checker enforces them.\n",
    ),
    (
        "rust-copy.md",
        "# Rust ownership\n\nOwnership and borrowing rules keep memory safe without a garbage collector. The borrow checker enforces these.\n",
    ),
    (
        "garden.md",
        "# Garden\n\nTomatoes need full sun, regular watering and a stake once the vines get heavy.\n",
    ),
    (
        "bread.md",
        "# Bread\n\nA sourdough starter needs feeding daily, and the dough wants a long cold proof overnight.\n",
    ),
];

async fn vault() -> (TempDir, Arc<VaultManager>) {
    let temp = TempDir::new().unwrap();
    for (path, content) in NOTES {
        tokio::fs::write(temp.path().join(path), content)
            .await
            .unwrap();
    }
    let mut config = ConfigProfile::Development.create_config();
    config
        .vaults
        .push(VaultConfig::builder("dups", temp.path()).build().unwrap());
    let manager = VaultManager::new(config).unwrap();
    manager.initialize().await.unwrap();
    (temp, Arc::new(manager))
}

#[tokio::test]
async fn a_pair_scores_the_same_as_the_ranking_says() {
    let (_temp, manager) = vault().await;
    let engine = SimilarityEngine::new(manager).await.unwrap();

    for (a, _) in NOTES {
        let ranked = engine.find_similar_notes(a, NOTES.len());
        for (b, _) in NOTES {
            if a == b {
                continue;
            }
            let from_ranking = ranked.iter().find(|r| r.path == *b).map(|r| r.score);
            let direct = engine
                .pair_similarity(a, b)
                .map(|score| (score * 10000.0).round() / 10000.0);
            assert_eq!(direct, from_ranking, "{a} against {b}");
        }
    }
}

/// The threshold is lower than the tool's default because four notes make a
/// harsh corpus for TF-IDF: the one word the copies differ by is rare, so it
/// weighs as much as several they share, and the pair scores about 0.78.
#[tokio::test]
async fn the_obvious_duplicate_is_found_and_nothing_else() {
    let (_temp, manager) = vault().await;
    let groups = DuplicateTools::new(manager)
        .find_duplicates(0.7, 10)
        .await
        .unwrap();
    assert_eq!(groups.len(), 1, "{groups:?}");
    let mut paths: Vec<String> = groups[0]
        .notes
        .iter()
        .map(|note| note.path.clone())
        .collect();
    paths.sort();
    assert_eq!(paths, vec!["rust-copy.md", "rust.md"]);
}
