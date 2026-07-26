//! Self-contained HTML visualization of a vault's concept graph.
//!
//! Renders the vault (treated as an OKF bundle) as a single HTML file: a
//! force-directed graph of every note, a detail panel with the rendered
//! markdown body and "cited by" backlinks, a type filter, and search. The
//! bundle is embedded as a JSON blob and never sent anywhere — no backend, no
//! analytics. [Cytoscape.js](https://js.cytoscape.org/) (graph),
//! [marked](https://marked.js.org/) (markdown), and
//! [DOMPurify](https://github.com/cure53/DOMPurify) (HTML sanitization of the
//! rendered note bodies) are loaded from a CDN, so viewing requires network
//! access even though the data is fully embedded.
//!
//! This is the *consumption* counterpart to [`crate::okf_tools`]: any vault
//! that TurboVault can read can be turned into a shareable, backend-free
//! artifact. It reuses the resolved link graph, so OKF cross-links and Obsidian
//! wikilinks both appear as edges.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use turbovault_core::Result;
use turbovault_core::okf;
use turbovault_vault::VaultManager;

/// A concept node in the visualization.
#[derive(Debug, Clone, Serialize)]
struct VizNode {
    /// OKF concept ID — the node's stable identity and link target.
    id: String,
    /// Display label (frontmatter `title`, else the file stem).
    label: String,
    /// OKF `type`, or `"note"` when absent — drives node color/filtering.
    #[serde(rename = "type")]
    type_: String,
    /// Frontmatter `description`, if any.
    description: String,
    /// Frontmatter `resource` URI, if any.
    resource: String,
    /// Markdown body (frontmatter stripped), rendered client-side by `marked`.
    body: String,
    /// Vault-relative path.
    path: String,
}

/// A directed edge between two concepts.
#[derive(Debug, Clone, Serialize)]
struct VizEdge {
    source: String,
    target: String,
}

/// The full payload embedded into the HTML.
#[derive(Debug, Clone, Serialize)]
struct VizData {
    name: String,
    nodes: Vec<VizNode>,
    edges: Vec<VizEdge>,
}

/// Summary returned to the caller after generating a visualization.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct VisualizationResult {
    /// Display name shown in the viewer header.
    pub name: String,
    /// Number of concept nodes.
    pub nodes: usize,
    /// Number of resolved edges.
    pub edges: usize,
    /// Size of the generated HTML in bytes.
    pub html_bytes: usize,
}

/// Builds HTML visualizations of a vault.
pub struct ViewerTools {
    manager: Arc<VaultManager>,
}

