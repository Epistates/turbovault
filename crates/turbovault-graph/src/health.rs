//! Vault health analysis and broken link detection.
//!
//! Provides tools for analyzing vault health, detecting broken links,
//! finding orphaned notes, and analyzing connectivity patterns.

use crate::graph::LinkGraph;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use turbovault_core::{Link, Result};

/// A broken link in the vault
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokenLink {
    /// Source file containing the broken link
    pub source_file: PathBuf,
    /// Target that couldn't be resolved
    pub target: String,
    /// Line number in source file
    pub line: usize,
    /// Suggested fixes
    pub suggestions: Vec<String>,
}

/// Health analysis report for the vault
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    /// Total number of notes
    pub total_notes: usize,
    /// Total number of links
    pub total_links: usize,
    /// Broken links found
    pub broken_links: Vec<BrokenLink>,
    /// Orphaned notes (no incoming or outgoing links)
    pub orphaned_notes: Vec<PathBuf>,
    /// Isolated clusters (groups of notes not connected to main graph)
    pub isolated_clusters: Vec<Vec<PathBuf>>,
    /// Hub notes (highly connected nodes)
    pub hub_notes: Vec<(PathBuf, usize)>,
    /// Dead end notes (no outgoing links)
    pub dead_end_notes: Vec<PathBuf>,
    /// Overall health score (0-100)
    pub health_score: u8,
}

impl HealthReport {
    /// Create a new empty health report
    pub fn new() -> Self {
        Self {
            total_notes: 0,
            total_links: 0,
            broken_links: Vec::new(),
            orphaned_notes: Vec::new(),
            isolated_clusters: Vec::new(),
            hub_notes: Vec::new(),
            dead_end_notes: Vec::new(),
            health_score: 100,
        }
    }

    /// Calculate health score based on issues
    pub fn calculate_score(&mut self) {
        if self.total_notes == 0 {
            self.health_score = 0;
            return;
        }

        let mut score = 100;

        // Penalize broken links (up to -30 points)
        let broken_ratio = self.broken_links.len() as f32 / self.total_links.max(1) as f32;
        score -= (broken_ratio * 30.0) as u8;

        // Penalize orphaned notes (up to -20 points)
        let orphaned_ratio = self.orphaned_notes.len() as f32 / self.total_notes as f32;
        score -= (orphaned_ratio * 20.0) as u8;

        // Penalize isolated clusters (up to -15 points)
        let isolated_ratio = self.isolated_clusters.len() as f32 / self.total_notes as f32;
        score -= (isolated_ratio * 15.0) as u8;

        // Penalize dead ends (up to -10 points)
        let dead_end_ratio = self.dead_end_notes.len() as f32 / self.total_notes as f32;
        score -= (dead_end_ratio * 10.0) as u8;

        self.health_score = score;
    }

    /// Check if vault is healthy (score >= 80)
    pub fn is_healthy(&self) -> bool {
        self.health_score >= 80
    }
}

impl Default for HealthReport {
    fn default() -> Self {
        Self::new()
    }
}

/// Vault health analyzer
pub struct HealthAnalyzer<'a> {
    graph: &'a LinkGraph,
    files: Option<&'a HashMap<PathBuf, Vec<Link>>>,
}

impl<'a> HealthAnalyzer<'a> {
    /// Create a new health analyzer
    pub fn new(graph: &'a LinkGraph) -> Self {
        Self { graph, files: None }
    }

    /// Create a new health analyzer with access to file links
    /// (needed for detecting broken links that aren't in the graph)
    pub fn with_files(graph: &'a LinkGraph, files: &'a HashMap<PathBuf, Vec<Link>>) -> Self {
        Self {
            graph,
            files: Some(files),
        }
    }

    /// Run a comprehensive health analysis
    pub fn analyze(&self) -> Result<HealthReport> {
        let mut report = HealthReport::new();

        // Basic stats
        report.total_notes = self.graph.node_count();
        report.total_links = self.graph.edge_count();

        // Find broken links
        report.broken_links = self.find_broken_links()?;

        // Find orphaned notes
        report.orphaned_notes = self.graph.orphaned_notes();

        // Find dead end notes (no outgoing links)
        report.dead_end_notes = self.find_dead_end_notes()?;

        // Find hub notes (highly connected)
        report.hub_notes = self.find_hub_notes(5)?;

        // Find isolated clusters
        report.isolated_clusters = self.find_isolated_clusters()?;

        // Calculate overall health score
        report.calculate_score();

        Ok(report)
    }

    /// Find all broken links in the vault
    fn find_broken_links(&self) -> Result<Vec<BrokenLink>> {
        let mut broken = Vec::new();

        // If we have access to raw file links, use those (more accurate)
        if let Some(files) = self.files {
            for (source, links) in files {
                for link in links {
                    if !link.is_valid {
                        // Try to find similar targets for suggestions
                        let suggestions = self.suggest_targets(&link.target);

                        broken.push(BrokenLink {
                            source_file: source.clone(),
                            target: link.target.clone(),
                            line: link.position.line,
                            suggestions,
                        });
                    }
                }
            }
        } else {
            // Fall back to graph links (less accurate - broken links won't be in graph)
            for (source, links) in self.graph.all_links() {
                for link in links {
                    if !link.is_valid {
                        let suggestions = self.suggest_targets(&link.target);

                        broken.push(BrokenLink {
                            source_file: source.clone(),
                            target: link.target.clone(),
                            line: link.position.line,
                            suggestions,
                        });
                    }
                }
            }
        }

        Ok(broken)
    }

