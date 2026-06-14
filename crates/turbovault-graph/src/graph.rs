//! Link graph using petgraph for vault relationship analysis

use petgraph::prelude::*;
use petgraph::unionfind::UnionFind;
use petgraph::visit::{EdgeRef, NodeIndexable};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use turbovault_core::prelude::*;

/// Node index type for graph
type NodeIndex = petgraph::graph::NodeIndex;

/// Link graph for analyzing vault relationships
pub struct LinkGraph {
    /// Directed graph: nodes are file paths, edges are links
    graph: DiGraph<PathBuf, Link>,

    /// Map from file name (stem, lowercased) to node indices.
    /// Multiple files may share the same lowercased stem on case-sensitive
    /// filesystems (e.g. `Note.md` and `NOTE.md` on ext4). We store all
    /// candidates and resolve to the first match, mirroring Obsidian's
    /// "first found wins" behaviour.
    file_index: HashMap<String, Vec<NodeIndex>>,

    /// Map from aliases (lowercased) to node indices.
    /// Same multi-value semantics as `file_index`.
    alias_index: HashMap<String, Vec<NodeIndex>>,

    /// Map from full path to node index (for quick lookups)
    path_index: HashMap<PathBuf, NodeIndex>,

    /// Links that could not be resolved to a target file, grouped by source path.
    /// Used by HealthAnalyzer for broken link detection.
    unresolved_links: HashMap<PathBuf, Vec<Link>>,

    /// Index from reversed lowercase path suffix to node indices for O(1) path-suffix resolution.
    /// Used by `resolve_link` to avoid O(N) scans of `path_index`.
    path_suffix_index: HashMap<Vec<String>, Vec<NodeIndex>>,
}

