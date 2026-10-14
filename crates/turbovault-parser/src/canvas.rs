//! JSON Canvas files (`.canvas`), as Obsidian's Canvas core plugin writes
//! them.
//!
//! Follows the JSON Canvas 1.0 spec (<https://jsoncanvas.org/spec/1.0/>): a
//! document has optional `nodes` and `edges` arrays. Every node has an `id`,
//! a `type` (`text`, `file`, `link` or `group`), integer `x`, `y`, `width`
//! and `height`, and an optional `color`; a text node adds `text`, a file
//! node `file` (a vault path) and an optional `subpath` starting with `#`, a
//! link node `url`, and a group an optional `label`, `background` and
//! `backgroundStyle` (`cover`, `ratio` or `repeat`). An edge has an `id`,
//! `fromNode` and `toNode`, and optional `fromSide`/`toSide` (`top`, `right`,
//! `bottom`, `left`), `fromEnd`/`toEnd` (`none` or `arrow`), `color` and
//! `label`. A color is a hex string or a preset `"1"` to `"6"`.
//!
//! Fields this model doesn't name (a plugin's own, or a later spec's) are
//! kept as they are, so reading a canvas and writing it back loses nothing.
//! Coordinates are kept as the JSON numbers they were written as.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

/// A whole canvas document.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Canvas {
    /// The canvas's nodes, in drawing order.
    #[serde(default)]
    pub nodes: Vec<CanvasNode>,
    /// The connections between nodes.
    #[serde(default)]
    pub edges: Vec<CanvasEdge>,
    /// Top-level fields beyond `nodes` and `edges`, kept as written.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One node on a canvas. Which of the type-specific fields apply depends on
/// `kind`; see the module docs. Fields are in the order Obsidian writes them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct CanvasNode {
    /// Unique among the canvas's nodes and edges.
    pub id: String,
    /// `text`, `file`, `link` or `group`.
    #[serde(rename = "type")]
    pub kind: String,
    /// A text node's markdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// A file node's vault path, with its extension.
    pub file: Option<String>,
    /// A file node's heading or block within the file, starting with `#`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
    /// A link node's URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// A group node's label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// A group node's background image path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(
        default,
        rename = "backgroundStyle",
        skip_serializing_if = "Option::is_none"
    )]
    /// How a group's background image is laid out: `cover`, `ratio` or
    /// `repeat`.
    pub background_style: Option<String>,
    /// Left edge, in canvas pixels.
    pub x: Number,
    /// Top edge, in canvas pixels.
    pub y: Number,
    /// Width, in canvas pixels.
    pub width: Number,
    /// Height, in canvas pixels.
    pub height: Number,
    /// A preset color `1` to `6`, or a hex color.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Fields the spec doesn't define, kept as written.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One edge between two nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct CanvasEdge {
    /// Unique among the canvas's nodes and edges.
    pub id: String,
    /// The id of the node the edge starts at.
    #[serde(rename = "fromNode")]
    pub from_node: String,
    /// The side it leaves from: `top`, `right`, `bottom` or `left`.
    #[serde(default, rename = "fromSide", skip_serializing_if = "Option::is_none")]
    pub from_side: Option<String>,
    /// The start's shape: `none` or `arrow`.
    #[serde(default, rename = "fromEnd", skip_serializing_if = "Option::is_none")]
    pub from_end: Option<String>,
    /// The id of the node the edge ends at.
    #[serde(rename = "toNode")]
    pub to_node: String,
    /// The side it arrives at: `top`, `right`, `bottom` or `left`.
    #[serde(default, rename = "toSide", skip_serializing_if = "Option::is_none")]
    pub to_side: Option<String>,
    /// The end's shape: `none` or `arrow`.
    #[serde(default, rename = "toEnd", skip_serializing_if = "Option::is_none")]
    pub to_end: Option<String>,
    /// A preset color `1` to `6`, or a hex color.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Text shown on the edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Fields the spec doesn't define, kept as written.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A note or file a canvas points at: a file node, or a link written in a
/// text node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanvasReference {
    /// A file node: `file` is a vault path, with its extension.
    File {
        /// The file node's id.
        node_id: String,
        /// The vault path it shows.
        path: String,
        /// The heading or block within the file, if any.
        subpath: Option<String>,
    },
    /// A wikilink, embed or Markdown link inside a text node.
    Text {
        /// The text node's id.
        node_id: String,
        /// The link, with its span in the node's text.
        link: turbovault_core::Link,
    },
}

/// Parse a canvas document. An empty file is an empty canvas, as Obsidian
/// treats it.
pub fn parse_canvas(json: &str) -> Result<Canvas, String> {
    if json.trim().is_empty() {
        return Ok(Canvas::default());
    }
    serde_json::from_str(json).map_err(|error| format!("not a JSON Canvas document: {error}"))
}

