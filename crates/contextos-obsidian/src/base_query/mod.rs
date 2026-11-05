//! Pure `base_query` execution: resolving a `.base` view or an inline
//! definition into an effective filter/column/sort/limit shape
//! ([`QueryDefinition`]), evaluating that filter tree against candidate
//! rows ([`evaluate_filters`]), resolving display columns and formulas
//! ([`resolve_column`], [`resolve_row`]), ordering results
//! ([`compare_values`]), and rendering them ([`render`]). No filesystem or
//! link-graph access: `contextos-mcp` supplies each [`RowContext`].

mod error;
mod filter;
mod property;
mod render;
mod scan_hint;
mod sort;

use serde_json::{Map, Value};

use crate::base::BaseDocument;

use self::property::validate_property_name;

pub use self::error::BaseQueryError;
pub use self::filter::{evaluate_filters, validate_filters};
pub use self::property::{ColumnValue, resolve_column, resolve_property, resolve_row};
pub use self::render::{QueryFormat, render};
pub use self::scan_hint::{RecordsFolder, ScanRootHint, records_folder, scan_root_hint};
pub use self::sort::compare_values;

/// Scalar `file.*` metadata `base_query` can filter, sort, or display on.
/// `name` is the full filename including its extension (`alpha.md`),
/// matching Obsidian's own `file.name`; `basename` is the name without an
/// extension (`alpha`). `folder` is the vault-relative parent directory of
/// `path` (`""` for a vault-root note). `ctime`/`mtime` are the platform's
/// rendered timestamp text (same `OffsetDateTime::to_string()` convention
/// `fs_get_file_info` already uses), `None` when the platform reports none;
/// only equality/inequality is supported on them, never ordering or
/// arithmetic, consistent with the rest of `base_query`'s filter grammar.
#[derive(Clone, Copy, Debug)]
pub struct FileMetadata<'a> {
    pub name: &'a str,
    pub basename: &'a str,
    pub path: &'a str,
    pub ext: &'a str,
    pub folder: &'a str,
    pub size: u64,
    pub ctime: Option<&'a str>,
    pub mtime: Option<&'a str>,
}

/// One candidate row: a note's frontmatter, `file.*` metadata, resolved
/// tags (frontmatter `tags` plus inline `#tag`, via
/// [`contextos_core::extract_tags`]), and the note's outgoing links,
/// outgoing embeds, and backlinks, ready for filter evaluation. `links`/
/// `embeds` are literal wikilink target text parsed from the note's own
/// body (matching `links_read`'s existing `outgoing[].target`
/// representation, not resolved through the link graph, so they work
/// without one); `backlinks` are vault-relative paths resolved through the
/// link graph when one is available, empty otherwise (see
/// `contextos-mcp`'s orchestration for how each is populated).
#[derive(Clone, Copy, Debug)]
pub struct RowContext<'a> {
    pub frontmatter: &'a Map<String, Value>,
    pub file: FileMetadata<'a>,
    pub tags: &'a [String],
    pub links: &'a [String],
    pub embeds: &'a [String],
    pub backlinks: &'a [String],
}

/// A sort key resolved from a view's `sort` list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SortKey {
    pub property: String,
    pub descending: bool,
}

/// The effective filter/column/sort/limit shape `base_query` executes,
/// resolved from either an existing `.base` file's named view
/// ([`QueryDefinition::from_document`]) or an inline ad hoc definition
/// ([`QueryDefinition::from_inline`]).
#[derive(Clone, Debug)]
pub struct QueryDefinition {
    pub filters: Option<Value>,
    pub columns: Vec<String>,
    pub sort: Vec<SortKey>,
    pub limit: Option<usize>,
    /// The document's top-level `formulas` map (name to expression string),
    /// empty when absent: `formulas` is a document-level-only key, never
    /// per-view, matching Obsidian's own Bases schema. Consulted only when
    /// resolving a `formula.*` display column ([`resolve_column`]); a
    /// `formula.*` reference in a filter or sort key is unaffected and
    /// still fails closed.
    pub formulas: Map<String, Value>,
}

impl QueryDefinition {
    /// Resolves the named view (or the first view when `view` is `None`)
    /// from an already schema-validated [`BaseDocument`].
    ///
    /// # Errors
    ///
    /// Returns [`BaseQueryError::ViewNotFound`] when `view` names a view the
    /// document does not contain, or [`BaseQueryError::NoColumns`] when
    /// neither the resolved view's `order` nor the document's top-level
    /// `properties` yield a display column set.
    pub fn from_document(document: &BaseDocument, view: Option<&str>) -> Result<Self, BaseQueryError> {
        let definition = document.definition();
        let views = definition
            .get("views")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let selected = match view {
            Some(name) => views
                .iter()
                .find(|candidate| candidate.get("name").and_then(Value::as_str) == Some(name))
                .ok_or_else(|| BaseQueryError::ViewNotFound { name: name.to_owned() })?,
            None => views.first().ok_or_else(|| BaseQueryError::MalformedDefinition {
                path: "views".to_owned(),
                violation: "base has no views to query".to_owned(),
            })?,
        };
        let selected_name = selected
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("(unnamed)")
            .to_owned();
        let selected = selected
            .as_object()
            .ok_or_else(|| BaseQueryError::MalformedDefinition {
                path: "views".to_owned(),
                violation: "view must be an object".to_owned(),
            })?;
        let filters = merge_and(definition.get("filters"), selected.get("filters"));
        let order = string_array(selected.get("order"), "views.order")?;
        let sort = parse_sort(selected.get("sort"))?;
        let limit = parse_limit(selected.get("limit"))?;
        let columns = if order.is_empty() {
            property_keys(definition.get("properties"))
        } else {
            order
        };
        if columns.is_empty() {
            return Err(BaseQueryError::NoColumns { view: selected_name });
        }
        let formulas = formula_map(definition.get("formulas"));
        Ok(Self {
            filters,
            columns,
            sort,
            limit,
            formulas,
        })
    }

