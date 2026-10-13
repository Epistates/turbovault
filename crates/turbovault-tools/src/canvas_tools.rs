//! Canvas files (`.canvas`): reading, validated writes, and the references
//! a canvas holds to notes.
//!
//! A canvas points at a note in two ways: a file node naming its path, and a
//! link written in a text node. The link graph, the search index, health
//! analysis and the freshness sweep all model notes (Markdown) only, so a
//! canvas is not a node in the graph. Its references are read from the
//! canvas files themselves when they matter: `get_backlinks` lists the
//! canvases that point at a note alongside the notes that link to it,
//! `delete_note` counts them before refusing, and moving a note re-points
//! them. Vaults hold few canvases, so reading them on demand stays cheap and
//! can never be stale.

use std::path::Path;
use std::sync::Arc;

use turbovault_core::prelude::*;
use turbovault_core::Precondition;
use turbovault_parser::{Canvas, CanvasReference, parse_canvas};
use turbovault_vault::VaultManager;

use crate::wikilink_rewriter::{links_in, rewrite_links_from};

/// Whether `path` names a canvas file.
pub fn is_canvas(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("canvas"))
}

/// Canvas operations on one vault.
#[derive(Clone)]
pub struct CanvasTools {
    manager: Arc<VaultManager>,
}

impl CanvasTools {
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self { manager }
    }

    /// Read the canvas at `path` (vault-relative), with the hash a later
    /// write passes back as its precondition.
    pub async fn read(&self, path: &str) -> Result<(Canvas, String)> {
        require_canvas(path)?;
        let content = self.manager.read_file(Path::new(path)).await?;
        let canvas = parse_canvas(&content)
            .map_err(|error| Error::parse_error(format!("{path}: {error}")))?;
        let hash = self.manager.hash_bytes(content.as_bytes())?;
        Ok((canvas, hash))
    }

    /// Write `canvas` to `path`, refusing a document that isn't valid JSON
    /// Canvas 1.0. Returns the number of bytes written.
    pub async fn write(
        &self,
        path: &str,
        canvas: &Canvas,
        precondition: Precondition,
        message: &str,
    ) -> Result<usize> {
        require_canvas(path)?;
        let problems = canvas.problems();
        if !problems.is_empty() {
            return Err(Error::validation_error(format!(
                "not a valid JSON Canvas 1.0 document: {}",
                problems.join("; ")
            )));
        }
        let content = canvas.to_obsidian_json();
        self.manager
            .write_file(Path::new(path), &content, precondition, message)
            .await?;
        Ok(content.len())
    }

    /// Every canvas file in the vault, as vault-relative paths, sorted.
    pub async fn canvas_paths(&self) -> Result<Vec<String>> {
        let mut paths: Vec<String> = self
            .manager
            .scan_vault()
            .await?
            .iter()
            .map(|path| self.manager.relative_path(path))
            .filter(|path| is_canvas(path))
            .collect();
        paths.sort();
        Ok(paths)
    }

    /// The canvases that point at the note `note` (vault-relative), sorted.
    /// A canvas that can't be read or parsed is skipped: it points at
    /// nothing anyone can open.
    pub async fn canvases_referencing(&self, note: &str) -> Result<Vec<String>> {
        let target = self.manager.vault_path().join(note);
        let graph = self.manager.link_graph_flushed().await;
        let graph = graph.read().await;
        let mut found = Vec::new();
        for canvas_path in self.canvas_paths().await? {
            let Ok(content) = self.manager.read_file(Path::new(&canvas_path)).await else {
                continue;
            };
            let Ok(canvas) = parse_canvas(&content) else {
                continue;
            };
            let points_here = canvas.references().iter().any(|reference| match reference {
                CanvasReference::File { path, .. } => same_vault_path(path, note),
                CanvasReference::Text { link, .. } => {
                    graph.resolve(&link.target).is_some_and(|p| p == &target)
                }
            });
            if points_here {
                found.push(canvas_path);
            }
        }
        Ok(found)
    }

    /// Each canvas that points at `from`, rewritten to point at `to`
    /// instead: as `(path, new content, hash of the content read)`. File
    /// nodes naming `from` are re-pointed, and links in text nodes are
    /// rewritten as a note's would be, relative ones from the canvas's own
    /// folder. A canvas with nothing to change is left out.
    pub async fn rewrites_for_move(
        &self,
        from: &str,
        to: &str,
    ) -> Result<Vec<(String, String, String)>> {
        let from_full = self.manager.vault_path().join(from);
        let graph = self.manager.link_graph_flushed().await;
        let graph = graph.read().await;
        let mut rewrites = Vec::new();
        for canvas_path in self.canvas_paths().await? {
            let Ok(content) = self.manager.read_file(Path::new(&canvas_path)).await else {
                continue;
            };
            let Ok(mut canvas) = parse_canvas(&content) else {
                continue;
            };
            let mut changed = false;
            for node in &mut canvas.nodes {
                match node.kind.as_str() {
                    "file" => {
                        if let Some(file) = &mut node.file
                            && same_vault_path(file, from)
                        {
                            *file = to.to_string();
                            changed = true;
                        }
                    }
                    "text" => {
                        let Some(text) = &mut node.text else { continue };
                        let links: Vec<_> = links_in(text)
                            .into_iter()
                            .filter(|link| {
                                graph.resolve(&link.target).is_some_and(|p| p == &from_full)
                            })
                            .collect();
                        if links.is_empty() {
                            continue;
                        }
                        let rewritten = rewrite_links_from(&canvas_path, text, &links, from, to);
                        if rewritten != *text {
                            *text = rewritten;
                            changed = true;
                        }
                    }
                    _ => {}
                }
            }
            if changed {
                let hash = self.manager.hash_bytes(content.as_bytes())?;
                rewrites.push((canvas_path, canvas.to_obsidian_json(), hash));
            }
        }
        Ok(rewrites)
    }
}

