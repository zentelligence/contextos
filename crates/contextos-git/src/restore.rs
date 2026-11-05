use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use contextos_core::{
    Clock, ContentHash, DeleteMode, DeleteMutation, OperationEvent, OperationWarning, Origin, RestoreMutation,
    VaultPath, WritesVault,
};
use git2::Repository;
use sha2::{Digest, Sha256};
use similar::TextDiff;

use crate::{Git2Vault, GitError, GitWriteError, open_repository};

/// Validated inputs for one forward-only historical restoration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitRestoreRequest {
    pub path: VaultPath,
    pub reference: String,
    pub dry_run: bool,
}

/// Historical restoration preview or applied outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitRestoreResult {
    pub diff: String,
    pub applied: bool,
    pub events: Vec<OperationEvent>,
    pub warnings: Vec<OperationWarning>,
}

/// What a restore would change: the diff that shows it, the files to write, and the owned files to delete.
struct RestorePlan {
    diff: String,
    writes: Vec<(VaultPath, String)>,
    deletes: Vec<VaultPath>,
}

/// Carries out `plan` through the shared write pipeline, each file as its own mutation.
fn apply_restore<W>(writer: &W, plan: RestorePlan) -> Result<GitRestoreResult, GitWriteError<W::Error>>
where
    W: WritesVault,
    W::Error: std::error::Error + 'static,
{
    let mut events = Vec::new();
    let mut warnings = Vec::new();
    for (path, content) in plan.writes {
        let (_, expected_hash) = current_text_and_hash(&path)?;
        let result = writer
            .restore(&RestoreMutation {
                path,
                content,
                expected_hash,
                origin: Origin::Tool("git_restore".to_owned()),
            })
            .map_err(GitWriteError::Write)?;
        events.extend(result.event);
        warnings.extend(result.warnings);
    }
    for path in plan.deletes {
        let mode = if <&Path>::from(&path).is_dir() {
            DeleteMode::HardRecursive
        } else {
            DeleteMode::Hard
        };
        let result = writer
            .delete(&DeleteMutation {
                path,
                mode,
                origin: Origin::Tool("git_restore".to_owned()),
            })
            .map_err(GitWriteError::Write)?;
        events.extend(result.event);
        warnings.extend(result.warnings);
    }
    Ok(GitRestoreResult {
        diff: plan.diff,
        applied: true,
        events,
        warnings,
    })
}

impl<C> Git2Vault<C>
where
    C: Clock,
{
    /// Restores one historical UTF-8 file or directory through the shared pipeline.
    ///
    /// # Errors
    ///
    /// Returns a typed reference, object, encoding, read, or persistence error.
    pub fn restore<W>(
        &self,
        request: &GitRestoreRequest,
        writer: &W,
    ) -> Result<GitRestoreResult, GitWriteError<W::Error>>
    where
        W: WritesVault,
        W::Error: std::error::Error + 'static,
    {
        let plan = self.plan_restore(request)?;
        if request.dry_run {
            return Ok(GitRestoreResult {
                diff: plan.diff,
                applied: false,
                events: Vec::new(),
                warnings: Vec::new(),
            });
        }
        apply_restore(writer, plan)
    }

    /// Works out what restoring `request` would write and delete, and the diff that shows it, without changing
    /// anything.
    fn plan_restore(&self, request: &GitRestoreRequest) -> Result<RestorePlan, GitError> {
        let relative = self.relative_path(&request.path)?;
        if self.is_restore_protected(relative) {
            return Err(GitError::ProtectedRestorePath);
        }
        let repository = open_repository(self.root.path())?;
        let object = repository
            .revparse_single(&request.reference)
            .map_err(GitError::Repository)?;
        let commit = object.peel_to_commit().map_err(GitError::Repository)?;
        let tree = commit.tree().map_err(GitError::Repository)?;
        let historical = historical_files(&repository, &tree, relative)?
            .into_iter()
            .filter(|(path, _)| !self.is_restore_protected(path))
            .collect::<BTreeMap<_, _>>();
        let historical_paths = historical.keys().cloned().collect::<BTreeSet<_>>();

        let mut writes = Vec::new();
        let mut diff = String::new();
        for (path, content) in historical {
            let vault_path = self.vault_path_for_relative(&path)?;
            let (current, _) = current_text_and_hash(&vault_path)?;
            if current == content {
                continue;
            }
            diff.push_str(
                &TextDiff::from_lines(&current, &content)
                    .unified_diff()
                    .header("working tree", &request.reference)
                    .to_string(),
            );
            writes.push((vault_path, content));
        }
        let deletes = self.owned_deletions(relative, &historical_paths)?;
        if !deletes.is_empty() && !self.allow_destructive_restore {
            return Err(GitError::DestructiveRestoreDisabled);
        }
        for path in &deletes {
            let relative = path.relative().to_str().ok_or_else(|| GitError::NonUtf8Path {
                path: path.relative().to_path_buf(),
            })?;
            diff.push_str("delete ");
            diff.push_str(relative);
            diff.push('\n');
        }
        Ok(RestorePlan { diff, writes, deletes })
    }

    fn owned_deletions(&self, selected: &Path, historical: &BTreeSet<PathBuf>) -> Result<Vec<VaultPath>, GitError> {
        let pending = self.pending.lock().map_err(|_| GitError::PendingStateUnavailable)?;
        let mut relative_paths = BTreeSet::new();
        for path in &pending.paths {
            if self.is_restore_protected(path) || self.holds_restore_protected(path) {
                continue;
            }
            if !selected.as_os_str().is_empty() && !path.starts_with(selected) {
                continue;
            }
            if historical
                .iter()
                .any(|historical_path| historical_path == path || historical_path.starts_with(path))
            {
                continue;
            }
            if self.root.path().join(path).exists() {
                relative_paths.insert(path.clone());
            }
        }
        drop(pending);
        let mut paths = relative_paths
            .into_iter()
            .map(|path| self.vault_path_for_relative(&path))
            .collect::<Result<Vec<_>, _>>()?;
        paths.sort_by(|left, right| {
            right
                .relative()
                .components()
                .count()
                .cmp(&left.relative().components().count())
                .then_with(|| right.relative().cmp(left.relative()))
        });
        Ok(paths)
    }

    /// Whether `path` is a folder that contains a protected path, so deleting it would delete that path too.
    fn holds_restore_protected(&self, path: &Path) -> bool {
        self.protected_restore_paths
            .iter()
            .any(|protected| protected != path && protected.starts_with(path))
    }

    fn is_restore_protected(&self, path: &Path) -> bool {
        self.protected_restore_paths
            .iter()
            .any(|protected| path == protected || path.starts_with(protected))
    }
}

