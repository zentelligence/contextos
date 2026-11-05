use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use contextos_core::{Clock, ContentHash, OpKind, OperationEvent, Origin};
use git2::{IndexAddOption, IndexEntry};
use sha2::{Digest, Sha256};

use crate::{Git2Vault, GitCommitFailure, GitError, PENDING_DIRECTORY, PendingBatch, open_repository};

/// One explicit or debounced commit outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitCommitResult {
    pub commit_id: Option<String>,
    pub committed_paths: Vec<PathBuf>,
    pub message: Option<String>,
}

impl<C> Git2Vault<C>
where
    C: Clock,
{
    /// Commits staged state left by an interrupted previous server process.
    ///
    /// # Errors
    ///
    /// Returns a typed repository or commit error.
    pub fn recover_staged(&self) -> Result<GitCommitResult, GitError> {
        if !self.is_repository() {
            return Ok(GitCommitResult {
                commit_id: None,
                committed_paths: Vec::new(),
                message: None,
            });
        }
        let recorded = self.recorded_pending_paths()?;
        if recorded.is_empty() {
            return Ok(GitCommitResult {
                commit_id: None,
                committed_paths: Vec::new(),
                message: None,
            });
        }
        let repository = open_repository(self.root.path())?;
        let mut index = repository.index().map_err(GitError::Repository)?;
        for path in &recorded {
            let absolute = self.root.path().join(path);
            if absolute.exists() {
                Self::stage_present_path(&mut index, path, &absolute)?;
            } else {
                index.remove_all([path], None).map_err(GitError::Repository)?;
            }
        }
        index.write().map_err(GitError::Repository)?;
        let head_tree = repository
            .head()
            .and_then(|head| head.peel_to_tree())
            .map_err(GitError::Repository)?;
        let diff = repository
            .diff_tree_to_index(Some(&head_tree), Some(&index), None)
            .map_err(GitError::Repository)?;
        let staged = diff
            .deltas()
            .filter_map(|delta| delta.new_file().path().or_else(|| delta.old_file().path()))
            .map(Path::to_path_buf)
            .collect::<BTreeSet<_>>();
        let paths = recorded.intersection(&staged).cloned().collect::<BTreeSet<_>>();
        let stale = recorded.difference(&paths).cloned().collect::<Vec<_>>();
        self.clear_pending_paths(&stale)?;
        if paths.is_empty() {
            return Ok(GitCommitResult {
                commit_id: None,
                committed_paths: Vec::new(),
                message: None,
            });
        }
        self.pending
            .lock()
            .map_err(|_| GitError::PendingStateUnavailable)?
            .paths = paths;
        self.commit(Some("mcp: recovered staged changes"))
    }

    /// Returns the monotonic generation of the current pending batch.
    ///
    /// # Errors
    ///
    /// Returns an error when pending state is unavailable.
    pub fn pending_generation(&self) -> Result<u64, GitError> {
        Ok(self
            .pending
            .lock()
            .map_err(|_| GitError::PendingStateUnavailable)?
            .generation)
    }

    /// Reports whether the current quiet-period batch will produce a commit.
    ///
    /// # Errors
    ///
    /// Returns an error when pending state is unavailable.
    pub fn pending_commit_count(&self) -> Result<usize, GitError> {
        let pending = self.pending.lock().map_err(|_| GitError::PendingStateUnavailable)?;
        Ok(usize::from(!pending.paths.is_empty()))
    }

    /// Commits only if no newer event has reset the debounce generation.
    ///
    /// # Errors
    ///
    /// Returns a typed pending-state or commit error.
    pub fn commit_if_generation(&self, generation: u64) -> Result<GitCommitResult, GitError> {
        let mut pending = self.pending.lock().map_err(|_| GitError::PendingStateUnavailable)?;
        if pending.generation != generation {
            return Ok(GitCommitResult {
                commit_id: None,
                committed_paths: Vec::new(),
                message: None,
            });
        }
        self.commit_recording_failure(&mut pending, None)
    }

    /// Commits only paths staged by this service, preserving operator staging.
    ///
    /// # Errors
    ///
    /// Returns a typed Git error when the owned staged snapshot cannot be
    /// materialised or committed.
    pub fn commit(&self, message: Option<&str>) -> Result<GitCommitResult, GitError> {
        let mut pending = self.pending.lock().map_err(|_| GitError::PendingStateUnavailable)?;
        self.commit_recording_failure(&mut pending, message)
    }

    /// Commits the pending batch, remembering a failure (and counting repeats) so it can be reported, and
    /// forgetting it once a commit succeeds or nothing is left pending. The paths stay pending on failure.
    fn commit_recording_failure(
        &self,
        pending: &mut PendingBatch,
        message: Option<&str>,
    ) -> Result<GitCommitResult, GitError> {
        match self.commit_pending(pending, message) {
            Ok(result) => {
                pending.failure = None;
                Ok(result)
            }
            Err(error) => {
                let attempts = pending
                    .failure
                    .as_ref()
                    .map_or(1, |failure| failure.attempts.saturating_add(1));
                pending.failure = Some(GitCommitFailure {
                    code: error.code().to_owned(),
                    message: describe(&error),
                    attempts,
                    at_unix: self.clock.now().unix_timestamp(),
                });
                Err(error)
            }
        }
    }

    fn commit_pending(&self, pending: &mut PendingBatch, message: Option<&str>) -> Result<GitCommitResult, GitError> {
        if pending.paths.is_empty() {
            return Ok(GitCommitResult {
                commit_id: None,
                committed_paths: Vec::new(),
                message: None,
            });
        }

        let repository = open_repository(self.root.path())?;
        let parent = repository
            .head()
            .and_then(|head| head.peel_to_commit())
            .map_err(GitError::Commit)?;
        let parent_tree = parent.tree().map_err(GitError::Commit)?;
        let staged = repository.index().map_err(GitError::Commit)?;
        let mut owned_index = git2::Index::new().map_err(GitError::Commit)?;
        owned_index.read_tree(&parent_tree).map_err(GitError::Commit)?;

        for path in &pending.paths {
            owned_index.remove_all([path], None).map_err(GitError::Commit)?;
            for entry in staged.iter().filter(|entry| entry_is_within(entry, path)) {
                owned_index.add(&entry).map_err(GitError::Commit)?;
            }
        }

        let tree_id = owned_index.write_tree_to(&repository).map_err(GitError::Commit)?;
        let tree = repository.find_tree(tree_id).map_err(GitError::Commit)?;
        let commit_message = message.filter(|candidate| !candidate.trim().is_empty()).map_or_else(
            || generated_commit_message(&pending.events, pending.paths.len()),
            str::to_owned,
        );
        let signature = self.signature()?;
        let commit_id = repository
            .commit(Some("HEAD"), &signature, &signature, &commit_message, &tree, &[&parent])
            .map_err(GitError::Commit)?;
        let committed_paths = pending.paths.iter().cloned().collect::<Vec<_>>();
        if let Err(error) = self.clear_pending_paths(&committed_paths) {
            tracing::warn!(
                code = error.code(),
                error = %error,
                "committed Git pending-path metadata could not be cleared"
            );
        }
        pending.paths.clear();
        pending.events.clear();
        pending.staged_at_unix = None;
        pending.first_staged_at_unix = None;

        Ok(GitCommitResult {
            commit_id: Some(commit_id.to_string()),
            committed_paths,
            message: Some(commit_message),
        })
    }

    pub(crate) fn stage_event(&self, event: &OperationEvent) -> Result<(), GitError> {
        let mut pending = self.pending.lock().map_err(|_| GitError::PendingStateUnavailable)?;
        let repository = open_repository(self.root.path())?;
        let mut index = repository.index().map_err(GitError::Repository)?;

        for path in &event.paths {
            let relative = self.relative_path(path)?;
            self.record_pending_path(relative)?;
            let remove = event.kind == OpKind::Delete || (event.kind == OpKind::Move && !<&Path>::from(path).exists());
            if remove {
                index.remove_all([relative], None).map_err(GitError::Repository)?;
            } else {
                Self::stage_present_path(&mut index, relative, <&Path>::from(path))?;
            }
            pending.paths.insert(relative.to_path_buf());
        }
        index.write().map_err(GitError::Repository)?;
        pending.events.push(event.clone());
        pending.generation = pending.generation.saturating_add(1);
        let staged_at = self.clock.now().unix_timestamp();
        pending.staged_at_unix = Some(staged_at);
        pending.first_staged_at_unix.get_or_insert(staged_at);
        Ok(())
    }

    /// Records `path` as pending on disk, or, when it already is, notes that it was touched again: the marker's
    /// modification time is the path's most recent staging, on the injected clock, which is what lets the doctor
    /// tell a batch that has gone quiet from one that is still receiving changes.
    fn record_pending_path(&self, path: &Path) -> Result<(), GitError> {
        let text = path.to_str().ok_or_else(|| GitError::NonUtf8Path {
            path: path.to_path_buf(),
        })?;
        let directory = self.pending_directory()?;
        std::fs::create_dir_all(&directory).map_err(GitError::PendingMetadata)?;
        let digest = ContentHash::from(<[u8; 32]>::from(Sha256::digest(text.as_bytes())));
        let marker = directory.join(format!("{}.pending", <&str>::from(&digest)));
        let touched_at = SystemTime::from(self.clock.now());
        if marker.exists() {
            return std::fs::File::options()
                .write(true)
                .open(&marker)
                .and_then(|file| file.set_modified(touched_at))
                .map_err(GitError::PendingMetadata);
        }
        let temporary = marker.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            self.clock.now().unix_timestamp_nanos()
        ));
        let write_result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(GitError::PendingMetadata)?;
            file.write_all(text.as_bytes()).map_err(GitError::PendingMetadata)?;
            file.set_modified(touched_at).map_err(GitError::PendingMetadata)?;
            file.sync_all().map_err(GitError::PendingMetadata)?;
            std::fs::rename(&temporary, &marker).map_err(GitError::PendingMetadata)
        })();
        if write_result.is_err() {
            let _cleanup = std::fs::remove_file(&temporary);
        }
        write_result
    }

    /// Reports whether the current batch began at least `age` ago, however recently its latest change arrived.
    ///
    /// The debounce only resets as changes arrive, so under sustained activity it never fires; a caller uses this to
    /// cap how long that may go on.
    ///
    /// # Errors
    ///
    /// Returns an error when pending state is unavailable.
    pub fn batch_older_than(&self, age: std::time::Duration) -> Result<bool, GitError> {
        let pending = self.pending.lock().map_err(|_| GitError::PendingStateUnavailable)?;
        let now = self.clock.now().unix_timestamp();
        Ok(pending.first_staged_at_unix.is_some_and(|first| {
            u64::try_from(now.saturating_sub(first)).is_ok_and(|elapsed| elapsed >= age.as_secs())
        }))
    }

    pub(crate) fn recorded_pending_paths(&self) -> Result<BTreeSet<PathBuf>, GitError> {
        Ok(self.recorded_pending_markers()?.into_keys().collect())
    }

    /// Every path recorded as pending on disk, with when it was first recorded in its batch: the marker file's own
    /// modification time, which survives a server restart.
    pub(crate) fn recorded_pending_markers(&self) -> Result<BTreeMap<PathBuf, SystemTime>, GitError> {
        let directory = self.pending_directory()?;
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new());
            }
            Err(source) => return Err(GitError::PendingMetadata(source)),
        };
        let mut markers = BTreeMap::new();
        for entry in entries {
            let entry = entry.map_err(GitError::PendingMetadata)?;
            if entry.path().extension().and_then(std::ffi::OsStr::to_str) != Some("pending") {
                continue;
            }
            let text = std::fs::read_to_string(entry.path()).map_err(GitError::PendingMetadata)?;
            let path = PathBuf::from(text);
            if path.as_os_str().is_empty() || path.starts_with(".git") {
                return Err(GitError::InvalidPendingMetadata);
            }
            let text = path.to_str().ok_or(GitError::InvalidPendingMetadata)?;
            let digest = ContentHash::from(<[u8; 32]>::from(Sha256::digest(text.as_bytes())));
            let expected = format!("{}.pending", <&str>::from(&digest));
            if entry.file_name() != std::ffi::OsString::from(expected) {
                return Err(GitError::InvalidPendingMetadata);
            }
            let validated = self.vault_path_for_relative(&path)?;
            if validated.relative() != path {
                return Err(GitError::InvalidPendingMetadata);
            }
            let recorded_at = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .map_err(GitError::PendingMetadata)?;
            markers.insert(path, recorded_at);
        }
        Ok(markers)
    }

    pub(crate) fn clear_pending_paths(&self, paths: &[PathBuf]) -> Result<(), GitError> {
        let directory = self.pending_directory()?;
        for path in paths {
            let text = path
                .to_str()
                .ok_or_else(|| GitError::NonUtf8Path { path: path.clone() })?;
            let digest = ContentHash::from(<[u8; 32]>::from(Sha256::digest(text.as_bytes())));
            let marker = directory.join(format!("{}.pending", <&str>::from(&digest)));
            match std::fs::remove_file(marker) {
                Ok(()) => {}
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => return Err(GitError::PendingMetadata(source)),
            }
        }
        Ok(())
    }

    fn pending_directory(&self) -> Result<PathBuf, GitError> {
        Ok(open_repository(self.root.path())?.path().join(PENDING_DIRECTORY))
    }

    pub(crate) fn stage_present_path(
        index: &mut git2::Index,
        relative: &Path,
        absolute: &Path,
    ) -> Result<(), GitError> {
        // Git tracks files, folders, and links only. Anything else at the path, such as a named pipe that replaced a
        // note, holds no content Git can record, and libgit2 would open it the usual way and wait on it for ever, so
        // it is staged as gone, as Git itself treats it. A swap between this check and libgit2's open is not covered:
        // libgit2 opens the file itself.
        if absolute.is_dir() {
            return index
                .add_all([relative], IndexAddOption::DEFAULT, None)
                .map_err(GitError::Repository);
        }
        let special = std::fs::symlink_metadata(absolute)
            .is_ok_and(|metadata| !metadata.file_type().is_file() && !metadata.file_type().is_symlink());
        if special {
            index.remove_all([relative], None).map_err(GitError::Repository)
        } else {
            index.add_path(relative).map_err(GitError::Repository)
        }
    }
}