impl Canvas {
    /// Everything that keeps this document from being valid JSON Canvas
    /// 1.0, one message per problem; empty when it is valid. Checks what the
    /// spec requires: unique ids across nodes and edges, the fields each
    /// node type needs, integer coordinates and sizes, edges that join
    /// existing nodes, and the enumerated values (`type`, sides, ends,
    /// `backgroundStyle`, colors).
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let mut ids = std::collections::HashSet::new();
        for node in &self.nodes {
            if node.id.is_empty() {
                problems.push("a node has an empty id".to_string());
            } else if !ids.insert(node.id.as_str()) {
                problems.push(format!("id {:?} is used more than once", node.id));
            }
            let required: &[(&str, bool)] = match node.kind.as_str() {
                "text" => &[("text", node.text.is_some())],
                "file" => &[("file", node.file.is_some())],
                "link" => &[("url", node.url.is_some())],
                "group" => &[],
                other => {
                    problems.push(format!(
                        "node {:?} has type {other:?}; expected text, file, link or group",
                        node.id
                    ));
                    &[]
                }
            };
            for (field, present) in required {
                if !present {
                    problems.push(format!("{} node {:?} has no {field}", node.kind, node.id));
                }
            }
            for (field, value) in [
                ("x", &node.x),
                ("y", &node.y),
                ("width", &node.width),
                ("height", &node.height),
            ] {
                if !(value.is_i64() || value.is_u64()) {
                    problems.push(format!(
                        "node {:?} has a non-integer {field} ({value})",
                        node.id
                    ));
                }
            }
            if let Some(subpath) = &node.subpath
                && !subpath.starts_with('#')
            {
                problems.push(format!(
                    "node {:?} has subpath {subpath:?}, which must start with #",
                    node.id
                ));
            }
            if let Some(style) = &node.background_style
                && !matches!(style.as_str(), "cover" | "ratio" | "repeat")
            {
                problems.push(format!(
                    "node {:?} has backgroundStyle {style:?}; expected cover, ratio or repeat",
                    node.id
                ));
            }
            if let Some(color) = &node.color
                && !is_canvas_color(color)
            {
                problems.push(format!("node {:?} has color {color:?}", node.id));
            }
        }
        for edge in &self.edges {
            if edge.id.is_empty() {
                problems.push("an edge has an empty id".to_string());
            } else if !ids.insert(edge.id.as_str()) {
                problems.push(format!("id {:?} is used more than once", edge.id));
            }
            for (end, node) in [("fromNode", &edge.from_node), ("toNode", &edge.to_node)] {
                if !self.nodes.iter().any(|n| &n.id == node) {
                    problems.push(format!(
                        "edge {:?} has {end} {node:?}, which is no node on the canvas",
                        edge.id
                    ));
                }
            }
            for (field, side) in [("fromSide", &edge.from_side), ("toSide", &edge.to_side)] {
                if let Some(side) = side
                    && !matches!(side.as_str(), "top" | "right" | "bottom" | "left")
                {
                    problems.push(format!(
                        "edge {:?} has {field} {side:?}; expected top, right, bottom or left",
                        edge.id
                    ));
                }
            }
            for (field, end) in [("fromEnd", &edge.from_end), ("toEnd", &edge.to_end)] {
                if let Some(end) = end
                    && !matches!(end.as_str(), "none" | "arrow")
                {
                    problems.push(format!(
                        "edge {:?} has {field} {end:?}; expected none or arrow",
                        edge.id
                    ));
                }
            }
            if let Some(color) = &edge.color
                && !is_canvas_color(color)
            {
                problems.push(format!("edge {:?} has color {color:?}", edge.id));
            }
        }
        problems
    }

    /// The notes and files this canvas points at: each file node, and each
    /// wikilink, embed or internal Markdown link in a text node.
    pub fn references(&self) -> Vec<CanvasReference> {
        let mut references = Vec::new();
        for node in &self.nodes {
            match node.kind.as_str() {
                "file" => {
                    if let Some(path) = &node.file {
                        references.push(CanvasReference::File {
                            node_id: node.id.clone(),
                            path: path.clone(),
                            subpath: node.subpath.clone(),
                        });
                    }
                }
                "text" => {
                    let Some(text) = &node.text else { continue };
                    let mut links = crate::parse_wikilinks(text);
                    links.extend(crate::parse_embeds(text));
                    links.extend(
                        crate::parse_markdown_links(text)
                            .into_iter()
                            .filter(|link| {
                                !matches!(
                                    link.type_,
                                    turbovault_core::LinkType::ExternalLink
                                        | turbovault_core::LinkType::Anchor
                                )
                            }),
                    );
                    references.extend(links.into_iter().map(|link| CanvasReference::Text {
                        node_id: node.id.clone(),
                        link,
                    }));
                }
                _ => {}
            }
        }
        references
    }

    /// The document as Obsidian writes it: tab-indented, one node or edge
    /// per line, so a change shows up in a diff as the lines it touched.
    pub fn to_obsidian_json(&self) -> String {
        fn line<T: Serialize>(item: &T) -> String {
            serde_json::to_string(item).unwrap_or_default()
        }
        let mut out = String::from("{\n");
        let mut sections: Vec<String> = Vec::new();
        for (key, items) in [
            ("nodes", self.nodes.iter().map(line).collect::<Vec<_>>()),
            ("edges", self.edges.iter().map(line).collect::<Vec<_>>()),
        ] {
            if items.is_empty() {
                sections.push(format!("\t\"{key}\":[]"));
            } else {
                sections.push(format!("\t\"{key}\":[\n\t\t{}\n\t]", items.join(",\n\t\t")));
            }
        }
        for (key, value) in &self.extra {
            sections.push(format!("\t{}:{}", line(key), line(value)));
        }
        out.push_str(&sections.join(",\n"));
        out.push_str("\n}");
        out
    }
}

