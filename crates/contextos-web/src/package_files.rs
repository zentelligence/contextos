//! The files a shared workflow's script reads, from the installed release its run resolved (`workflow_file` and
//! `workflow_files`).
//!
//! An installed release is immutable and holds no link, since an install refuses an archive member that is not a
//! regular file or a folder ([`crate::package_bundle`]). A link found here anyway, at any level beneath the release, is
//! refused rather than followed, so a store changed by hand still cannot hand a script anything outside its release.
//! Every read is bounded before content is held, and runs on a blocking thread.

use std::fs::{self, File, Metadata};
use std::future::Future;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::pin::Pin;

use contextos_core::ContentHash;
use contextos_workflows::{
    MAX_WORKFLOW_FILE_BYTES, WorkflowFailureKind, WorkflowFile, WorkflowFileFailure, WorkflowFilePath,
    WorkflowFileSource,
};
use sha2::{Digest, Sha256};

/// The files of one installed release, rooted at its version directory.
#[derive(Clone, Debug)]
pub struct InstalledPackageFiles {
    root: PathBuf,
}

impl From<PathBuf> for InstalledPackageFiles {
    /// `root` is the release's own directory, `packages/{name}/{version}/`.
    fn from(root: PathBuf) -> Self {
        Self { root }
    }
}

impl WorkflowFileSource for InstalledPackageFiles {
    fn read<'a>(
        &'a self,
        path: &'a WorkflowFilePath,
    ) -> Pin<Box<dyn Future<Output = Result<WorkflowFile, WorkflowFileFailure>> + Send + 'a>> {
        let root = self.root.clone();
        let path = path.clone();
        Box::pin(async move { on_blocking_thread(move || read_file(&root, &path)).await })
    }

    fn list<'a>(
        &'a self,
        entry: &'a WorkflowFilePath,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WorkflowFilePath>, WorkflowFileFailure>> + Send + 'a>> {
        let root = self.root.clone();
        let entry = entry.clone();
        Box::pin(async move { on_blocking_thread(move || list_entry(&root, &entry, limit)).await })
    }
}

async fn on_blocking_thread<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, WorkflowFileFailure> + Send + 'static,
) -> Result<T, WorkflowFileFailure> {
    tokio::task::spawn_blocking(work).await.unwrap_or_else(|join_error| {
        Err(WorkflowFileFailure::new(
            WorkflowFailureKind::FileUnreadable,
            format!("the read did not complete: {join_error}"),
        ))
    })
}

/// The size of the file at `path` in the release at `root`, from its details and without reading it, refusing a link at
/// any level as a read does. A folder has no size a module could have.
pub(crate) fn file_size(root: &Path, path: &WorkflowFilePath) -> Result<u64, WorkflowFileFailure> {
    let (_absolute, metadata) = resolve(root, path)?;
    if !metadata.is_file() {
        return Err(not_found(path, "is a folder, not a file"));
    }
    Ok(metadata.len())
}

pub(crate) fn read_file(root: &Path, path: &WorkflowFilePath) -> Result<WorkflowFile, WorkflowFileFailure> {
    let (absolute, metadata) = resolve(root, path)?;
    if !metadata.is_file() {
        return Err(not_found(path, "is a folder, not a file"));
    }
    if metadata.len() > MAX_WORKFLOW_FILE_BYTES {
        return Err(too_large(path));
    }
    let mut bytes = Vec::new();
    File::open(&absolute)
        .and_then(|file| {
            file.take(MAX_WORKFLOW_FILE_BYTES.saturating_add(1))
                .read_to_end(&mut bytes)
        })
        .map_err(|error| unreadable(path, &error))?;
    // The file may have grown since its size was read; what was read is what is judged.
    if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_WORKFLOW_FILE_BYTES) {
        return Err(too_large(path));
    }
    let content_hash = ContentHash::from(<[u8; 32]>::from(Sha256::digest(&bytes)));
    let content = String::from_utf8(bytes)
        .map_err(|_| WorkflowFileFailure::new(WorkflowFailureKind::FileNotText, format!("{path} is not UTF-8 text")))?;
    Ok(WorkflowFile { content, content_hash })
}