impl ViewerTools {
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self { manager }
    }

    /// Build the visualization payload and render it to a self-contained HTML
    /// document. Returns `(html, summary)`.
    pub async fn generate(&self, name: Option<&str>) -> Result<(String, VisualizationResult)> {
        let root = self.manager.vault_path().clone();
        let display_name = name
            .map(|s| s.to_string())
            .or_else(|| {
                root.file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "Vault".to_string());

        // Cache-first: parsed notes validated against disk mtime, no re-scan.
        let files = self.manager.vault_files_validated().await;

        // Build nodes, and a path -> concept_id map for edge resolution.
        let mut nodes = Vec::with_capacity(files.len());
        let mut id_by_path: HashMap<std::path::PathBuf, String> = HashMap::new();

        for vf in &files {
            let path = &vf.path;
            let cid = okf::concept_id(&root, path);
            id_by_path.insert(path.clone(), cid.clone());

            let fm = vf.frontmatter.as_ref();
            let label = fm.and_then(|f| f.okf_title()).unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&cid)
                    .to_string()
            });
            let type_ = fm
                .and_then(|f| f.okf_type())
                .unwrap_or_else(|| "note".to_string());

            nodes.push(VizNode {
                id: cid,
                label,
                type_,
                description: fm.and_then(|f| f.okf_description()).unwrap_or_default(),
                resource: fm.and_then(|f| f.okf_resource()).unwrap_or_default(),
                body: vf.content.clone(),
                path: self.manager.relative_path(path),
            });
        }
        // Stable node order (cache iteration order is unspecified).
        nodes.sort_by(|a, b| a.id.cmp(&b.id));

        // Resolved edges from the link graph (covers OKF + wikilinks).
        let graph = self.manager.link_graph_flushed().await;
        let graph = graph.read().await;
        let mut edges = Vec::new();
        for vf in &files {
            let path = &vf.path;
            let Some(source_id) = id_by_path.get(path) else {
                continue;
            };
            if let Ok(forward) = graph.forward_links(path) {
                for (target_path, links) in forward {
                    if let Some(target_id) = id_by_path.get(&target_path) {
                        // One edge per resolved target (collapse parallel links).
                        if !links.is_empty() && source_id != target_id {
                            edges.push(VizEdge {
                                source: source_id.clone(),
                                target: target_id.clone(),
                            });
                        }
                    }
                }
            }
        }
        drop(graph);
        // Stable edge order for reproducible output.
        edges.sort_by(|a, b| a.source.cmp(&b.source).then(a.target.cmp(&b.target)));

        let summary_nodes = nodes.len();
        let summary_edges = edges.len();

        let data = VizData {
            name: display_name.clone(),
            nodes,
            edges,
        };
        let json = serde_json::to_string(&data).map_err(|e| {
            turbovault_core::Error::parse_error(format!("failed to serialize viz data: {e}"))
        })?;
        // Prevent `</script>` (or any `</`) in embedded JSON from closing the tag.
        let json = json.replace("</", "<\\/");

        let html = render_html(&display_name, &json);
        let html_bytes = html.len();

        Ok((
            html,
            VisualizationResult {
                name: display_name,
                nodes: summary_nodes,
                edges: summary_edges,
                html_bytes,
            },
        ))
    }
}

/// Render the self-contained HTML document around the embedded JSON payload.
fn render_html(name: &str, json_data: &str) -> String {
    let safe_name = html_escape(name);
    TEMPLATE
        .replace("{{NAME}}", &safe_name)
        .replace("{{DATA}}", json_data)
}

/// Minimal HTML-escape for text interpolated into markup (not the JSON blob).
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Self-contained viewer template. `{{NAME}}` and `{{DATA}}` are substituted;
/// Cytoscape.js and marked load from a CDN, the bundle is embedded as JSON.
const TEMPLATE: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{{NAME}} — TurboVault</title>
<script src="https://cdn.jsdelivr.net/npm/cytoscape@3.30.2/dist/cytoscape.min.js"></script>
<script src="https://cdn.jsdelivr.net/npm/marked@13.0.3/marked.min.js"></script>
<script src="https://cdn.jsdelivr.net/npm/dompurify@3.1.6/dist/purify.min.js"></script>
<style>
  :root { --bg:#0f1117; --panel:#1a1d27; --fg:#e6e6e6; --muted:#8a8f9c; --accent:#5b9dff; --border:#2a2e3a; }
  * { box-sizing: border-box; }
  html, body { margin:0; height:100%; background:var(--bg); color:var(--fg);
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; }
  #app { display:flex; height:100vh; }
  #graph { flex:1; min-width:0; }
  #side { width:420px; max-width:45vw; background:var(--panel); border-left:1px solid var(--border);
    display:flex; flex-direction:column; }
  header { padding:12px 16px; border-bottom:1px solid var(--border); }
  header h1 { font-size:15px; margin:0 0 8px; font-weight:600; }
  .controls { display:flex; gap:8px; flex-wrap:wrap; }
  input, select { background:var(--bg); color:var(--fg); border:1px solid var(--border);
    border-radius:6px; padding:6px 8px; font-size:13px; }
  input#search { flex:1; min-width:120px; }
  #detail { padding:16px; overflow:auto; flex:1; }
  #detail .empty { color:var(--muted); font-size:13px; }
  #detail h2 { font-size:17px; margin:0 0 4px; }
  #detail .type { display:inline-block; font-size:11px; padding:2px 8px; border-radius:10px;
    background:var(--border); color:var(--fg); margin-bottom:8px; }
  #detail .desc { color:var(--muted); font-size:13px; margin-bottom:12px; }
  #detail .body { font-size:13px; line-height:1.55; border-top:1px solid var(--border); padding-top:12px; }
  #detail .body pre { background:var(--bg); padding:10px; border-radius:6px; overflow:auto; }
  #detail .body code { background:var(--bg); padding:1px 4px; border-radius:4px; }
  #detail .body table { border-collapse:collapse; } #detail .body td, #detail .body th { border:1px solid var(--border); padding:4px 8px; }
  #detail a { color:var(--accent); cursor:pointer; }
  .backlinks { margin-top:16px; border-top:1px solid var(--border); padding-top:12px; }
  .backlinks h3 { font-size:12px; text-transform:uppercase; color:var(--muted); margin:0 0 6px; }
  .backlinks ul { margin:0; padding-left:18px; font-size:13px; }
  footer { padding:8px 16px; font-size:11px; color:var(--muted); border-top:1px solid var(--border); }
