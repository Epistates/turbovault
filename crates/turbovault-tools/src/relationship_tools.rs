//! Relationship analysis tools for link strength, suggestions, and centrality
//!
//! Each analysis walks the link graph on the blocking pool, so a large vault
//! never ties up an async worker, and counts a note's neighbours without
//! cloning its links.

use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use turbovault_core::prelude::*;
use turbovault_graph::LinkGraph;
use turbovault_vault::VaultManager;

/// Result of link strength calculation
#[derive(Debug, Clone)]
pub struct LinkStrengthResult {
    pub source: String,
    pub target: String,
    pub strength: f64,
    pub direct_links: usize,
    pub backlinks: usize,
    pub shared_references: usize,
}

/// Suggested link with reasoning
#[derive(Debug, Clone)]
pub struct LinkSuggestion {
    pub target: String,
    pub strength: f64,
    pub reasons: Vec<String>,
}

/// Centrality rank for a file
#[derive(Debug, Clone)]
pub struct CentralityRank {
    pub rank: usize,
    pub file: String,
    pub score: f64,
    pub betweenness: f64,
    pub closeness: f64,
    pub eigenvector: f64,
    pub interpretation: String,
}

/// Relationship analysis tools
pub struct RelationshipTools {
    pub manager: Arc<VaultManager>,
}

impl RelationshipTools {
    /// Create new relationship tools
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self { manager }
    }

    /// Run `analysis` over the link graph (after the freshness gate) on the
    /// blocking pool.
    async fn with_graph<T, F>(&self, analysis: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&LinkGraph, &VaultManager) -> T + Send + 'static,
    {
        let graph = self.manager.link_graph_flushed().await;
        let manager = Arc::clone(&self.manager);
        tokio::task::spawn_blocking(move || analysis(&graph.blocking_read(), &manager))
            .await
            .map_err(|e| Error::other(format!("graph analysis task failed: {e}")))
    }

    /// Calculate link strength between two files (0.0-1.0)
    pub async fn get_link_strength(&self, source: &str, target: &str) -> Result<Value> {
        let source_path = self.manager.resolve_path(std::path::Path::new(source))?;
        let target_path = self.manager.resolve_path(std::path::Path::new(target))?;

        let (direct_links, backlinks, shared_references) = self
            .with_graph(move |read, _| {
                // A direct link from source to target, and one back
                let direct_links = usize::from(
                    read.forward_link_targets(&source_path)
                        .contains(&&target_path),
                );
                let backlinks = usize::from(
                    read.forward_link_targets(&target_path)
                        .contains(&&source_path),
                );

                // Shared references: the other notes that link to both
                let to_target: HashSet<&PathBuf> =
                    read.backlink_sources(&target_path).into_iter().collect();
                let shared_references = read
                    .backlink_sources(&source_path)
                    .into_iter()
                    .filter(|file| **file != source_path && **file != target_path)
                    .filter(|file| to_target.contains(file))
                    .count();
                (direct_links, backlinks, shared_references)
            })
            .await?;

        // Calculate strength: direct*1.0 + backlinks*0.7 + shared*0.3
        let raw_strength = (direct_links as f64 * 1.0)
            + (backlinks as f64 * 0.7)
            + (shared_references as f64 * 0.3);

        // Normalize to 0.0-1.0
        let strength = (raw_strength / 2.0).min(1.0);

        Ok(json!({
            "source": source,
            "target": target,
            "strength": strength,
            "components": {
                "direct_links": direct_links,
                "backlinks": backlinks,
                "shared_references": shared_references
            },
            "interpretation": interpret_strength(strength)
        }))
    }

    /// Suggest files to link from a given file
    pub async fn suggest_links(&self, file: &str, limit: usize) -> Result<Value> {
        let file_path = self.manager.resolve_path(std::path::Path::new(file))?;

        let mut suggestions = self
            .with_graph(move |read, manager| link_suggestions(read, manager, &file_path))
            .await?;

        // Sort by strength descending (total_cmp is panic-free for f64)
        suggestions.sort_by(|a, b| {
            b.strength
                .total_cmp(&a.strength)
                .then_with(|| a.target.cmp(&b.target))
        });

        // Take top N
        let results: Vec<_> = suggestions
            .into_iter()
            .take(limit)
            .map(|s| {
                json!({
                    "target": s.target,
                    "strength": s.strength,
                    "reasons": s.reasons
                })
            })
            .collect();

        Ok(json!({
            "file": file,
            "suggestions": results
        }))
    }

    /// Get centrality ranking for all files
    pub async fn get_centrality_ranking(&self) -> Result<Value> {
        let mut rankings = self.with_graph(centrality_scores).await?;

        // Sort by combined score descending (total_cmp is panic-free for f64)
        // Ties broken by path, so the ranking and its pages are the same on
        // every call.
        rankings.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        // Build result
        let ranked: Vec<_> = rankings
            .into_iter()
            .enumerate()
            .map(|(idx, (file, score, metrics))| {
                let b = metrics.get("betweenness").copied().unwrap_or(0.0);
                let c = metrics.get("closeness").copied().unwrap_or(0.0);
                let e = metrics.get("eigenvector").copied().unwrap_or(0.0);

                let interpretation = if b > 0.7 {
                    "Central hub"
                } else if e > 0.7 {
                    "Authority file"
                } else if c > 0.7 {
                    "Highly connected"
                } else {
                    "Peripheral"
                };

                json!({
                    "rank": idx + 1,
                    "file": file,
                    "score": score,
                    "betweenness": b,
                    "closeness": c,
                    "eigenvector": e,
                    "interpretation": interpretation
                })
            })
            .collect();

        Ok(json!({
            "total_files": ranked.len(),
            "rankings": ranked
        }))
    }
}

