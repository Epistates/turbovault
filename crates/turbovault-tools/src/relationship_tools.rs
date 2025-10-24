//! Relationship analysis tools for link strength, suggestions, and centrality

use turbovault_core::prelude::*;
use turbovault_vault::VaultManager;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

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

    /// Calculate link strength between two files (0.0-1.0)
    pub async fn get_link_strength(&self, source: &str, target: &str) -> Result<Value> {
        let graph = self.manager.link_graph();
        let read = graph.read().await;

        // Count direct links from source to target
        let source_path = std::path::PathBuf::from(source);
        let target_path = std::path::PathBuf::from(target);

        let mut direct_links = 0;
        if let Ok(forward_links) = read.forward_links(&source_path) {
            for (linked_file, _) in forward_links {
                if linked_file.to_string_lossy().contains(target) {
                    direct_links += 1;
                }
            }
        }

        // Count backlinks from target to source
        let mut backlinks = 0;
        if let Ok(back_links) = read.backlinks(&target_path) {
            for (linked_file, _) in back_links {
                if linked_file.to_string_lossy().contains(source) {
                    backlinks += 1;
                }
            }
        }

        // Count shared references (files that link to both)
        let mut shared_references = 0;
        let all_files = read.all_files();

        for file in all_files {
            if file == source_path || file == target_path {
                continue;
            }

            if let Ok(links) = read.forward_links(&file) {
                let links_source = links.iter().any(|(f, _)| f == &source_path);
                let links_target = links.iter().any(|(f, _)| f == &target_path);
                if links_source && links_target {
                    shared_references += 1;
                }
            }
        }

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
        let graph = self.manager.link_graph();
        let read = graph.read().await;

        let file_path = std::path::PathBuf::from(file);
        let all_files = read.all_files();

        // Get existing forward links to exclude
        let mut existing_links = std::collections::HashSet::new();
        if let Ok(forward_links) = read.forward_links(&file_path) {
            for (linked_file, _) in forward_links {
                existing_links.insert(linked_file.to_string_lossy().to_string());
            }
        }

        // Score each candidate
        let mut suggestions: Vec<LinkSuggestion> = Vec::new();

        for candidate in all_files {
            let candidate_str = candidate.to_string_lossy().to_string();

            // Skip self and existing links
            if candidate_str.contains(file) || existing_links.contains(&candidate_str) {
                continue;
            }

            // Calculate co-reference strength (shared backlinks)
            let mut shared_refs = Vec::new();
            if let Ok(source_backlinks) = read.backlinks(&file_path)
                && let Ok(target_backlinks) = read.backlinks(&candidate)
            {
                let source_set: std::collections::HashSet<_> =
                    source_backlinks.iter().map(|(p, _)| p.clone()).collect();
                let target_set: std::collections::HashSet<_> =
                    target_backlinks.iter().map(|(p, _)| p.clone()).collect();

                for intersection_file in source_set.intersection(&target_set) {
                    if let Some(name) = intersection_file.file_name() {
                        shared_refs.push(name.to_string_lossy().to_string());
                    }
                }
            }

            let shared_count = shared_refs.len();
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
                    target: candidate_str,
                    strength,
                    reasons,
                });
            }
        }

        // Sort by strength descending
        suggestions.sort_by(|a, b| b.strength.partial_cmp(&a.strength).unwrap());

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
        let graph = self.manager.link_graph();
        let read = graph.read().await;

        let all_files = read.all_files();

        // Simple heuristic-based centrality calculation
        let mut rankings: Vec<(String, f64, HashMap<&str, f64>)> = Vec::new();

        for file in all_files {
            let file_str = file.to_string_lossy().to_string();

            // Betweenness: count edges if this file connects two others
            let forward = read.forward_links(&file).unwrap_or_default().len() as f64;
            let backward = read.backlinks(&file).unwrap_or_default().len() as f64;
            let betweenness = ((forward + backward) / 10.0).min(1.0);

            // Closeness: ability to reach others (normalized edge count)
            let all_count = read.all_files().len() as f64;
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

        // Sort by combined score descending
        rankings.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

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
}
