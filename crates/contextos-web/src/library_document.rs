//! Reading one file from a managed library (Standards, Templates, a registry type) for display, and the line-ending
//! bookkeeping that lets the whole-file editor give the file back byte for byte.
//!
//! A library file is source the vault's agents and skills consume, so it is edited whole, frontmatter included,
//! rather than through a frontmatter form: a template's frontmatter carries Templater commands a form would mangle,
//! and a definition's frontmatter is a contract its skill reads. Reading it for display splits it into a properties
//! table (from `frontmatter_read`) and the rendered body; a file that is not Markdown is shown as highlighted source.

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::mcp_client::{McpCallError, McpClient};
use crate::note_render::{self, NoteView};
use crate::rendering::code::{self, Language};
use crate::vault_content::{AttachedRead, read_attached};
use crate::vault_relative_path::VaultRelativePath;

/// How a library shows its Markdown files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderMode {
    /// As an ordinary rendered note.
    Document,
    /// As a template: Templater commands and `{{placeholders}}` stay visible and marked.
    Template,
}

/// One frontmatter property, shown as text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Property {
    pub key: String,
    /// Each value: one for a scalar, one per item for a list, none for an empty value.
    pub values: Vec<String>,
}

/// What a library file looks like when read.
#[derive(Clone, Debug)]
pub enum DocumentBody {
    Markdown {
        properties: Vec<Property>,
        /// The frontmatter block exactly as written, shown instead of `properties` when it could not be parsed.
        raw_frontmatter: Option<String>,
        html: String,
    },
    Code {
        html: String,
        language: Language,
    },
    /// The file exists but is not text.
    NotText,
}

/// A library file read for display and editing.
#[derive(Clone, Debug)]
pub struct Document {
    /// The file's exact text, empty for a file that is not text.
    pub raw: String,
    pub content_hash: Option<String>,
    pub body: DocumentBody,
}

/// Why a library file could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ReadDocumentError {
    #[error("this file could not be read")]
    Refused { code: Option<String> },
    #[error(transparent)]
    Unreachable(#[from] McpCallError),
}

#[derive(Debug, Default, Deserialize)]
struct FrontmatterReadResult {
    #[serde(default)]
    frontmatter: Map<String, Value>,
}

fn property_values(value: &Value) -> Vec<String> {
    match value {
        Value::Null => Vec::new(),
        Value::String(text) if text.is_empty() => Vec::new(),
        Value::String(text) => vec![text.clone()],
        Value::Array(items) => items.iter().flat_map(property_values).collect(),
        other => vec![other.to_string()],
    }
}

/// Every frontmatter property in its written order.
#[must_use]
pub fn properties(frontmatter: &Map<String, Value>) -> Vec<Property> {
    frontmatter
        .iter()
        .map(|(key, value)| Property {
            key: key.clone(),
            values: property_values(value),
        })
        .collect()
}

/// The leading frontmatter block of `raw`, delimiters included, if it has one.
#[must_use]
pub fn frontmatter_block(raw: &str) -> Option<&str> {
    let normalised_start = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let body = crate::vault_content::body_of(normalised_start);
    (body.len() < normalised_start.len()).then(|| &normalised_start[..normalised_start.len() - body.len()])
}

async fn read_properties(client: &McpClient, vault_path: &str) -> Option<Map<String, Value>> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(vault_path.to_owned()));
    let result = client.call_tool("frontmatter_read".to_owned(), args).await.ok()?;
    if result.is_error == Some(true) {
        return None;
    }
    result
        .into_typed::<FrontmatterReadResult>()
        .ok()
        .map(|read| read.frontmatter)
}

async fn render_markdown(
    client: &McpClient,
    vault_name: &str,
    path: &VaultRelativePath,
    raw: &str,
    mode: RenderMode,
) -> DocumentBody {
    let vault_path = format!("{vault_name}://{path}");
    let block = frontmatter_block(raw);
    let (properties, raw_frontmatter) = match block {
        None => (Vec::new(), None),
        Some(block) => match read_properties(client, &vault_path).await {
            Some(frontmatter) => (properties(&frontmatter), None),
            None => (Vec::new(), Some(block.to_owned())),
        },
    };
    let html = note_render::render_note(
        client,
        vault_name,
        NoteView {
            path: path.as_str(),
            raw: Some(raw),
            template: mode == RenderMode::Template,
        },
    )
    .await
    .html;
    DocumentBody::Markdown {
        properties,
        raw_frontmatter,
        html,
    }
}

/// Reads `path` in `vault_name` for display.
///
/// # Errors
///
/// Returns [`ReadDocumentError::Refused`] when the server will not read the file, or
/// [`ReadDocumentError::Unreachable`] when the transport fails.
pub async fn read_document(
    client: &McpClient,
    vault_name: &str,
    path: &VaultRelativePath,
    mode: RenderMode,
) -> Result<Document, ReadDocumentError> {
    match read_attached(client, &format!("{vault_name}://{path}")).await? {
        AttachedRead::Refused { code } => Err(ReadDocumentError::Refused { code }),
        AttachedRead::NotText => Ok(Document {
            raw: String::new(),
            content_hash: None,
            body: DocumentBody::NotText,
        }),
        AttachedRead::Text(attached) => {
            let is_markdown = path.file_name().to_ascii_lowercase().ends_with(".md");
            let body = if is_markdown {
                render_markdown(client, vault_name, path, &attached.text, mode).await
            } else {
                let language = Language::for_file_name(path.file_name());
                DocumentBody::Code {
                    html: code::highlight(&attached.text, language),
                    language,
                }
            };
            Ok(Document {
                raw: attached.text,
                content_hash: attached.content_hash,
                body,
            })
        }
    }
}

/// Whether saving a file from a browser editor changes its line endings. The vault stores all text with `\n` line
/// endings, so a file with `\r\n` or lone `\r` ones is converted when it is saved, and the editor says so first.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineBreaks {
    Unix,
    /// At least one `\r\n` or lone `\r`, which a save writes as `\n`.
    Converted,
}

impl From<&str> for LineBreaks {
    fn from(text: &str) -> Self {
        if text.contains('\r') {
            Self::Converted
        } else {
            Self::Unix
        }
    }
}

/// What an editor says before saving a file that a save converts to `\n` line endings.
pub const CONVERTED_LINE_ENDINGS: &str = "This file has Windows (\\r\\n) or classic Mac (\\r) line endings. Saving \
                                          writes it with \\n line endings, as the vault stores all text.";

#[cfg(test)]
#[path = "library_document_test.rs"]
mod tests;
