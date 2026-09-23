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

        // Compute all penalties independently to avoid sequential saturation
        let broken_ratio = self.broken_links.len() as f64 / self.total_links.max(1) as f64;
        let orphaned_ratio = self.orphaned_notes.len() as f64 / self.total_notes as f64;
        let isolated_ratio = self.isolated_clusters.len() as f64 / self.total_notes as f64;
        let dead_end_ratio = self.dead_end_notes.len() as f64 / self.total_notes as f64;

        let total_penalty = (broken_ratio * 30.0
            + orphaned_ratio * 20.0
            + isolated_ratio * 15.0
            + dead_end_ratio * 10.0)
            .min(100.0);

        self.health_score = 100u8.saturating_sub(total_penalty as u8);
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

/// Configuration for health analysis
#[derive(Debug, Clone)]
pub struct AnalysisConfig {
    /// Maximum number of hub notes to include in the report (default: 10)
    pub hub_notes_limit: usize,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            hub_notes_limit: 10,
        }
    }
}

/// Vault health analyzer
pub struct HealthAnalyzer<'a> {
    graph: &'a LinkGraph,
    files: Option<&'a HashMap<PathBuf, Vec<Link>>>,
    config: AnalysisConfig,
}

impl<'a> HealthAnalyzer<'a> {
    /// Create a new health analyzer (graph-only, no broken link detection)
    pub fn new(graph: &'a LinkGraph) -> Self {
        Self::with_config(graph, None, AnalysisConfig::default())
    }

    /// Create a new health analyzer with access to unresolved links.
    /// Uses [`AnalysisConfig::default`] for analysis parameters.
    /// (needed for detecting broken links that aren't in the graph)
    pub fn with_files(graph: &'a LinkGraph, files: &'a HashMap<PathBuf, Vec<Link>>) -> Self {
        Self::with_config(graph, Some(files), AnalysisConfig::default())
    }

