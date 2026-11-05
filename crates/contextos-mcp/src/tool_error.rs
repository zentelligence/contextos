//! Shared tool-call error taxonomy and dispatch helpers. Every tool-router
//! module (`tools/*.rs`) maps its domain errors into [`ToolError`] and
//! propagates them through [`execute`]/[`evaluate`]/[`execute_value`], so
//! there is exactly one place translating internal error types into the
//! stable, machine-readable MCP error codes and remediation text a caller
//! sees (`ToolFailure`).

use contextos_core::PathError;
use contextos_ephemeris::EphemerisError;
use contextos_fs::FsError;
use contextos_git::{GitError, GitWriteError};
use contextos_index::IndexServiceError;
use contextos_obsidian::{
    BaseError, BaseQueryError, BaseSidecarError, CanvasError, FrontmatterError, LinkError, MarkdownError, NoteError,
};
use contextos_oplog::OperationLogError;
use contextos_search::SearchError;
use rmcp::handler::server::tool::IntoCallToolResult;
use rmcp::handler::server::wrapper::Json;
use rmcp::{ErrorData, schemars};
use serde::Serialize;
use thiserror::Error;

use crate::resource_support::ResourceError;

const FIXED_FIELD_CONFLICT_REMEDIATION: &str = "Omit each listed field to have the template's value stamped, or create the \
     record in the series whose template fixes the value you want (prefix), and retry.";

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct ToolFailure {
    pub(crate) code: String,
    pub(crate) message: String,
    // `#[serde(default)]` is required alongside `schema_with` here: see
    // `IndexesStatusToolResult::state_directory`'s identical comment
    // (`tools/index_status.rs`) for why, and `optional_u64_schema`'s own
    // existing callers (`tools/query.rs`, `tools/fs_types.rs`) for the
    // established precedent. Currently masked in practice by
    // `fallible_output_schema_for` stripping `required` entirely from the
    // merged schema it builds, but kept correct here regardless: nothing
    // guarantees `path` is only ever reached through that merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_path_schema")]
    pub(crate) path: Option<std::path::PathBuf>,
    pub(crate) remediation: String,
    /// Structured facts about the failure beyond its code, serialised beside `code`, `message`, and `remediation`
    /// (for example the `fields`, `series`, and `template` of a fixed-field conflict), and absent when there are none.
    /// Never holds a value read from a note.
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    pub(crate) details: Option<Box<serde_json::Map<String, serde_json::Value>>>,
}

impl IntoCallToolResult for ToolFailure {
    fn into_call_tool_result(self) -> Result<rmcp::model::CallToolResponse, ErrorData> {
        Json(self).into_call_tool_result()
    }
}

