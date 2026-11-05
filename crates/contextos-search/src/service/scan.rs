//! Filesystem walk and helper functions shared by the combined search
//! service: markdown discovery, exclude-prefix matching, throwaway
//! [`VaultSet`] construction, and derived-store modification-time probing.

use std::fs;
use std::path::{Path, PathBuf};

use contextos_core::{PathError, VaultRoot, VaultSet};
use contextos_fs::HiddenPaths;
use time::OffsetDateTime;

use crate::SearchError;

/// Reports whether `relative` is an in-scope markdown path: it must end
/// with `.md` and must not fall under any configured exclude prefix. This
/// mirrors `sync::in_scope`, duplicated here (rather than shared) to keep
/// the already-delivered `sync` module unchanged.
pub(super) fn is_markdown(relative: &str) -> bool {
    Path::new(relative)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

pub(super) fn is_excluded(excludes: &[String], relative: &str) -> bool {
    excludes.iter().any(|prefix| {
        relative == prefix.as_str()
            || relative
                .strip_prefix(prefix.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

fn forward_slash(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

pub(super) fn walk_markdown(
    root: &Path,
    directory: &Path,
    excludes: &[String],
    files: &mut Vec<(String, PathBuf)>,
) -> Result<(), SearchError> {
    let entries = fs::read_dir(directory).map_err(|source| SearchError::DocumentRead {
        path: directory.display().to_string(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| SearchError::DocumentRead {
            path: directory.display().to_string(),
            source,
        })?;
        let path = entry.path();
        let Ok(relative_path) = path.strip_prefix(root) else {
            continue;
        };
        let relative = forward_slash(relative_path);
        if is_excluded(excludes, &relative) {
            continue;
        }
        let file_type = entry.file_type().map_err(|source| SearchError::DocumentRead {
            path: path.display().to_string(),
            source,
        })?;
        if file_type.is_dir() {
            walk_markdown(root, &path, excludes, files)?;
        } else if file_type.is_file() && is_markdown(&relative) {
            files.push((relative, path));
        }
    }
    Ok(())
}

/// Collects every regular file under `directory` (the vault root when called from outside), as a forward-slash path
/// relative to `root`, leaving out what the vault hides. Symbolic links are not followed or listed. This is the listing
/// links resolve against: it is not narrowed by the search exclusions, because a link to an image, a canvas, or a note
/// in an excluded folder resolves all the same, but it is narrowed by the hidden patterns every other tool honours, so
/// a link never reaches a file they all hide.
pub(super) fn walk_files(
    root: &Path,
    directory: &Path,
    hidden: &HiddenPaths,
    files: &mut Vec<String>,
) -> Result<(), SearchError> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        // Removed while the walk was under way: it is no longer in the vault.
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(SearchError::DocumentRead {
                path: directory.display().to_string(),
                source,
            });
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(SearchError::DocumentRead {
                    path: directory.display().to_string(),
                    source,
                });
            }
        };
        let path = entry.path();
        let Ok(relative_path) = path.strip_prefix(root) else {
            continue;
        };
        let relative = forward_slash(relative_path);
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if hidden.hides(&relative, file_type.is_dir()) {
            continue;
        }
        if file_type.is_dir() {
            walk_files(root, &path, hidden, files)?;
        } else if file_type.is_file() {
            files.push(relative);
        }
    }
    Ok(())
}

/// Builds a single-root `VaultSet` from the already-resolved vault root,
/// used to construct throwaway `VaultPath`s for files discovered by the
/// rebuild walk.
pub(super) fn resolve_single_root_set(root: &Path) -> Result<VaultSet, SearchError> {
    let vault_root = VaultRoot::try_from(root.to_path_buf()).map_err(|source| root_set_error(root, source))?;
    VaultSet::try_from(vec![vault_root]).map_err(|source| root_set_error(root, source))
}

fn root_set_error(root: &Path, source: PathError) -> SearchError {
    SearchError::IndexDirectory {
        path: root.display().to_string(),
        source: std::io::Error::other(source),
    }
}

/// Returns the newest modification time of any file under `dir`, or `None`
/// when `dir` is missing or contains no files.
pub(super) fn newest_mtime_under(dir: &Path) -> Result<Option<OffsetDateTime>, SearchError> {
    let mut newest: Option<OffsetDateTime> = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(SearchError::DocumentRead {
                    path: current.display().to_string(),
                    source,
                });
            }
        };
        for entry in entries {
            let entry = entry.map_err(|source| SearchError::DocumentRead {
                path: current.display().to_string(),
                source,
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|source| SearchError::DocumentRead {
                path: path.display().to_string(),
                source,
            })?;
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            let modified = OffsetDateTime::from(modified);
            if newest.is_none_or(|current_newest| modified > current_newest) {
                newest = Some(modified);
            }
        }
    }
    Ok(newest)
}