fn historical_files(
    repository: &Repository,
    tree: &git2::Tree<'_>,
    selected: &Path,
) -> Result<BTreeMap<PathBuf, String>, GitError> {
    let mut files = BTreeMap::new();
    if selected.as_os_str().is_empty() {
        collect_historical_files(repository, tree, Path::new(""), &mut files)?;
        return Ok(files);
    }
    let entry = tree.get_path(selected).map_err(GitError::Repository)?;
    match entry.kind() {
        Some(git2::ObjectType::Blob) => {
            files.insert(
                selected.to_path_buf(),
                historical_blob(repository, entry.id(), selected)?,
            );
        }
        Some(git2::ObjectType::Tree) => {
            let subtree = repository.find_tree(entry.id()).map_err(GitError::Repository)?;
            collect_historical_files(repository, &subtree, selected, &mut files)?;
        }
        _ => return Err(GitError::UnsupportedRestoreObject),
    }
    Ok(files)
}

fn collect_historical_files(
    repository: &Repository,
    tree: &git2::Tree<'_>,
    base: &Path,
    files: &mut BTreeMap<PathBuf, String>,
) -> Result<(), GitError> {
    for entry in tree {
        let name = entry.name().map_err(|_| GitError::NonUtf8Path {
            path: base.to_path_buf(),
        })?;
        let path = base.join(name);
        match entry.kind() {
            Some(git2::ObjectType::Blob) => {
                files.insert(path.clone(), historical_blob(repository, entry.id(), &path)?);
            }
            Some(git2::ObjectType::Tree) => {
                let subtree = repository.find_tree(entry.id()).map_err(GitError::Repository)?;
                collect_historical_files(repository, &subtree, &path, files)?;
            }
            _ => return Err(GitError::UnsupportedRestoreObject),
        }
    }
    Ok(())
}

fn historical_blob(repository: &Repository, id: git2::Oid, path: &Path) -> Result<String, GitError> {
    let blob = repository.find_blob(id).map_err(GitError::Repository)?;
    Ok(std::str::from_utf8(blob.content())
        .map_err(|source| GitError::NonUtf8Restore {
            path: path.to_path_buf(),
            source,
        })?
        .to_owned())
}

fn current_text_and_hash(path: &VaultPath) -> Result<(String, Option<ContentHash>), GitError> {
    let target = <&Path>::from(path);
    match contextos_fs::read_regular_file(target) {
        Ok(bytes) => {
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let hash = ContentHash::from(<[u8; 32]>::from(hasher.finalize()));
            let content = String::from_utf8(bytes).map_err(|source| GitError::NonUtf8Current {
                path: path.relative().to_path_buf(),
                source,
            })?;
            Ok((content, Some(hash)))
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok((String::new(), None)),
        Err(source) => Err(GitError::ReadRestoreTarget {
            path: path.relative().to_path_buf(),
            source,
        }),
    }
}