impl LinkGraph {
    /// Create a new link graph
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            file_index: HashMap::new(),
            alias_index: HashMap::new(),
            path_index: HashMap::new(),
            unresolved_links: HashMap::new(),
            path_suffix_index: HashMap::new(),
        }
    }

    /// Total number of unresolved links across all source files.
    pub fn unresolved_link_count(&self) -> usize {
        self.unresolved_links.values().map(|v| v.len()).sum()
    }

    /// Add a file to the graph
    pub fn add_file(&mut self, file: &VaultFile) -> Result<()> {
        let path = file.path.clone();

        // Create node if not exists
        let node_idx = if let Some(&idx) = self.path_index.get(&path) {
            idx
        } else {
            let idx = self.graph.add_node(path.clone());
            self.path_index.insert(path.clone(), idx);

            // Add to file_index by stem (lowercased for case-insensitive resolution)
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                self.file_index
                    .entry(stem.to_lowercase())
                    .or_default()
                    .push(idx);
            }

            // Build path suffix entries for folder-qualified lookups like [[Folder/Note]]
            let components: Vec<String> = path
                .iter()
                .filter_map(|c| c.to_str())
                .map(|s| {
                    let lower = s.to_lowercase();
                    lower.strip_suffix(".md").unwrap_or(&lower).to_string()
                })
                .collect();
            for i in (0..components.len()).rev() {
                let suffix = components[i..].to_vec();
                self.path_suffix_index.entry(suffix).or_default().push(idx);
            }

            idx
        };

        // Register aliases from frontmatter (lowercased for case-insensitive resolution).
        // Guard against duplicates: add_file may be called multiple times for the
        // same path (e.g. on every write_file), so only push if not already present.
        if let Some(fm) = &file.frontmatter {
            for alias in fm.aliases() {
                let entries = self.alias_index.entry(alias.to_lowercase()).or_default();
                if !entries.contains(&node_idx) {
                    entries.push(node_idx);
                }
            }
        }

        Ok(())
    }

    /// Remove a file from the graph.
    ///
    /// **Important**: petgraph's `remove_node` uses swap-remove — the last node
    /// in the graph is moved into the removed node's slot. We must update all
    /// external index maps (`path_index`, `file_index`, `alias_index`) to reflect
    /// the swapped node's new `NodeIndex`.
    pub fn remove_file(&mut self, path: &PathBuf) -> Result<()> {
        if let Some(&idx) = self.path_index.get(path) {
            // Remove the target node from all indices
            self.path_index.remove(path);
            self.unresolved_links.remove(path);

            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                let key = stem.to_lowercase();
                if let Some(indices) = self.file_index.get_mut(&key) {
                    indices.retain(|&i| i != idx);
                    if indices.is_empty() {
                        self.file_index.remove(&key);
                    }
                }
            }

            // Remove aliases pointing to this node
            for indices in self.alias_index.values_mut() {
                indices.retain(|&i| i != idx);
            }
            self.alias_index.retain(|_, indices| !indices.is_empty());

            // Remove path_suffix_index entries pointing to this node
            for indices in self.path_suffix_index.values_mut() {
                indices.retain(|&i| i != idx);
            }
            self.path_suffix_index
                .retain(|_, indices| !indices.is_empty());

            // Before removing, identify the node that will be swapped into `idx`.
            // petgraph moves the last node (highest index) into the removed slot.
            let last_idx = NodeIndex::new(self.graph.node_count() - 1);
            let swapped_path = if last_idx != idx {
                Some(self.graph[last_idx].clone())
            } else {
                None
            };

            // Remove node and all edges
            self.graph.remove_node(idx);

            // Fix up index maps for the swapped node (formerly at last_idx, now at idx)
            if let Some(swapped_path) = swapped_path {
                // Update path_index
                self.path_index.insert(swapped_path.clone(), idx);

                // Update file_index: replace last_idx with idx
                if let Some(stem) = swapped_path.file_stem().and_then(|s| s.to_str()) {
                    let key = stem.to_lowercase();
                    if let Some(indices) = self.file_index.get_mut(&key) {
                        for node_idx in indices.iter_mut() {
                            if *node_idx == last_idx {
                                *node_idx = idx;
                            }
                        }
                    }
                }

                // Update alias_index: replace last_idx with idx
                for indices in self.alias_index.values_mut() {
                    for node_idx in indices.iter_mut() {
                        if *node_idx == last_idx {
                            *node_idx = idx;
                        }
                    }
                }

                // Update path_suffix_index: replace last_idx with idx
                for indices in self.path_suffix_index.values_mut() {
                    for node_idx in indices.iter_mut() {
                        if *node_idx == last_idx {
                            *node_idx = idx;
                        }
                    }
                }

                // Update unresolved_links key if the swapped node had entries
                // (key is by path, not by index, so no change needed — paths don't move)
            }
        }

        Ok(())
    }

    /// Add links from a parsed file to the graph
    pub fn update_links(&mut self, file: &VaultFile) -> Result<()> {
        let source_path = &file.path;

        // Get or create source node
        let source_idx = if let Some(&idx) = self.path_index.get(source_path) {
            idx
        } else {
            let idx = self.graph.add_node(source_path.clone());
            self.path_index.insert(source_path.clone(), idx);
            // Also populate file_index and path_suffix_index for stem-based resolution
            if let Some(stem) = source_path.file_stem().and_then(|s| s.to_str()) {
                self.file_index
                    .entry(stem.to_lowercase())
                    .or_default()
                    .push(idx);
            }
            let components: Vec<String> = source_path
                .iter()
                .filter_map(|c| c.to_str())
                .map(|s| {
                    let lower = s.to_lowercase();
                    lower.strip_suffix(".md").unwrap_or(&lower).to_string()
                })
                .collect();
            for i in (0..components.len()).rev() {
                let suffix = components[i..].to_vec();
                self.path_suffix_index.entry(suffix).or_default().push(idx);
            }
            idx
        };

        // Remove old outgoing edges and unresolved links for this source
        let outgoing: Vec<_> = self.graph.edges(source_idx).map(|e| e.id()).collect();
        for edge_id in outgoing {
            self.graph.remove_edge(edge_id);
        }
        self.unresolved_links.remove(source_path);

        // Add edges for each internal note link.
        //
        // A link becomes a graph edge iff its target is a *note* — i.e. not an
        // attachment/media/data file (see `is_note_reference`). This one rule
        // covers every link form uniformly:
        // - Obsidian wikilinks/embeds/heading-refs/block-refs to notes
        //   (`[[Note]]`, `[[Note#H]]`); `[[image.png]]`/`![[chart.svg]]` are
        //   attachments and are skipped.
        // - OKF cross-links (spec §5), which are standard markdown links to a
        //   `.md` document (`[customers](/tables/customers.md)`); markdown links
        //   to images/PDFs/external URLs are skipped.
        // Skipped links never enter the note graph or broken-link reports.
        for link in &file.links {
            let is_graph_link = match link.type_ {
                LinkType::WikiLink
                | LinkType::Embed
                | LinkType::BlockRef
                | LinkType::HeadingRef
                | LinkType::MarkdownLink => is_note_reference(&link.target),
                LinkType::Anchor | LinkType::ExternalLink => false,
            };
            if is_graph_link {
                // Skip same-document anchors like [[#Heading]]
                let clean_target = link.target.split('#').next().unwrap_or("").trim();
                if clean_target.is_empty() {
                    continue;
                }

                if let Some(target_idx) = self.resolve_link(&link.target) {
                    // Skip self-references (a note linking to itself) — they are
                    // resolved, not broken, but must not become graph self-loops
                    // (they would distort cycle detection and centrality).
                    if target_idx != source_idx {
                        self.graph.add_edge(source_idx, target_idx, link.clone());
                    }
                } else {
                    // Track unresolved links for broken link detection
                    let mut broken = link.clone();
                    broken.is_valid = false;
                    self.unresolved_links
                        .entry(source_path.clone())
                        .or_default()
                        .push(broken);
                }
            }
        }

        Ok(())
    }

    /// Resolve a link target to a node index.
    ///
    /// Handles both Obsidian wikilink targets (`Note`, `Folder/Note`,
    /// `Note#Heading`) and OKF cross-link targets (`/tables/orders.md`,
    /// `./customers.md`). Resolution is case-insensitive to match Obsidian's
    /// behaviour, and the `.md` suffix / leading `/` / `./` are normalized away
    /// so both link styles share one resolution path.
    fn resolve_link(&self, target: &str) -> Option<NodeIndex> {
        // Normalize into lowercased, `.md`-stripped path components.
        // Returns None for external URLs, pure anchors, and empty targets.
        let parts = turbovault_core::okf::normalize_link_target(target)?;

        // Single component (`Note`, `orders.md`): try file stem first.
        if parts.len() == 1
            && let Some(indices) = self.file_index.get(&parts[0])
            && let Some(&idx) = indices.first()
        {
            return Some(idx);
        }

        // Alias match against the full target — aliases are arbitrary strings
        // and may contain `/` (e.g. an alias literally `Projects/Roadmap`), so
        // match the joined form, not just single-component targets.
        let joined = parts.join("/");
        if let Some(indices) = self.alias_index.get(&joined)
            && let Some(&idx) = indices.first()
        {
            return Some(idx);
        }

        // Folder-qualified or bundle-relative links: path-suffix match.
        if let Some(candidates) = self.path_suffix_index.get(&parts) {
            if candidates.len() == 1 {
                return Some(candidates[0]);
            }
            // Multiple matches — pick the shortest path (most specific)
            if !candidates.is_empty() {
                return candidates
                    .iter()
                    .min_by_key(|&&idx| self.graph[idx].components().count())
                    .copied();
            }
        }

        None
    }

    /// Get all backlinks to a file (files that link to this file)
    pub fn backlinks(&self, path: &PathBuf) -> Result<Vec<(PathBuf, Vec<Link>)>> {
        if let Some(&target_idx) = self.path_index.get(path) {
            let backlinks: Vec<_> = self
                .graph
                .edges_directed(target_idx, Incoming)
                .map(|edge| {
                    let source_idx = edge.source();
                    let source_path = self.graph[source_idx].clone();
                    (source_path, edge.weight().clone())
                })
                .fold(HashMap::new(), |mut acc, (path, link)| {
                    acc.entry(path).or_insert_with(Vec::new).push(link);
                    acc
                })
                .into_iter()
                .collect();

            Ok(backlinks)
        } else {
            Ok(vec![])
        }
    }

    /// Get all forward links from a file (files this file links to)
    pub fn forward_links(&self, path: &PathBuf) -> Result<Vec<(PathBuf, Vec<Link>)>> {
        if let Some(&source_idx) = self.path_index.get(path) {
            let forward_links: Vec<_> = self
                .graph
                .edges(source_idx)
                .map(|edge| {
                    let target_idx = edge.target();
                    let target_path = self.graph[target_idx].clone();
                    (target_path, edge.weight().clone())
                })
                .fold(HashMap::new(), |mut acc, (path, link)| {
                    acc.entry(path).or_insert_with(Vec::new).push(link);
                    acc
                })
                .into_iter()
                .collect();

            Ok(forward_links)
        } else {
            Ok(vec![])
        }
    }

    /// Find all orphaned notes (no incoming or outgoing links)
    pub fn orphaned_notes(&self) -> Vec<PathBuf> {
        self.graph
            .node_indices()
            .filter(|&idx| {
                let in_degree = self.graph.edges_directed(idx, Incoming).count();
                let out_degree = self.graph.edges(idx).count();
                in_degree == 0 && out_degree == 0
            })
            .map(|idx| self.graph[idx].clone())
            .collect()
    }

    /// Find related notes within N hops (breadth-first search)
    pub fn related_notes(&self, path: &PathBuf, max_hops: usize) -> Result<Vec<PathBuf>> {
        if let Some(&start_idx) = self.path_index.get(path) {
            let mut visited = HashSet::new();
            let mut queue = VecDeque::new();
            queue.push_back((start_idx, 0));
            let mut related = Vec::new();

            visited.insert(start_idx);

            while let Some((idx, hops)) = queue.pop_front() {
                if hops > 0 {
                    related.push(self.graph[idx].clone());
                }

                if hops < max_hops {
                    // Add all neighbors
                    for neighbor_idx in self.graph.neighbors(idx) {
                        if visited.insert(neighbor_idx) {
                            queue.push_back((neighbor_idx, hops + 1));
                        }
                    }

                    // Also traverse incoming edges
                    for neighbor_idx in self.graph.edges_directed(idx, Incoming).map(|e| e.source())
                    {
                        if visited.insert(neighbor_idx) {
                            queue.push_back((neighbor_idx, hops + 1));
                        }
                    }
                }
            }

            Ok(related)
        } else {
            Ok(vec![])
        }
    }

    /// Find strongly connected components (cycles in the graph)
    pub fn cycles(&self) -> Vec<Vec<PathBuf>> {
        let sccs = petgraph::algo::kosaraju_scc(&self.graph);
        sccs.into_iter()
            .filter(|scc| scc.len() > 1) // Only return actual cycles (size > 1)
            .map(|scc| scc.iter().map(|&idx| self.graph[idx].clone()).collect())
            .collect()
    }

    /// Get statistics about the graph
    pub fn stats(&self) -> GraphStats {
        let node_count = self.graph.node_count();
        let edge_count = self.graph.edge_count();

        let orphaned_count = self.orphaned_notes().len();

        let avg_links_per_file = if node_count > 0 {
            edge_count as f64 / node_count as f64
        } else {
            0.0
        };

        GraphStats {
            total_files: node_count,
            total_links: edge_count,
            orphaned_files: orphaned_count,
            average_links_per_file: avg_links_per_file,
        }
    }

    /// Get all file paths in the graph
    pub fn all_files(&self) -> Vec<PathBuf> {
        self.graph
            .node_indices()
            .map(|idx| self.graph[idx].clone())
            .collect()
    }

    /// Get node count
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Get edge count
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Get incoming links to a file (just the Link objects)
    pub fn incoming_links(&self, path: &PathBuf) -> Result<Vec<Link>> {
        if let Some(&target_idx) = self.path_index.get(path) {
            let links: Vec<Link> = self
                .graph
                .edges_directed(target_idx, Incoming)
                .map(|edge| edge.weight().clone())
                .collect();
            Ok(links)
        } else {
            Ok(vec![])
        }
    }

    /// Get outgoing links from a file (just the Link objects)
    pub fn outgoing_links(&self, path: &PathBuf) -> Result<Vec<Link>> {
        if let Some(&source_idx) = self.path_index.get(path) {
            let links: Vec<Link> = self
                .graph
                .edges(source_idx)
                .map(|edge| edge.weight().clone())
                .collect();
            Ok(links)
        } else {
            Ok(vec![])
        }
    }

    /// Get all links in the graph, grouped by source file
    pub fn all_links(&self) -> HashMap<PathBuf, Vec<Link>> {
        let mut result = HashMap::new();

        for node_idx in self.graph.node_indices() {
            let source_path = self.graph[node_idx].clone();
            let links: Vec<Link> = self
                .graph
                .edges(node_idx)
                .map(|edge| edge.weight().clone())
                .collect();

            if !links.is_empty() {
                result.insert(source_path, links);
            }
        }

        result
    }

    /// Get all unresolved links, grouped by source file.
    /// Each link has `is_valid == false` and represents a wikilink or embed
    /// whose target could not be resolved to an existing vault file.
    pub fn all_unresolved_links(&self) -> &HashMap<PathBuf, Vec<Link>> {
        &self.unresolved_links
    }

    /// Find weakly connected components in the graph (treating edges as undirected).
    /// Uses UnionFind for O(V + E * alpha(V)) performance.
    pub fn connected_components(&self) -> Result<Vec<Vec<PathBuf>>> {
        let node_bound = self.graph.node_bound();
        if node_bound == 0 {
            return Ok(Vec::new());
        }

        let mut uf = UnionFind::new(node_bound);
        for edge in self.graph.edge_references() {
            uf.union(edge.source().index(), edge.target().index());
        }

        // Group node indices by their representative
        let mut groups: HashMap<usize, Vec<NodeIndex>> = HashMap::new();
        for idx in self.graph.node_indices() {
            let rep = uf.find(idx.index());
            groups.entry(rep).or_default().push(idx);
        }

        let result: Vec<Vec<PathBuf>> = groups
            .into_values()
            .map(|component| {
                component
                    .iter()
                    .map(|&idx| self.graph[idx].clone())
                    .collect()
            })
            .collect();

        Ok(result)
    }
}