    /// Find notes with no outgoing links
    fn find_dead_end_notes(&self) -> Result<Vec<PathBuf>> {
        let mut dead_ends = Vec::new();

        for path in self.graph.all_files() {
            let outgoing = self.graph.outgoing_links(&path)?;
            if outgoing.is_empty() {
                // Check if it has any incoming links (not completely orphaned)
                let incoming = self.graph.incoming_links(&path)?;
                if !incoming.is_empty() {
                    dead_ends.push(path);
                }
            }
        }

        Ok(dead_ends)
    }

    /// Find hub notes (notes with many connections)
    fn find_hub_notes(&self, limit: usize) -> Result<Vec<(PathBuf, usize)>> {
        let mut hubs: Vec<(PathBuf, usize)> = Vec::new();

        for path in self.graph.all_files() {
            let incoming = self.graph.incoming_links(&path)?;
            let outgoing = self.graph.outgoing_links(&path)?;
            let total_connections = incoming.len() + outgoing.len();

            if total_connections > 0 {
                hubs.push((path, total_connections));
            }
        }

        // Sort by connection count (descending)
        hubs.sort_by(|a, b| b.1.cmp(&a.1));
        hubs.truncate(limit);

        Ok(hubs)
    }

    /// Find isolated clusters using connected components analysis
    fn find_isolated_clusters(&self) -> Result<Vec<Vec<PathBuf>>> {
        let components = self.graph.connected_components()?;

        // Find clusters that are isolated from the main graph
        // A cluster is considered isolated if it has fewer than 3 nodes
        // and no connections to larger clusters
        let isolated: Vec<Vec<PathBuf>> = components
            .into_iter()
            .filter(|component| component.len() > 1 && component.len() < 5)
            .collect();

        Ok(isolated)
    }

    /// Suggest similar targets for a broken link
    fn suggest_targets(&self, target: &str) -> Vec<String> {
        let mut suggestions = Vec::new();
        let target_lower = target.to_lowercase();

        for path in self.graph.all_files() {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                let stem_lower = stem.to_lowercase();

                // Check for similar names using basic string similarity
                if stem_lower.contains(&target_lower) || target_lower.contains(&stem_lower) {
                    suggestions.push(stem.to_string());
                }

                // Limit suggestions
                if suggestions.len() >= 5 {
                    break;
                }
            }
        }