</style>
</head>
<body>
<div id="app">
  <div id="graph"></div>
  <div id="side">
    <header>
      <h1>{{NAME}}</h1>
      <div class="controls">
        <input id="search" type="search" placeholder="Search title, id, type…">
        <select id="typeFilter"><option value="">All types</option></select>
        <select id="layout">
          <option value="cose">cose</option>
          <option value="concentric">concentric</option>
          <option value="breadthfirst">breadthfirst</option>
          <option value="circle">circle</option>
          <option value="grid">grid</option>
        </select>
      </div>
    </header>
    <div id="detail"><p class="empty">Select a node to see its content.</p></div>
    <footer id="stats"></footer>
  </div>
</div>
<script>
const DATA = {{DATA}};
const PALETTE = ["#5b9dff","#ff8a5b","#5bd6a0","#d98aff","#ffd45b","#ff5b8a","#5bd6ff","#a0d65b","#c0c4cc"];
const types = [...new Set(DATA.nodes.map(n => n.type))].sort();
const colorOf = t => PALETTE[Math.max(0, types.indexOf(t)) % PALETTE.length];

const nodeById = Object.fromEntries(DATA.nodes.map(n => [n.id, n]));
const backlinks = {};
DATA.edges.forEach(e => { (backlinks[e.target] ||= []).push(e.source); });

const cy = cytoscape({
  container: document.getElementById('graph'),
  elements: [
    ...DATA.nodes.map(n => ({ data: { id: n.id, label: n.label, type: n.type } })),
    ...DATA.edges.map((e, i) => ({ data: { id: 'e'+i, source: e.source, target: e.target } })),
  ],
  style: [
    { selector: 'node', style: { 'background-color': ele => colorOf(ele.data('type')),
      'label': 'data(label)', 'color': '#e6e6e6', 'font-size': 9, 'text-wrap':'wrap',
      'text-max-width': 90, 'width': 16, 'height': 16 } },
    { selector: 'edge', style: { 'width': 1, 'line-color': '#3a3f4d', 'curve-style': 'bezier',
      'target-arrow-color': '#3a3f4d', 'target-arrow-shape': 'triangle', 'arrow-scale': 0.7 } },
    { selector: 'node.faded', style: { 'opacity': 0.12 } },
    { selector: 'edge.faded', style: { 'opacity': 0.05 } },
    { selector: 'node.sel', style: { 'border-width': 3, 'border-color': '#fff' } },
  ],
  layout: { name: 'cose', animate: false },
});