    /// Create a health analyzer with full configuration
    pub fn with_config(
        graph: &'a LinkGraph,
        files: Option<&'a HashMap<PathBuf, Vec<Link>>>,
        config: AnalysisConfig,
    ) -> Self {
        Self {
            graph,
            files,
            config,
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
        report.hub_notes = self.find_hub_notes(self.config.hub_notes_limit)?;

        // Find isolated clusters
        report.isolated_clusters = self.find_isolated_clusters()?;

        // Calculate overall health score
        report.calculate_score();

        Ok(report)
    }

    /// Find all broken links in the vault
    fn find_broken_links(&self) -> Result<Vec<BrokenLink>> {
        let mut broken = Vec::new();
        let stems = self.note_stems();
        // Broken links cluster on a few missing targets, so each target's
        // suggestions are worked out once.
        let mut suggested: HashMap<String, Vec<String>> = HashMap::new();
        let mut suggest = |target: &str| {
            suggested
                .entry(target.to_string())
                .or_insert_with(|| Self::suggest_targets(&stems, target))
                .clone()
        };

        // If we have access to raw file links, use those (more accurate)
        if let Some(files) = self.files {
            for (source, links) in files {
                for link in links {
                    if !link.is_valid {
                        // Try to find similar targets for suggestions
                        let suggestions = suggest(&link.target);

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
            // Fall back to graph's unresolved links (links that couldn't be resolved)
            for (source, links) in self.graph.all_unresolved_links() {
                for link in links {
                    let suggestions = suggest(&link.target);

                    broken.push(BrokenLink {
                        source_file: source.clone(),
                        target: link.target.clone(),
                        line: link.position.line,
                        suggestions,
                    });
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
        hubs.sort_by_key(|b| std::cmp::Reverse(b.1));
        hubs.truncate(limit);

        Ok(hubs)
    }

    /// Find isolated clusters using weakly connected components analysis.
    /// Returns all components except the largest (the "main graph").
    fn find_isolated_clusters(&self) -> Result<Vec<Vec<PathBuf>>> {
        let mut components = self.graph.connected_components()?;

        if components.len() <= 1 {
            return Ok(Vec::new());
        }

        // Find the largest component (the "main graph") and remove it
        let max_idx = components
            .iter()
            .enumerate()
            .max_by_key(|(_, c)| c.len())
            .map(|(i, _)| i)
            .unwrap_or(0);
        components.remove(max_idx);

        // Return all non-main components as isolated clusters (filter singletons)
        Ok(components.into_iter().filter(|c| c.len() > 1).collect())
    }

    /// Every note's stem, as written and lowercased, for [`Self::suggest_targets`].
    ///
    /// Built once per report. Suggesting used to re-list and re-lowercase every
    /// note for every broken link, so a check that is meant to be quick grew
    /// with broken links times notes.
    fn note_stems(&self) -> Vec<(String, String)> {
        self.graph
            .all_files()
            .iter()
            .filter_map(|path| path.file_stem().and_then(|s| s.to_str()))
            .map(|stem| (stem.to_string(), stem.to_lowercase()))
            .collect()
    }

    /// Suggest similar targets for a broken link, at most five.
    fn suggest_targets(stems: &[(String, String)], target: &str) -> Vec<String> {
        let target_lower = target.to_lowercase();
        stems
            .iter()
            .filter(|(_, lower)| lower.contains(&target_lower) || target_lower.contains(lower))
            .map(|(stem, _)| stem.clone())
            .take(5)
            .collect()
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
        let suggestions = HealthAnalyzer::suggest_targets(&analyzer.note_stems(), "similar");

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

    // --- score independence tests ---

    #[test]
    fn test_score_independent_penalties_all_broken_all_orphaned() {
        // broken_ratio = 1.0 (10 broken / 10 total links) — penalty 30
        // orphaned_ratio = 1.0 (10 orphaned / 10 total notes) — penalty 20
        // total penalty = 50 → health_score = 50
        let mut report = HealthReport::new();
        report.total_notes = 10;
        report.total_links = 10;
        for i in 0..10 {
            report.broken_links.push(BrokenLink {
                source_file: PathBuf::from(format!("file{}.md", i)),
                target: "broken".to_string(),
                line: 1,
                suggestions: Vec::new(),
            });
            report
                .orphaned_notes
                .push(PathBuf::from(format!("orphan{}.md", i)));
        }
        report.calculate_score();
        // penalty = 30 + 20 = 50 → score = 50
        assert_eq!(
            report.health_score, 50,
            "with all links broken and all notes orphaned the score should be 50"
        );
    }

    #[test]
    fn test_score_independent_penalties_small_broken_ratio() {
        // 1 broken link out of 100 total links → broken_ratio = 0.01 → penalty = 0.3
        // No orphans, no isolated clusters, no dead ends.
        // total_penalty = 0.3 → cast to u8 = 0 → score = 100.
        // The score should NOT round to 0; it stays at 100 due to truncating cast.
        let mut report = HealthReport::new();
        report.total_notes = 100;
        report.total_links = 100;
        report.broken_links.push(BrokenLink {
            source_file: PathBuf::from("file1.md"),
            target: "missing".to_string(),
            line: 1,
            suggestions: Vec::new(),
        });
        report.calculate_score();
        // 0.01 * 30.0 = 0.3 → as u8 truncates to 0 → 100 - 0 = 100
        assert_eq!(
            report.health_score, 100,
            "a single broken link out of 100 should not reduce score below 100 after integer truncation"
        );
    }

    #[test]
    fn test_score_independent_penalties_moderate_broken() {
        // 5 broken out of 10 → broken_ratio = 0.5 → penalty = 15
        // 3 orphans out of 10 → orphaned_ratio = 0.3 → penalty = 6
        // total_penalty = 21.0 → health_score = 79
        let mut report = HealthReport::new();
        report.total_notes = 10;
        report.total_links = 10;
        for i in 0..5 {
            report.broken_links.push(BrokenLink {
                source_file: PathBuf::from(format!("f{}.md", i)),
                target: "x".to_string(),
                line: 0,
                suggestions: Vec::new(),
            });
        }
        for i in 0..3 {
            report
                .orphaned_notes
                .push(PathBuf::from(format!("o{}.md", i)));
        }
        report.calculate_score();
        // 0.5*30 + 0.3*20 = 15 + 6 = 21 → 100 - 21 = 79
        assert_eq!(report.health_score, 79);
    }

    // --- isolated_clusters tests ---

    #[test]
    fn test_isolated_clusters_returns_non_main() {
        // Large component: 10-node chain.
        // Medium component: 3-node chain.
        // Small component: 2-node chain.
        // find_isolated_clusters() should return medium and small, not the large one.
        let mut graph = LinkGraph::new();

        // Build the 10-node chain: node0 → node1 → … → node9
        for i in 0..10usize {
            let f = create_test_file(&format!("main{}.md", i));
            graph.add_file(&f).unwrap();
        }
        for i in 1..10usize {
            let src = format!("main{}.md", i);
            let tgt = format!("main{}", i - 1);
            let link = Link {
                type_: LinkType::WikiLink,
                source_file: PathBuf::from(&src),
                target: tgt,
                display_text: None,
                position: SourcePosition::start(),
                resolved_target: None,
                is_valid: true,
            };
            let mut f = create_test_file(&src);
            f.links = vec![link];
            graph.update_links(&f).unwrap();
        }

        // Medium: med0 → med1 → med2
        for i in 0..3usize {
            let f = create_test_file(&format!("med{}.md", i));
            graph.add_file(&f).unwrap();
        }
        for i in 1..3usize {
            let src = format!("med{}.md", i);
            let tgt = format!("med{}", i - 1);
            let link = Link {
                type_: LinkType::WikiLink,
                source_file: PathBuf::from(&src),
                target: tgt,
                display_text: None,
                position: SourcePosition::start(),
                resolved_target: None,
                is_valid: true,
            };
            let mut f = create_test_file(&src);
            f.links = vec![link];
            graph.update_links(&f).unwrap();
        }

        // Small: small0 → small1
        for i in 0..2usize {
            let f = create_test_file(&format!("small{}.md", i));
            graph.add_file(&f).unwrap();
        }
        {
            let link = Link {
                type_: LinkType::WikiLink,
                source_file: PathBuf::from("small1.md"),
                target: "small0".to_string(),
                display_text: None,
                position: SourcePosition::start(),
                resolved_target: None,
                is_valid: true,
            };
            let mut f = create_test_file("small1.md");
            f.links = vec![link];
            graph.update_links(&f).unwrap();
        }

        let analyzer = HealthAnalyzer::new(&graph);
        let isolated = analyzer.find_isolated_clusters().unwrap();

        // Should have exactly 2 non-main clusters (medium and small)
        assert_eq!(
            isolated.len(),
            2,
            "should return medium and small clusters, not the large one"
        );

        let mut sizes: Vec<usize> = isolated.iter().map(|c| c.len()).collect();
        sizes.sort_unstable();
        assert_eq!(
            sizes,
            vec![2, 3],
            "non-main clusters should have sizes 2 and 3"
        );

        // Verify the large component (size 10) is NOT included
        assert!(
            isolated.iter().all(|c| c.len() < 10),
            "the largest component (10 nodes) must not appear in isolated_clusters"
        );
    }

    #[test]
    fn test_isolated_clusters_single_component() {
        // All nodes connected → no isolated clusters.
        let mut graph = LinkGraph::new();
        let a = create_test_file("ca.md");
        let b = create_test_file("cb.md");
        let c = create_test_file("cc.md");
        graph.add_file(&a).unwrap();
        graph.add_file(&b).unwrap();
        graph.add_file(&c).unwrap();

        // a → b → c
        let link_b = Link {
            type_: LinkType::WikiLink,
            source_file: PathBuf::from("cb.md"),
            target: "ca".to_string(),
            display_text: None,
            position: SourcePosition::start(),
            resolved_target: None,
            is_valid: true,
        };
        let mut fb = create_test_file("cb.md");
        fb.links = vec![link_b];
        graph.update_links(&fb).unwrap();

        let link_c = Link {
            type_: LinkType::WikiLink,
            source_file: PathBuf::from("cc.md"),
            target: "cb".to_string(),
            display_text: None,
            position: SourcePosition::start(),
            resolved_target: None,
            is_valid: true,
        };
        let mut fc = create_test_file("cc.md");
        fc.links = vec![link_c];
        graph.update_links(&fc).unwrap();

        let analyzer = HealthAnalyzer::new(&graph);
        let isolated = analyzer.find_isolated_clusters().unwrap();
        assert!(
            isolated.is_empty(),
            "single component should return no isolated clusters"
        );
    }

    #[test]
    fn test_isolated_clusters_singletons_filtered() {
        // Large component of 5 nodes plus several singleton orphans.
        // find_isolated_clusters() filters singletons (len == 1).
        let mut graph = LinkGraph::new();

        // Build 5-node chain: hub0 → hub1 → hub2 → hub3 → hub4
        for i in 0..5usize {
            let f = create_test_file(&format!("hub{}.md", i));
            graph.add_file(&f).unwrap();
        }
        for i in 1..5usize {
            let src = format!("hub{}.md", i);
            let tgt = format!("hub{}", i - 1);
            let link = Link {
                type_: LinkType::WikiLink,
                source_file: PathBuf::from(&src),
                target: tgt,
                display_text: None,
                position: SourcePosition::start(),
                resolved_target: None,
                is_valid: true,
            };
            let mut f = create_test_file(&src);
            f.links = vec![link];
            graph.update_links(&f).unwrap();
        }

        // Add 3 singleton orphans (no links whatsoever)
        for i in 0..3usize {
            let f = create_test_file(&format!("singleton{}.md", i));
            graph.add_file(&f).unwrap();
        }

        let analyzer = HealthAnalyzer::new(&graph);
        let isolated = analyzer.find_isolated_clusters().unwrap();

        // Singletons must be filtered out; result should be empty
        // (the hub chain is the main component, singletons are len==1 → filtered)
        assert!(
            isolated.is_empty(),
            "singleton orphans should be filtered from isolated_clusters; got: {:?}",
            isolated
        );
    }

    // --- broken_links fallback to unresolved_links ---

    #[test]
    fn test_broken_links_fallback_uses_unresolved() {
        // HealthAnalyzer::new() (no `files` map) should fall back to
        // graph.all_unresolved_links() when find_broken_links() is called.
        let mut graph = LinkGraph::new();

        // target.md exists; missing.md does not.
        let target = create_test_file("target.md");
        graph.add_file(&target).unwrap();

        // source.md links to both; "missing" will become an unresolved link.
        let missing_link = Link {
            type_: LinkType::WikiLink,
            source_file: PathBuf::from("source.md"),
            target: "missing".to_string(),
            display_text: None,
            position: SourcePosition::start(),
            resolved_target: None,
            is_valid: true,
        };
        let valid_link = Link {
            type_: LinkType::WikiLink,
            source_file: PathBuf::from("source.md"),
            target: "target".to_string(),
            display_text: None,
            position: SourcePosition::start(),
            resolved_target: None,
            is_valid: true,
        };
        let mut source = create_test_file("source.md");
        source.links = vec![missing_link, valid_link];
        graph.add_file(&source).unwrap();
        graph.update_links(&source).unwrap();

        // Confirm the graph has tracked the unresolved link
        assert_eq!(graph.unresolved_link_count(), 1);

        // Use HealthAnalyzer::new() — no files map → fallback path
        let analyzer = HealthAnalyzer::new(&graph);
        let broken = analyzer.find_broken_links().unwrap();

        assert_eq!(
            broken.len(),
            1,
            "fallback should surface the one unresolved link"
        );
        assert_eq!(broken[0].target, "missing");
        assert_eq!(broken[0].source_file, PathBuf::from("source.md"));
    }
}