    /// Resolves an inline ad hoc definition: the same `filters`/`order`/
    /// `sort`/`limit`/`properties` shape as one view, without a wrapping
    /// `views` array.
    ///
    /// # Errors
    ///
    /// Returns [`BaseQueryError::MalformedDefinition`] for a structurally
    /// invalid `order`, `sort`, or `limit`, or [`BaseQueryError::NoColumns`]
    /// when neither `order` nor `properties` yield a display column set.
    pub fn from_inline(definition: &Map<String, Value>) -> Result<Self, BaseQueryError> {
        let filters = definition.get("filters").cloned();
        let order = string_array(definition.get("order"), "order")?;
        let sort = parse_sort(definition.get("sort"))?;
        let limit = parse_limit(definition.get("limit"))?;
        let columns = if order.is_empty() {
            property_keys(definition.get("properties"))
        } else {
            order
        };
        if columns.is_empty() {
            return Err(BaseQueryError::NoColumns {
                view: "(inline)".to_owned(),
            });
        }
        let formulas = formula_map(definition.get("formulas"));
        Ok(Self {
            filters,
            columns,
            sort,
            limit,
            formulas,
        })
    }
}

/// Reads a `formulas` map's string-valued entries only: a non-string
/// formula body (structurally invalid per the schema) is dropped rather
/// than rejecting the whole query, since [`resolve_column`] degrades to
/// the "not evaluated" marker for any formula it does not recognise
/// anyway.
fn formula_map(value: Option<&Value>) -> Map<String, Value> {
    value
        .and_then(Value::as_object)
        .map(|formulas| {
            formulas
                .iter()
                .filter(|(_, expression)| expression.is_string())
                .map(|(name, expression)| (name.clone(), expression.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn merge_and(top: Option<&Value>, view: Option<&Value>) -> Option<Value> {
    match (top, view) {
        (Some(top), Some(view)) => Some(Value::Object(Map::from_iter([(
            "and".to_owned(),
            Value::Array(vec![top.clone(), view.clone()]),
        )]))),
        (Some(only), None) | (None, Some(only)) => Some(only.clone()),
        (None, None) => None,
    }
}

fn string_array(value: Option<&Value>, path: &str) -> Result<Vec<String>, BaseQueryError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let items = value.as_array().ok_or_else(|| BaseQueryError::MalformedDefinition {
        path: path.to_owned(),
        violation: "must be an array of strings".to_owned(),
    })?;
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| BaseQueryError::MalformedDefinition {
                    path: path.to_owned(),
                    violation: "every entry must be a string".to_owned(),
                })
        })
        .collect()
}

fn property_keys(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_object)
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default()
}

fn parse_sort(value: Option<&Value>) -> Result<Vec<SortKey>, BaseQueryError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let entries = value.as_array().ok_or_else(|| BaseQueryError::MalformedDefinition {
        path: "sort".to_owned(),
        violation: "must be an array".to_owned(),
    })?;
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let path = format!("sort[{index}]");
            let entry = entry.as_object().ok_or_else(|| BaseQueryError::MalformedDefinition {
                path: path.clone(),
                violation: "sort entry must be an object".to_owned(),
            })?;
            let property =
                entry
                    .get("property")
                    .and_then(Value::as_str)
                    .ok_or_else(|| BaseQueryError::MalformedDefinition {
                        path: format!("{path}.property"),
                        violation: "sort property must be a string".to_owned(),
                    })?;
            validate_property_name(property, &format!("{path}.property"))?;
            let descending = match entry.get("direction").and_then(Value::as_str) {
                Some("DESC") => true,
                Some("ASC") | None => false,
                Some(_) => {
                    return Err(BaseQueryError::MalformedDefinition {
                        path: format!("{path}.direction"),
                        violation: "direction must be ASC or DESC".to_owned(),
                    });
                }
            };
            Ok(SortKey {
                property: property.to_owned(),
                descending,
            })
        })
        .collect()
}

fn parse_limit(value: Option<&Value>) -> Result<Option<usize>, BaseQueryError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let limit = value
        .as_u64()
        .and_then(|limit| usize::try_from(limit).ok())
        .filter(|limit| *limit > 0)
        .ok_or_else(|| BaseQueryError::MalformedDefinition {
            path: "limit".to_owned(),
            violation: "limit must be a positive integer".to_owned(),
        })?;
    Ok(Some(limit))
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
