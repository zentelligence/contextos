use thiserror::Error;

/// Why a field of a recorded event was refused.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum FieldRejection {
    #[error("it is required by the kind and was not given")]
    Missing,
    #[error("the kind declares no such field")]
    Undeclared,
    #[error("it is a common note field, which events never carry")]
    CommonNoteField,
    #[error("its value must be a single-line string, a number, a boolean, or a list of those")]
    InvalidValue,
}

/// Why a recorded event's subject was refused.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SubjectRejection {
    #[error("the kind requires a subject and none was given")]
    Required,
    #[error("the kind takes no subject")]
    NotAccepted,
    #[error("the subject must be one wikilink, such as [[a-001|A-001]], on a single line")]
    Malformed,
    #[error(
        "the subject is a record of a keyed base, which is referenced by its key, such as [[a-001|A-001]], never its path"
    )]
    NotByKey,
    #[error("the subject link names more than one note; a note that is no record can be linked by its full path")]
    Ambiguous,
}

/// Why a key series named in a request was refused.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SeriesRejection {
    #[error("the kind assigns a key, so the request must name the series to take it from")]
    Missing,
    #[error("the kind assigns no key, so no series is accepted")]
    NotAccepted,
    #[error("the events sidecar does not map that series to this kind")]
    NotMapped,
}

/// Why a kind definition cannot be used.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum DefinitionRejection {
    #[error("it is missing")]
    Missing,
    #[error("it has the wrong type")]
    WrongType,
    #[error("it is empty")]
    Empty,
    #[error("it is not one of the values the field allows")]
    NotAllowed,
    #[error("it repeats a value")]
    Duplicate,
    #[error("it differs from the file's name")]
    BasenameMismatch,
    #[error("it is not a valid name")]
    InvalidName,
    #[error("it collides with a field every event carries, a common note field, or a server-stamped name")]
    ReservedName,
    #[error("it names a field the kind also declares as required or optional, so no record could ever supply it")]
    DeclaredAsField,
    #[error("it must be a positive whole number of days")]
    NotPositive,
    #[error("the note is not UTF-8 text with well-formed frontmatter")]
    Malformed,
}

/// Failures of the vault events domain logic.
#[derive(Debug, Error)]
pub enum EventsError {
    #[error(
        "`{value}` is not a valid event kind name; use lower-case segments of letters, digits, and hyphens joined by dots"
    )]
    InvalidKindName { value: String },
    #[error(
        "`{value}` is not a valid field name; use a lower-case letter followed by lower-case letters, digits, underscores, or hyphens"
    )]
    InvalidFieldName { value: String },
    #[error("`{value}` is not a severity; use info, warning, or error")]
    InvalidSeverity { value: String },
    #[error("the event kind is not declared by a usable definition")]
    UnknownKind { kind: Option<String> },
    #[error("`{kind}` is fired by the server, so it cannot be recorded or defined")]
    ServerFiredKind { kind: String },
    #[error("severity `{severity}` is not one `{kind}` allows")]
    SeverityNotAllowed { kind: String, severity: String },
    #[error("field `{field}` of `{kind}` was refused: {reason}")]
    InvalidFields {
        kind: String,
        field: String,
        reason: FieldRejection,
    },
    #[error("the subject of `{kind}` was refused: {reason}")]
    InvalidSubject { kind: String, reason: SubjectRejection },
    #[error("the key series for `{kind}` was refused: {reason}")]
    InvalidSeries {
        kind: String,
        prefix: Option<String>,
        reason: SeriesRejection,
    },
    #[error("`{field}` is stamped by the server and cannot be supplied")]
    ServerStamped { field: String },
    #[error("the definition's `{field}` cannot be used: {reason}")]
    InvalidDefinition {
        field: &'static str,
        reason: DefinitionRejection,
    },
    #[error("`{kind}` is defined more than once")]
    DuplicateDefinition { kind: String },
    #[error("`{kind}` declares a key field but no key was assigned")]
    KeyNotAssigned { kind: String },
    #[error("`{kind}` declares no key field but a key was assigned")]
    KeyNotExpected { kind: String },
    #[error("the warning code `{code}` is not classified as a failure or a notice")]
    UnclassifiedWarning { code: String },
    #[error("the task configuration could not be read: {reason}")]
    TasksConfigUnreadable { reason: String },
    #[error("the last-written task table could not be read: {reason}")]
    TableUnreadable { reason: String },
    #[error("the last-written task table could not be written: {reason}")]
    TableUnwritable { reason: String },
}

impl EventsError {
    /// Returns a stable machine-readable error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnknownKind { .. } => "events/unknown-kind",
            Self::ServerFiredKind { .. } => "events/server-fired-kind",
            Self::InvalidSeverity { .. } | Self::SeverityNotAllowed { .. } => "events/invalid-severity",
            Self::InvalidFields { .. } | Self::InvalidKindName { .. } | Self::InvalidFieldName { .. } => {
                "events/invalid-fields"
            }
            Self::InvalidSubject { .. } => "events/invalid-subject",
            Self::InvalidSeries { .. } => "events/invalid-series",
            Self::ServerStamped { .. } => "events/server-stamped",
            Self::InvalidDefinition { .. } => "events/invalid-definition",
            Self::DuplicateDefinition { .. } => "events/duplicate-definition",
            Self::KeyNotAssigned { .. } | Self::KeyNotExpected { .. } => "events/key-mismatch",
            Self::UnclassifiedWarning { .. } => "events/unclassified-warning",
            Self::TasksConfigUnreadable { .. } => "events/tasks-config-unreadable",
            Self::TableUnreadable { .. } => "events/table-unreadable",
            Self::TableUnwritable { .. } => "events/table-unwritable",
        }
    }

    /// Returns an actionable remediation hint for the operator.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        match self {
            Self::InvalidKindName { .. } | Self::InvalidFieldName { .. } => {
                "Name the kind with lower-case dot-separated segments, such as `import.completed`."
            }
            Self::InvalidSeverity { .. } | Self::SeverityNotAllowed { .. } => {
                "Use a severity the kind's definition in registry/events allows."
            }
            Self::UnknownKind { .. } => {
                "Declare the kind in registry/events/{kind}.md, or correct the definition doctor reports."
            }
            Self::ServerFiredKind { .. } => {
                "Server-fired kinds are raised by the server; record a kind your vault declares instead."
            }
            Self::InvalidFields { .. } => {
                "Give exactly the required and optional fields the kind declares, and no common note fields."
            }
            Self::InvalidSubject { .. } => {
                "Give a subject only when the kind accepts one, and always when it requires one."
            }
            Self::InvalidSeries { .. } => {
                "Pass `prefix` exactly when the kind has a key field, naming a series the events sidecar maps to the kind in `series_kinds`."
            }
            Self::ServerStamped { .. } => "Leave `at`, the path, the filename, and the kind's key field to the server.",
            Self::InvalidDefinition { .. } | Self::DuplicateDefinition { .. } => {
                "Correct the kind definition in registry/events and run doctor."
            }
            Self::KeyNotAssigned { .. } | Self::KeyNotExpected { .. } => {
                "Assign a key from the kind's series exactly when its definition names a key field."
            }
            Self::UnclassifiedWarning { .. } => {
                "Classify the code in warning_codes.rs; until then it is treated as a failure."
            }
            Self::TasksConfigUnreadable { .. } => {
                "Correct .contextos/tasks.toml; until then no task is reported as holding an undeclared state."
            }
            Self::TableUnreadable { .. } | Self::TableUnwritable { .. } => {
                "The table is rebuildable derived state; check the state directory is writable and let it reseed."
            }
        }
    }
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
