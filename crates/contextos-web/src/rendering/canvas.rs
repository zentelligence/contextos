//! `.canvas` SVG rendering: a JSON Canvas 1.0
//! document (`obsidian:json-canvas` skill), laid out to SVG using each
//! node's own `x`/`y`/`width`/`height`. No auto-layout runs at any point:
//! every coordinate used is the one the file itself already carries.
//!
//! A `canvas_read` diagnostic (dangling edge, duplicate id, unparseable
//! file) is the caller's signal to render
//! [`diagnostics::render_diagnostic_panel`](super::diagnostics::render_diagnostic_panel)
//! instead of calling [`render_svg`] at all; this module assumes it is
//! only ever asked to lay out an already-diagnostic-free node/edge set.

use std::collections::HashMap;

use contextos_markdown::{FileKind, Fragment, LinkDestination, LinkRouter, LinkTarget};
use serde::Deserialize;

use crate::mcp_client::McpClient;
use crate::note_render::render_texts;
use crate::rendering::escape_html;
use crate::vault_content::read_expanded_text;

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CanvasNode {
    Text {
        id: String,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        text: String,
        #[serde(default)]
        color: Option<String>,
    },
    File {
        id: String,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        file: String,
        #[serde(default)]
        subpath: Option<String>,
        #[serde(default)]
        color: Option<String>,
    },
    Link {
        id: String,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        url: String,
        #[serde(default)]
        color: Option<String>,
    },
    Group {
        id: String,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        background: Option<String>,
        #[serde(default, rename = "backgroundStyle")]
        background_style: Option<String>,
        #[serde(default)]
        color: Option<String>,
    },
}

impl CanvasNode {
    #[must_use]
    fn geometry(&self) -> (f64, f64, f64, f64) {
        match self {
            Self::Text {
                x, y, width, height, ..
            }
            | Self::File {
                x, y, width, height, ..
            }
            | Self::Link {
                x, y, width, height, ..
            }
            | Self::Group {
                x, y, width, height, ..
            } => (*x, *y, *width, *height),
        }
    }

    #[must_use]
    fn id(&self) -> &str {
        match self {
            Self::Text { id, .. } | Self::File { id, .. } | Self::Link { id, .. } | Self::Group { id, .. } => id,
        }
    }

