#![forbid(unsafe_code)]

mod base;
mod base_link;
mod base_link_fields;
mod base_query;
mod base_series_templates;
mod base_sidecar;
mod canvas;
mod canvas_expansion;
mod declared_base;
mod frontmatter_links;
mod frontmatter_span;
mod link_blocks;
mod link_expansion;
mod link_resolution;
mod link_scan;
mod markdown;
mod note_update;
mod series_template;
mod vault_files;

use serde_json::{Map, Value};
use thiserror::Error;

pub use base::{BaseDiagnostic, BaseDocument, BaseError, BaseOperation};
pub use base_link::{BASE_PROPERTY, BaseLink, BaseLinkInput, BaseLinkTarget};
pub use base_link_fields::LinkField;
pub use base_query::{
    BaseQueryError, ColumnValue, FileMetadata, QueryDefinition, QueryFormat, RecordsFolder, RowContext, ScanRootHint,
    SortKey, compare_values, evaluate_filters, records_folder, render, resolve_column, resolve_property, resolve_row,
    scan_root_hint, validate_filters,
};
pub use base_series_templates::{SeriesTemplates, TemplateLink};
pub use base_sidecar::{BaseKeyScheme, BaseSidecar, BaseSidecarError, BaseSidecarInput, BaseSidecarPatch, KeySeries};
pub use canvas::{CanvasCreateInput, CanvasDiagnostic, CanvasDocument, CanvasError, CanvasOperation};
pub use canvas_expansion::{CanvasExpansionInput, ExpandedCanvas};
pub use declared_base::{BaseRecords, DeclaredBase, NoDeclaredBase, NoRecordsFolder};
pub use frontmatter_links::{LinkEdit, LinkError, LinkOutcome, LinkTarget, WikiLink, WikiLinkInput};
pub use frontmatter_span::FrontmatterSpan;
pub use link_expansion::{AMBIGUOUS_MARKER, ExpandedMarkdown, LinkExpansionInput, NOT_FOUND_MARKER};
pub use link_resolution::{ExpansionError, LinkResolution, LinkTargets, resolve_link, resolve_link_narrowed};
pub use markdown::{LinkCollection, LinkForm, MarkdownError, ObsidianLink, ValidatedMarkdown, is_placeholder_target};
pub use note_update::{NoteUpdate, NoteUpdateInput};
pub use series_template::{FixedFields, SeriesTemplateError, SeriesTemplateInput};
pub use vault_files::VaultFiles;

/// Inputs for one validated Obsidian note document.
#[derive(Clone, Debug, PartialEq)]
pub struct NoteCreateInput<'a> {
    pub title: &'a str,
    pub frontmatter: Map<String, Value>,
    pub content: &'a str,
    pub timestamp: &'a str,
    /// The `entity` the note's location in its vault calls for; `None` writes no `entity` default. A caller-supplied
    /// `entity` in `frontmatter` still takes precedence.
    pub entity: Option<&'a str>,
}

/// A validated note with resolved, ordered default frontmatter.
#[derive(Clone, Debug, PartialEq)]
pub struct NoteDocument(FrontmatterDocument);

impl TryFrom<NoteCreateInput<'_>> for NoteDocument {
    type Error = NoteError;

    fn try_from(value: NoteCreateInput<'_>) -> Result<Self, Self::Error> {
        if value.timestamp.trim().is_empty() {
            return Err(NoteError::EmptyTimestamp);
        }
        ValidatedMarkdown::try_from(value.content)?;
        let frontmatter = DefaultFrontmatter::try_from(DefaultFrontmatterInput {
            kind: "note",
            title: value.title,
            timestamp: value.timestamp,
            entity: value.entity,
            supplied: value.frontmatter,
        })?;
        Ok(Self(FrontmatterDocument {
            frontmatter: frontmatter.into(),
            body: value.content.to_owned(),
            body_start_line: 1,
            byte_order_mark: false,
        }))
    }
}

/// Inputs for resolving a note-like document's default frontmatter fields,
/// independent of body content validation. Shared by [`NoteDocument`]
/// construction and by any other document (a managed `index.md`, a
/// `base_record_create` record) that needs the same default field set and
/// title-distinctness guard without also running [`ValidatedMarkdown`]
/// against content it does not own.
#[derive(Clone, Debug, PartialEq)]
pub struct DefaultFrontmatterInput<'a> {
    pub kind: &'a str,
    pub title: &'a str,
    pub timestamp: &'a str,
    /// The `entity` derived from the document's location; `None` leaves the property out rather than guessing one.
    pub entity: Option<&'a str>,
    pub supplied: Map<String, Value>,
}

/// Resolved default frontmatter: the standard field set, with the caller's
/// own properties merged in, validated for title-distinctness.
#[derive(Clone, Debug, PartialEq)]
pub struct DefaultFrontmatter(Map<String, Value>);

