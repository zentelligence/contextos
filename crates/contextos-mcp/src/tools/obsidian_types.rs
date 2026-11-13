//! DTOs, request/result shapes, and structured-path helpers for the
//! Obsidian domain tools in [`crate::tools::obsidian`]. Split out purely
//! to keep `obsidian.rs` under the project's file-size limit; every item
//! here is `pub(crate)` for that sibling module to use.

use contextos_core::{ContentHash, PipelineResult, VaultPath, VaultPathInput, VaultSet, WriteOutcome};
use contextos_obsidian::{
    BaseDiagnostic, BaseDocument, BaseError, BaseOperation, CanvasDiagnostic, CanvasDocument, CanvasError,
    CanvasOperation, FrontmatterDocument,
};
use rmcp::schemars;
use serde::{Deserialize, Serialize};

use crate::server::WarningMessages;
use crate::tool_error::ToolError;
use crate::tools::diagnostics::StructuredDiagnosticToolResult;

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct NoteValidationToolResult {
    warnings: Vec<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct NoteCreateToolResult {
    path: String,
    content_hash: String,
    validation: NoteValidationToolResult,
    warnings: Vec<String>,
}

impl From<PipelineResult<WriteOutcome>> for NoteCreateToolResult {
    fn from(value: PipelineResult<WriteOutcome>) -> Self {
        let hash: &str = (&value.value.content_hash).into();
        let WarningMessages(warnings) = WarningMessages::from(value.warnings);
        Self {
            path: value.value.path.portable_relative(),
            content_hash: hash.to_owned(),
            validation: NoteValidationToolResult { warnings: Vec::new() },
            warnings,
        }
    }
}

/// The input of `frontmatter_read`.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrontmatterReadToolInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name; use
    /// `{name}://.` to address that vault's root.
    pub(crate) path: String,
    /// Markdown notes only (`.md`; any other file is rejected). Return every link value in the frontmatter rewritten
    /// in place to the vault-rooted path it names, by the same rule as `fs_attach_file`'s `expand_links`: a bare
    /// name resolves from the note's own folder and its nearest ancestor first, so a bare `[[<name>.base]]` names the
    /// nearest base file of that name, whatever the name is. A link that names no file becomes `/:!` followed by its original text, and one that
    /// names several becomes `/:?` followed by its original text. The result reports `links_expanded` and no
    /// `content_hash`: the values are a reading of the note, never to be written back.
    #[serde(default)]
    pub(crate) expand_links: bool,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct FrontmatterReadToolResult {
    frontmatter: serde_json::Map<String, serde_json::Value>,
    body_start_line: usize,
    /// The hash of the file as read, for a later write's `expected_hash`; omitted when `expand_links` was set,
    /// because expanded values are not the file's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_hash: Option<String>,
    /// Present, and true, only when every link value was expanded to a vault-rooted path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    links_expanded: Option<bool>,
}

impl From<(FrontmatterDocument, ContentHash)> for FrontmatterReadToolResult {
    fn from(value: (FrontmatterDocument, ContentHash)) -> Self {
        let hash: &str = (&value.1).into();
        Self {
            frontmatter: value.0.frontmatter().clone(),
            body_start_line: value.0.body_start_line(),
            content_hash: Some(hash.to_owned()),
            links_expanded: None,
        }
    }
}

/// The frontmatter of a note read with every link expanded, which carries no hash.
pub(crate) struct ExpandedFrontmatter(pub(crate) FrontmatterDocument);

impl From<ExpandedFrontmatter> for FrontmatterReadToolResult {
    fn from(value: ExpandedFrontmatter) -> Self {
        Self {
            frontmatter: value.0.frontmatter().clone(),
            body_start_line: value.0.body_start_line(),
            content_hash: None,
            links_expanded: Some(true),
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct NoteUpdateToolResult {
    path: String,
    content_hash: String,
    warnings: Vec<String>,
}

impl From<PipelineResult<WriteOutcome>> for NoteUpdateToolResult {
    fn from(value: PipelineResult<WriteOutcome>) -> Self {
        let hash: &str = (&value.value.content_hash).into();
        let WarningMessages(warnings) = WarningMessages::from(value.warnings);
        Self {
            path: value.value.path.portable_relative(),
            content_hash: hash.to_owned(),
            warnings,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct FrontmatterUpdateToolResult {
    path: String,
    content_hash: String,
    warnings: Vec<String>,
}

impl From<BaseDiagnostic> for StructuredDiagnosticToolResult {
    fn from(value: BaseDiagnostic) -> Self {
        Self {
            code: value.code,
            path: value.path,
            message: value.message,
        }
    }
}

impl From<CanvasDiagnostic> for StructuredDiagnosticToolResult {
    fn from(value: CanvasDiagnostic) -> Self {
        Self {
            code: value.code,
            path: value.path,
            message: value.message,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct StructuredWriteToolResult {
    path: String,
    content_hash: String,
    warnings: Vec<String>,
}

impl From<PipelineResult<WriteOutcome>> for StructuredWriteToolResult {
    fn from(value: PipelineResult<WriteOutcome>) -> Self {
        let hash: &str = (&value.value.content_hash).into();
        let WarningMessages(warnings) = WarningMessages::from(value.warnings);
        Self {
            path: value.value.path.portable_relative(),
            content_hash: hash.to_owned(),
            warnings,
        }
    }
}

impl StructuredWriteToolResult {
    /// Appends further warnings raised by a secondary effect the caller
    /// carries out itself after the primary write (a `.base`'s paired
    /// sidecar, say), rather than one the write pipeline already routed.
    pub(crate) fn warnings_mut(&mut self) -> &mut Vec<String> {
        &mut self.warnings
    }
}

/// `base_create`'s result: the `.base` file's own outcome, plus the
/// vault-relative path of its paired sidecar and any warning raised while
/// creating or checking it (see [`crate::tools::base_sidecar`]).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct BaseCreateToolResult {
    path: String,
    content_hash: String,
    sidecar_path: String,
    warnings: Vec<String>,
}

impl BaseCreateToolResult {
    pub(crate) fn new(
        base_result: PipelineResult<WriteOutcome>,
        sidecar_path: String,
        sidecar_warnings: Vec<String>,
    ) -> Self {
        let hash: &str = (&base_result.value.content_hash).into();
        let WarningMessages(mut warnings) = WarningMessages::from(base_result.warnings);
        warnings.extend(sidecar_warnings);
        Self {
            path: base_result.value.path.portable_relative(),
            content_hash: hash.to_owned(),
            sidecar_path,
            warnings,
        }
    }
}

/// `base_record_create`'s result: the new record note's own outcome, the
/// key assigned to it, and the sidecar's own path plus any warnings raised
/// advancing it. `key` is left out, never `null`, when no key is assigned (a
/// `FILEPATH`-keyed base, or an event of a kind with no key field): the
/// advertised output schema types it as a string, and a client that checks
/// results against that schema refuses a `null`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct BaseRecordCreateToolResult {
    path: String,
    content_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    sidecar_path: String,
    warnings: Vec<String>,
}

impl From<crate::tools::base_record::RecordCreateOutcome> for BaseRecordCreateToolResult {
    fn from(value: crate::tools::base_record::RecordCreateOutcome) -> Self {
        let hash: &str = (&value.note_result.value.content_hash).into();
        let WarningMessages(mut warnings) = WarningMessages::from(value.note_result.warnings);
        warnings.extend(value.sidecar_warnings);
        Self {
            path: value.note_result.value.path.portable_relative(),
            content_hash: hash.to_owned(),
            key: value.key,
            sidecar_path: value.sidecar_path,
            warnings,
        }
    }
}

pub(crate) struct BaseReadSource {
    /// `Err` when the file fails to parse at all (e.g. malformed YAML); a
    /// document that parses but fails Bases schema rules is always `Ok`,
    /// with those violations surfacing through
    /// [`BaseDocument::diagnostics`] instead. Reporting the parse-failure
    /// case here as a diagnostic entry rather than propagating it as a
    /// tool error is what let `base_validate` retire: `base_read` is now a
    /// strict superset of what it reported.
    pub(crate) document: Result<BaseDocument, BaseError>,
    pub(crate) content_hash: ContentHash,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct BaseReadToolResult {
    definition: serde_json::Map<String, serde_json::Value>,
    /// The folder the base's own top-level filters confine its records to, read from `file.inFolder("...")` or
    /// `file.folder == "..."`: where `base_record_create` places a record when no folder is given, and the folder a
    /// link written relative to the base is read from. Omitted when the filters name no single folder below the
    /// vault root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    records_folder: Option<String>,
    /// Present with `records_folder`: true when records may sit in folders beneath it (`file.inFolder`), false when
    /// they sit directly in it (`file.folder ==`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    records_include_subfolders: Option<bool>,
    content_hash: String,
    diagnostics: Vec<StructuredDiagnosticToolResult>,
}

impl From<BaseReadSource> for BaseReadToolResult {
    fn from(value: BaseReadSource) -> Self {
        let hash: &str = (&value.content_hash).into();
        let (definition, folder, diagnostics) = match value.document {
            Ok(document) => (
                document.definition().clone(),
                contextos_obsidian::records_folder(document.definition().get("filters"))
                    .map(|folder| (folder.path.to_owned(), folder.includes_subfolders)),
                document
                    .diagnostics()
                    .into_iter()
                    .map(StructuredDiagnosticToolResult::from)
                    .collect(),
            ),
            Err(error) => (
                serde_json::Map::new(),
                None,
                vec![StructuredDiagnosticToolResult {
                    code: error.code().to_owned(),
                    path: "$".to_owned(),
                    message: error.to_string(),
                }],
            ),
        };
        let (records_folder, records_include_subfolders) = folder.unzip();
        Self {
            definition,
            records_folder,
            records_include_subfolders,
            content_hash: hash.to_owned(),
            diagnostics,
        }
    }
}

pub(crate) struct CanvasReadSource {
    /// See [`BaseReadSource::document`]; the same reasoning applies to
    /// `canvas_validate`'s retirement.
    pub(crate) document: Result<CanvasDocument, CanvasError>,
    pub(crate) content_hash: ContentHash,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct CanvasReadToolResult {
    nodes: Vec<serde_json::Map<String, serde_json::Value>>,
    edges: Vec<serde_json::Map<String, serde_json::Value>>,
    content_hash: String,
    diagnostics: Vec<StructuredDiagnosticToolResult>,
}

impl From<CanvasReadSource> for CanvasReadToolResult {
    fn from(value: CanvasReadSource) -> Self {
        let hash: &str = (&value.content_hash).into();
        let as_object_vec = |values: &[serde_json::Value]| {
            values
                .iter()
                .map(|value| value.as_object().cloned().unwrap_or_default())
                .collect()
        };
        let (nodes, edges, diagnostics) = match value.document {
            Ok(document) => (
                as_object_vec(document.nodes()),
                as_object_vec(document.edges()),
                document
                    .diagnostics()
                    .into_iter()
                    .map(StructuredDiagnosticToolResult::from)
                    .collect(),
            ),
            Err(error) => (
                Vec::new(),
                Vec::new(),
                vec![StructuredDiagnosticToolResult {
                    code: error.code().to_owned(),
                    path: "$".to_owned(),
                    message: error.to_string(),
                }],
            ),
        };
        Self {
            nodes,
            edges,
            content_hash: hash.to_owned(),
            diagnostics,
        }
    }
}

impl From<PipelineResult<WriteOutcome>> for FrontmatterUpdateToolResult {
    fn from(value: PipelineResult<WriteOutcome>) -> Self {
        let hash: &str = (&value.value.content_hash).into();
        let WarningMessages(warnings) = WarningMessages::from(value.warnings);
        Self {
            path: value.value.path.portable_relative(),
            content_hash: hash.to_owned(),
            warnings,
        }
    }
}

pub(crate) struct StructuredPathInput<'a> {
    pub(crate) roots: &'a VaultSet,
    pub(crate) raw: &'a str,
}

pub(crate) struct BaseVaultPath(pub(crate) VaultPath);

impl TryFrom<StructuredPathInput<'_>> for BaseVaultPath {
    type Error = ToolError;

    fn try_from(value: StructuredPathInput<'_>) -> Result<Self, Self::Error> {
        let path = VaultPath::try_from(VaultPathInput {
            roots: value.roots,
            raw: value.raw,
        })?;
        if path.relative().extension().and_then(std::ffi::OsStr::to_str) != Some("base") {
            return Err(ToolError::Invalid("Base path must end in .base"));
        }
        Ok(Self(path))
    }
}

pub(crate) struct CanvasVaultPath(pub(crate) VaultPath);

impl TryFrom<StructuredPathInput<'_>> for CanvasVaultPath {
    type Error = ToolError;

    fn try_from(value: StructuredPathInput<'_>) -> Result<Self, Self::Error> {
        let path = VaultPath::try_from(VaultPathInput {
            roots: value.roots,
            raw: value.raw,
        })?;
        if path.relative().extension().and_then(std::ffi::OsStr::to_str) != Some("canvas") {
            return Err(ToolError::Invalid("Canvas path must end in .canvas"));
        }
        Ok(Self(path))
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct NoteReferenceInput {
    pub(crate) target: String,
    pub(crate) summary: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct NoteCreateToolInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name.
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) frontmatter: serde_json::Map<String, serde_json::Value>,
    /// The note's body. Give this or `content_from`, not both.
    #[serde(default)]
    pub(crate) content: Option<String>,
    /// A vault file whose body the server copies into the note verbatim, so the content never passes through the
    /// caller. Give this or `content`, not both.
    #[serde(default)]
    pub(crate) content_from: Option<NoteContentSourceInput>,
    #[serde(default)]
    pub(crate) references: Vec<NoteReferenceInput>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct NoteContentSourceInput {
    /// The file to copy: vault-relative or absolute, or `{name}://{relative-path}` to address a specific configured
    /// vault by name. A Markdown (`.md`) source has its frontmatter left behind and its body copied; any other text
    /// file is copied whole.
    pub(crate) path: String,
    /// The source's `content_hash` as last read; a source that has changed since is refused.
    #[serde(default)]
    pub(crate) expected_hash: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrontmatterUpdateInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name.
    pub(crate) path: String,
    pub(crate) patch: serde_json::Map<String, serde_json::Value>,
    pub(crate) expected_hash: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct NoteUpdateToolInput {
    /// Vault-relative or absolute path of an existing `.md` note, or `{name}://{relative-path}` to address a specific
    /// configured vault by name.
    pub(crate) path: String,
    /// The `content_hash` of the version this update was made from; a note that has changed since is refused.
    pub(crate) expected_hash: String,
    /// RFC 7386 merge patch for the frontmatter: a value sets a field, `null` removes it. May not name `created` or
    /// `updated`, which the server keeps and stamps.
    #[serde(default)]
    pub(crate) patch: serde_json::Map<String, serde_json::Value>,
    /// Replacement body: everything after the frontmatter's closing `---` line, in the note's own line endings.
    /// Omit to keep the current body.
    #[serde(default)]
    pub(crate) body: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct BaseCreateToolInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name.
    pub(crate) path: String,
    pub(crate) definition: serde_json::Map<String, serde_json::Value>,
    /// Selects a real key sequence for this base's paired `<name>.md`
    /// sidecar (`key_format: "{key_prefix}-{key_id}"`) and makes it the
    /// default series, used when `base_record_create` is not told which to
    /// use; when this and `key_prefixes` are both omitted, the sidecar's
    /// `key_format` is the literal `FILEPATH`. A prefix is ASCII letters and
    /// digits, starting with a letter.
    pub(crate) key_prefix: Option<String>,
    /// Further key series the sidecar declares, each with its own
    /// independent counter starting at zero (for example `["D", "Q"]` beside
    /// a default `key_prefix` of `A`). May be used without `key_prefix`, in
    /// which case `base_record_create` must always be told which series to
    /// use. A prefix equal to `key_prefix` is declared once. Prefixes that
    /// differ only by case are rejected.
    pub(crate) key_prefixes: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct BaseApplyToolInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name.
    pub(crate) path: String,
    #[schemars(schema_with = "base_operations_schema")]
    pub(crate) operations: Vec<BaseOperationToolInput>,
    pub(crate) expected_hash: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct BaseRecordCreateToolInput {
    /// Vault-relative or absolute path to the `.base` file whose scope this
    /// record belongs to, or `{name}://{relative-path}` to address a
    /// specific configured vault by name.
    pub(crate) path: String,
    /// Same shape as `note_create`'s `frontmatter`. Setting `id` here is
    /// rejected when the base's own sidecar will assign one itself
    /// (any `key_format` other than `FILEPATH`) - omit it and read the
    /// assigned id back from this call's own `key` result field instead.
    /// For a `FILEPATH`-keyed base (no id of its own to assign), a
    /// caller-supplied `id` is accepted here as an ordinary optional
    /// field, not required. A caller-supplied id is checked against every
    /// other note already in this base's own scope, and the call is
    /// refused if it already exists elsewhere. A server-assigned key that a
    /// note written outside this tool already holds is never refused:
    /// the series counter is caught up to the highest key of that series
    /// in scope and the next free key is assigned, with a warning.
    #[serde(default)]
    pub(crate) frontmatter: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub(crate) content: String,
    /// Sub-folder relative to the folder the `.base`'s own top-level filters
    /// confine its records to (`file.inFolder("...")` or `file.folder == "..."`);
    /// a leading `/` makes it vault-root-relative instead, ignoring that folder
    /// entirely. Required (as an absolute `/`-prefixed path) when the filters
    /// name no folder.
    pub(crate) folder: Option<String>,
    /// Explicit filename (must end in `.md`); defaults to the assigned
    /// key lower-cased, and is required when the base has no key scheme
    /// (`key_format: FILEPATH`).
    pub(crate) filename: Option<String>,
    /// Which of the sidecar's declared key series to assign the id from,
    /// for example `D` for a decision beside a default `A` for an action.
    /// Each series keeps its own counter. Omit it to use the sidecar's
    /// default `key_prefix`; a base whose sidecar declares no default series
    /// requires it. Rejected (`io/invalid-key-prefix`) when the
    /// sidecar has not declared the prefix, and when the base is
    /// `FILEPATH`-keyed and so assigns no ids. Matching is case-sensitive.
    pub(crate) prefix: Option<String>,
    /// Frontmatter fields (1 to 8 names) whose values together identify this record, for example `["origin"]`. When
    /// a record in the base's scope already holds the same value for every named field, the call is refused with
    /// `io/duplicate-record` naming that record's id and path, and nothing is written. Each named field must be
    /// present in `frontmatter` with a string, number, or boolean value. Checked inside the vault's write lock, so of
    /// two processes raising the same record at once exactly one succeeds.
    pub(crate) unique_fields: Option<Vec<String>>,
}

/// Renders as an empty object schema (`{}`) rather than schemars' default bare
/// `true`, since some MCP clients reject boolean sub-schemas in tool schemas.
/// Both forms mean "any JSON value is valid" per the JSON Schema spec.
fn any_json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::Schema::from(serde_json::Map::new())
}

/// Converts a hand-written `serde_json::json!` object literal into a
/// `schemars::Schema`: `Schema` only implements `From` for
/// `serde_json::Map`, not the `Value` the macro produces, and every
/// caller here always passes an object literal, never any other JSON
/// value shape. Falls back to the empty (any-value) schema in the
/// unreachable non-object case rather than panicking.
fn object_schema(value: serde_json::Value) -> schemars::Schema {
    match value {
        serde_json::Value::Object(map) => schemars::Schema::from(map),
        _ => schemars::Schema::from(serde_json::Map::new()),
    }
}

/// Replaces `BaseOperationToolInput`'s default derived schema, a `oneOf`
/// of eight exact per-variant shapes, with one flat object schema: an
/// `op` discriminator plus every variant's fields declared optional.
/// Confirmed live (0.7.1): a root-level `oneOf` alone, with no null
/// branch involved, broke Cowork's whole per-task tool registry; every
/// other tool in this catalogue's advertised schema is already free of
/// `oneOf`/`anyOf`/`allOf` (`sanitise_nullable_unions`, `server.rs`), and
/// this was the one deliberate holdout because it is a genuine
/// discriminated choice, not a nullability shape that transform can
/// collapse. `BaseOperationToolInput` itself, and its `deny_unknown_fields`
/// serde tag matching, are unchanged: a caller supplying a field that does
/// not belong to the given `op`, or omitting one that operation actually
/// requires, is still rejected at deserialization; only the advertised
/// schema's own precision is traded for a shape every MCP client is
/// confirmed to tolerate.
fn base_operations_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    object_schema(serde_json::json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "op": {
                    "type": "string",
                    "enum": [
                        "set_filters", "add_formula", "remove_formula", "set_property",
                        "remove_property", "add_view", "remove_view", "update_view",
                        "set_summary", "remove_summary"
                    ],
                    "description": "Which Base operation this entry performs; determines \
                        which of the other fields apply and are required."
                },
                "filters": {
                    "description": "set_filters only: the Base's replacement filter \
                        definition, any JSON value."
                },
                "name": {
                    "type": "string",
                    "description": "add_formula, remove_formula, set_property, \
                        remove_property, add_view, remove_view, update_view, set_summary, \
                        remove_summary: the target formula, property, or view's name."
                },
                "expression": {
                    "type": "string",
                    "description": "add_formula, set_summary only: the formula expression."
                },
                "property": {
                    "type": "object",
                    "additionalProperties": true,
                    "description": "set_property only: the property definition."
                },
                "view": {
                    "type": "object",
                    "additionalProperties": true,
                    "description": "add_view only: the new view's definition."
                },
                "patch": {
                    "type": "object",
                    "additionalProperties": true,
                    "description": "update_view only: fields to merge into the existing view."
                }
            },
            "required": ["op"],
            "additionalProperties": false
        }
    }))
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum BaseOperationToolInput {
    SetFilters {
        #[schemars(schema_with = "any_json_schema")]
        filters: serde_json::Value,
    },
    AddFormula {
        name: String,
        expression: String,
    },
    RemoveFormula {
        name: String,
    },
    SetProperty {
        name: String,
        property: serde_json::Map<String, serde_json::Value>,
    },
    RemoveProperty {
        name: String,
    },
    AddView {
        name: String,
        view: serde_json::Map<String, serde_json::Value>,
    },
    RemoveView {
        name: String,
    },
    UpdateView {
        name: String,
        patch: serde_json::Map<String, serde_json::Value>,
    },
    SetSummary {
        name: String,
        expression: String,
    },
    RemoveSummary {
        name: String,
    },
}

impl From<BaseOperationToolInput> for BaseOperation {
    fn from(value: BaseOperationToolInput) -> Self {
        match value {
            BaseOperationToolInput::SetFilters { filters } => Self::SetFilters { filters },
            BaseOperationToolInput::AddFormula { name, expression } => Self::AddFormula { name, expression },
            BaseOperationToolInput::RemoveFormula { name } => Self::RemoveFormula { name },
            BaseOperationToolInput::SetProperty { name, property } => Self::SetProperty {
                name,
                definition: property,
            },
            BaseOperationToolInput::RemoveProperty { name } => Self::RemoveProperty { name },
            BaseOperationToolInput::AddView { name, view } => Self::AddView { name, definition: view },
            BaseOperationToolInput::RemoveView { name } => Self::RemoveView { name },
            BaseOperationToolInput::UpdateView { name, patch } => Self::UpdateView { name, patch },
            BaseOperationToolInput::SetSummary { name, expression } => Self::SetSummary { name, expression },
            BaseOperationToolInput::RemoveSummary { name } => Self::RemoveSummary { name },
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CanvasCreateToolInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name.
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) nodes: Vec<serde_json::Map<String, serde_json::Value>>,
    #[serde(default)]
    pub(crate) edges: Vec<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CanvasApplyToolInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name.
    pub(crate) path: String,
    #[schemars(schema_with = "canvas_operations_schema")]
    pub(crate) operations: Vec<CanvasOperationToolInput>,
    pub(crate) expected_hash: Option<String>,
}

/// [`base_operations_schema`]'s counterpart for `CanvasOperationToolInput`:
/// same rationale, same `oneOf`-avoidance, same unchanged runtime
/// validation via `CanvasOperationToolInput`'s own `deny_unknown_fields`
/// serde tag matching.
fn canvas_operations_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    object_schema(serde_json::json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "op": {
                    "type": "string",
                    "enum": [
                        "add_node", "update_node", "remove_node",
                        "add_edge", "update_edge", "remove_edge", "group"
                    ],
                    "description": "Which Canvas operation this entry performs; determines \
                        which of the other fields apply and are required."
                },
                "node": {
                    "type": "object",
                    "additionalProperties": true,
                    "description": "add_node only: the new node's definition."
                },
                "edge": {
                    "type": "object",
                    "additionalProperties": true,
                    "description": "add_edge only: the new edge's definition."
                },
                "id": {
                    "type": "string",
                    "description": "update_node, remove_node, update_edge, remove_edge: the \
                        target node or edge's id."
                },
                "patch": {
                    "type": "object",
                    "additionalProperties": true,
                    "description": "update_node, update_edge: fields to merge into the \
                        existing node or edge."
                },
                "group": {
                    "type": "object",
                    "additionalProperties": true,
                    "description": "group only: the new group node's definition."
                },
                "members": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "group only: ids of the nodes this group contains."
                }
            },
            "required": ["op"],
            "additionalProperties": false
        }
    }))
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum CanvasOperationToolInput {
    AddNode {
        node: serde_json::Map<String, serde_json::Value>,
    },
    UpdateNode {
        id: String,
        patch: serde_json::Map<String, serde_json::Value>,
    },
    RemoveNode {
        id: String,
    },
    AddEdge {
        edge: serde_json::Map<String, serde_json::Value>,
    },
    UpdateEdge {
        id: String,
        patch: serde_json::Map<String, serde_json::Value>,
    },
    RemoveEdge {
        id: String,
    },
    Group {
        group: serde_json::Map<String, serde_json::Value>,
        members: Vec<String>,
    },
}

impl From<CanvasOperationToolInput> for CanvasOperation {
    fn from(value: CanvasOperationToolInput) -> Self {
        match value {
            CanvasOperationToolInput::AddNode { node } => Self::AddNode { node },
            CanvasOperationToolInput::UpdateNode { id, patch } => Self::UpdateNode { id, patch },
            CanvasOperationToolInput::RemoveNode { id } => Self::RemoveNode { id },
            CanvasOperationToolInput::AddEdge { edge } => Self::AddEdge { edge },
            CanvasOperationToolInput::UpdateEdge { id, patch } => Self::UpdateEdge { id, patch },
            CanvasOperationToolInput::RemoveEdge { id } => Self::RemoveEdge { id },
            CanvasOperationToolInput::Group { group, members } => Self::Group { group, members },
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum LinkDirectionInput {
    #[default]
    Out,
    In,
    Both,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct LinksReadInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name.
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) direction: LinkDirectionInput,
}

/// `base_query` accepts exactly one of `path` (an existing `.base` file) or
/// `definition` (an inline ad hoc definition); `vault` selects which
/// configured vault to scan and is only consulted for `definition`, since
/// `path` already unambiguously names a vault.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct BaseQueryToolInput {
    /// Existing `.base` file to query: vault-relative or absolute, or
    /// `{name}://{relative-path}`. Exactly one of `path` or
    /// `definition` is required.
    pub(crate) path: Option<String>,
    /// An inline query, given instead of `path`: one view, with its keys at the top level. `filters` selects the
    /// notes, `order` lists the columns (or `properties` names them when `order` is omitted), `sort` and `limit`
    /// shape the rows, and `formulas` defines any `formula.*` column. It is not the shape of a `.base` file: a
    /// `views` list, or any other key, is refused with `query/base-query-malformed` rather than ignored. For
    /// example `{"filters": "file.inFolder(\"memory/events\")", "order": ["file.name", "kind"]}`.
    pub(crate) definition: Option<serde_json::Map<String, serde_json::Value>>,
    /// The view of the `.base` file at `path` to run; omit it to run the first view. Requires `path`: an inline
    /// `definition` is itself one view, so `view` alongside it is refused with `io/invalid-argument`.
    pub(crate) view: Option<String>,
    /// The vault to scan when using `definition` (ignored for `path`,
    /// which already names a vault): `{name}://.` to select a specific
    /// configured vault by name (a bare vault name alone is not
    /// accepted for this parameter); omit to use the sole configured
    /// vault.
    pub(crate) vault: Option<String>,
    /// Output format for a view or definition query: `table` (default), `json`, or `csv`. Not accepted with `ids`.
    pub(crate) format: Option<BaseQueryFormatInput>,
    pub(crate) limit: Option<u64>,
    /// Record ids to look up within the base's own scope (its top-level `filters`, never a view's), for example
    /// `["A-001", "D-002"]`. Each record found is returned in `records`, in the order asked, with its path,
    /// frontmatter, and content hash (usable as a later write's `expected_hash`); an id with no record in scope is
    /// listed in `not_found`. Requires `path`; not accepted with `view`, `definition`, `limit`, or `format`. 1 to 100
    /// distinct, non-empty ids.
    pub(crate) ids: Option<Vec<String>>,
    /// With `ids`, also return each record's body below its frontmatter as `content`. Default `false`.
    pub(crate) include_content: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum BaseQueryFormatInput {
    #[default]
    Table,
    Json,
    Csv,
}

impl From<BaseQueryFormatInput> for contextos_obsidian::QueryFormat {
    fn from(value: BaseQueryFormatInput) -> Self {
        match value {
            BaseQueryFormatInput::Table => Self::Table,
            BaseQueryFormatInput::Json => Self::Json,
            BaseQueryFormatInput::Csv => Self::Csv,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct BaseQueryToolResult {
    pub(crate) content: String,
    pub(crate) columns: Vec<String>,
    pub(crate) matched: usize,
    pub(crate) truncated: bool,
    pub(crate) diagnostics: Vec<StructuredDiagnosticToolResult>,
    /// Present only for an `ids` lookup: every record found, in the order its id was asked for. An id held by more
    /// than one record in scope returns each of them, with a `query/base-query-duplicate-id` diagnostic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) records: Option<Vec<BaseQueryRecordToolResult>>,
    /// Present only for an `ids` lookup: the ids asked for that no record in the base's scope holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) not_found: Option<Vec<String>>,
}

/// One record an `ids` lookup found.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct BaseQueryRecordToolResult {
    pub(crate) id: String,
    /// Vault-relative path of the record's note.
    pub(crate) path: String,
    /// SHA-256 of the note as read, for a later write's `expected_hash`.
    pub(crate) content_hash: String,
    pub(crate) frontmatter: serde_json::Map<String, serde_json::Value>,
    /// The body below the frontmatter, only when `include_content` was set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) content: Option<String>,
}

impl From<crate::tools::base_query::FoundRecord> for BaseQueryRecordToolResult {
    fn from(value: crate::tools::base_query::FoundRecord) -> Self {
        Self {
            id: value.id,
            path: value.path,
            content_hash: value.content_hash,
            frontmatter: value.frontmatter,
            content: value.content,
        }
    }
}
