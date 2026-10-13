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
        self.health_score = Self::score(&HealthCounts {
            total_notes: self.total_notes,
            total_links: self.total_links,
            broken_links: self.broken_links.len(),
            orphaned_notes: self.orphaned_notes.len(),
            isolated_clusters: self.isolated_clusters.len(),
            dead_end_notes: self.dead_end_notes.len(),
        });
    }

    /// The health score for a vault with these counts, the same score
    /// [`Self::calculate_score`] gives a report holding them.
    pub fn score(counts: &HealthCounts) -> u8 {
        if counts.total_notes == 0 {
            return 0;
        }

        // Compute all penalties independently to avoid sequential saturation
        let broken_ratio = counts.broken_links as f64 / counts.total_links.max(1) as f64;
        let orphaned_ratio = counts.orphaned_notes as f64 / counts.total_notes as f64;
        let isolated_ratio = counts.isolated_clusters as f64 / counts.total_notes as f64;
        let dead_end_ratio = counts.dead_end_notes as f64 / counts.total_notes as f64;

        let total_penalty = (broken_ratio * 30.0
            + orphaned_ratio * 20.0
            + isolated_ratio * 15.0
            + dead_end_ratio * 10.0)
            .min(100.0);

        100u8.saturating_sub(total_penalty as u8)
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

/// How many of each issue a vault has, which is all a health score needs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HealthCounts {
    /// Total number of notes
    pub total_notes: usize,
    /// Total number of links
    pub total_links: usize,
    /// Broken links
    pub broken_links: usize,
    /// Notes with no links in or out
    pub orphaned_notes: usize,
    /// Groups of notes cut off from the main graph
    pub isolated_clusters: usize,
    /// Notes with incoming links but none out
    pub dead_end_notes: usize,
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

    /// Every broken link in the vault, each with up to five notes whose name
    /// is close to its target.
    pub fn broken_links(&self) -> Result<Vec<BrokenLink>> {
        self.find_broken_links()
    }

    /// How many broken links the vault has, without working out suggestions.
    pub fn broken_link_count(&self) -> usize {
        let check_valid = self.files.is_some();
        self.broken_link_sources()
            .map(|(_, links)| {
                links
                    .iter()
                    .filter(|link| !(check_valid && link.is_valid))
                    .count()
            })
            .sum()
    }

    /// Notes that are linked to but link nowhere.
    pub fn dead_end_notes(&self) -> Vec<PathBuf> {
        self.graph
            .degrees()
            .filter(|&(_, incoming, outgoing)| outgoing == 0 && incoming > 0)
            .map(|(path, _, _)| path.clone())
            .collect()
    }

    /// The `limit` most connected notes, with their link counts in and out.
    pub fn hub_notes(&self, limit: usize) -> Vec<(PathBuf, usize)> {
        let mut hubs: Vec<(&PathBuf, usize)> = self
            .graph
            .degrees()
            .map(|(path, incoming, outgoing)| (path, incoming + outgoing))
            .filter(|&(_, connections)| connections > 0)
            .collect();
        // Sort by connection count (descending); a stable sort keeps notes
        // with the same count in graph order.
        hubs.sort_by_key(|&(_, connections)| std::cmp::Reverse(connections));
        hubs.truncate(limit);
        hubs.into_iter()
            .map(|(path, connections)| (path.clone(), connections))
            .collect()
    }

    /// Groups of linked notes cut off from the largest group.
    pub fn isolated_clusters(&self) -> Result<Vec<Vec<PathBuf>>> {
        self.find_isolated_clusters()
    }

    /// The counts a health score is made of. Unlike [`Self::analyze`] it
    /// lists nothing and suggests nothing, so it costs a pass over the graph.
    pub fn counts(&self) -> Result<HealthCounts> {
        Ok(HealthCounts {
            total_notes: self.graph.node_count(),
            total_links: self.graph.edge_count(),
            broken_links: self.broken_link_count(),
            orphaned_notes: self.graph.orphaned_notes().len(),
            isolated_clusters: self.find_isolated_clusters()?.len(),
            dead_end_notes: self.dead_end_notes().len(),
        })
    }

    /// Where broken links are read from: the files' own links when given,
    /// otherwise the links the graph could not resolve.
    fn broken_link_sources(&self) -> impl Iterator<Item = (&PathBuf, &Vec<Link>)> {
        self.files
            .unwrap_or_else(|| self.graph.all_unresolved_links())
            .iter()
    }

    /// Find all broken links in the vault
    fn find_broken_links(&self) -> Result<Vec<BrokenLink>> {
        let mut broken = Vec::new();
        let mut stems: Option<StemIndex> = None;
        // Broken links cluster on a few missing targets, so each target's
        // suggestions are worked out once.
        let mut suggested: HashMap<String, Vec<String>> = HashMap::new();
        let mut suggest = |target: &str| {
            suggested
                .entry(target.to_string())
                .or_insert_with(|| {
                    stems
                        .get_or_insert_with(|| StemIndex::new(self.graph))
                        .suggest(target)
                })
                .clone()
        };

        // Raw file links, when given, mark which ones are broken; the graph's
        // own unresolved links are all broken.
        let check_valid = self.files.is_some();
        for (source, links) in self.broken_link_sources() {
            for link in links {
                if check_valid && link.is_valid {
                    continue;
                }
                broken.push(BrokenLink {
                    source_file: source.clone(),
                    target: link.target.clone(),
                    line: link.position.line,
                    suggestions: suggest(&link.target),
                });
            }
        }

        Ok(broken)
    }

    /// Find notes with no outgoing links
    fn find_dead_end_notes(&self) -> Result<Vec<PathBuf>> {
        Ok(self.dead_end_notes())
    }

    /// Find hub notes (notes with many connections)
    fn find_hub_notes(&self, limit: usize) -> Result<Vec<(PathBuf, usize)>> {
        Ok(self.hub_notes(limit))
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

/// The most suggestions a broken link gets.
const MAX_SUGGESTIONS: usize = 5;

/// Every note's stem, lowercased, indexed so a broken link's suggestions are
/// found without comparing its target against every note.
///
/// A note is suggested for a target when either name contains the other,
/// case-insensitively, and the first five such notes in graph order are
/// kept. Names inside the target are its substrings, looked up directly;
/// names containing the target share all of its character trigrams, so the
/// candidates are the notes in every one of those trigrams' lists. Checking
/// every note for every distinct broken target made a health check grow
/// with broken targets times notes.
struct StemIndex {
    /// Each note's stem as written and lowercased, in graph order.
    stems: Vec<(String, String)>,
    /// Lowercased stem to the positions of the notes that have it.
    by_stem: HashMap<String, Vec<usize>>,
    /// Character trigram to the positions of the stems containing it, each
    /// position once and in ascending order.
    by_trigram: HashMap<[char; 3], Vec<usize>>,
    /// The longest stem, in characters.
    longest: usize,
}

impl StemIndex {
    fn new(graph: &LinkGraph) -> Self {
        let stems: Vec<(String, String)> = graph
            .degrees()
            .filter_map(|(path, _, _)| path.file_stem().and_then(|s| s.to_str()))
            .map(|stem| (stem.to_string(), stem.to_lowercase()))
            .collect();
        let mut by_stem: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_trigram: HashMap<[char; 3], Vec<usize>> = HashMap::new();
        let mut longest = 0;
        for (position, (_, lower)) in stems.iter().enumerate() {
            by_stem.entry(lower.clone()).or_default().push(position);
            let chars: Vec<char> = lower.chars().collect();
            longest = longest.max(chars.len());
            for window in chars.windows(3) {
                let list = by_trigram
                    .entry([window[0], window[1], window[2]])
                    .or_default();
                if list.last() != Some(&position) {
                    list.push(position);
                }
            }
        }
        Self {
            stems,
            by_stem,
            by_trigram,
            longest,
        }
    }

    /// The first five stems, in graph order, that contain `target` or that
    /// `target` contains, case-insensitively.
    fn suggest(&self, target: &str) -> Vec<String> {
        let target_lower = target.to_lowercase();
        let chars: Vec<char> = target_lower.chars().collect();
        let mut matches: Vec<usize> = Vec::new();

        // Stems the target contains: every substring of the target no
        // longer than the longest stem, the empty one included, looked up as
        // a whole stem.
        let boundaries: Vec<usize> = target_lower
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(target_lower.len()))
            .collect();
        for (a, &start) in boundaries.iter().enumerate() {
            for &end in boundaries[a..].iter().take(self.longest + 1) {
                if let Some(positions) = self.by_stem.get(&target_lower[start..end]) {
                    matches.extend(positions);
                }
            }
        }

        // Stems that contain the target. A target shorter than a trigram
        // has none to narrow by, so every stem is checked.
        if chars.len() < 3 {
            matches.extend(
                self.stems
                    .iter()
                    .enumerate()
                    .filter(|(_, (_, lower))| lower.contains(&target_lower))
                    .map(|(position, _)| position),
            );
        } else {
            // A trigram no stem has rules every stem out.
            let lists: Option<Vec<&Vec<usize>>> = chars
                .windows(3)
                .map(|w| self.by_trigram.get(&[w[0], w[1], w[2]]))
                .collect();
            if let Some(mut lists) = lists {
                lists.sort_by_key(|list| list.len());
                if let Some((shortest, rest)) = lists.split_first() {
                    matches.extend(shortest.iter().copied().filter(|&position| {
                        rest.iter()
                            .all(|list| list.binary_search(&position).is_ok())
                            && self.stems[position].1.contains(&target_lower)
                    }));
                }
            }
        }

        matches.sort_unstable();
        matches.dedup();
        matches
            .into_iter()
            .take(MAX_SUGGESTIONS)
            .map(|position| self.stems[position].0.clone())
            .collect()
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

        let suggestions = StemIndex::new(&graph).suggest("similar");

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

    /// The suggestions the old scan gave: the first five stems in graph
    /// order that contain the target or that the target contains.
    fn suggest_by_scan(graph: &LinkGraph, target: &str) -> Vec<String> {
        let target_lower = target.to_lowercase();
        graph
            .all_files()
            .iter()
            .filter_map(|path| path.file_stem().and_then(|s| s.to_str()))
            .filter(|stem| {
                let lower = stem.to_lowercase();
                lower.contains(&target_lower) || target_lower.contains(&lower)
            })
            .map(str::to_string)
            .take(MAX_SUGGESTIONS)
            .collect()
    }

    #[test]
    fn indexed_suggestions_match_the_exhaustive_scan() {
        // A small xorshift, so the names are varied but the same every run.
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = move |n: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n as u64) as usize
        };
        let alphabet: Vec<char> = "abAB c-1éÉßİ".chars().collect();
        let word = |next: &mut dyn FnMut(usize) -> usize| {
            let len = next(9);
            (0..len)
                .map(|_| alphabet[next(alphabet.len())])
                .collect::<String>()
        };

        let mut graph = LinkGraph::new();
        for i in 0..400 {
            let name = word(&mut next);
            let file = create_test_file(&format!("d{}/{name}.md", i % 7));
            graph.add_file(&file).unwrap();
        }
        let index = StemIndex::new(&graph);
        for _ in 0..2000 {
            let target = word(&mut next);
            assert_eq!(
                index.suggest(&target),
                suggest_by_scan(&graph, &target),
                "suggestions for {target:?}"
            );
        }
    }

    #[test]
    fn counts_match_the_full_analysis() {
        let mut graph = LinkGraph::new();
        let files = [
            ("hub.md", vec!["a", "b", "missing"]),
            ("a.md", vec!["hub"]),
            ("b.md", vec![]),
            ("lonely.md", vec![]),
            ("x.md", vec!["y"]),
            ("y.md", vec!["x", "gone"]),
        ];
        for (path, _) in &files {
            graph.add_file(&create_test_file(path)).unwrap();
        }
        for (path, targets) in &files {
            let links = targets
                .iter()
                .map(|t| create_test_link(path, t, false))
                .collect();
            graph
                .update_links(&create_test_file_with_links(path, links))
                .unwrap();
        }

        let analyzer = HealthAnalyzer::with_files(&graph, graph.all_unresolved_links());
        let report = analyzer.analyze().unwrap();
        let counts = analyzer.counts().unwrap();
        assert_eq!(counts.broken_links, report.broken_links.len());
        assert_eq!(counts.orphaned_notes, report.orphaned_notes.len());
        assert_eq!(counts.dead_end_notes, report.dead_end_notes.len());
        assert_eq!(counts.isolated_clusters, report.isolated_clusters.len());
        assert_eq!(HealthReport::score(&counts), report.health_score);
        assert_eq!(analyzer.hub_notes(10), report.hub_notes);
        assert_eq!(analyzer.dead_end_notes(), report.dead_end_notes);
        assert_eq!(counts.broken_links, 2);
        assert_eq!(counts.orphaned_notes, 1);
    }
}