    #[must_use]
    fn color(&self) -> Option<&str> {
        match self {
            Self::Text { color, .. }
            | Self::File { color, .. }
            | Self::Link { color, .. }
            | Self::Group { color, .. } => color.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct CanvasEdge {
    pub id: String,
    #[serde(rename = "fromNode")]
    pub from_node: String,
    #[serde(default, rename = "fromSide")]
    pub from_side: Option<String>,
    #[serde(default, rename = "fromEnd")]
    pub from_end: Option<String>,
    #[serde(rename = "toNode")]
    pub to_node: String,
    #[serde(default, rename = "toSide")]
    pub to_side: Option<String>,
    #[serde(default, rename = "toEnd")]
    pub to_end: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

/// Maps a JSON Canvas preset colour (`"1"` to `"6"`) or an already-literal
/// hex value to a CSS colour. The preset-to-actual-colour mapping is a
/// `contextos-web` UI theme decision: the JSON Canvas presets carry a conventional hue meaning (1 red,
/// 2 orange, 3 yellow, 4 green, 5 cyan, 6 purple), drawn here from the
/// design system's own hue family. These are fixed values in both skins,
/// consistent with the exemption of diagram interiors from the automated
/// contrast check: a canvas node's colour is file-authored data, not
/// chrome `contextos-web` themes.
fn resolve_color(color: Option<&str>, fallback: &str) -> String {
    match color {
        None => fallback.to_owned(),
        Some("1") => "#F4586B".to_owned(),
        Some("2") => "#F5B44D".to_owned(),
        Some("3") => "#EBCB5E".to_owned(),
        Some("4") => "#5FE393".to_owned(),
        Some("5") => "#43D9C6".to_owned(),
        Some("6") => "#B49CFB".to_owned(),
        Some(hex) => hex.to_owned(),
    }
}

/// What a canvas's nodes show that needs the vault, by node id: each text node's rendered Markdown, and where each file
/// node goes (`None` when no screen shows its file, or it names none).
#[derive(Clone, Debug, Default)]
pub struct CanvasContent {
    pub texts: HashMap<String, String>,
    pub files: HashMap<String, Option<String>>,
}

/// The JSON Canvas fields this module reads from the expanded canvas: a node's id, kind, and the text or file it holds.
#[derive(Debug, Deserialize)]
struct ExpandedNodes {
    #[serde(default)]
    nodes: Vec<ExpandedNode>,
}

#[derive(Debug, Deserialize)]
struct ExpandedNode {
    id: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    subpath: Option<String>,
}

/// Reads the canvas at `path` with its links expanded by the server, renders every text node through the note
/// renderer (sharing what they need), and routes every file node, so the board's links work as a note's do.
pub async fn canvas_content(client: &McpClient, vault_name: &str, path: &str) -> CanvasContent {
    let expanded = read_expanded_text(client, &format!("{vault_name}://{path}"))
        .await
        .ok()
        .flatten()
        .and_then(|read| serde_json::from_str::<ExpandedNodes>(&read.text).ok())
        .map(|parsed| parsed.nodes)
        .unwrap_or_default();
    let texts: Vec<(&str, &str)> = expanded
        .iter()
        .filter_map(|node| node.text.as_deref().map(|text| (node.id.as_str(), text)))
        .collect();
    let files: Vec<(&ExpandedNode, LinkTarget)> = expanded
        .iter()
        .filter_map(|node| {
            node.file.as_deref().map(|file| {
                (
                    node,
                    LinkTarget::from(LinkDestination {
                        text: file,
                        percent_encoded: false,
                    }),
                )
            })
        })
        .collect();
    let file_paths: Vec<String> = files
        .iter()
        .filter_map(|(_, target)| match target {
            LinkTarget::Vault { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect();
    let rendered = render_texts(
        client,
        vault_name,
        &texts.iter().map(|(_, text)| *text).collect::<Vec<_>>(),
        &file_paths,
    )
    .await;
    let mut content = CanvasContent::default();
    for ((id, _), note) in texts.iter().zip(rendered.notes) {
        content.texts.insert((*id).to_owned(), note.html);
    }
    for (node, target) in files {
        let href = match target {
            LinkTarget::Vault { path, .. } if FileKind::from(path.as_str()).is_page() => {
                let anchor = node
                    .subpath
                    .as_deref()
                    .and_then(|subpath| subpath.strip_prefix('#'))
                    .map(|fragment| Fragment::from(fragment.to_owned()).anchor());
                rendered.routes.page(&path, anchor.as_deref())
            }
            LinkTarget::Vault { path, .. } => Some(rendered.routes.file(&path)),
            _ => None,
        };
        content.files.insert(node.id.clone(), href);
    }
    content
}

/// Renders a full `.canvas` document to SVG. `viewport` is the SVG's own
/// `width`/`height` attributes (the rendered frame's CSS box, not the
/// canvas's content bounds, which the returned `viewBox` covers instead).
#[must_use]
pub fn render_svg(nodes: &[CanvasNode], edges: &[CanvasEdge], content: &CanvasContent) -> String {
    let (min_x, min_y, max_x, max_y) = bounds(nodes);
    let padding = 40.0;
    let view_box = format!(
        "{} {} {} {}",
        min_x - padding,
        min_y - padding,
        (max_x - min_x) + 2.0 * padding,
        (max_y - min_y) + 2.0 * padding
    );

    let mut body = String::new();
    for node in nodes {
        body.push_str(&render_node(node, content));
    }
    for edge in edges {
        if let Some(svg) = render_edge(edge, nodes) {
            body.push_str(&svg);
        }
    }

    format!(
        "<svg class=\"canvas-svg\" viewBox=\"{view_box}\" xmlns=\"http://www.w3.org/2000/svg\">\
<defs><marker id=\"canvas-arrow\" markerWidth=\"8\" markerHeight=\"8\" refX=\"6\" refY=\"3\" orient=\"auto\">\
<path d=\"M0,0 L6,3 L0,6 Z\" fill=\"currentColor\"/></marker></defs>\
{body}</svg>"
    )
}

fn bounds(nodes: &[CanvasNode]) -> (f64, f64, f64, f64) {
    if nodes.is_empty() {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let mut min_x = f64::MAX;
    let mut min_y = f64::MAX;
    let mut max_x = f64::MIN;
    let mut max_y = f64::MIN;
    for node in nodes {
        let (x, y, w, h) = node.geometry();
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x + w);
        max_y = max_y.max(y + h);
    }
    (min_x, min_y, max_x, max_y)
}

fn render_node(node: &CanvasNode, content: &CanvasContent) -> String {
    let (x, y, w, h) = node.geometry();
    let id = escape_html(node.id());
    match node {
        CanvasNode::Group { label, background, .. } => {
            let stroke = resolve_color(node.color(), "#2A3348");
            let fill = background.clone().unwrap_or_else(|| "none".to_owned());
            let label_html = label
                .as_ref()
                .map(|l| {
                    format!(
                        "<text x=\"{}\" y=\"{}\" class=\"canvas-group-label\">{}</text>",
                        x + 6.0,
                        y - 6.0,
                        escape_html(l)
                    )
                })
                .unwrap_or_default();
            format!(
                "<g class=\"canvas-node canvas-node-group\" data-id=\"{id}\">\
<rect class=\"canvas-group-bg\" x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" \
fill=\"{fill}\" stroke=\"{stroke}\" stroke-dasharray=\"4 3\"/>{label_html}</g>"
            )
        }
        CanvasNode::Text { text, .. } => {
            let stroke = resolve_color(node.color(), "#2A3348");
            let content = content
                .texts
                .get(node.id())
                .cloned()
                .unwrap_or_else(|| format!("<p>{}</p>", escape_html(text)));
            format!(
                "<g class=\"canvas-node canvas-node-text\" data-id=\"{id}\">\
<rect class=\"node-bg\" x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" \
fill=\"none\" stroke=\"{stroke}\"/>\
<foreignObject x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\">\
<div xmlns=\"http://www.w3.org/1999/xhtml\" class=\"canvas-node-text\">{content}</div>\
</foreignObject></g>"
            )
        }
        CanvasNode::File { file, .. } => {
            let stroke = resolve_color(node.color(), "#2A3348");
            let (tx, ty) = (x + w / 2.0, y + h / 2.0);
            let label = escape_html(file);
            let text = match content.files.get(node.id()).cloned().flatten() {
                Some(href) => format!(
                    "<a href=\"{}\"><text x=\"{tx}\" y=\"{ty}\" class=\"canvas-node-text\">{label}</text></a>",
                    escape_html(&href)
                ),
                // No screen shows the file, or it names none: its name is shown without a link.
                None => format!(
                    "<text x=\"{tx}\" y=\"{ty}\" class=\"canvas-node-text canvas-file-unavailable\">{label}</text>"
                ),
            };
            format!(
                "<g class=\"canvas-node canvas-node-file\" data-id=\"{id}\">\
<rect class=\"node-bg\" x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" \
fill=\"none\" stroke=\"{stroke}\"/>{text}</g>"
            )
        }
        CanvasNode::Link { url, .. } => {
            let stroke = resolve_color(node.color(), "#2A3348");
            format!(
                "<g class=\"canvas-node canvas-node-link\" data-id=\"{id}\">\
<rect class=\"node-bg\" x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" \
fill=\"none\" stroke=\"{stroke}\"/>\
<a href=\"{href}\" target=\"_blank\" rel=\"noopener noreferrer\">\
<text x=\"{tx}\" y=\"{ty}\" class=\"canvas-node-text\">{label}</text></a></g>",
                href = escape_html(url),
                tx = x + w / 2.0,
                ty = y + h / 2.0,
                label = escape_html(url),
            )
        }
    }
}

fn render_edge(edge: &CanvasEdge, nodes: &[CanvasNode]) -> Option<String> {
    let from = nodes.iter().find(|n| n.id() == edge.from_node)?;
    let to = nodes.iter().find(|n| n.id() == edge.to_node)?;
    let (fx, fy, fw, fh) = from.geometry();
    let (tx, ty, tw, th) = to.geometry();
    let start = anchor_point(fx, fy, fw, fh, edge.from_side.as_deref());
    let end = anchor_point(tx, ty, tw, th, edge.to_side.as_deref());
    let arrow_end = edge.to_end.as_deref().unwrap_or("arrow");
    let marker = if arrow_end == "none" {
        String::new()
    } else {
        " marker-end=\"url(#canvas-arrow)\"".to_owned()
    };
    let color = resolve_color(edge.color.as_deref(), "#727A92");
    let label_html = edge
        .label
        .as_ref()
        .map(|label| {
            format!(
                "<text x=\"{}\" y=\"{}\" class=\"canvas-edge-label\">{}</text>",
                f64::midpoint(start.0, end.0),
                f64::midpoint(start.1, end.1) - 4.0,
                escape_html(label)
            )
        })
        .unwrap_or_default();
    Some(format!(
        "<path class=\"canvas-edge\" d=\"M{},{} L{},{}\" stroke=\"{color}\" fill=\"none\"{marker}/>{label_html}",
        start.0, start.1, end.0, end.1
    ))
}

fn anchor_point(x: f64, y: f64, w: f64, h: f64, side: Option<&str>) -> (f64, f64) {
    match side {
        Some("top") => (x + w / 2.0, y),
        Some("bottom") => (x + w / 2.0, y + h),
        Some("left") => (x, y + h / 2.0),
        Some("right") | None => (x + w, y + h / 2.0),
        Some(_) => (x + w / 2.0, y + h / 2.0),
    }
}

#[cfg(test)]
#[path = "canvas_test.rs"]
mod tests;