/// Every note `file_path` might link to, in graph order: those that notes
/// linking to it also link to (shared backlinks), apart from itself and the
/// notes it links to already.
fn link_suggestions(
    read: &LinkGraph,
    manager: &VaultManager,
    file_path: &PathBuf,
) -> Vec<LinkSuggestion> {
    // Get existing forward links to exclude
    let existing_links: HashSet<&PathBuf> =
        read.forward_link_targets(file_path).into_iter().collect();

    // Co-reference strength: how many of the notes linking here also link
    // to each candidate
    let mut shared: HashMap<&PathBuf, usize> = HashMap::new();
    for source in read.backlink_sources(file_path) {
        for candidate in read.forward_link_targets(source) {
            *shared.entry(candidate).or_insert(0) += 1;
        }
    }

    // Score each candidate
    let mut suggestions: Vec<LinkSuggestion> = Vec::new();
    for (candidate, _, _) in read.degrees() {
        // Skip self and existing links
        if candidate == file_path || existing_links.contains(candidate) {
            continue;
        }

        let shared_count = shared.get(candidate).copied().unwrap_or(0);
        let strength = ((shared_count as f64) * 0.3).min(1.0);

        if strength > 0.0 || shared_count > 0 {
            let mut reasons = Vec::new();
            if shared_count > 0 {
                reasons.push(format!("{} shared backlinks", shared_count));
            }
            if strength > 0.7 {
                reasons.push("Frequently co-referenced".to_string());
            }
            if reasons.is_empty() {
                reasons.push("Related file".to_string());
            }

            suggestions.push(LinkSuggestion {
                target: manager.relative_path(candidate),
                strength,
                reasons,
            });
        }
    }
    suggestions
}

/// Each note's path, combined centrality score and metrics, in graph order.
fn centrality_scores(
    read: &LinkGraph,
    manager: &VaultManager,
) -> Vec<(String, f64, HashMap<&'static str, f64>)> {
    let all_count = read.node_count() as f64;

    // Simple heuristic-based centrality calculation
    let mut rankings = Vec::new();

    for (file, _, _) in read.degrees() {
        let file_str = manager.relative_path(file);

        // Betweenness: count edges if this file connects two others
        let forward = read.forward_link_targets(file).len() as f64;
        let backward = read.backlink_source_count(file) as f64;
        let betweenness = ((forward + backward) / 10.0).min(1.0);

        // Closeness: ability to reach others (normalized edge count)
        let closeness = (forward / all_count).min(1.0);

        // Eigenvector: importance based on connection to important files
        // Simplified: count backlinks (files that link to this one)
        let eigenvector = (backward / all_count).min(1.0);

        // Combined score (equal weighting)
        let combined = (betweenness * 0.33 + closeness * 0.33 + eigenvector * 0.34) / 1.0;

        let mut metrics = HashMap::new();
        metrics.insert("betweenness", betweenness);
        metrics.insert("closeness", closeness);
        metrics.insert("eigenvector", eigenvector);

        rankings.push((file_str, combined, metrics));
    }
    rankings
}