function showDetail(id) {
  const n = nodeById[id];
  if (!n) return;
  cy.nodes().removeClass('sel');
  cy.getElementById(id).addClass('sel');
  const cites = (backlinks[id] || []).map(s =>
    `<li><a data-id="${s}">${(nodeById[s]||{}).label || s}</a></li>`).join('');
  const resource = n.resource ? `<p class="desc"><a href="${n.resource}" target="_blank" rel="noopener">${n.resource}</a></p>` : '';
  document.getElementById('detail').innerHTML =
    `<h2>${escapeHtml(n.label)}</h2>`
    + `<span class="type" style="background:${colorOf(n.type)}33;color:${colorOf(n.type)}">${escapeHtml(n.type)}</span>`
    + (n.description ? `<p class="desc">${escapeHtml(n.description)}</p>` : '')
    + resource
    + `<div class="body">${DOMPurify.sanitize(marked.parse(n.body || ''))}</div>`
    + (cites ? `<div class="backlinks"><h3>Cited by</h3><ul>${cites}</ul></div>` : '');
  document.querySelectorAll('#detail a[data-id]').forEach(a =>
    a.onclick = () => { showDetail(a.dataset.id); cy.getElementById(a.dataset.id).select(); });
}
function escapeHtml(s){return (s||'').replace(/[&<>]/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;'}[c]));}

cy.on('tap', 'node', e => showDetail(e.target.id()));

const tf = document.getElementById('typeFilter');
types.forEach(t => { const o = document.createElement('option'); o.value=t; o.textContent=t; tf.appendChild(o); });

function applyFilter() {
  const q = document.getElementById('search').value.toLowerCase();
  const t = tf.value;
  cy.batch(() => {
    cy.nodes().forEach(node => {
      const n = nodeById[node.id()];
      const matchQ = !q || (n.label+' '+n.id+' '+n.type).toLowerCase().includes(q);
      const matchT = !t || n.type === t;
      node.toggleClass('faded', !(matchQ && matchT));
    });
    cy.edges().forEach(edge => {
      const faded = edge.source().hasClass('faded') || edge.target().hasClass('faded');
      edge.toggleClass('faded', faded);
    });
  });
}
document.getElementById('search').addEventListener('input', applyFilter);
tf.addEventListener('change', applyFilter);
document.getElementById('layout').addEventListener('change', e =>
  cy.layout({ name: e.target.value, animate: false }).run());

document.getElementById('stats').textContent =
  `${DATA.nodes.length} concepts · ${DATA.edges.length} links · ${types.length} types`;
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn make_manager(vault_dir: &Path) -> Arc<VaultManager> {
        use turbovault_core::{ServerConfig, VaultConfig};
        let mut config = ServerConfig::new();
        config
            .vaults
            .push(VaultConfig::builder("test", vault_dir).build().unwrap());
        Arc::new(VaultManager::new(config).unwrap())
    }

    #[tokio::test]
    async fn generates_html_with_nodes_and_edges() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join("tables")).unwrap();
        std::fs::write(
            temp.path().join("tables/customers.md"),
            "---\ntype: BigQuery Table\ntitle: Customers\ndescription: One row per customer.\n---\n# Schema\n",
        )
        .unwrap();
        std::fs::write(
            temp.path().join("tables/orders.md"),
            "---\ntype: BigQuery Table\ntitle: Orders\n---\n# Joins\n\nJoined with [customers](/tables/customers.md).\n",
        )
        .unwrap();

        let manager = make_manager(temp.path());
        manager.initialize().await.unwrap();
        let tools = ViewerTools::new(manager);

        let (html, summary) = tools.generate(Some("My Bundle")).await.unwrap();
        assert_eq!(summary.nodes, 2);
        assert_eq!(summary.edges, 1);
        assert!(html.contains("My Bundle"));
        assert!(html.contains("cytoscape"));
        // Embedded data present and the concept appears.
        assert!(html.contains("\"Customers\""));
        assert!(html.contains("tables/customers"));
    }

    #[tokio::test]
    async fn escapes_script_breakers_in_body() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            temp.path().join("x.md"),
            "---\ntype: note\n---\nText with a </script> breaker.\n",
        )
        .unwrap();
        let manager = make_manager(temp.path());
        manager.initialize().await.unwrap();
        let tools = ViewerTools::new(manager);

        let (html, _) = tools.generate(None).await.unwrap();
        // The literal closing tag must not survive inside the embedded JSON.
        assert!(!html.contains("</script> breaker"));
        assert!(html.contains("<\\/script> breaker"));
    }
}