impl TryFrom<DefaultFrontmatterInput<'_>> for DefaultFrontmatter {
    type Error = NoteError;

    fn try_from(value: DefaultFrontmatterInput<'_>) -> Result<Self, Self::Error> {
        if value.title.trim().is_empty() {
            return Err(NoteError::EmptyTitle);
        }
        let mut frontmatter = Map::new();
        frontmatter.insert("type".to_owned(), Value::String(value.kind.to_owned()));
        frontmatter.insert("title".to_owned(), Value::String(value.title.to_owned()));
        if let Some(entity) = value.entity {
            frontmatter.insert("entity".to_owned(), Value::String(entity.to_owned()));
        }
        frontmatter.insert("status".to_owned(), Value::String("new".to_owned()));
        frontmatter.insert("created".to_owned(), Value::String(value.timestamp.to_owned()));
        frontmatter.insert("updated".to_owned(), Value::String(value.timestamp.to_owned()));
        frontmatter.insert("tags".to_owned(), Value::Array(Vec::new()));
        frontmatter.insert("aliases".to_owned(), Value::Array(Vec::new()));
        frontmatter.insert("description".to_owned(), Value::String(String::new()));
        frontmatter.insert("summary".to_owned(), Value::String(String::new()));
        frontmatter.insert("excerpt".to_owned(), Value::String(String::new()));
        for (key, property) in value.supplied {
            frontmatter.insert(key, property);
        }
        for field in ["description", "summary", "excerpt"] {
            if frontmatter.get(field).and_then(Value::as_str) == Some(value.title) {
                return Err(NoteError::TitleCollision { field });
            }
        }
        Ok(Self(frontmatter))
    }
}

impl From<DefaultFrontmatter> for Map<String, Value> {
    fn from(value: DefaultFrontmatter) -> Self {
        value.0
    }
}

impl TryFrom<&DefaultFrontmatter> for String {
    type Error = FrontmatterError;

    /// Renders just the frontmatter block text (`---\n...\n---\n`), with no
    /// body, so a caller building a different kind of document (a managed
    /// `index.md`, say) can prepend it without constructing a
    /// [`FrontmatterDocument`] of its own.
    fn try_from(value: &DefaultFrontmatter) -> Result<Self, Self::Error> {
        let yaml = yaml_serde::to_string(&value.0).map_err(|source| FrontmatterError::Serialise { source })?;
        let mut rendered = String::with_capacity(yaml.len().saturating_add(8));
        rendered.push_str("---\n");
        rendered.push_str(&yaml);
        rendered.push_str("---\n");
        Ok(rendered)
    }
}

impl TryFrom<NoteDocument> for String {
    type Error = FrontmatterError;

    fn try_from(value: NoteDocument) -> Result<Self, Self::Error> {
        Self::try_from(&value.0)
    }
}

/// Typed note construction failures.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum NoteError {
    #[error("note title must not be empty")]
    EmptyTitle,
    #[error("note title must be text")]
    TitleNotText,
    #[error("note timestamp must not be empty")]
    EmptyTimestamp,
    /// `field` is `description`, `summary`, or `excerpt`: whichever
    /// duplicated `title` exactly, which would otherwise let a title-derived
    /// value silently reappear as low-value, confusing summary content.
    #[error("note {field} must not be identical to its title")]
    TitleCollision { field: &'static str },
    /// `field` is `created` or `updated`, which the server maintains: an update keeps the first and stamps the second.
    #[error("note {field} is maintained by the server and cannot be set or removed by an update")]
    ManagedField { field: &'static str },
    #[error(transparent)]
    Markdown(#[from] MarkdownError),
}

impl NoteError {
    /// Returns the stable machine-readable error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Markdown(error) => error.code(),
            Self::EmptyTitle | Self::TitleNotText | Self::EmptyTimestamp | Self::TitleCollision { .. } => "format/note",
            Self::ManagedField { .. } => "format/note-managed-field",
        }
    }

    /// Returns an actionable correction for the malformed note.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        match self {
            Self::Markdown(error) => error.remediation(),
            Self::EmptyTitle | Self::TitleNotText | Self::EmptyTimestamp => {
                "Correct the note title or Obsidian Markdown syntax, and retry."
            }
            Self::TitleCollision { .. } => {
                "Give the note's description, summary, or excerpt a value distinct from its title, and retry."
            }
            Self::ManagedField { .. } => {
                "Leave created and updated out of the patch: created is kept as it is and updated is stamped on every update."
            }
        }
    }
}

/// An Obsidian note split into ordered JSON frontmatter and untouched body.
#[derive(Clone, Debug, PartialEq)]
pub struct FrontmatterDocument {
    frontmatter: Map<String, Value>,
    body: String,
    body_start_line: usize,
    /// Whether the note opens with a UTF-8 byte order mark, which is set aside so the frontmatter after it is still
    /// recognised, and written back.
    byte_order_mark: bool,
}

/// Inputs for building a frontmatter document directly from already-known
/// parts, rather than parsing one: for a file this crate constructs fresh
/// (a `.base` sidecar, say) whose frontmatter fields are not the standard
/// note-default set and so should not gain a generic `updated` stamp or any
/// other note-specific default.
#[derive(Clone, Debug, PartialEq)]
pub struct FrontmatterInput {
    pub frontmatter: Map<String, Value>,
    pub body: String,
}

