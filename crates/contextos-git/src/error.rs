use std::path::PathBuf;

use contextos_core::OperationWarning;
use thiserror::Error;

/// Typed local Git adapter failures.
#[derive(Debug, Error)]
pub enum GitError {
    #[error("Git author name must not be empty")]
    InvalidAuthorName,
    #[error("Git author email must contain a local and domain part")]
    InvalidAuthorEmail,
    #[error("Git root is not present in the configured vault set")]
    RootNotConfigured,
    #[error("Git repository operation failed")]
    Repository(#[source] git2::Error),
    #[error("Git could not record the commit")]
    Commit(#[source] git2::Error),
    #[error("vault is not a Git repository")]
    NotRepository,
    #[error(".gitignore could not be read: {path}")]
    ReadIgnore {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(".gitignore is not valid UTF-8: {path}")]
    NonUtf8Ignore {
        path: PathBuf,
        #[source]
        source: std::string::FromUtf8Error,
    },
    #[error("Git-managed path is not valid UTF-8: {path}")]
    NonUtf8Path { path: PathBuf },
    #[error("Git-managed path is invalid")]
    Path(#[source] contextos_core::PathError),
    #[error("Git operation path belongs to another vault")]
    WrongVault,
    #[error("Git protected restore paths must be non-empty relative paths without traversal")]
    InvalidProtectedRestorePath,
    #[error("Git pending-path state is unavailable")]
    PendingStateUnavailable,
    #[error("Git pending-path metadata could not be accessed")]
    PendingMetadata(#[source] std::io::Error),
    #[error("Git pending-path metadata is invalid")]
    InvalidPendingMetadata,
    #[error("Git status contains a path that is not valid UTF-8")]
    NonUtf8StatusPath,
    #[error("Git diff byte limit must be greater than zero")]
    InvalidDiffLimit,
    #[error("Git log limit must be greater than zero")]
    InvalidLogLimit,
    #[error("historical Git content is not UTF-8: {path}")]
    NonUtf8Restore {
        path: PathBuf,
        #[source]
        source: std::str::Utf8Error,
    },
    #[error("current restore target is not UTF-8: {path}")]
    NonUtf8Current {
        path: PathBuf,
        #[source]
        source: std::string::FromUtf8Error,
    },
    #[error("current restore target could not be read: {path}")]
    ReadRestoreTarget {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("restore target could not be inspected: {path}")]
    WalkRestoreTarget {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("historical Git object is not a file or directory")]
    UnsupportedRestoreObject,
    #[error("restore would delete MCP-owned paths but destructive restore is disabled")]
    DestructiveRestoreDisabled,
    #[error("operation-log content is append-only and cannot be selected for Git restore")]
    ProtectedRestorePath,
}

impl GitError {
    /// Returns a stable machine-readable Git error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Path(error) => error.code(),
            Self::InvalidAuthorName
            | Self::InvalidAuthorEmail
            | Self::RootNotConfigured
            | Self::InvalidProtectedRestorePath
            | Self::Repository(_)
            | Self::ReadIgnore { .. }
            | Self::NonUtf8Ignore { .. }
            | Self::NonUtf8Path { .. } => "git/init",
            Self::Commit(_) => "git/commit",
            Self::NotRepository => "git/not-a-repo",
            Self::WrongVault
            | Self::PendingStateUnavailable
            | Self::PendingMetadata(_)
            | Self::InvalidPendingMetadata => "git/stage",
            Self::NonUtf8StatusPath | Self::InvalidDiffLimit | Self::InvalidLogLimit => "git/inspect",
            Self::NonUtf8Restore { .. }
            | Self::NonUtf8Current { .. }
            | Self::ReadRestoreTarget { .. }
            | Self::WalkRestoreTarget { .. }
            | Self::UnsupportedRestoreObject
            | Self::DestructiveRestoreDisabled
            | Self::ProtectedRestorePath => "git/restore",
        }
    }
}

impl From<GitError> for OperationWarning {
    fn from(value: GitError) -> Self {
        Self {
            code: value.code().to_owned(),
            message: value.to_string(),
        }
    }
}

/// A Git operation that can additionally fail at the vault write boundary.
#[derive(Debug, Error)]
pub enum GitWriteError<E>
where
    E: std::error::Error + 'static,
{
    #[error(transparent)]
    Git(#[from] GitError),
    #[error("Git-requested vault content could not be persisted")]
    Write(#[source] E),
}

impl<E> GitWriteError<E>
where
    E: std::error::Error + 'static,
{
    /// Returns a stable machine-readable write-operation error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Git(error) => error.code(),
            Self::Write(_) => "git/write",
        }
    }
}
