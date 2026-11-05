//! Inputs and result shapes shared by `query_text`, `query_semantic`, and
//! `query_blended`: the paging, sorting, relevance, and note type controls,
//! and the per-hit note metadata.
//!
//! Every schema here follows the flat-schema rules the whole catalogue
//! obeys: enumerations are plain strings, optional output fields are
//! omitted rather than serialised as `null`, and nothing relies on
//! `oneOf`/`anyOf`.

use contextos_search::{HitMetadata, MinRelevance, ResultControls, SearchMode, SortKey, SortOrder, SortSpec};
use rmcp::schemars;
use serde::{Deserialize, Serialize};

use crate::resource_support::optional_string_schema;
use crate::tool_error::ToolError;

/// The property results are ordered by: `relevance` (default), `title`, or
/// `modified` (file modification time).
#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SortKeyInput {
    Relevance,
    Title,
    Modified,
}

impl From<SortKeyInput> for SortKey {
    fn from(value: SortKeyInput) -> Self {
        match value {
            SortKeyInput::Relevance => Self::Relevance,
            SortKeyInput::Title => Self::Title,
            SortKeyInput::Modified => Self::Modified,
        }
    }
}

/// The direction results are ordered in: `asc` or `desc`. Defaults to the
/// natural direction for the sort key (most relevant and most recent first,
/// titles alphabetical).
#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SortOrderInput {
    Asc,
    Desc,
}

impl From<SortOrderInput> for SortOrder {
    fn from(value: SortOrderInput) -> Self {
        match value {
            SortOrderInput::Asc => Self::Ascending,
            SortOrderInput::Desc => Self::Descending,
        }
    }
}

/// Which indexes a blended search consults: `blended` (default, both,
/// fused by rank), `text`, or `semantic`.
#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SearchModeInput {
    Blended,
    Text,
    Semantic,
}

impl From<SearchModeInput> for SearchMode {
    fn from(value: SearchModeInput) -> Self {
        match value {
            SearchModeInput::Blended => Self::Blended,
            SearchModeInput::Text => Self::Text,
            SearchModeInput::Semantic => Self::Semantic,
        }
    }
}

/// The control arguments as they arrive from a tool call, before
/// validation.
pub(crate) struct ControlsArgs<'a> {
    pub(crate) offset: u64,
    pub(crate) sort: Option<SortKeyInput>,
    pub(crate) order: Option<SortOrderInput>,
    pub(crate) min_relevance: Option<f64>,
    pub(crate) note_type: Option<&'a str>,
}

impl<'a> TryFrom<ControlsArgs<'a>> for ResultControls<'a> {
    type Error = ToolError;

    /// An offset too large for the platform is passed on as the largest
    /// representable one, so it fails the same page-bounds check as any
    /// other out-of-range offset instead of a generic conversion error.
    fn try_from(value: ControlsArgs<'a>) -> Result<Self, Self::Error> {
        let min_relevance = match value.min_relevance {
            Some(threshold) => MinRelevance::try_from(threshold)?,
            None => MinRelevance::default(),
        };
        let sort = value.sort.map_or_else(SortSpec::default, |key| {
            SortSpec::from((SortKey::from(key), value.order.map(SortOrder::from)))
        });
        Ok(Self {
            note_type: value.note_type,
            offset: usize::try_from(value.offset).unwrap_or(usize::MAX),
            sort,
            min_relevance,
        })
    }
}

/// A note's descriptive frontmatter fields. Each is omitted, never `null`,
/// when the note does not declare it.
#[derive(Debug, Default, Serialize, schemars::JsonSchema)]
pub(crate) struct HitMetadataToolResult {
    // `#[serde(default)]` alongside `schema_with`: the schema macro derives
    // optionality from the overridden subschema, which a bare function's
    // return type does not mark as an `Option`. Without it the field would
    // be advertised as required though it is genuinely omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "optional_string_schema")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "optional_string_schema")]
    summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "optional_string_schema")]
    excerpt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "optional_string_schema")]
    status: Option<String>,
    /// The frontmatter `updated` value exactly as written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "optional_string_schema")]
    updated: Option<String>,
    /// The frontmatter `type` value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "optional_string_schema")]
    note_type: Option<String>,
}

impl From<&HitMetadata> for HitMetadataToolResult {
    fn from(value: &HitMetadata) -> Self {
        Self {
            description: value.description.clone(),
            summary: value.summary.clone(),
            excerpt: value.excerpt.clone(),
            status: value.status.clone(),
            updated: value.updated.clone(),
            note_type: value.note_type.clone(),
        }
    }
}