impl Default for LinkGraph {
    fn default() -> Self {
        Self::new()
    }
}

/// True if a link target refers to a *note* (as opposed to an attachment,
/// image, media, or data file), ignoring any `#fragment`.
///
/// The discriminator is the target's file extension, which is independent of
/// link syntax (wikilink vs markdown) and of whether the vault is an OKF
/// bundle: a target is a note unless its final path segment carries a known
/// non-note extension. Extension-less targets (`Note`, `Folder/Note`) and
/// dotted note names (`Release v1.2`) are notes; `image.png`, `report.pdf`,
/// `data.csv` are not. A fragment-only target (`#heading`) is not a note
/// reference.
fn is_note_reference(target: &str) -> bool {
    let path = target.split('#').next().unwrap_or("").trim_end();
    if path.is_empty() {
        return false;
    }
    let last = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match last.rsplit_once('.') {
        // Has an extension with a non-empty stem → note unless it's an attachment.
        Some((stem, ext)) if !stem.is_empty() => !is_attachment_ext(ext),
        // No extension (or a leading-dot name) → treat as a note.
        _ => true,
    }
}

/// True if `ext` (any case) is a known non-note file extension — images,
/// documents, data, web assets, media, archives, and office formats. `md`,
/// `markdown`, and `txt` are intentionally absent (those are notes).
fn is_attachment_ext(ext: &str) -> bool {
    const ATTACHMENT_EXTS: &[&str] = &[
        // images
        "png", "jpg", "jpeg", "gif", "svg", "webp", "bmp", "ico", "avif", "tiff", //
        // documents / data
        "pdf", "csv", "tsv", "json", "yaml", "yml", "xml", "parquet", "sqlite", "db", //
        // web assets
        "html", "htm", "css", "js", "mjs", "wasm", //
        // media
        "mp4", "mov", "webm", "mkv", "mp3", "wav", "ogg", "m4a", "flac", //
        // archives
        "zip", "tar", "gz", "tgz", "7z", "rar", //
        // office
        "xlsx", "docx", "pptx", "key", "numbers", "pages",
    ];
    let lower = ext.to_ascii_lowercase();
    ATTACHMENT_EXTS.contains(&lower.as_str())
}

