//! The clean form of an MCP tool result, as a workflow script sees it.
//!
//! A script never receives the protocol envelope. A result with `structured_content` is that value. A result that is
//! one JSON text block is that JSON. `fs_attach_file` is neither: it embeds the file as a resource block, followed by
//! text blocks holding `content_hash` or `links_expanded`. It is returned here in the shape of `fs_read_text_file`,
//! `{ "content", "content_hash" }`, with any marker fields merged in, so a script can use either tool the same way.
//! The resource's URI is left out, because it carries a host filesystem path.
//!
//! The decoding follows the shape of the result, not the tool's name, so it applies to any tool on any configured
//! server. What has no script form is refused with [`ToolResultError::Unsupported`] and never partly delivered: a
//! resource link, an image or audio block, more than one embedded resource, and more than one text block.

use rmcp::model::{CallToolResult, ContentBlock, ResourceContents};
use serde_json::{Map, Value};
use thiserror::Error;

/// A tool result in its clean JSON form.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolResultJson(pub Value);

#[derive(Debug, Error)]
pub enum ToolResultError {
    #[error("the result's text is not JSON: {0}")]
    NotJson(#[source] serde_json::Error),
    /// The result holds something with no defined script form. It is refused, never partly delivered.
    #[error("the result cannot be given to a script: {reason}")]
    Unsupported { reason: &'static str },
}

impl TryFrom<CallToolResult> for ToolResultJson {
    type Error = ToolResultError;

    fn try_from(result: CallToolResult) -> Result<Self, ToolResultError> {
        if let Some(structured) = result.structured_content {
            return Ok(Self(structured));
        }
        let mut texts = Vec::new();
        let mut resources = Vec::new();
        for block in &result.content {
            match block {
                ContentBlock::Text(text) => texts.push(text.text.as_str()),
                ContentBlock::Resource(embedded) => resources.push(&embedded.resource),
                ContentBlock::Image(_) => {
                    return Err(ToolResultError::Unsupported {
                        reason: "an image block",
                    });
                }
                ContentBlock::Audio(_) => {
                    return Err(ToolResultError::Unsupported {
                        reason: "an audio block",
                    });
                }
                ContentBlock::ResourceLink(_) => {
                    return Err(ToolResultError::Unsupported {
                        reason: "a resource link, which references content rather than holding it",
                    });
                }
                _ => {
                    return Err(ToolResultError::Unsupported {
                        reason: "a content block of a kind this server does not know",
                    });
                }
            }
        }
        match (resources.as_slice(), texts.as_slice()) {
            ([], []) => Ok(Self(Value::Null)),
            ([], [only]) => serde_json::from_str(only).map(Self).map_err(ToolResultError::NotJson),
            ([], _) => Err(ToolResultError::Unsupported {
                reason: "more than one text block",
            }),
            ([resource], _) => embedded_resource(resource, &texts).map(|object| Self(Value::Object(object))),
            (_, _) => Err(ToolResultError::Unsupported {
                reason: "more than one embedded resource",
            }),
        }
    }
}

/// The embedded resource as `content` (or `blob` and `mime_type` for binary), with each accompanying text block, which
/// must be a JSON object, merged in. A key already present is never overwritten, so a marker cannot replace the
/// content.
fn embedded_resource(resource: &ResourceContents, texts: &[&str]) -> Result<Map<String, Value>, ToolResultError> {
    let mut object = Map::new();
    match resource {
        ResourceContents::TextResourceContents { text, .. } => {
            object.insert("content".to_owned(), Value::String(text.clone()));
        }
        ResourceContents::BlobResourceContents { blob, mime_type, .. } => {
            object.insert("blob".to_owned(), Value::String(blob.clone()));
            if let Some(mime_type) = mime_type {
                object.insert("mime_type".to_owned(), Value::String(mime_type.clone()));
            }
        }
        _ => {
            return Err(ToolResultError::Unsupported {
                reason: "an embedded resource of a kind this server does not know",
            });
        }
    }
    for text in texts {
        match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(fields)) => {
                for (key, value) in fields {
                    object.entry(key).or_insert(value);
                }
            }
            _ => {
                return Err(ToolResultError::Unsupported {
                    reason: "a text block beside an embedded resource that is not a JSON object",
                });
            }
        }
    }
    Ok(object)
}

#[cfg(test)]
#[path = "tool_result_test.rs"]
mod tests;