fn entry_is_within(entry: &IndexEntry, path: &Path) -> bool {
    let Some(path) = path.to_str() else {
        return false;
    };
    let normalised = path.replace('\\', "/");
    entry.path == normalised.as_bytes() || entry.path.starts_with(format!("{normalised}/").as_bytes())
}

/// An error with its causes appended, so a report says why (a locked reference, say) and not only what failed.
fn describe(error: &GitError) -> String {
    let mut text = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// Operation summaries listed in a generated message body; a larger operation is summarised by a count instead, so
/// a whole-vault rebuild reads as one commit rather than one line per index.
const MESSAGE_BODY_LINES: usize = 50;

/// `mcp: <who> (<n> files)`, then what was done. `<who>` is the tool or tools that made the change; work an
/// internal service did on a tool's behalf carries that tool's name, and only a batch with no tool at all is named
/// for the internal services themselves.
fn generated_commit_message(events: &[OperationEvent], files: usize) -> String {
    let mut tools = Vec::new();
    let mut services = Vec::new();
    for event in events {
        let (names, name) = match &event.origin {
            Origin::Tool(name) => (&mut tools, name),
            Origin::Internal(name) => (&mut services, name),
        };
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    let who = if tools.is_empty() { services } else { tools };
    let noun = if files == 1 { "file" } else { "files" };
    let mut message = if who.is_empty() {
        format!("mcp: changes ({files} {noun})")
    } else {
        format!("mcp: {} ({files} {noun})", who.join(", "))
    };
    if !events.is_empty() {
        message.push_str("\n\n");
        for event in events.iter().take(MESSAGE_BODY_LINES) {
            message.push_str(&event.summary);
            message.push('\n');
        }
        let omitted = events.len().saturating_sub(MESSAGE_BODY_LINES);
        if omitted > 0 {
            message.push_str("and ");
            message.push_str(&omitted.to_string());
            message.push_str(" more operations\n");
        }
    }
    message
}