fn list_entry(
    root: &Path,
    entry: &WorkflowFilePath,
    limit: usize,
) -> Result<Vec<WorkflowFilePath>, WorkflowFileFailure> {
    let (absolute, metadata) = resolve(root, entry)?;
    if metadata.is_file() {
        return Ok(vec![entry.clone()]);
    }
    let mut found = Vec::new();
    walk(&absolute, entry, limit, &mut found)?;
    Ok(found)
}

/// Adds every regular file beneath `folder` to `found`, refusing a link, a name no script could address, and more than
/// `limit` files.
fn walk(
    folder: &Path,
    relative: &WorkflowFilePath,
    limit: usize,
    found: &mut Vec<WorkflowFilePath>,
) -> Result<(), WorkflowFileFailure> {
    let entries = fs::read_dir(folder).map_err(|error| unreadable(relative, &error))?;
    for child in entries {
        let child = child.map_err(|error| unreadable(relative, &error))?;
        let name = child.file_name();
        let child_path = name
            .to_str()
            .and_then(|name| WorkflowFilePath::try_from(format!("{relative}/{name}").as_str()).ok())
            .ok_or_else(|| {
                WorkflowFileFailure::new(
                    WorkflowFailureKind::FileUnreadable,
                    format!(
                        "{relative} holds \"{}\", a name no workflow path can address",
                        name.to_string_lossy()
                    ),
                )
            })?;
        let file_type = child.file_type().map_err(|error| unreadable(&child_path, &error))?;
        if file_type.is_symlink() {
            return Err(link(&child_path));
        }
        if file_type.is_dir() {
            walk(&child.path(), &child_path, limit, found)?;
        } else if file_type.is_file() {
            found.push(child_path);
            if found.len() > limit {
                return Err(WorkflowFileFailure::new(
                    WorkflowFailureKind::FileTooLarge,
                    format!("{relative} holds more than {limit} files"),
                ));
            }
        }
    }
    Ok(())
}

/// The absolute path of `path` within `root` and its metadata, refusing a link at any level from the root down.
fn resolve(root: &Path, path: &WorkflowFilePath) -> Result<(PathBuf, Metadata), WorkflowFileFailure> {
    let mut absolute = root.to_path_buf();
    let mut metadata = None;
    for segment in path.segments() {
        absolute.push(segment);
        let found = match fs::symlink_metadata(&absolute) {
            Ok(found) => found,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(not_found(path, "is not in the release"));
            }
            Err(error) => return Err(unreadable(path, &error)),
        };
        if found.file_type().is_symlink() {
            return Err(link(path));
        }
        metadata = Some(found);
    }
    // A path always has at least one segment, so the loop has always set this.
    metadata
        .map(|metadata| (absolute, metadata))
        .ok_or_else(|| not_found(path, "is empty"))
}

fn not_found(path: &WorkflowFilePath, why: &str) -> WorkflowFileFailure {
    WorkflowFileFailure::new(WorkflowFailureKind::FileNotFound, format!("{path} {why}"))
}

fn too_large(path: &WorkflowFilePath) -> WorkflowFileFailure {
    WorkflowFileFailure::new(
        WorkflowFailureKind::FileTooLarge,
        format!("{path} is larger than the {MAX_WORKFLOW_FILE_BYTES} byte limit"),
    )
}

fn link(path: &WorkflowFilePath) -> WorkflowFileFailure {
    WorkflowFileFailure::new(
        WorkflowFailureKind::FileUnreadable,
        format!("{path} is or passes through a link, which an installed release never holds"),
    )
}

fn unreadable(path: &WorkflowFilePath, error: &io::Error) -> WorkflowFileFailure {
    WorkflowFileFailure::new(
        WorkflowFailureKind::FileUnreadable,
        format!("{path} could not be read: {error}"),
    )
}

#[cfg(test)]
#[path = "package_files_test.rs"]
mod tests;