#[derive(Debug, Error)]
pub(crate) enum ToolError {
    #[error(transparent)]
    Path(#[from] PathError),
    #[error(transparent)]
    Filesystem(FsError),
    #[error(transparent)]
    Hash(#[from] contextos_core::ContentHashError),
    #[error("numeric parameter is too large for this platform")]
    Number(#[from] std::num::TryFromIntError),
    #[error("blocking filesystem task failed")]
    Join(#[from] tokio::task::JoinError),
    #[error("invalid tool arguments: {0}")]
    Invalid(&'static str),
    #[error("the key {key} was reserved in the sidecar but its record note was not written: {source}")]
    KeyReservedNoteNotWritten {
        key: String,
        /// Boxed so this variant does not set the size of every `ToolError`: beside `key`, a whole `FsError` reaches
        /// 128 bytes on Windows, where a `PathBuf` is larger.
        #[source]
        source: Box<FsError>,
    },
    #[error("batch result assembly did not preserve input cardinality")]
    BatchAssembly,
    #[error("configured root {root_index} has no write coordinator")]
    RootLockMissing { root_index: usize },
    /// A caller supplied a value for a field the series' template fixes, and it differs.
    #[error(
        "{} {} fixed by the template of series {series} ({template}), and the supplied value differs; omit it to have it stamped",
        .fields.join(", "),
        if .fields.len() == 1 { "is" } else { "are" }
    )]
    FixedFieldConflict {
        fields: Vec<String>,
        series: String,
        template: String,
    },
    /// The template a base's sidecar declares for a series cannot be used.
    #[error("the template {link} of {series} cannot be used: {problem}")]
    SeriesTemplate {
        /// `series A`, or `every series`.
        series: String,
        link: String,
        problem: crate::tools::base_record_template::TemplateProblem,
    },
    #[error("{id} ({path}) already holds the same {fields}, so a second record is not created")]
    DuplicateRecord { id: String, path: String, fields: String },
    #[error("another contextos process held this vault's write lock for more than {seconds} seconds")]
    VaultBusy { lock: std::path::PathBuf, seconds: u64 },
    #[error("could not open or take this vault's host write lock: {source}")]
    HostWriteLock {
        lock: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("hard delete is disabled for this vault")]
    DestructiveDeleteDisabled,
    #[error("managed indexes are disabled for this vault")]
    IndexDisabled,
    #[error(transparent)]
    Index(#[from] IndexServiceError<FsError, FsError>),
    #[error("operation logging is disabled for this vault")]
    OperationLogDisabled,
    #[error("manual operation-log append emitted no persistence event")]
    ManualLogEventMissing,
    #[error(transparent)]
    OperationLog(#[from] OperationLogError<FsError>),
    #[error(transparent)]
    Note(#[from] NoteError),
    #[error(transparent)]
    Frontmatter(#[from] FrontmatterError),
    #[error(transparent)]
    Markdown(#[from] MarkdownError),
    #[error(transparent)]
    Base(#[from] BaseError),
    #[error(transparent)]
    BaseSidecar(#[from] BaseSidecarError),
    /// A base's sidecar that could have governed the note was unusable. Names the base so the operator can find the
    /// file, since the sidecar's own error does not.
    #[error("the sidecar of {base} cannot be used: {source}")]
    BaseSidecarOf {
        base: String,
        #[source]
        source: BaseSidecarError,
    },
    /// As [`ToolError::BaseSidecarOf`], for a sidecar whose frontmatter cannot be parsed at all.
    #[error("the sidecar of {base} cannot be read: {source}")]
    BaseSidecarUnparsable {
        base: String,
        #[source]
        source: FrontmatterError,
    },
    /// The vault's `.contextos/entities.toml` is present but cannot be read as its list of entities, so the `entity`
    /// a new note's location calls for cannot be derived.
    #[error("the vault's .contextos/entities.toml is not a valid list of entities: {source}")]
    EntityList {
        #[source]
        source: toml::de::Error,
    },
    #[error(transparent)]
    Link(#[from] LinkError),
    #[error(transparent)]
    BaseQuery(#[from] BaseQueryError),
    #[error(transparent)]
    Canvas(#[from] CanvasError),
    #[error("Git is disabled for this vault")]
    GitDisabled,
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    GitWrite(#[from] GitWriteError<FsError>),
    #[error("search is disabled for this vault")]
    SearchDisabled,
    #[error(transparent)]
    Search(#[from] SearchError),
    #[error("no fenced ```mermaid code block was found in the note")]
    MermaidFenceMissing,
    /// Defensive only: `MermanParser::render` always produces UTF-8 bytes
    /// (`String::into_bytes`), so this is unreachable with the current
    /// `RendersMermaid` implementation. It guards the trait's wider
    /// `Vec<u8>` contract against a future implementation (e.g. the
    /// documented raster-output follow-up) that might not uphold that
    /// invariant.
    #[error("rendered Mermaid SVG was not valid UTF-8")]
    MermaidRenderNotUtf8,
    /// Defensive only: serialising `StructuredValidationToolResult` (plain
    /// `String`/`bool`/`Vec` fields) cannot fail; this guards against a
    /// future field type that could.
    #[error("failed to serialise Mermaid diagnostics: {0}")]
    MermaidDiagnosticSerialisation(serde_json::Error),
    /// Defensive only: a `VaultPath`'s `root_id` always refers to a root in
    /// the same `VaultSet` it was resolved against (`VaultPath::try_new`),
    /// so `fs_attach_file`'s `{name}://{relative-path}` URI construction
    /// should never actually fail to find that root and hit this.
    #[error("attachment path could not be represented as a resource URI")]
    AttachmentUriInvalid,
    /// `fs_attach_file`'s `expand_links` names link syntax, which only a Markdown note or a canvas has.
    #[error("expand_links applies only to {accepted} here, and {path} is not one")]
    ExpandLinksNotMarkdown { path: String, accepted: &'static str },
    /// `expand_links` grew a note beyond the largest attachment the server returns.
    #[error("the note with its links expanded is larger than {maximum} bytes")]
    ExpandedContentTooLarge { maximum: usize },
    /// A canvas given to `expand_links` is not a JSON Canvas that can be read.
    #[error("the canvas could not be read to expand its links: {reason}")]
    ExpandedCanvasInvalid { reason: String },
    /// A note names more distinct link targets than `expand_links` will resolve in one call.
    #[error("the note names more than {maximum} distinct link targets, which is more than expand_links will resolve")]
    TooManyLinks { maximum: usize },
    /// Defensive only: `ReadTextToolResult`/`ReadManyToolResult`/
    /// `TreeNodeToolResult` are plain `String`/`usize`/`bool`/`Vec`/`Self`
    /// fields, which cannot fail to serialise; this guards against a
    /// future field type that could.
    #[error("failed to serialise the tool result: {0}")]
    CallToolResultSerialisation(serde_json::Error),
    #[error(transparent)]
    Resource(#[from] ResourceError),
    /// `doctor`/`doctor_resolve` could not assess the
    /// effective configuration at all (for example, a vault root that
    /// disappeared between server start and the call). Distinct from a
    /// `DoctorCheck` reporting `Fail`, which is a successful assessment
    /// that found a problem, not a tool-level error.
    #[error(transparent)]
    Doctor(#[from] crate::doctor::DoctorError),
    #[error(transparent)]
    Ephemeris(#[from] EphemerisError),
    #[error(transparent)]
    AgentState(#[from] crate::tools::agents::AgentStateError),
    #[error(transparent)]
    Events(#[from] contextos_events::EventsError),
    /// Defensive only: every instant the ephemeris tools
    /// build comes from a real calendar date well within RFC
    /// 3339's representable range, so this should never actually fail.
    #[error("failed to format an ephemeris instant as RFC 3339: {0}")]
    EphemerisInstantFormatting(time::error::Format),
}

/// A host write lock failure keeps its dedicated variants, so it carries the same codes and handling wherever it
/// surfaces; every other filesystem failure is wrapped as it is.
impl From<FsError> for ToolError {
    fn from(error: FsError) -> Self {
        match error {
            FsError::HostLockBusy { lock, seconds } => Self::VaultBusy { lock, seconds },
            FsError::HostLock { lock, source } => Self::HostWriteLock { lock, source },
            other => Self::Filesystem(other),
        }
    }
}

impl From<contextos_obsidian::ExpansionError<ToolError>> for ToolError {
    fn from(value: contextos_obsidian::ExpansionError<ToolError>) -> Self {
        match value {
            contextos_obsidian::ExpansionError::Lookup(error) => error,
            contextos_obsidian::ExpansionError::TooManyTargets { maximum } => Self::TooManyLinks { maximum },
            contextos_obsidian::ExpansionError::OutputTooLarge { maximum } => Self::ExpandedContentTooLarge { maximum },
            contextos_obsidian::ExpansionError::InvalidCanvas { reason } => Self::ExpandedCanvasInvalid { reason },
        }
    }
}

/// A tool error's stable code, the path it is about, and what to do about it.
type Classified<'a> = (&'a str, Option<std::path::PathBuf>, &'a str);

/// The structured details a failure carries beyond its message.
fn details_of(value: &ToolError) -> serde_json::Map<String, serde_json::Value> {
    let mut details = serde_json::Map::new();
    match value {
        ToolError::Filesystem(FsError::VersionConflict { current_version, .. }) => {
            details.insert(
                "current_version".to_owned(),
                serde_json::Value::String(<&str>::from(current_version).to_owned()),
            );
        }
        ToolError::FixedFieldConflict {
            fields,
            series,
            template,
        } => {
            details.insert(
                "fields".to_owned(),
                serde_json::Value::Array(fields.iter().cloned().map(serde_json::Value::String).collect()),
            );
            details.insert("series".to_owned(), serde_json::Value::String(series.clone()));
            details.insert("template".to_owned(), serde_json::Value::String(template.clone()));
        }
        _ => {}
    }
    details
}

/// The classification of an error that carries its own code and, mostly, its own remediation; `None` for every other.
fn classify_wrapped(value: &ToolError) -> Option<Classified<'_>> {
    Some(match value {
        ToolError::Path(error) => (
            error.code(),
            error.path().map(std::path::Path::to_path_buf),
            error.remediation(),
        ),
        ToolError::Filesystem(error) => (
            error.code(),
            error.path().map(std::path::Path::to_path_buf),
            error.remediation(),
        ),
        ToolError::KeyReservedNoteNotWritten { source, .. } => (
            "io/key-reserved-note-not-written",
            source.path().map(std::path::Path::to_path_buf),
            "The series counter already counts this key, so the series has a gap there and the next \
             creation skips it. Retry the creation, or write the note by hand with that id.",
        ),
        ToolError::Index(error) => (
            error.code(),
            None,
            "Resolve the reported index conflict, and retry the subtree rebuild.",
        ),
        ToolError::OperationLog(error) => (
            error.code(),
            None,
            "Correct the log entry or vault log configuration, and retry.",
        ),
        ToolError::Note(error) => (error.code(), None, error.remediation()),
        ToolError::Frontmatter(error) | ToolError::BaseSidecarUnparsable { source: error, .. } => {
            (error.code(), None, error.remediation())
        }
        ToolError::Markdown(error) => (error.code(), None, error.remediation()),
        ToolError::Base(error) => (error.code(), None, error.remediation()),
        ToolError::BaseSidecar(error) | ToolError::BaseSidecarOf { source: error, .. } => {
            (error.code(), None, error.remediation())
        }
        ToolError::Link(error) => (error.code(), None, error.remediation()),
        ToolError::BaseQuery(error) => (error.code(), None, error.remediation()),
        ToolError::Canvas(error) => (error.code(), None, error.remediation()),
        ToolError::Git(error) => (
            error.code(),
            None,
            "Initialise the repository or correct the Git request, and retry.",
        ),
        ToolError::GitWrite(error) => (error.code(), None, "Resolve the Git or vault write error, and retry."),
        ToolError::Search(error) => (error.code(), None, error.remediation()),
        ToolError::Resource(error) => {
            let (code, remediation) = error.code_and_remediation();
            (code, None, remediation)
        }
        ToolError::Ephemeris(error) => (error.code(), None, error.remediation()),
        ToolError::AgentState(error) => (error.code(), None, error.remediation()),
        ToolError::Events(error) => (error.code(), None, error.remediation()),
        _ => return None,
    })
}

/// The classification of an error the server words itself about how a tool was used or what is switched off; `None`
/// for every other.
fn classify_usage(value: &ToolError) -> Option<Classified<'_>> {
    Some(match value {
        ToolError::Hash(_) => (
            "io/invalid-hash",
            None,
            "Pass a complete 64-character hexadecimal SHA-256 hash.",
        ),
        ToolError::Number(_) | ToolError::Invalid(_) => (
            "io/invalid-argument",
            None,
            "Correct the tool arguments using the advertised input schema.",
        ),
        ToolError::DestructiveDeleteDisabled => (
            "io/destructive-delete-disabled",
            None,
            "Enable vault.git.destructive_delete or omit hard to use platform trash.",
        ),
        ToolError::IndexDisabled => (
            "index/disabled",
            None,
            "Enable index_md for this managed vault before rebuilding indexes.",
        ),
        ToolError::OperationLogDisabled => (
            "log/disabled",
            None,
            "Enable oplog for this managed vault before appending entries.",
        ),
        ToolError::ManualLogEventMissing => (
            "log/append",
            None,
            "Retry the append and inspect server diagnostics if it fails again.",
        ),
        ToolError::EntityList { .. } => (
            "format/entities",
            None,
            "Correct .contextos/entities.toml so each [[entities]] row has a slug, name, and accent, or remove \
             the file so notes are created without an entity, and retry.",
        ),
        ToolError::GitDisabled => ("git/disabled", None, "Enable Git for this managed vault, and retry."),
        ToolError::SearchDisabled => (
            "index/disabled",
            None,
            "Enable [vault.search] text or graph for this managed vault, and retry.",
        ),
        ToolError::MermaidFenceMissing => (
            "format/mermaid-schema",
            None,
            "Add a fenced ```mermaid code block to the note, or pass 'source' directly.",
        ),
        ToolError::ExpandLinksNotMarkdown { path, .. } => (
            "format/not-markdown",
            Some(std::path::PathBuf::from(path)),
            "Omit expand_links, or pass the path of a file it accepts, as the message names.",
        ),
        ToolError::ExpandedContentTooLarge { .. } => (
            "io/too-large",
            None,
            "Read the note without expand_links, or split it into smaller notes.",
        ),
        ToolError::ExpandedCanvasInvalid { .. } => (
            "format/canvas-schema",
            None,
            "Correct the canvas so it is a JSON Canvas, or read it without expand_links.",
        ),
        ToolError::TooManyLinks { .. } => (
            "format/too-many-links",
            None,
            "Read the note without expand_links, or split its links across several notes.",
        ),
        ToolError::Doctor(_) => (
            "doctor/unavailable",
            None,
            "Correct the effective configuration and retry.",
        ),
        _ => return None,
    })
}

/// The classification of an error the server words itself about a record, a lock, or its own fault. Exhaustive, so a
/// new error must be placed in [`classify_wrapped`], [`classify_usage`], or here before the crate compiles; those the
/// other two answer for are listed last and never reach it.
fn classify_server_worded(value: &ToolError) -> Classified<'_> {
    match value {
        ToolError::FixedFieldConflict { .. } => ("io/fixed-field-conflict", None, FIXED_FIELD_CONFLICT_REMEDIATION),
        // The remediation names the template's problem, so it is worded by the caller from the error.
        ToolError::SeriesTemplate { .. } => ("format/series-template", None, ""),
        ToolError::DuplicateRecord { path, .. } => (
            "io/duplicate-record",
            Some(std::path::PathBuf::from(path)),
            "Carry on with the existing record instead of creating another, or give the new record different \
             values for its unique fields.",
        ),
        ToolError::VaultBusy { lock, .. } => (
            "io/vault-busy",
            Some(lock.clone()),
            "Retry shortly. If it persists, another contextos process is stuck mid-write on this vault: stop it.",
        ),
        ToolError::HostWriteLock { lock, .. } => (
            "io/write-lock",
            Some(lock.clone()),
            "Check the vault's state directory is on a local disk the server can write, and retry.",
        ),
        ToolError::Join(_)
        | ToolError::BatchAssembly
        | ToolError::RootLockMissing { .. }
        | ToolError::MermaidRenderNotUtf8
        | ToolError::MermaidDiagnosticSerialisation(_)
        | ToolError::AttachmentUriInvalid
        | ToolError::CallToolResultSerialisation(_)
        | ToolError::EphemerisInstantFormatting(_)
        // Answered for by `classify_usage` or `classify_wrapped`, so never reach here.
        | ToolError::Hash(_)
        | ToolError::Number(_)
        | ToolError::Invalid(_)
        | ToolError::DestructiveDeleteDisabled
        | ToolError::IndexDisabled
        | ToolError::OperationLogDisabled
        | ToolError::ManualLogEventMissing
        | ToolError::EntityList { .. }
        | ToolError::GitDisabled
        | ToolError::SearchDisabled
        | ToolError::MermaidFenceMissing
        | ToolError::ExpandLinksNotMarkdown { .. }
        | ToolError::ExpandedContentTooLarge { .. }
        | ToolError::ExpandedCanvasInvalid { .. }
        | ToolError::TooManyLinks { .. }
        | ToolError::Doctor(_)
        | ToolError::Path(_)
        | ToolError::Filesystem(_)
        | ToolError::KeyReservedNoteNotWritten { .. }
        | ToolError::Index(_)
        | ToolError::OperationLog(_)
        | ToolError::Note(_)
        | ToolError::Frontmatter(_)
        | ToolError::BaseSidecarUnparsable { .. }
        | ToolError::Markdown(_)
        | ToolError::Base(_)
        | ToolError::BaseSidecar(_)
        | ToolError::BaseSidecarOf { .. }
        | ToolError::Link(_)
        | ToolError::BaseQuery(_)
        | ToolError::Canvas(_)
        | ToolError::Git(_)
        | ToolError::GitWrite(_)
        | ToolError::Search(_)
        | ToolError::Resource(_)
        | ToolError::Ephemeris(_)
        | ToolError::AgentState(_)
        | ToolError::Events(_) => (
            "server/internal",
            None,
            "Retry the operation and inspect server diagnostics if it fails again.",
        ),
    }
}

impl From<ToolError> for ToolFailure {
    fn from(value: ToolError) -> Self {
        let details = details_of(&value);
        let (code, path, remediation) = classify_wrapped(&value)
            .or_else(|| classify_usage(&value))
            .unwrap_or_else(|| classify_server_worded(&value));
        // A remediation that names the field or template involved, which a `&'static str` cannot.
        let tailored = match &value {
            ToolError::SeriesTemplate { problem, .. } => Some(problem.remediation()),
            _ => None,
        };
        Self {
            code: code.to_owned(),
            message: value.to_string(),
            path,
            remediation: tailored.unwrap_or_else(|| remediation.to_owned()),
            details: (!details.is_empty()).then(|| Box::new(details)),
        }
    }
}

pub(crate) async fn execute<T, F>(operation: F) -> Result<Json<T>, ToolFailure>
where
    T: Serialize + schemars::JsonSchema + Send + 'static,
    F: FnOnce() -> Result<T, ToolError> + Send + 'static,
{
    Ok(Json(evaluate(operation).await?))
}

pub(crate) async fn evaluate<T, F>(operation: F) -> Result<T, ToolFailure>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ToolError> + Send + 'static,
{
    let result = tokio::task::spawn_blocking(operation).await.map_err(ToolError::from)?;
    result.map_err(ToolFailure::from)
}

pub(crate) async fn execute_value<T, F>(operation: F) -> Result<Json<T>, ToolFailure>
where
    T: Serialize + schemars::JsonSchema + Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let value = tokio::task::spawn_blocking(operation).await.map_err(ToolError::from)?;
    Ok(Json(value))
}

#[cfg(test)]
#[path = "tool_error_test.rs"]
mod tests;
