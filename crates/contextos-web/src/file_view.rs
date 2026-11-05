//! The shared view of a page file that is not a note: a base, a canvas, or a diagram, shown inside whichever screen
//! owns its folder. Every screen shows these the same way, so the rendering lives here once.

use contextos_markdown::FileKind;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::mcp_client::{McpCallError, McpClient};
use crate::rendering::{base, canvas, diagnostics};
use crate::vault_content::read_attached_text;

/// A page file a screen shows through this view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageFile {
    Base,
    Canvas,
    Diagram,
}

/// The file is a note, or not a page at all.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NotAPageFile;

impl TryFrom<&str> for PageFile {
    type Error = NotAPageFile;

    /// The kind of page file at `path`, by its extension.
    fn try_from(path: &str) -> Result<Self, Self::Error> {
        match FileKind::from(path) {
            FileKind::Base => Ok(Self::Base),
            FileKind::Canvas => Ok(Self::Canvas),
            FileKind::Diagram => Ok(Self::Diagram),
            _ => Err(NotAPageFile),
        }
    }
}

#[derive(Debug, Deserialize)]
struct CanvasReadResult {
    nodes: Vec<canvas::CanvasNode>,
    edges: Vec<canvas::CanvasEdge>,
    #[serde(default)]
    diagnostics: Vec<crate::rendering::Diagnostic>,
}

/// What a screen shows as the body of the page file at `path` (vault-relative): a base's view (`view` when given),
/// a canvas's board, or a diagram. A file that cannot be read or parsed is shown as its problems, never as an error
/// page.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the transport fails.
pub async fn page_file_html(
    client: &McpClient,
    vault_name: &str,
    kind: PageFile,
    path: &str,
    view: Option<&str>,
) -> Result<String, McpCallError> {
    Ok(match kind {
        PageFile::Base => base::render_view(client, vault_name, path, view).await?,
        PageFile::Canvas => canvas_html(client, vault_name, path).await?,
        PageFile::Diagram => match read_attached_text(client, &format!("{vault_name}://{path}")).await? {
            Some(read) => crate::note_render::diagram_html(client, &read.text).await,
            None => unreadable(path),
        },
    })
}

async fn canvas_html(client: &McpClient, vault_name: &str, path: &str) -> Result<String, McpCallError> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{path}")));
    let result = client.call_tool("canvas_read".to_owned(), args).await?;
    let Ok(parsed) = result.into_typed::<CanvasReadResult>() else {
        return Ok(unreadable(path));
    };
    if !parsed.diagnostics.is_empty() {
        return Ok(diagnostics::render_diagnostic_panel(&parsed.diagnostics));
    }
    let content = canvas::canvas_content(client, vault_name, path).await;
    Ok(canvas::render_svg(&parsed.nodes, &parsed.edges, &content))
}

/// Whether a file exists at `path` (vault-relative), for a screen whose own listing holds notes only.
pub async fn page_file_exists(client: &McpClient, vault_name: &str, path: &str) -> bool {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{path}")));
    client
        .call_tool("fs_get_file_info".to_owned(), args)
        .await
        .is_ok_and(|result| result.is_error != Some(true))
}

fn unreadable(path: &str) -> String {
    diagnostics::render_diagnostic_panel(&[crate::rendering::Diagnostic {
        code: "io/unreadable".to_owned(),
        path: path.to_owned(),
        message: "This file could not be read.".to_owned(),
    }])
}

#[cfg(test)]
#[path = "file_view_test.rs"]
mod tests;