/// A preset (`"1"` to `"6"`) or a `#RGB` / `#RRGGBB` hex color.
fn is_canvas_color(color: &str) -> bool {
    if matches!(color, "1" | "2" | "3" | "4" | "5" | "6") {
        return true;
    }
    color
        .strip_prefix('#')
        .is_some_and(|hex| matches!(hex.len(), 3 | 6) && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r##"{
	"nodes":[
		{"id":"a","type":"text","text":"See [[Plan]] and [x](notes/Idea.md)","x":0,"y":0,"width":250,"height":60},
		{"id":"b","type":"file","file":"notes/Plan.md","subpath":"#Goals","x":300,"y":0,"width":400,"height":400,"color":"4"},
		{"id":"c","type":"link","url":"https://example.com","x":0,"y":500,"width":250,"height":60},
		{"id":"g","type":"group","label":"Ideas","x":-50,"y":-50,"width":900,"height":700,"plugin-field":true}
	],
	"edges":[
		{"id":"e1","fromNode":"a","fromSide":"right","toNode":"b","toSide":"left","label":"plans"}
	]
}"##;

    #[test]
    fn reads_every_node_type_and_keeps_unknown_fields() {
        let canvas = parse_canvas(SAMPLE).unwrap();
        assert_eq!(canvas.nodes.len(), 4);
        assert_eq!(canvas.edges[0].from_node, "a");
        assert_eq!(canvas.nodes[3].extra["plugin-field"], Value::Bool(true));
        assert!(canvas.problems().is_empty(), "{:?}", canvas.problems());
    }

    #[test]
    fn writes_back_what_it_read() {
        let canvas = parse_canvas(SAMPLE).unwrap();
        let written = canvas.to_obsidian_json();
        assert_eq!(written, SAMPLE);
        assert_eq!(parse_canvas(&written).unwrap(), canvas);
    }

    #[test]
    fn an_empty_file_is_an_empty_canvas() {
        assert_eq!(parse_canvas("").unwrap(), Canvas::default());
        assert_eq!(
            Canvas::default().to_obsidian_json(),
            "{\n\t\"nodes\":[],\n\t\"edges\":[]\n}"
        );
    }

    #[test]
    fn references_are_file_nodes_and_links_in_text() {
        let canvas = parse_canvas(SAMPLE).unwrap();
        let references = canvas.references();
        assert!(references.contains(&CanvasReference::File {
            node_id: "b".into(),
            path: "notes/Plan.md".into(),
            subpath: Some("#Goals".into()),
        }));
        let text_targets: Vec<&str> = references
            .iter()
            .filter_map(|r| match r {
                CanvasReference::Text { link, .. } => Some(link.target.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text_targets, ["Plan", "notes/Idea.md"]);
    }

    #[test]
    fn problems_name_what_the_spec_requires() {
        let canvas = parse_canvas(
            r#"{"nodes":[
                {"id":"a","type":"text","x":0,"y":0,"width":1.5,"height":1},
                {"id":"a","type":"file","file":"x.md","subpath":"Goals","x":0,"y":0,"width":1,"height":1,"color":"9"},
                {"id":"s","type":"shape","x":0,"y":0,"width":1,"height":1}
            ],"edges":[
                {"id":"e","fromNode":"a","toNode":"missing","toEnd":"circle","fromSide":"middle"}
            ]}"#,
        )
        .unwrap();
        let problems = canvas.problems().join("\n");
        for expected in [
            "text node \"a\" has no text",
            "non-integer width",
            "id \"a\" is used more than once",
            "subpath \"Goals\"",
            "color \"9\"",
            "type \"shape\"",
            "toNode \"missing\"",
            "toEnd \"circle\"",
            "fromSide \"middle\"",
        ] {
            assert!(
                problems.contains(expected),
                "missing {expected:?} in\n{problems}"
            );
        }
    }
}