/// Statistics about the graph
#[derive(Debug, Clone)]
pub struct GraphStats {
    pub total_files: usize,
    pub total_links: usize,
    pub orphaned_files: usize,
    pub average_links_per_file: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_file(path: &str, links: Vec<&str>) -> VaultFile {
        let parsed_links: Vec<Link> = links
            .into_iter()
            .enumerate()
            .map(|(i, target)| Link {
                type_: LinkType::WikiLink,
                source_file: PathBuf::from(path),
                target: target.to_string(),
                display_text: None,
                position: SourcePosition::new(0, 0, i * 10, 10),
                resolved_target: None,
                is_valid: true,
            })
            .collect();

        let mut vault_file = VaultFile::new(
            PathBuf::from(path),
            String::new(),
            FileMetadata {
                path: PathBuf::from(path),
                size: 0,
                created_at: 0.0,
                modified_at: 0.0,
                checksum: String::new(),
                is_attachment: false,
            },
        );
        vault_file.links = parsed_links;
        vault_file
    }

    #[test]
    fn test_add_file() {
        let mut graph = LinkGraph::new();
        let file = create_test_file("note.md", vec![]);

        assert!(graph.add_file(&file).is_ok());
        assert_eq!(graph.node_count(), 1);
    }

    #[test]
    fn test_add_multiple_files() {
        let mut graph = LinkGraph::new();
        let file1 = create_test_file("note1.md", vec![]);
        let file2 = create_test_file("note2.md", vec![]);

        graph.add_file(&file1).unwrap();
        graph.add_file(&file2).unwrap();

        assert_eq!(graph.node_count(), 2);
    }

    #[test]
    fn test_update_links() {
        let mut graph = LinkGraph::new();
        let file1 = create_test_file("note1.md", vec![]);
        let file2 = create_test_file("note2.md", vec!["note1"]);

        graph.add_file(&file1).unwrap();
        graph.add_file(&file2).unwrap();
        graph.update_links(&file2).unwrap();

        assert_eq!(graph.edge_count(), 1);
    }