impl From<FrontmatterInput> for FrontmatterDocument {
    fn from(value: FrontmatterInput) -> Self {
        Self {
            frontmatter: value.frontmatter,
            body: value.body,
            body_start_line: 1,
            byte_order_mark: false,
        }
    }
}

impl TryFrom<&str> for FrontmatterDocument {
    type Error = FrontmatterError;

    fn try_from(source: &str) -> Result<Self, Self::Error> {
        let byte_order_mark = source.starts_with('\u{feff}');
        match FrontmatterSpan::from(source) {
            FrontmatterSpan::Absent => Ok(Self {
                frontmatter: Map::new(),
                body: source.strip_prefix('\u{feff}').unwrap_or(source).to_owned(),
                body_start_line: 1,
                byte_order_mark,
            }),
            FrontmatterSpan::Unclosed => Err(FrontmatterError::Unclosed { line: 1 }),
            FrontmatterSpan::Closed {
                yaml,
                body_start,
                body_start_line,
            } => {
                let yaml = source.get(yaml).unwrap_or_default();
                let frontmatter = yaml_serde::from_str::<Map<String, Value>>(yaml).map_err(|source| {
                    let (line, column) = source.location().map_or((2, 1), |location| {
                        (location.line().saturating_add(1), location.column())
                    });
                    FrontmatterError::InvalidYaml { line, column, source }
                })?;
                Ok(Self {
                    frontmatter,
                    body: source.get(body_start..).unwrap_or_default().to_owned(),
                    body_start_line,
                    byte_order_mark,
                })
            }
        }
    }
}

impl TryFrom<String> for FrontmatterDocument {
    type Error = FrontmatterError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

impl FrontmatterDocument {
    /// Returns frontmatter properties in their source order.
    #[must_use]
    pub const fn frontmatter(&self) -> &Map<String, Value> {
        &self.frontmatter
    }

    /// Returns the body byte-for-byte from the source note.
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }

    /// Returns the 1-based source line at which the body begins.
    #[must_use]
    pub const fn body_start_line(&self) -> usize {
        self.body_start_line
    }

    /// Applies RFC 7386 object semantics and stamps `updated` when omitted.
    pub fn apply_merge_patch(&mut self, patch: Map<String, Value>, updated: &str) {
        let explicitly_updates_timestamp = patch.contains_key("updated");
        apply_object_patch(&mut self.frontmatter, patch);
        if !explicitly_updates_timestamp {
            self.frontmatter
                .insert("updated".to_owned(), Value::String(updated.to_owned()));
        }
    }

    /// Applies an RFC 7386 merge patch to frontmatter only, with no side
    /// effect on any other field: for a caller (a `.base` sidecar's own
    /// `last_update`, say) that manages its own timestamp-shaped property
    /// and must not also gain the generic `updated` stamp
    /// [`Self::apply_merge_patch`] adds for an ordinary note.
    pub fn merge_frontmatter(&mut self, patch: Map<String, Value>) {
        apply_object_patch(&mut self.frontmatter, patch);
    }
}

impl TryFrom<&FrontmatterDocument> for String {
    type Error = FrontmatterError;

    fn try_from(value: &FrontmatterDocument) -> Result<Self, Self::Error> {
        let yaml =
            yaml_serde::to_string(&value.frontmatter).map_err(|source| FrontmatterError::Serialise { source })?;
        let mut rendered = String::from(if value.byte_order_mark { "\u{feff}" } else { "" });
        rendered.push_str("---\n");
        rendered.push_str(&yaml);
        rendered.push_str("---\n");
        rendered.push_str(&value.body);
        Ok(rendered)
    }
}

fn apply_object_patch(target: &mut Map<String, Value>, patch: Map<String, Value>) {
    for (key, patch_value) in patch {
        match patch_value {
            Value::Null => {
                target.shift_remove(&key);
            }
            Value::Object(nested_patch) => {
                let target_value = target.entry(key).or_insert_with(|| Value::Object(Map::new()));
                if !target_value.is_object() {
                    *target_value = Value::Object(Map::new());
                }
                if let Some(target_object) = target_value.as_object_mut() {
                    apply_object_patch(target_object, nested_patch);
                }
            }
            replacement => {
                target.insert(key, replacement);
            }
        }
    }
}

/// Typed frontmatter codec failures.
#[derive(Debug, Error)]
pub enum FrontmatterError {
    #[error("YAML frontmatter opened on line {line} but has no closing delimiter")]
    Unclosed { line: usize },
    #[error("YAML frontmatter is invalid at line {line}, column {column}")]
    InvalidYaml {
        line: usize,
        column: usize,
        #[source]
        source: yaml_serde::Error,
    },
    #[error("frontmatter could not be serialised as YAML")]
    Serialise {
        #[source]
        source: yaml_serde::Error,
    },
}

impl FrontmatterError {
    /// Returns the stable machine-readable error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        "format/frontmatter"
    }

    /// Returns an actionable correction for the malformed note.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        "Correct the YAML frontmatter without changing the note body."
    }
}
