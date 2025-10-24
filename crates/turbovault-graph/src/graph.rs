//! Link graph using petgraph for vault relationship analysis

use turbovault_core::prelude::*;
use petgraph::algo::kosaraju_scc;
use petgraph::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Node index type for graph
type NodeIndex = petgraph::graph::NodeIndex;

/// Link graph for analyzing vault relationships
pub struct LinkGraph {
    /// Directed graph: nodes are file paths, edges are links
    graph: DiGraph<PathBuf, Link>,

    /// Map from file name (stem) to node index
    file_index: HashMap<String, NodeIndex>,

    /// Map from aliases to node index
    alias_index: HashMap<String, NodeIndex>,

    /// Map from full path to node index (for quick lookups)
    path_index: HashMap<PathBuf, NodeIndex>,
}

impl LinkGraph {
    /// Create a new link graph
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            file_index: HashMap::new(),
            alias_index: HashMap::new(),
            path_index: HashMap::new(),
        }
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

            // Add to file_index by stem
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                self.file_index.insert(stem.to_string(), idx);
            }

            idx
        };

        // Register aliases from frontmatter
        if let Some(fm) = &file.frontmatter {
            for alias in fm.aliases() {
                self.alias_index.insert(alias, node_idx);
            }
        }

        Ok(())
    }

    /// Remove a file from the graph
    pub fn remove_file(&mut self, path: &PathBuf) -> Result<()> {
        if let Some(&idx) = self.path_index.get(path) {
            // Remove from all indices
            self.path_index.remove(path);

            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                self.file_index.remove(stem);
            }

            // Remove aliases pointing to this node
            self.alias_index.retain(|_, &mut node_idx| node_idx != idx);

            // Remove node and all edges
            self.graph.remove_node(idx);
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
            idx
        };

        // Remove old outgoing edges
        let outgoing: Vec<_> = self.graph.edges(source_idx).map(|e| e.id()).collect();
        for edge_id in outgoing {
            self.graph.remove_edge(edge_id);
        }

        // Add edges for each internal link (wikilinks and embeds)
        for link in &file.links {
            if matches!(link.type_, LinkType::WikiLink | LinkType::Embed)
                && let Some(target_idx) = self.resolve_link(&link.target)
            {
                // Add edge (both nodes already exist from add_file)
                self.graph.add_edge(source_idx, target_idx, link.clone());
            }
        }

        Ok(())
    }

    /// Resolve a wikilink target to a file path and node index
    fn resolve_link(&self, target: &str) -> Option<NodeIndex> {
        // Remove block/heading references
        let clean_target = target.split('#').next()?.trim();

        // Try direct stem match
        if let Some(&idx) = self.file_index.get(clean_target) {
            return Some(idx);
        }

        // Try alias match
        if let Some(&idx) = self.alias_index.get(clean_target) {
            return Some(idx);
        }

        // Try path-like match (folder/Note)
        let target_parts: Vec<&str> = clean_target.split('/').filter(|p| !p.is_empty()).collect();
        if target_parts.is_empty() {
            return None;
        }

        // Find file path that matches the tail of the target path
        for (path, &idx) in self.path_index.iter() {
            let path_parts: Vec<&str> = path.iter().filter_map(|p| p.to_str()).collect();

            if path_parts.len() >= target_parts.len() {
                let start = path_parts.len() - target_parts.len();
                if path_parts[start..] == target_parts[..] {
                    return Some(idx);
                }
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
            let mut queue = vec![(start_idx, 0)];
            let mut related = Vec::new();

            visited.insert(start_idx);

            while let Some((idx, hops)) = queue.pop() {
                if hops > 0 {
                    related.push(self.graph[idx].clone());
                }

                if hops < max_hops {
                    // Add all neighbors
                    for neighbor_idx in self.graph.neighbors(idx) {
                        if visited.insert(neighbor_idx) {
                            queue.push((neighbor_idx, hops + 1));
                        }
                    }

                    // Also traverse incoming edges
                    for neighbor_idx in self.graph.edges_directed(idx, Incoming).map(|e| e.source())
                    {
                        if visited.insert(neighbor_idx) {
                            queue.push((neighbor_idx, hops + 1));
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
        let sccs = kosaraju_scc(&self.graph);
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

    /// Find connected components in the graph (using undirected view)
    pub fn connected_components(&self) -> Result<Vec<Vec<PathBuf>>> {
        use petgraph::algo::tarjan_scc;

        // Use Tarjan's algorithm for strongly connected components
        let components = tarjan_scc(&self.graph);

        let result: Vec<Vec<PathBuf>> = components
            .into_iter()
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
}
