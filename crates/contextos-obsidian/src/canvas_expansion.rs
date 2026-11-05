//! Rewrites every vault link in a JSON Canvas to a vault-rooted path, in place.

use serde_json::{Map, Value};

use crate::link_expansion::expand_markdown;
use crate::link_resolution::{ExpansionError, LinkResolution, LinkTargets, Resolver};
use crate::{AMBIGUOUS_MARKER, NOT_FOUND_MARKER};

/// A canvas to expand and the vault it is expanded against.
#[derive(Debug)]
pub struct CanvasExpansionInput<'a, T> {
    /// The canvas file's text.
    pub source: &'a str,
    /// The vault-relative path of the canvas: links in its text nodes resolve from its folder.
    pub canvas: &'a str,
    /// What the vault can say about a link target.
    pub targets: &'a T,
    /// The most distinct link targets to resolve, across text nodes and file nodes together.
    pub max_distinct_targets: usize,
    /// The most bytes the expanded canvas may hold.
    pub max_output_bytes: usize,
}

/// A canvas whose text node links and file node paths have all been rooted or marked.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpandedCanvas(String);

impl<T: LinkTargets> TryFrom<CanvasExpansionInput<'_, T>> for ExpandedCanvas {
    type Error = ExpansionError<T::Error>;

    fn try_from(value: CanvasExpansionInput<'_, T>) -> Result<Self, Self::Error> {
        let invalid = |reason: &str| ExpansionError::InvalidCanvas {
            reason: reason.to_owned(),
        };
        let mut canvas = serde_json::from_str::<Map<String, Value>>(value.source).map_err(|error| {
            ExpansionError::InvalidCanvas {
                reason: error.to_string(),
            }
        })?;
        let mut resolver = Resolver::new(value.targets, value.canvas, value.max_distinct_targets);
        let mut budget = value.max_output_bytes;
        if let Some(nodes) = canvas.get_mut("nodes") {
            let nodes = nodes.as_array_mut().ok_or_else(|| invalid("nodes is not an array"))?;
            for node in nodes {
                let node = node.as_object_mut().ok_or_else(|| invalid("a node is not an object"))?;
                let kind = node.get("type").and_then(Value::as_str).unwrap_or_default().to_owned();
                match kind.as_str() {
                    "text" => {
                        if let Some(Value::String(text)) = node.get_mut("text") {
                            *text = expand_markdown(text, &mut resolver, budget)?;
                            budget = budget.saturating_sub(text.len());
                        }
                    }
                    "file" => {
                        if let Some(Value::String(file)) = node.get_mut("file") {
                            *file = expanded_file(file, &mut resolver)?;
                        }
                    }
                    _ => {}
                }
            }
        }
        let expanded = serde_json::to_string_pretty(&canvas).map_err(|error| ExpansionError::InvalidCanvas {
            reason: error.to_string(),
        })?;
        if expanded.len() > value.max_output_bytes {
            return Err(ExpansionError::OutputTooLarge {
                maximum: value.max_output_bytes,
            });
        }
        Ok(Self(expanded))
    }
}

/// A file node's `file`, a path from the vault root, rooted when it names a file and marked when it does not. It is
/// exact, never searched for by name, as Obsidian writes it.
fn expanded_file<T: LinkTargets>(
    file: &str,
    resolver: &mut Resolver<'_, T>,
) -> Result<String, ExpansionError<T::Error>> {
    if file.starts_with(NOT_FOUND_MARKER) || file.starts_with(AMBIGUOUS_MARKER) {
        return Ok(file.to_owned());
    }
    let rooted = format!("/{}", file.trim_start_matches('/'));
    Ok(match resolver.resolve(&rooted)? {
        LinkResolution::Found(path) => format!("/{path}"),
        LinkResolution::NotFound | LinkResolution::Ambiguous { .. } => format!("{NOT_FOUND_MARKER}{file}"),
    })
}

impl AsRef<str> for ExpandedCanvas {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<ExpandedCanvas> for String {
    fn from(value: ExpandedCanvas) -> Self {
        value.0
    }
}

#[cfg(test)]
#[path = "canvas_expansion_test.rs"]
mod tests;
