use std::path::{Path, PathBuf};

use contextos_core::{Clock, PortableRelativePath};
use git2::{DiffFormat, DiffOptions, Sort, Status, StatusOptions};

use crate::{DIFF_TRUNCATION_NOTICE, Git2Vault, GitError, open_repository};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitStatusResult {
    pub branch: String,
    pub ahead: usize,
    pub behind: usize,
    pub staged: Vec<String>,
    pub unstaged: Vec<String>,
    pub untracked: Vec<String>,
    pub pending_paths: Vec<String>,
    pub staged_at_unix: Option<i64>,
    /// The most recent failed attempt to commit the pending paths, until a commit succeeds or nothing is pending.
    pub commit_failure: Option<GitCommitFailure>,
}

/// A commit of MCP-staged paths that failed after the writes themselves succeeded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitCommitFailure {
    /// The stable machine-readable error code of the latest failure.
    pub code: String,
    pub message: String,
    /// Consecutive failed attempts since the last successful commit.
    pub attempts: u32,
    /// When the latest attempt failed, as a Unix timestamp.
    pub at_unix: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitLogRequest {
    pub path: Option<PathBuf>,
    pub limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitLogEntry {
    pub id: String,
    pub short_id: String,
    pub time: i64,
    pub message: String,
    pub files_changed: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitDiffRequest {
    pub from: Option<String>,
    pub to: Option<String>,
    pub path: Option<PathBuf>,
    pub max_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitDiffResult {
    pub content: String,
    pub truncated: bool,
}

impl<C> Git2Vault<C>
where
    C: Clock,
{
    /// Reports repository and MCP-owned pending state.
    ///
    /// # Errors
    ///
    /// Returns a typed repository or pending-state error.
    pub fn status(&self) -> Result<GitStatusResult, GitError> {
        let repository = open_repository(self.root.path())?;
        let branch = repository
            .head()
            .map_err(GitError::Repository)?
            .shorthand()
            .unwrap_or("HEAD")
            .to_owned();
        let mut options = StatusOptions::new();
        options.include_untracked(true).recurse_untracked_dirs(true);
        let statuses = repository.statuses(Some(&mut options)).map_err(GitError::Repository)?;
        let mut staged = Vec::new();
        let mut unstaged = Vec::new();
        let mut untracked = Vec::new();
        for entry in statuses.iter() {
            let path = entry.path().map_err(GitError::Repository)?.to_owned();
            let status = entry.status();
            if status.intersects(
                Status::INDEX_NEW
                    | Status::INDEX_MODIFIED
                    | Status::INDEX_DELETED
                    | Status::INDEX_RENAMED
                    | Status::INDEX_TYPECHANGE,
            ) {
                staged.push(path.clone());
            }
            if status.contains(Status::WT_NEW) {
                untracked.push(path.clone());
            }
            if status.intersects(Status::WT_MODIFIED | Status::WT_DELETED | Status::WT_RENAMED | Status::WT_TYPECHANGE)
            {
                unstaged.push(path);
            }
        }
        let pending = self.pending.lock().map_err(|_| GitError::PendingStateUnavailable)?;
        let pending_paths = pending
            .paths
            .iter()
            .map(|path| String::from(PortableRelativePath(path)))
            .collect();
        let staged_at_unix = pending.staged_at_unix;
        let commit_failure = pending.failure.clone();
        Ok(GitStatusResult {
            branch,
            ahead: 0,
            behind: 0,
            staged,
            unstaged,
            untracked,
            pending_paths,
            staged_at_unix,
            commit_failure,
        })
    }

    /// Reads newest-first local history with an optional path filter.
    ///
    /// # Errors
    ///
    /// Returns a typed repository traversal or diff error.
    pub fn log(&self, request: &GitLogRequest) -> Result<Vec<GitLogEntry>, GitError> {
        if request.limit == 0 {
            return Err(GitError::InvalidLogLimit);
        }
        let repository = open_repository(self.root.path())?;
        let mut walk = repository.revwalk().map_err(GitError::Repository)?;
        walk.set_sorting(Sort::TIME).map_err(GitError::Repository)?;
        walk.push_head().map_err(GitError::Repository)?;
        let mut entries = Vec::new();
        for id in walk {
            let commit = repository
                .find_commit(id.map_err(GitError::Repository)?)
                .map_err(GitError::Repository)?;
            let tree = commit.tree().map_err(GitError::Repository)?;
            let parent_tree = commit.parent(0).ok().and_then(|parent| parent.tree().ok());
            let diff = repository
                .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)
                .map_err(GitError::Repository)?;
            let mut files = diff
                .deltas()
                .filter_map(|delta| delta.new_file().path().or_else(|| delta.old_file().path()))
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            files.sort();
            files.dedup();
            if request.path.as_ref().is_some_and(|path| {
                !files
                    .iter()
                    .any(|file| Path::new(file) == path || Path::new(file).starts_with(path))
            }) {
                continue;
            }
            let id = commit.id().to_string();
            entries.push(GitLogEntry {
                short_id: id.chars().take(7).collect(),
                id,
                time: commit.time().seconds(),
                message: commit.message().unwrap_or("").to_owned(),
                files_changed: files,
            });
            if entries.len() >= request.limit {
                break;
            }
        }
        Ok(entries)
    }

    /// Produces a byte-capped unified diff between refs or the working tree.
    ///
    /// # Errors
    ///
    /// Returns a typed reference, repository, or limit error.
    pub fn diff(&self, request: &GitDiffRequest) -> Result<GitDiffResult, GitError> {
        if request.max_bytes == 0 {
            return Err(GitError::InvalidDiffLimit);
        }
        let repository = open_repository(self.root.path())?;
        let from = request.from.as_deref().unwrap_or("HEAD");
        let from_tree = repository
            .revparse_single(from)
            .and_then(|object| object.peel_to_tree())
            .map_err(GitError::Repository)?;
        let mut options = DiffOptions::new();
        if let Some(path) = &request.path {
            options.pathspec(path);
        }
        let diff = if let Some(to) = &request.to {
            let to_tree = repository
                .revparse_single(to)
                .and_then(|object| object.peel_to_tree())
                .map_err(GitError::Repository)?;
            repository
                .diff_tree_to_tree(Some(&from_tree), Some(&to_tree), Some(&mut options))
                .map_err(GitError::Repository)?
        } else {
            repository
                .diff_tree_to_workdir_with_index(Some(&from_tree), Some(&mut options))
                .map_err(GitError::Repository)?
        };
        let capture_limit = request.max_bytes.saturating_add(1);
        let mut bytes = Vec::new();
        let mut truncated = false;
        diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
            if matches!(line.origin(), '+' | '-' | ' ') {
                let mut encoded = [0_u8; 4];
                let prefix = line.origin().encode_utf8(&mut encoded).as_bytes();
                let available = capture_limit.saturating_sub(bytes.len());
                let retained = available.min(prefix.len());
                bytes.extend_from_slice(&prefix[..retained]);
                truncated |= retained < prefix.len();
            }
            let available = capture_limit.saturating_sub(bytes.len());
            let retained = available.min(line.content().len());
            bytes.extend_from_slice(&line.content()[..retained]);
            truncated |= retained < line.content().len();
            true
        })
        .map_err(GitError::Repository)?;
        truncated |= bytes.len() > request.max_bytes;
        let content = if truncated && request.max_bytes >= DIFF_TRUNCATION_NOTICE.len() {
            let content_limit = request.max_bytes - DIFF_TRUNCATION_NOTICE.len();
            let mut content = String::from_utf8_lossy(&bytes[..bytes.len().min(content_limit)]).into_owned();
            while content.len() > content_limit {
                content.pop();
            }
            content.push_str(DIFF_TRUNCATION_NOTICE);
            content
        } else {
            let mut content = String::from_utf8_lossy(&bytes[..bytes.len().min(request.max_bytes)]).into_owned();
            while content.len() > request.max_bytes {
                content.pop();
            }
            content
        };
        Ok(GitDiffResult { content, truncated })
    }
}
