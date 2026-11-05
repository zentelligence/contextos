#![forbid(unsafe_code)]

//! Local `libgit2` adapter for `ContextOS` vault recovery history.
//!
//! The public surface is re-exported here; the implementation is split by
//! responsibility across sibling modules: repository inspection
//! ([`inspect`]), the MCP-owned staging and debounced commit batch
//! ([`commit`]), repository initialisation and ignore policy ([`init`]),
//! forward-only historical restoration ([`restore`]), and the typed error
//! surface ([`error`]).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use contextos_core::{
    Clock, OperationEvent, OperationWarning, VaultPath, VaultPathInput, VaultRoot, VaultSet, VersionsVault,
};
use git2::{Repository, Signature, Time};

mod commit;
mod creation;
mod error;
mod init;
mod inspect;
mod restore;
mod uncommitted;

pub use commit::GitCommitResult;
pub use error::{GitError, GitWriteError};
pub use init::GitInitResult;
pub use inspect::{GitCommitFailure, GitDiffRequest, GitDiffResult, GitLogEntry, GitLogRequest, GitStatusResult};
pub use restore::{GitRestoreRequest, GitRestoreResult};
pub use uncommitted::GitUncommitted;

pub(crate) const DIFF_TRUNCATION_NOTICE: &str = "\n[diff truncated]\n";
pub(crate) const PENDING_DIRECTORY: &str = "contextos-mcp/pending";

/// Trusted dependencies and identity for one local Git vault.
#[derive(Clone, Debug)]
pub struct Git2VaultConfig<C> {
    pub root: VaultRoot,
    pub roots: VaultSet,
    pub clock: C,
    pub author_name: String,
    pub author_email: String,
    pub allow_destructive_restore: bool,
    pub protected_restore_paths: Vec<PathBuf>,
}

/// Local libgit2 adapter scoped to one configured vault root.
#[derive(Clone, Debug)]
pub struct Git2Vault<C> {
    root: VaultRoot,
    roots: VaultSet,
    clock: C,
    author_name: String,
    author_email: String,
    allow_destructive_restore: bool,
    protected_restore_paths: Vec<PathBuf>,
    pending: Arc<Mutex<PendingBatch>>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PendingBatch {
    paths: BTreeSet<PathBuf>,
    events: Vec<OperationEvent>,
    generation: u64,
    staged_at_unix: Option<i64>,
    /// When the first change of the current batch was staged: unlike `staged_at_unix` it does not move as more
    /// changes arrive, so it measures how long the batch has been deferring its commit.
    first_staged_at_unix: Option<i64>,
    failure: Option<GitCommitFailure>,
}

impl<C> TryFrom<Git2VaultConfig<C>> for Git2Vault<C> {
    type Error = GitError;

    fn try_from(value: Git2VaultConfig<C>) -> Result<Self, Self::Error> {
        if value.author_name.trim().is_empty() {
            return Err(GitError::InvalidAuthorName);
        }
        if value.author_email.trim().is_empty() || !value.author_email.contains('@') {
            return Err(GitError::InvalidAuthorEmail);
        }
        if !value.roots.iter().any(|root| root == &value.root) {
            return Err(GitError::RootNotConfigured);
        }
        if value.protected_restore_paths.iter().any(|path| {
            path.as_os_str().is_empty()
                || path.is_absolute()
                || path.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
        }) {
            return Err(GitError::InvalidProtectedRestorePath);
        }
        Ok(Self {
            root: value.root,
            roots: value.roots,
            clock: value.clock,
            author_name: value.author_name,
            author_email: value.author_email,
            allow_destructive_restore: value.allow_destructive_restore,
            protected_restore_paths: value.protected_restore_paths,
            pending: Arc::new(Mutex::new(PendingBatch::default())),
        })
    }
}

impl<C> Git2Vault<C>
where
    C: Clock,
{
    /// Reports whether this vault currently contains a Git repository.
    #[must_use]
    pub fn is_repository(&self) -> bool {
        Repository::open(self.root.path()).is_ok()
    }

    /// Reports whether the vault's repository has any commits yet. One that has none (just created by `git init`)
    /// cannot take a commit from the write pipeline until [`Self::initialise`] gives it its first.
    ///
    /// # Errors
    ///
    /// Returns a typed repository error when the vault has no readable repository.
    pub fn has_commits(&self) -> Result<bool, GitError> {
        let repository = open_repository(self.root.path())?;
        match repository.head() {
            Ok(_) => Ok(true),
            Err(error) if error.code() == git2::ErrorCode::UnbornBranch => Ok(false),
            Err(error) => Err(GitError::Repository(error)),
        }
    }

    fn vault_path_for_relative(&self, relative: &Path) -> Result<VaultPath, GitError> {
        let absolute = self.root.path().join(relative);
        let raw = absolute
            .to_str()
            .ok_or_else(|| GitError::NonUtf8Path { path: absolute.clone() })?;
        VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw,
        })
        .map_err(GitError::Path)
    }

    fn relative_path<'a>(&self, path: &'a VaultPath) -> Result<&'a Path, GitError> {
        let absolute = <&Path>::from(path);
        if !absolute.starts_with(self.root.path()) {
            return Err(GitError::WrongVault);
        }
        Ok(path.relative())
    }

    fn signature(&self) -> Result<Signature<'static>, GitError> {
        let now = self.clock.now();
        let time = Time::new(now.unix_timestamp(), i32::from(now.offset().whole_minutes()));
        Signature::new(&self.author_name, &self.author_email, &time).map_err(GitError::Repository)
    }
}

pub(crate) fn open_repository(root: &Path) -> Result<Repository, GitError> {
    Repository::open(root).map_err(|error| {
        if error.code() == git2::ErrorCode::NotFound {
            GitError::NotRepository
        } else {
            GitError::Repository(error)
        }
    })
}

impl<C> VersionsVault for Git2Vault<C>
where
    C: Clock,
{
    fn stage(&self, event: &OperationEvent) -> Result<(), OperationWarning> {
        self.stage_event(event).map_err(OperationWarning::from)
    }
}
