//! The errors building and shutting down the server can raise.

use contextos_core::PathError;
use contextos_fs::FsError;
use contextos_git::{GitError, GitWriteError};
use contextos_index::IndexServiceError;
use contextos_oplog::OperationLogError;
use contextos_search::SearchError;
use thiserror::Error;

use crate::ConfigError;
use crate::module::ModuleRegistryError;

#[derive(Debug, Error)]
pub enum ServerBuildError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Filesystem(#[from] FsError),
    #[error(transparent)]
    Index(#[from] IndexServiceError<FsError, FsError>),
    #[error(transparent)]
    OperationLog(#[from] OperationLogError<FsError>),
    #[error("operation-log path is not valid UTF-8: {path}")]
    NonUtf8OperationLogPath { path: std::path::PathBuf },
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    GitWrite(#[from] GitWriteError<FsError>),
    #[error(transparent)]
    Path(#[from] PathError),
    #[error(transparent)]
    Search(#[from] SearchError),
    #[error(transparent)]
    Module(#[from] ModuleRegistryError),
    #[error(transparent)]
    StateDir(#[from] crate::state_dir::StateDirError),
    #[error("at least one vault is required")]
    NoVaults,
    #[error("configured maximum read size overflows bytes")]
    ReadLimitOverflow,
    #[error("configured resource_link_threshold_kb overflows a byte count")]
    ResourceLinkThresholdOverflow,
    #[error("server.resource_link_threshold_kb must be at least 1: it caps every part of fs_read_text_file")]
    ResourceLinkThresholdZero,
}

/// Typed failure from graceful substrate persistence during server shutdown.
#[derive(Debug, Error)]
pub enum ShutdownFlushError {
    #[error(transparent)]
    OperationLog(#[from] OperationLogError<FsError>),
    #[error(transparent)]
    Git(#[from] GitError),
}