        suggestions
    }

    /// Quick health check (just broken links and orphans)
    pub fn quick_check(&self) -> Result<HealthReport> {
        let mut report = HealthReport::new();

        report.total_notes = self.graph.node_count();
        report.total_links = self.graph.edge_count();
        report.broken_links = self.find_broken_links()?;
        report.orphaned_notes = self.graph.orphaned_notes();

        report.calculate_score();

        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::LinkGraph;
    use std::collections::HashSet;
    use std::path::PathBuf;
    use turbovault_core::{FileMetadata, LinkType, SourcePosition, VaultFile};

    fn create_test_file(path: &str) -> VaultFile {
        VaultFile {
            path: PathBuf::from(path),
            content: "# Test".to_string(),
            metadata: FileMetadata {
                path: PathBuf::from(path),
                size: 10,
                created_at: 0.0,
                modified_at: 0.0,
                checksum: "abc123".to_string(),
                is_attachment: false,
            },
            frontmatter: None,
            headings: Vec::new(),
            links: Vec::new(),
            backlinks: HashSet::new(),
            blocks: Vec::new(),
            tags: Vec::new(),
            callouts: Vec::new(),
            tasks: Vec::new(),
            is_parsed: true,
            parse_error: None,
            last_parsed: Some(0.0),
        }
    }

    fn create_test_file_with_links(path: &str, links: Vec<Link>) -> VaultFile {
        let mut file = create_test_file(path);
        file.links = links;
        file
    }

    fn create_test_link(source: &str, target: &str, is_valid: bool) -> Link {
        Link {
            type_: LinkType::WikiLink,
            source_file: PathBuf::from(source),
            target: target.to_string(),
            display_text: None,
            position: SourcePosition::start(),
            resolved_target: if is_valid {
                Some(PathBuf::from(format!("{}.md", target)))
            } else {
                None
            },
            is_valid,
        }
    }

    #[test]
    fn test_health_report_creation() {
        let report = HealthReport::new();
        assert_eq!(report.total_notes, 0);
        assert_eq!(report.total_links, 0);
        assert_eq!(report.health_score, 100);
        assert!(report.is_healthy());
    }

    #[test]
    fn test_health_score_calculation() {
        let mut report = HealthReport::new();
        report.total_notes = 10;
        report.total_links = 10;

        // Add significant issues to affect score
        for i in 0..3 {
            report.broken_links.push(BrokenLink {
                source_file: PathBuf::from(format!("file{}.md", i)),
                target: "broken".to_string(),
                line: 1,
                suggestions: Vec::new(),
            });
        }
        report.orphaned_notes.push(PathBuf::from("orphan.md"));
        report.dead_end_notes.push(PathBuf::from("deadend.md"));

        report.calculate_score();

        // Score should be less than 100 due to issues
        assert!(report.health_score < 100);
        // Note: health_score is u8, so >= 0 is always true
    }

    #[test]
    fn test_health_analyzer_creation() {
        let graph = LinkGraph::new();
        let analyzer = HealthAnalyzer::new(&graph);

        let report = analyzer.analyze().unwrap();
        assert_eq!(report.total_notes, 0);
        assert_eq!(report.health_score, 0); // Empty vault = 0 score
    }

    #[test]
    fn test_find_broken_links() {
        let mut graph = LinkGraph::new();

        let broken_link = create_test_link("file1.md", "nonexistent", false);
        let valid_link = create_test_link("file1.md", "file2", true);

        let file1 =
            create_test_file_with_links("file1.md", vec![broken_link.clone(), valid_link.clone()]);
        let file2 = create_test_file("file2.md");

        graph.add_file(&file1).unwrap();
        graph.add_file(&file2).unwrap();
        graph.update_links(&file1).unwrap();

        // Create file links map for health analyzer
        let mut files = HashMap::new();
        files.insert(PathBuf::from("file1.md"), vec![broken_link, valid_link]);

        let analyzer = HealthAnalyzer::with_files(&graph, &files);
        let broken = analyzer.find_broken_links().unwrap();

        assert_eq!(broken.len(), 1);
        assert_eq!(broken[0].target, "nonexistent");
    }

    #[test]
    fn test_find_dead_end_notes() {
        let mut graph = LinkGraph::new();

        let link = create_test_link("file1.md", "file2", true);
        let file1 = create_test_file_with_links("file1.md", vec![link]);
        let file2 = create_test_file("file2.md");
        let file3 = create_test_file("file3.md");

        graph.add_file(&file1).unwrap();
        graph.add_file(&file2).unwrap();
        graph.add_file(&file3).unwrap();
        graph.update_links(&file1).unwrap();

        let analyzer = HealthAnalyzer::new(&graph);
        let dead_ends = analyzer.find_dead_end_notes().unwrap();

        let file2_path = PathBuf::from("file2.md");
        let file3_path = PathBuf::from("file3.md");

        // file2 should be a dead end (has incoming but no outgoing)
        assert!(dead_ends.contains(&file2_path));

        // file3 should NOT be in dead ends (it's orphaned, not a dead end)
        assert!(!dead_ends.contains(&file3_path));
    }

    #[test]
    fn test_find_hub_notes() {
        let mut graph = LinkGraph::new();

        let hub_links = vec![
            create_test_link("hub.md", "file1", true),
            create_test_link("hub.md", "file2", true),
        ];
        let file1_links = vec![create_test_link("file1.md", "hub", true)];
        let file2_links = vec![create_test_link("file2.md", "hub", true)];

        let hub = create_test_file_with_links("hub.md", hub_links);
        let file1 = create_test_file_with_links("file1.md", file1_links);
        let file2 = create_test_file_with_links("file2.md", file2_links);

        graph.add_file(&hub).unwrap();
        graph.add_file(&file1).unwrap();
        graph.add_file(&file2).unwrap();
        graph.update_links(&hub).unwrap();
        graph.update_links(&file1).unwrap();
        graph.update_links(&file2).unwrap();

        let analyzer = HealthAnalyzer::new(&graph);
        let hubs = analyzer.find_hub_notes(5).unwrap();

        let hub_path = PathBuf::from("hub.md");

        // Hub should be the top note
        assert!(!hubs.is_empty());
        assert_eq!(hubs[0].0, hub_path);
        assert!(hubs[0].1 >= 4); // At least 4 connections
    }

    #[test]
    fn test_quick_check() {
        let mut graph = LinkGraph::new();

        let file1 = create_test_file("file1.md");
        graph.add_file(&file1).unwrap();

        let analyzer = HealthAnalyzer::new(&graph);
        let report = analyzer.quick_check().unwrap();

        assert_eq!(report.total_notes, 1);
        assert_eq!(report.broken_links.len(), 0);
        assert_eq!(report.orphaned_notes.len(), 1); // file1 is orphaned
    }

    #[test]
    fn test_broken_link_suggestions() {
        let mut graph = LinkGraph::new();

        let file1 = create_test_file("SimilarName.md");
        let file2 = create_test_file("similar_name.md");
        graph.add_file(&file1).unwrap();
        graph.add_file(&file2).unwrap();

        let analyzer = HealthAnalyzer::new(&graph);
        let suggestions = analyzer.suggest_targets("similar");

        // Should find both files
        assert!(!suggestions.is_empty());
    }

    #[test]
    fn test_health_report_is_healthy() {
        let mut report = HealthReport::new();
        report.health_score = 85;
        assert!(report.is_healthy());

        report.health_score = 75;
        assert!(!report.is_healthy());
    }
}