fn require_canvas(path: &str) -> Result<()> {
    if is_canvas(path) {
        Ok(())
    } else {
        Err(Error::invalid_path(format!(
            "{path} is not a canvas: canvas files end in .canvas"
        )))
    }
}

/// Whether two vault-relative paths name the same file. A canvas stores the
/// path with `/` separators; case is ignored, as on the filesystems most
/// vaults live on.
fn same_vault_path(a: &str, b: &str) -> bool {
    let normalize = |p: &str| p.trim_start_matches("./").replace('\\', "/");
    normalize(a).eq_ignore_ascii_case(&normalize(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use turbovault_core::{ConfigProfile, VaultConfig};

    async fn vault(files: &[(&str, &str)]) -> (TempDir, Arc<VaultManager>) {
        let dir = TempDir::new().unwrap();
        for (path, content) in files {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, content).unwrap();
        }
        let mut config = ConfigProfile::Development.create_config();
        config.vaults = vec![VaultConfig::builder("v", dir.path()).build().unwrap()];
        let manager = Arc::new(VaultManager::new(config).unwrap());
        manager.initialize().await.unwrap();
        (dir, manager)
    }

    const BOARD: &str = r#"{"nodes":[
        {"id":"f","type":"file","file":"notes/Plan.md","x":0,"y":0,"width":100,"height":100},
        {"id":"t","type":"text","text":"Read [[Plan]] then [[Other]].","x":0,"y":200,"width":100,"height":100}
    ],"edges":[{"id":"e","fromNode":"t","toNode":"f"}]}"#;

    #[tokio::test]
    async fn finds_the_canvases_that_point_at_a_note() {
        let (_dir, manager) = vault(&[
            ("notes/Plan.md", "# Plan\n"),
            ("Other.md", "# Other\n"),
            ("Unrelated.md", "# Unrelated\n"),
            ("boards/Board.canvas", BOARD),
            ("boards/Broken.canvas", "not json"),
        ])
        .await;
        let tools = CanvasTools::new(manager);

        assert_eq!(
            tools.canvases_referencing("notes/Plan.md").await.unwrap(),
            ["boards/Board.canvas"]
        );
        assert_eq!(
            tools.canvases_referencing("Other.md").await.unwrap(),
            ["boards/Board.canvas"]
        );
        assert!(
            tools
                .canvases_referencing("Unrelated.md")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn moving_a_note_repoints_file_nodes_and_text_links() {
        let (_dir, manager) = vault(&[
            ("notes/Plan.md", "# Plan\n"),
            ("Other.md", "# Other\n"),
            ("boards/Board.canvas", BOARD),
        ])
        .await;
        let tools = CanvasTools::new(manager);

        let rewrites = tools
            .rewrites_for_move("notes/Plan.md", "archive/Roadmap.md")
            .await
            .unwrap();
        assert_eq!(rewrites.len(), 1);
        let (path, content, _hash) = &rewrites[0];
        assert_eq!(path, "boards/Board.canvas");
        let canvas = parse_canvas(content).unwrap();
        assert_eq!(canvas.nodes[0].file.as_deref(), Some("archive/Roadmap.md"));
        assert_eq!(
            canvas.nodes[1].text.as_deref(),
            Some("Read [[Roadmap]] then [[Other]].")
        );
        assert!(canvas.problems().is_empty());

        assert!(
            tools
                .rewrites_for_move("Unrelated.md", "x.md")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn writes_refuse_an_invalid_canvas_or_another_extension() {
        let (_dir, manager) = vault(&[]).await;
        let tools = CanvasTools::new(manager);
        let mut canvas = parse_canvas(BOARD).unwrap();
        canvas.edges[0].to_node = "nowhere".into();

        let error = tools
            .write("b.canvas", &canvas, Precondition::ExpectAbsent, "w")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("nowhere"), "{error}");
        let error = tools
            .write("b.md", &Canvas::default(), Precondition::ExpectAbsent, "w")
            .await
            .unwrap_err();
        assert!(error.to_string().contains(".canvas"), "{error}");

        tools
            .write("b.canvas", &Canvas::default(), Precondition::ExpectAbsent, "w")
            .await
            .unwrap();
        let (read, hash) = tools.read("b.canvas").await.unwrap();
        assert_eq!(read, Canvas::default());
        assert!(!hash.is_empty());
    }
}