    #[test]
    fn test_orphaned_notes() {
        let mut graph = LinkGraph::new();
        let orphan = create_test_file("orphan.md", vec![]);
        let linked1 = create_test_file("note1.md", vec![]);
        let linked2 = create_test_file("note2.md", vec!["note1"]);

        graph.add_file(&orphan).unwrap();
        graph.add_file(&linked1).unwrap();
        graph.add_file(&linked2).unwrap();
        graph.update_links(&linked2).unwrap();

        let orphans = graph.orphaned_notes();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0], PathBuf::from("orphan.md"));
    }

    #[test]
    fn test_graph_stats() {
        let mut graph = LinkGraph::new();
        let file1 = create_test_file("note1.md", vec![]);
        let file2 = create_test_file("note2.md", vec!["note1"]);

        graph.add_file(&file1).unwrap();
        graph.add_file(&file2).unwrap();
        graph.update_links(&file2).unwrap();

        let stats = graph.stats();
        assert_eq!(stats.total_files, 2);
        assert_eq!(stats.total_links, 1);
        assert_eq!(stats.orphaned_files, 0); // Both notes have links: note1 has incoming, note2 has outgoing
    }

    #[test]
    fn test_unresolved_links_tracked() {
        let mut graph = LinkGraph::new();
        let file1 = create_test_file("note1.md", vec![]);
        // note2 links to note1 (exists) and nonexistent (doesn't exist)
        let file2 = create_test_file("note2.md", vec!["note1", "nonexistent"]);

        graph.add_file(&file1).unwrap();
        graph.add_file(&file2).unwrap();
        graph.update_links(&file2).unwrap();

        // Resolved link should be in the graph
        assert_eq!(graph.edge_count(), 1);

        // Unresolved link should be tracked
        let unresolved = graph.all_unresolved_links();
        let note2_path = PathBuf::from("note2.md");
        assert!(unresolved.contains_key(&note2_path));
        assert_eq!(unresolved[&note2_path].len(), 1);
        assert_eq!(unresolved[&note2_path][0].target, "nonexistent");
        assert!(!unresolved[&note2_path][0].is_valid);
    }

    #[test]
    fn test_case_insensitive_resolution() {
        let mut graph = LinkGraph::new();
        let file1 = create_test_file("My Note.md", vec![]);
        // Link uses different case
        let file2 = create_test_file("linker.md", vec!["my note"]);

        graph.add_file(&file1).unwrap();
        graph.add_file(&file2).unwrap();
        graph.update_links(&file2).unwrap();

        // Should resolve despite case mismatch
        assert_eq!(graph.edge_count(), 1);
        assert!(graph.all_unresolved_links().is_empty());
    }

    #[test]
    fn test_unresolved_links_cleared_on_update() {
        let mut graph = LinkGraph::new();
        let file1 = create_test_file("note1.md", vec![]);
        let file2_broken = create_test_file("note2.md", vec!["nonexistent"]);

        graph.add_file(&file1).unwrap();
        graph.add_file(&file2_broken).unwrap();
        graph.update_links(&file2_broken).unwrap();

        assert_eq!(graph.all_unresolved_links().len(), 1);

        // Now update note2 to link to note1 instead
        let file2_fixed = create_test_file("note2.md", vec!["note1"]);
        graph.update_links(&file2_fixed).unwrap();

        // Unresolved links should be cleared
        assert!(graph.all_unresolved_links().is_empty());
        assert_eq!(graph.edge_count(), 1);
    }

    #[test]
    fn test_case_insensitive_collision_both_indexed() {
        // On case-sensitive filesystems, Note.md and NOTE.md can coexist.
        // Both should be in the graph and the first-added should win for
        // resolution, but neither should be silently dropped.
        let mut graph = LinkGraph::new();
        let file1 = create_test_file("Note.md", vec![]);
        let file2 = create_test_file("NOTE.md", vec![]);
        let linker = create_test_file("linker.md", vec!["note"]);

        graph.add_file(&file1).unwrap();
        graph.add_file(&file2).unwrap();
        graph.add_file(&linker).unwrap();
        graph.update_links(&linker).unwrap();

        // Both files should exist as nodes
        assert_eq!(graph.node_count(), 3);

        // Link should resolve (to whichever was added first)
        assert_eq!(graph.edge_count(), 1);
        assert!(graph.all_unresolved_links().is_empty());
    }

    #[test]
    fn test_remove_file_with_case_collision() {
        let mut graph = LinkGraph::new();
        let file1 = create_test_file("Note.md", vec![]);
        let file2 = create_test_file("NOTE.md", vec![]);

        graph.add_file(&file1).unwrap();
        graph.add_file(&file2).unwrap();
        assert_eq!(graph.node_count(), 2);

        // Remove first file — second should still be findable
        graph.remove_file(&PathBuf::from("Note.md")).unwrap();

        let linker = create_test_file("linker.md", vec!["note"]);
        graph.add_file(&linker).unwrap();
        graph.update_links(&linker).unwrap();

        // Should resolve to NOTE.md, not to linker itself (self-loop)
        assert_eq!(graph.edge_count(), 1);
        assert!(graph.all_unresolved_links().is_empty());

        // Verify the edge target is actually NOTE.md
        let forward = graph.forward_links(&PathBuf::from("linker.md")).unwrap();
        assert_eq!(forward.len(), 1);
        assert_eq!(forward[0].0, PathBuf::from("NOTE.md"));
    }

    #[test]
    fn test_remove_node_swap_fixup_three_nodes() {
        // Regression test for petgraph swap-remove index invalidation.
        // When the first node is removed, petgraph moves the last node
        // into its slot. Our index maps must be updated accordingly.
        let mut graph = LinkGraph::new();
        let a = create_test_file("a.md", vec![]);
        let b = create_test_file("b.md", vec![]);
        let c = create_test_file("c.md", vec!["b"]);

        graph.add_file(&a).unwrap(); // NodeIndex(0)
        graph.add_file(&b).unwrap(); // NodeIndex(1)
        graph.add_file(&c).unwrap(); // NodeIndex(2)
        graph.update_links(&c).unwrap();

        assert_eq!(graph.edge_count(), 1);

        // Remove a.md — petgraph swaps c.md (last) into slot 0.
        // All index maps for c.md must be updated.
        graph.remove_file(&PathBuf::from("a.md")).unwrap();

        assert_eq!(graph.node_count(), 2);

        // Verify c.md is still reachable and its edges are correct
        let forward = graph.forward_links(&PathBuf::from("c.md")).unwrap();
        assert_eq!(forward.len(), 1);
        assert_eq!(forward[0].0, PathBuf::from("b.md"));

        // Verify b.md backlinks still point to c.md
        let back = graph.backlinks(&PathBuf::from("b.md")).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].0, PathBuf::from("c.md"));

        // Adding a new link to c.md should still work
        let d = create_test_file("d.md", vec!["c"]);
        graph.add_file(&d).unwrap();
        graph.update_links(&d).unwrap();

        let c_back = graph.backlinks(&PathBuf::from("c.md")).unwrap();
        assert_eq!(c_back.len(), 1);
        assert_eq!(c_back[0].0, PathBuf::from("d.md"));
    }

    #[test]
    fn test_resolve_link_path_suffix_without_extension() {
        // Obsidian wikilinks like [[folder/Note]] should resolve to
        // folder/Note.md without requiring the .md extension.
        let mut graph = LinkGraph::new();
        let file = create_test_file("projects/ideas/My Note.md", vec![]);
        let linker = create_test_file("index.md", vec!["ideas/My Note"]);

        graph.add_file(&file).unwrap();
        graph.add_file(&linker).unwrap();
        graph.update_links(&linker).unwrap();

        assert_eq!(graph.edge_count(), 1);
        assert!(graph.all_unresolved_links().is_empty());
    }

    // --- connected_components tests ---

    #[test]
    fn test_connected_components_weakly_connected() {
        // A→B→C is a directed chain. Weakly connected: all 3 belong to one component.
        let mut graph = LinkGraph::new();
        let a = create_test_file("a.md", vec![]);
        let b = create_test_file("b.md", vec!["a"]);
        let c = create_test_file("c.md", vec!["b"]);

        graph.add_file(&a).unwrap();
        graph.add_file(&b).unwrap();
        graph.add_file(&c).unwrap();
        graph.update_links(&b).unwrap();
        graph.update_links(&c).unwrap();

        let components = graph.connected_components().unwrap();
        assert_eq!(
            components.len(),
            1,
            "chain A→B→C should form a single weakly-connected component"
        );
        assert_eq!(components[0].len(), 3);
    }

    #[test]
    fn test_connected_components_two_islands() {
        // A→B and C→D with no link between the pairs → 2 components.
        let mut graph = LinkGraph::new();
        let a = create_test_file("island_a1.md", vec![]);
        let b = create_test_file("island_a2.md", vec!["island_a1"]);
        let c = create_test_file("island_b1.md", vec![]);
        let d = create_test_file("island_b2.md", vec!["island_b1"]);

        graph.add_file(&a).unwrap();
        graph.add_file(&b).unwrap();
        graph.add_file(&c).unwrap();
        graph.add_file(&d).unwrap();
        graph.update_links(&b).unwrap();
        graph.update_links(&d).unwrap();

        let components = graph.connected_components().unwrap();
        assert_eq!(
            components.len(),
            2,
            "two disconnected pairs should yield 2 components"
        );
        let sizes: Vec<usize> = {
            let mut s: Vec<usize> = components.iter().map(|c| c.len()).collect();
            s.sort_unstable();
            s
        };
        assert_eq!(sizes, vec![2, 2]);
    }

    #[test]
    fn test_connected_components_empty_graph() {
        let graph = LinkGraph::new();
        let components = graph.connected_components().unwrap();
        assert!(components.is_empty(), "empty graph should return empty vec");
    }

    // --- path_suffix_index tests ---

    #[test]
    fn test_path_suffix_index_basic() {
        // Two files share the stem "note" but live in different folders.
        // [[note]] resolves via file_index (stem) — hits one of them.
        // [[2024/note]] resolves via path_suffix_index to projects/2024/note.md only.
        let mut graph = LinkGraph::new();
        let deep = create_test_file("projects/2024/note.md", vec![]);
        let daily = create_test_file("daily/note.md", vec![]);

        graph.add_file(&deep).unwrap();
        graph.add_file(&daily).unwrap();

        // [[note]] stems match both → file_index has 2 entries; first-found wins.
        // Either way the link must resolve (edge count = 1).
        let linker_stem = create_test_file("linker_stem.md", vec!["note"]);
        graph.add_file(&linker_stem).unwrap();
        graph.update_links(&linker_stem).unwrap();
        assert_eq!(
            graph.edge_count(),
            1,
            "[[note]] should resolve via file_index to one of the two files"
        );

        // Remove that edge so we can test suffix resolution cleanly.
        let linker_stem_path = PathBuf::from("linker_stem.md");
        graph.remove_file(&linker_stem_path).unwrap();

        // [[2024/note]] — path suffix ["2024", "note"] should match only projects/2024/note.md.
        let linker_suffix = create_test_file("linker_suffix.md", vec!["2024/note"]);
        graph.add_file(&linker_suffix).unwrap();
        graph.update_links(&linker_suffix).unwrap();

        assert!(
            graph.all_unresolved_links().is_empty(),
            "[[2024/note]] should resolve successfully"
        );
        let forward = graph
            .forward_links(&PathBuf::from("linker_suffix.md"))
            .unwrap();
        assert_eq!(forward.len(), 1);
        assert_eq!(forward[0].0, PathBuf::from("projects/2024/note.md"));
    }

    #[test]
    fn test_path_suffix_index_disambiguation() {
        // a/shared.md and b/shared.md share stem "shared".
        // [[shared]] matches both via file_index → first-found wins.
        // [[a/shared]] matches only a/shared.md via path_suffix_index.
        let mut graph = LinkGraph::new();
        let a = create_test_file("a/shared.md", vec![]);
        let b = create_test_file("b/shared.md", vec![]);

        graph.add_file(&a).unwrap();
        graph.add_file(&b).unwrap();

        // [[shared]] → file_index, multiple candidates, first wins → exactly 1 edge.
        let linker1 = create_test_file("linker1.md", vec!["shared"]);
        graph.add_file(&linker1).unwrap();
        graph.update_links(&linker1).unwrap();
        assert_eq!(
            graph.edge_count(),
            1,
            "[[shared]] should resolve to first-added candidate"
        );

        // Remove linker1 to test suffix resolution in isolation.
        graph.remove_file(&PathBuf::from("linker1.md")).unwrap();

        // [[a/shared]] → path_suffix_index, should match only a/shared.md.
        let linker2 = create_test_file("linker2.md", vec!["a/shared"]);
        graph.add_file(&linker2).unwrap();
        graph.update_links(&linker2).unwrap();

        assert!(
            graph.all_unresolved_links().is_empty(),
            "[[a/shared]] should resolve"
        );
        let forward = graph.forward_links(&PathBuf::from("linker2.md")).unwrap();
        assert_eq!(forward.len(), 1);
        assert_eq!(forward[0].0, PathBuf::from("a/shared.md"));
    }

    // --- HeadingRef / BlockRef edge creation tests ---

    fn create_link_with_type(path: &str, target: &str, link_type: LinkType) -> Link {
        Link {
            type_: link_type,
            source_file: PathBuf::from(path),
            target: target.to_string(),
            display_text: None,
            position: SourcePosition::new(0, 0, 0, 10),
            resolved_target: None,
            is_valid: true,
        }
    }

    fn create_test_file_with_typed_link(
        path: &str,
        target: &str,
        link_type: LinkType,
    ) -> VaultFile {
        let link = create_link_with_type(path, target, link_type);
        let mut file = VaultFile::new(
            PathBuf::from(path),
            String::new(),
            FileMetadata {
                path: PathBuf::from(path),
                size: 0,
                created_at: 0.0,
                modified_at: 0.0,
                checksum: String::new(),
                is_attachment: false,
            },
        );
        file.links = vec![link];
        file
    }

    #[test]
    fn test_heading_ref_creates_edge() {
        // [[B#heading]] — HeadingRef — should create an edge from A to B.
        let mut graph = LinkGraph::new();
        let b = create_test_file("B.md", vec![]);
        graph.add_file(&b).unwrap();

        let a = create_test_file_with_typed_link("A.md", "B#heading", LinkType::HeadingRef);
        graph.add_file(&a).unwrap();
        graph.update_links(&a).unwrap();

        assert_eq!(
            graph.edge_count(),
            1,
            "HeadingRef link should create an edge"
        );
        assert!(graph.all_unresolved_links().is_empty());

        let forward = graph.forward_links(&PathBuf::from("A.md")).unwrap();
        assert_eq!(forward.len(), 1);
        assert_eq!(forward[0].0, PathBuf::from("B.md"));
    }

    #[test]
    fn test_block_ref_creates_edge() {
        // [[B#^blockid]] — BlockRef — should create an edge from A to B.
        let mut graph = LinkGraph::new();
        let b = create_test_file("B.md", vec![]);
        graph.add_file(&b).unwrap();

        let a = create_test_file_with_typed_link("A.md", "B#^blockid", LinkType::BlockRef);
        graph.add_file(&a).unwrap();
        graph.update_links(&a).unwrap();

        assert_eq!(graph.edge_count(), 1, "BlockRef link should create an edge");
        assert!(graph.all_unresolved_links().is_empty());

        let forward = graph.forward_links(&PathBuf::from("A.md")).unwrap();
        assert_eq!(forward.len(), 1);
        assert_eq!(forward[0].0, PathBuf::from("B.md"));
    }

    #[test]
    fn test_same_document_anchor_skipped() {
        // [[#heading]] — target is "#heading", clean_target is "" after split('#').
        // update_links must skip it: no self-loop and not in unresolved_links.
        let mut graph = LinkGraph::new();
        let a = create_test_file_with_typed_link("A.md", "#heading", LinkType::HeadingRef);
        graph.add_file(&a).unwrap();
        graph.update_links(&a).unwrap();

        assert_eq!(
            graph.edge_count(),
            0,
            "same-document anchor must not create any edge"
        );
        assert!(
            graph.all_unresolved_links().is_empty(),
            "same-document anchor must not appear in unresolved_links"
        );
    }

    // --- BFS order test ---

    #[test]
    fn test_related_notes_bfs_order() {
        // A→B, A→C, B→D.
        // related_notes("A", 2) should return B and C before D (hop-1 before hop-2).
        let mut graph = LinkGraph::new();
        let a = create_test_file("A.md", vec![]);
        let b = create_test_file("B.md", vec![]);
        let c = create_test_file("C.md", vec![]);
        let d = create_test_file("D.md", vec![]);

        graph.add_file(&a).unwrap();
        graph.add_file(&b).unwrap();
        graph.add_file(&c).unwrap();
        graph.add_file(&d).unwrap();

        // A links to B and C
        let a_linked = {
            let link_b = create_link_with_type("A.md", "B", LinkType::WikiLink);
            let link_c = create_link_with_type("A.md", "C", LinkType::WikiLink);
            let mut f = VaultFile::new(
                PathBuf::from("A.md"),
                String::new(),
                FileMetadata {
                    path: PathBuf::from("A.md"),
                    size: 0,
                    created_at: 0.0,
                    modified_at: 0.0,
                    checksum: String::new(),
                    is_attachment: false,
                },
            );
            f.links = vec![link_b, link_c];
            f
        };
        graph.update_links(&a_linked).unwrap();

        // B links to D
        let b_linked = create_test_file_with_typed_link("B.md", "D", LinkType::WikiLink);
        graph.update_links(&b_linked).unwrap();

        let path_a = PathBuf::from("A.md");
        let path_b = PathBuf::from("B.md");
        let path_c = PathBuf::from("C.md");
        let path_d = PathBuf::from("D.md");

        let related = graph.related_notes(&path_a, 2).unwrap();

        // All three of B, C, D must be present
        assert!(related.contains(&path_b), "B should be related to A");
        assert!(related.contains(&path_c), "C should be related to A");
        assert!(related.contains(&path_d), "D should be related to A");

        // B and C (hop 1) must appear before D (hop 2)
        let pos_b = related.iter().position(|p| p == &path_b).unwrap();
        let pos_c = related.iter().position(|p| p == &path_c).unwrap();
        let pos_d = related.iter().position(|p| p == &path_d).unwrap();
        let hop1_max = pos_b.max(pos_c);
        assert!(
            hop1_max < pos_d,
            "B and C (hop 1) must appear before D (hop 2) in BFS order; got pos_b={}, pos_c={}, pos_d={}",
            pos_b,
            pos_c,
            pos_d
        );
    }

    // --- update_links creates file_index for nodes not previously add_file()'d ---

    #[test]
    fn test_update_links_creates_file_index_for_new_node() {
        // Call update_links() for a source file that was never add_file()'d.
        // The file should appear in the graph and be resolvable by stem.
        let mut graph = LinkGraph::new();

        // target.md is registered via add_file
        let target = create_test_file("target.md", vec![]);
        graph.add_file(&target).unwrap();

        // source.md is never add_file()'d; update_links should create its node
        // (and populate file_index so it can be resolved by others).
        let source = create_test_file("source.md", vec!["target"]);
        graph.update_links(&source).unwrap();

        // source node must exist in the graph now
        assert_eq!(graph.node_count(), 2);

        // The edge source→target must exist
        assert_eq!(graph.edge_count(), 1);
        assert!(graph.all_unresolved_links().is_empty());

        // source.md should be resolvable by stem: a third file linking to "source"
        // should create an edge, not an unresolved link.
        let third = create_test_file("third.md", vec!["source"]);
        graph.add_file(&third).unwrap();
        graph.update_links(&third).unwrap();

        // Now we should have 2 edges: source→target and third→source
        assert_eq!(graph.edge_count(), 2);
        assert!(graph.all_unresolved_links().is_empty());
    }

    /// Build a file whose links are typed (for OKF markdown-link tests).
    fn create_typed_file(path: &str, links: Vec<(LinkType, &str)>) -> VaultFile {
        let parsed_links: Vec<Link> = links
            .into_iter()
            .enumerate()
            .map(|(i, (type_, target))| Link {
                type_,
                source_file: PathBuf::from(path),
                target: target.to_string(),
                display_text: None,
                position: SourcePosition::new(0, 0, i * 10, 10),
                resolved_target: None,
                is_valid: true,
            })
            .collect();

        let mut vault_file = create_test_file(path, vec![]);
        vault_file.links = parsed_links;
        vault_file
    }

    #[test]
    fn test_okf_bundle_relative_markdown_link_resolves() {
        // OKF cross-link `[customers](/tables/customers.md)` must become an edge.
        let mut graph = LinkGraph::new();
        let customers = create_test_file("/vault/tables/customers.md", vec![]);
        let orders = create_typed_file(
            "/vault/tables/orders.md",
            vec![(LinkType::MarkdownLink, "/tables/customers.md")],
        );

        graph.add_file(&customers).unwrap();
        graph.add_file(&orders).unwrap();
        graph.update_links(&orders).unwrap();

        assert_eq!(graph.edge_count(), 1);
        assert!(graph.all_unresolved_links().is_empty());
    }

    #[test]
    fn test_okf_relative_markdown_link_with_heading_resolves() {
        // `[schema](./customers.md#schema)` classifies as HeadingRef and resolves.
        let mut graph = LinkGraph::new();
        let customers = create_test_file("/vault/tables/customers.md", vec![]);
        let orders = create_typed_file(
            "/vault/tables/orders.md",
            vec![(LinkType::HeadingRef, "./customers.md#schema")],
        );

        graph.add_file(&customers).unwrap();
        graph.add_file(&orders).unwrap();
        graph.update_links(&orders).unwrap();

        assert_eq!(graph.edge_count(), 1);
        assert!(graph.all_unresolved_links().is_empty());
    }

    #[test]
    fn test_markdown_link_to_non_md_is_not_a_graph_edge() {
        // Links to images/attachments/external resources must not pollute the
        // note graph or broken-link reports.
        let mut graph = LinkGraph::new();
        let note = create_typed_file(
            "/vault/note.md",
            vec![
                (LinkType::MarkdownLink, "/assets/diagram.png"),
                (LinkType::ExternalLink, "https://example.com"),
            ],
        );

        graph.add_file(&note).unwrap();
        graph.update_links(&note).unwrap();

        assert_eq!(graph.edge_count(), 0);
        assert!(graph.all_unresolved_links().is_empty());
    }

    #[test]
    fn test_markdown_self_link_is_not_a_self_loop() {
        // A note linking to itself (common in OKF index/log docs) must not
        // produce a graph self-loop.
        let mut graph = LinkGraph::new();
        let orders = create_typed_file(
            "/vault/tables/orders.md",
            vec![(LinkType::MarkdownLink, "/tables/orders.md")],
        );
        graph.add_file(&orders).unwrap();
        graph.update_links(&orders).unwrap();

        assert_eq!(graph.edge_count(), 0);
        assert!(graph.all_unresolved_links().is_empty());
    }

    #[test]
    fn test_multi_segment_alias_resolves() {
        // An alias containing '/' (a legal frontmatter alias) must still resolve
        // via a wikilink — regression guard for the resolve_link rewrite.
        let mut graph = LinkGraph::new();

        let mut target = create_test_file("/vault/team/roadmap.md", vec![]);
        let mut data = std::collections::HashMap::new();
        data.insert(
            "aliases".to_string(),
            serde_json::Value::Array(vec![serde_json::Value::String("Projects/Roadmap".into())]),
        );
        target.frontmatter = Some(turbovault_core::Frontmatter {
            data,
            position: SourcePosition::start(),
        });

        let linker = create_typed_file(
            "/vault/notes/plan.md",
            vec![(LinkType::WikiLink, "Projects/Roadmap")],
        );

        graph.add_file(&target).unwrap();
        graph.add_file(&linker).unwrap();
        graph.update_links(&linker).unwrap();

        assert_eq!(
            graph.edge_count(),
            1,
            "slash-containing alias should resolve"
        );
        assert!(graph.all_unresolved_links().is_empty());
    }

    #[test]
    fn test_attachment_heading_ref_is_not_a_broken_link() {
        // A markdown link to a non-note resource with a fragment classifies as
        // HeadingRef; it must NOT be treated as a note edge or a broken link.
        let mut graph = LinkGraph::new();
        let note = create_typed_file(
            "/vault/note.md",
            vec![
                (LinkType::HeadingRef, "report.pdf#page=2"),
                (LinkType::HeadingRef, "assets/diagram.svg#layer1"),
            ],
        );
        graph.add_file(&note).unwrap();
        graph.update_links(&note).unwrap();

        assert_eq!(graph.edge_count(), 0);
        assert_eq!(graph.unresolved_link_count(), 0);
    }

    #[test]
    fn test_image_embed_is_not_a_broken_link() {
        // `![[chart.png]]` embeds an attachment, not a note — it must not be
        // tracked as a broken note link.
        let mut graph = LinkGraph::new();
        let note = create_typed_file("/vault/note.md", vec![(LinkType::Embed, "chart.png")]);
        graph.add_file(&note).unwrap();
        graph.update_links(&note).unwrap();

        assert_eq!(graph.edge_count(), 0);
        assert_eq!(graph.unresolved_link_count(), 0);
    }

    #[test]
    fn test_dotted_note_name_wikilink_resolves() {
        // A note whose name contains a dot (`Release v1.2.md`) must still
        // resolve via a wikilink — the extension heuristic must not reject it.
        let mut graph = LinkGraph::new();
        let target = create_test_file("/vault/Release v1.2.md", vec![]);
        let linker = create_typed_file(
            "/vault/notes/plan.md",
            vec![(LinkType::WikiLink, "Release v1.2")],
        );

        graph.add_file(&target).unwrap();
        graph.add_file(&linker).unwrap();
        graph.update_links(&linker).unwrap();

        assert_eq!(graph.edge_count(), 1, "dotted note name should resolve");
        assert!(graph.all_unresolved_links().is_empty());
    }

    #[test]
    fn test_okf_broken_cross_link_tracked() {
        // A `.md` markdown link with no target file is a genuine broken link.
        let mut graph = LinkGraph::new();
        let note = create_typed_file(
            "/vault/note.md",
            vec![(LinkType::MarkdownLink, "/tables/missing.md")],
        );

        graph.add_file(&note).unwrap();
        graph.update_links(&note).unwrap();

        assert_eq!(graph.edge_count(), 0);
        assert_eq!(graph.unresolved_link_count(), 1);
    }
}