/// Interpret link strength as human-readable text
fn interpret_strength(strength: f64) -> String {
    match strength {
        s if s > 0.8 => "Very strong - extensively cross-referenced".to_string(),
        s if s > 0.6 => "Strong - frequently connected".to_string(),
        s if s > 0.4 => "Moderate - some connection".to_string(),
        s if s > 0.2 => "Weak - minimal connection".to_string(),
        _ => "No connection".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interpret_strength() {
        assert!(interpret_strength(0.9).contains("Very strong"));
        assert!(interpret_strength(0.7).contains("Strong"));
        assert!(interpret_strength(0.5).contains("Moderate"));
        assert!(interpret_strength(0.3).contains("Weak"));
        assert!(interpret_strength(0.0).contains("No"));
    }

    /// A small vault where notes link each other with repeats, self links,
    /// mutual links and missing targets.
    async fn linked_vault() -> (tempfile::TempDir, Arc<VaultManager>) {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move |n: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n as u64) as usize
        };
        for i in 0..40 {
            let links: Vec<String> = (0..next(7))
                .map(|_| match next(10) {
                    0 => "[[missing]]".to_string(),
                    _ => format!("[[n{}]]", next(40)),
                })
                .collect();
            std::fs::write(
                tmp.path().join(format!("n{i}.md")),
                format!("# n{i}\n\n{}\n", links.join(" ")),
            )
            .unwrap();
        }
        let mut config = turbovault_core::ServerConfig::new();
        config.vaults.push(
            turbovault_core::VaultConfig::builder("v", tmp.path())
                .build()
                .unwrap(),
        );
        let manager = Arc::new(VaultManager::new(config).unwrap());
        manager.initialize().await.unwrap();
        (tmp, manager)
    }

    /// The counts by distinct neighbour are the ones the old scans made
    /// over every note's cloned links.
    #[tokio::test]
    async fn matches_scanning_every_note() {
        let (_tmp, manager) = linked_vault().await;
        let tools = RelationshipTools::new(Arc::clone(&manager));
        let graph = manager.link_graph_flushed().await;
        let read = graph.read().await;
        let all = read.all_files();

        let centrality = tools.get_centrality_ranking().await.unwrap();
        for entry in centrality["rankings"].as_array().unwrap() {
            let path = manager.vault_path().join(entry["file"].as_str().unwrap());
            let forward = read.forward_links(&path).unwrap().len() as f64;
            let backward = read.backlinks(&path).unwrap().len() as f64;
            assert_eq!(
                entry["betweenness"].as_f64().unwrap(),
                ((forward + backward) / 10.0).min(1.0)
            );
            assert_eq!(
                entry["closeness"].as_f64().unwrap(),
                (forward / all.len() as f64).min(1.0)
            );
        }

        for i in [0, 3, 11, 25] {
            let file = manager.vault_path().join(format!("n{i}.md"));
            let existing: HashSet<PathBuf> = read
                .forward_links(&file)
                .unwrap()
                .into_iter()
                .map(|(p, _)| p)
                .collect();
            let sources: HashSet<PathBuf> = read
                .backlinks(&file)
                .unwrap()
                .into_iter()
                .map(|(p, _)| p)
                .collect();
            let mut expected: Vec<(String, usize)> = all
                .iter()
                .filter(|c| **c != file && !existing.contains(*c))
                .filter_map(|c| {
                    let theirs: HashSet<PathBuf> = read
                        .backlinks(c)
                        .unwrap()
                        .into_iter()
                        .map(|(p, _)| p)
                        .collect();
                    let shared = sources.intersection(&theirs).count();
                    (shared > 0).then(|| (manager.relative_path(c), shared))
                })
                .collect();
            expected.sort();
            let mut found: Vec<(String, usize)> = link_suggestions(&read, &manager, &file)
                .into_iter()
                .map(|s| {
                    let count = s.reasons[0].split(' ').next().unwrap().parse().unwrap();
                    (s.target, count)
                })
                .collect();
            found.sort();
            assert_eq!(found, expected, "suggestions for n{i}");

            for j in [1, 7, 30] {
                let other = manager.vault_path().join(format!("n{j}.md"));
                let shared = all
                    .iter()
                    .filter(|f| **f != file && **f != other)
                    .filter(|f| {
                        let links = read.forward_links(f).unwrap();
                        links.iter().any(|(t, _)| *t == file)
                            && links.iter().any(|(t, _)| *t == other)
                    })
                    .count();
                let strength = tools
                    .get_link_strength(&format!("n{i}.md"), &format!("n{j}.md"))
                    .await
                    .unwrap();
                assert_eq!(
                    strength["components"]["shared_references"]
                        .as_u64()
                        .unwrap() as usize,
                    shared,
                    "n{i} and n{j}"
                );
            }
        }
    }
}
