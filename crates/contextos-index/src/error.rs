use contextos_core::OperationWarning;
use contextos_obsidian::{FrontmatterError, NoteError};
use thiserror::Error;

/// Typed managed-index application-service failures.
#[derive(Debug, Error)]
pub enum IndexServiceError<R, W>
where
    R: std::error::Error + 'static,
    W: std::error::Error + 'static,
{
    #[error("vault content could not be read for index reconciliation")]
    Read(#[source] R),
    #[error("managed index could not be persisted")]
    Write(#[source] W),
    #[error("managed index path is invalid")]
    Path(#[source] contextos_core::PathError),
    #[error("managed index path is not valid UTF-8: {path}")]
    NonUtf8Path { path: std::path::PathBuf },
    #[error("managed index root is not present in the configured vault set")]
    RootNotConfigured,
    #[error("managed index request selected a different vault root")]
    WrongRoot,
    #[error("both _index.md and index.md exist in {directory}; merge or rename one before rebuilding")]
    LegacyIndexConflict { directory: std::path::PathBuf },
    #[error("the vault's .contextos/entities.toml is not a valid list of entities")]
    EntityRoster {
        #[source]
        source: toml::de::Error,
    },
    #[error(transparent)]
    Index(#[from] IndexError),
}

impl<R, W> IndexServiceError<R, W>
where
    R: std::error::Error + 'static,
    W: std::error::Error + 'static,
{
    /// Returns a stable machine-readable service error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Index(error) => error.code(),
            Self::LegacyIndexConflict { .. } => "index/legacy-conflict",
            Self::EntityRoster { .. } => "index/entities",
            Self::RootNotConfigured | Self::WrongRoot => "index/root",
            Self::Read(_) | Self::Write(_) | Self::Path(_) | Self::NonUtf8Path { .. } => "index/reconcile",
        }
    }
}

impl<R, W> From<IndexServiceError<R, W>> for OperationWarning
where
    R: std::error::Error + 'static,
    W: std::error::Error + 'static,
{
    fn from(value: IndexServiceError<R, W>) -> Self {
        Self {
            code: value.code().to_owned(),
            message: value.to_string(),
        }
    }
}

/// Typed managed-index reconciliation failures.
#[derive(Debug, Error)]
pub enum IndexError {
    #[error("managed index markers are missing, duplicated, or out of order")]
    InvalidMarkers,
    #[error("managed index row on line {line} is malformed")]
    InvalidRow { line: usize },
    #[error("index entry name is invalid: {name}")]
    InvalidEntryName { name: String },
    #[error("index entry appears more than once: {name}")]
    DuplicateEntry { name: String },
    /// `path` is the offending file's vault-relative path, attached at the
    /// one call site (`IndexService::persist_directory`) that has both the
    /// containing directory and the entry's filename in scope;
    /// `IndexSummary::try_from` itself only ever sees a bare filename, so
    /// it cannot build this path on its own (hence `FrontmatterError`
    /// rather than `Self` as that conversion's error type).
    #[error("YAML frontmatter is invalid in {path}: {source}")]
    Frontmatter {
        path: std::path::PathBuf,
        #[source]
        source: FrontmatterError,
    },
    /// The managed `index.md` file's own frontmatter block (not an entry's)
    /// failed to parse or could not be rendered.
    #[error("YAML frontmatter is invalid in the managed index.md for {path}: {source}")]
    DirectoryFrontmatter {
        path: std::path::PathBuf,
        #[source]
        source: FrontmatterError,
    },
    /// Building a fresh default frontmatter block for a managed `index.md`
    /// that has none yet failed its own validation (for example, an
    /// impossible title/description collision).
    #[error(transparent)]
    DirectoryFrontmatterDefault(#[from] NoteError),
    /// The managed `index.md` frontmatter needs an edit, but its layout (flow style, quoted keys, or a value split
    /// by blank lines) cannot be changed line by line without risking the operator's own content.
    #[error("the frontmatter of the managed index.md for {path} cannot be edited in place near {key}")]
    FrontmatterLayout { path: std::path::PathBuf, key: String },
}

impl IndexError {
    /// Returns the stable machine-readable error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Frontmatter { source, .. } | Self::DirectoryFrontmatter { source, .. } => source.code(),
            Self::DirectoryFrontmatterDefault(source) => source.code(),
            Self::FrontmatterLayout { .. } => "format/frontmatter",
            Self::InvalidMarkers
            | Self::InvalidRow { .. }
            | Self::InvalidEntryName { .. }
            | Self::DuplicateEntry { .. } => "index/stale",
        }
    }

    /// Returns an actionable recovery instruction.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        match self {
            Self::Frontmatter { source, .. } | Self::DirectoryFrontmatter { source, .. } => source.remediation(),
            Self::DirectoryFrontmatterDefault(source) => source.remediation(),
            Self::FrontmatterLayout { .. } => {
                "Rewrite the index.md frontmatter as plain block-style YAML, one `key: value` per line, and retry."
            }
            Self::InvalidMarkers
            | Self::InvalidRow { .. }
            | Self::InvalidEntryName { .. }
            | Self::DuplicateEntry { .. } => "Correct the managed index block or rebuild the affected subtree.",
        }
    }
}
