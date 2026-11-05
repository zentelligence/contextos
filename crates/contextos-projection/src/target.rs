use std::cell::RefCell;
use std::fs::{self, Metadata};
use std::io;
use std::path::{Path, PathBuf};

use contextos_core::ContentHash;
use contextos_core::registry_layout::is_managed_index;
use contextos_fs::host_file::{
    FolderRecovery, FolderReplaceObserver, NoFolderFaults, ReplaceStep, recover_folder, remove_entry, replace_folder,
    write_file_atomically,
};
#[cfg(test)]
use contextos_fs::links::FILE_ATTRIBUTE_REPARSE_POINT;
use contextos_fs::links::{EntryClass, EntryFlags};

use crate::hash::content_hash;
use crate::vault_source::{MAX_SKILL_BYTES, MAX_SKILL_DEPTH, MAX_SKILL_FILES};
use crate::{FileExtension, ProjectionError, SkillFilePath, SkillFolder, Slug, TargetLimit, TargetRootRejection};

/// A trusted projection target root and the only surface through which anything below it is written.
///
/// The root comes from configuration and is resolved once when the target is built (the root itself may be a link the
/// operator made). Everything below it is addressed by validated names and re-checked for links before every
/// operation; no method accepts a raw path. A residual race remains between the final check and the filesystem call
/// (no portable way to pin a folder handle exists); the host-level projection lock and the checks immediately before
/// each write and before a folder is installed narrow it to an attacker with write access to the target itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectionTarget {
    /// The path as configured, re-resolved at every operation.
    configured: PathBuf,
    /// Where `configured` resolved to when the target was built.
    root: PathBuf,
}

impl TryFrom<&Path> for ProjectionTarget {
    type Error = ProjectionError;

    fn try_from(value: &Path) -> Result<Self, Self::Error> {
        if !value.is_absolute() {
            return Err(ProjectionError::TargetRootRejected {
                reason: TargetRootRejection::NotAbsolute,
            });
        }
        let root = dunce::canonicalize(value).map_err(|source| ProjectionError::TargetRootUnresolvable { source })?;
        if !root.is_dir() {
            return Err(ProjectionError::TargetRootRejected {
                reason: TargetRootRejection::NotFolder,
            });
        }
        Ok(Self {
            configured: value.to_path_buf(),
            root,
        })
    }
}

impl<'a> From<&'a ProjectionTarget> for &'a Path {
    fn from(value: &'a ProjectionTarget) -> Self {
        &value.root
    }
}

/// What an entry that already exists below the root must be.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Kind {
    File,
    Folder,
}

/// Refuses an entry that is a link or a reparse point, naming it by `entry`, its path below the target root.
fn refuse_links(metadata: &Metadata, entry: &str) -> Result<(), ProjectionError> {
    match EntryClass::from(EntryFlags::from(metadata)) {
        EntryClass::Plain => Ok(()),
        EntryClass::Link => Err(ProjectionError::LinkBelowRoot {
            entry: entry.to_owned(),
        }),
        EntryClass::ReparsePoint => Err(ProjectionError::ReparsePoint {
            entry: entry.to_owned(),
        }),
    }
}

impl ProjectionTarget {
    /// Refuses the target when its configured path no longer resolves to the root it had at start. A link the operator
    /// made is followed once, at start; a later re-link could lead the write into a vault or another host's files.
    fn confirm_root_unchanged(&self) -> Result<(), ProjectionError> {
        let now = dunce::canonicalize(&self.configured)
            .map_err(|source| ProjectionError::TargetRootUnresolvable { source })?;
        if now == self.root {
            Ok(())
        } else {
            Err(ProjectionError::TargetRootRejected {
                reason: TargetRootRejection::Relinked,
            })
        }
    }

    /// Resolves `components` below the root, rejecting any existing component that is a link or the wrong kind.
    /// Components that do not exist yet are fine: they will be created by the write. Returns the path and whether the
    /// final component exists.
    fn locate(&self, components: &[&str], last: Kind) -> Result<(PathBuf, bool), ProjectionError> {
        self.confirm_root_unchanged()?;
        let mut current = self.root.clone();
        let mut relative = String::new();
        for (index, component) in components.iter().enumerate() {
            current.push(component);
            if !relative.is_empty() {
                relative.push('/');
            }
            relative.push_str(component);
            let metadata = match current.symlink_metadata() {
                Ok(metadata) => metadata,
                Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok((current, false)),
                Err(source) => {
                    return Err(ProjectionError::Read {
                        entry: relative,
                        source,
                    });
                }
            };
            refuse_links(&metadata, &relative)?;
            let expected = if index + 1 == components.len() {
                last
            } else {
                Kind::Folder
            };
            let fits = match expected {
                Kind::File => metadata.is_file(),
                Kind::Folder => metadata.is_dir(),
            };
            if !fits {
                return Err(ProjectionError::UnsupportedEntry { entry: relative });
            }
        }
        Ok((current, true))
    }

    fn agent_name(slug: &Slug, extension: &FileExtension) -> String {
        format!("{}.{}", <&str>::from(slug), <&str>::from(extension))
    }

    /// Writes an agent file `<slug>.<extension>` and returns the hash of what was written.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::LinkBelowRoot`] or [`ProjectionError::UnsupportedEntry`] when the destination or any
    /// existing component below the root is a link or not a regular entry, and [`ProjectionError::Write`] when the
    /// write fails.
    pub fn write_agent_file(
        &self,
        slug: &Slug,
        extension: &FileExtension,
        contents: &[u8],
    ) -> Result<ContentHash, ProjectionError> {
        let name = Self::agent_name(slug, extension);
        let (path, _) = self.locate(&[&name], Kind::File)?;
        write_file_atomically(&path, contents)?;
        Ok(content_hash(contents))
    }

    /// Removes the agent file `<slug>.<extension>`, reporting whether it existed.
    ///
    /// # Errors
    ///
    /// As [`Self::write_agent_file`], with [`ProjectionError::Write`] when the removal fails.
    pub fn remove_agent_file(&self, slug: &Slug, extension: &FileExtension) -> Result<bool, ProjectionError> {
        let name = Self::agent_name(slug, extension);
        let (path, exists) = self.locate(&[&name], Kind::File)?;
        if !exists {
            return Ok(false);
        }
        Ok(remove_entry(&path)?)
    }

    /// The hash of the agent file `<slug>.<extension>`, or `None` when absent.
    ///
    /// # Errors
    ///
    /// As [`Self::write_agent_file`], and [`ProjectionError::Read`] when the file cannot be read.
    pub fn agent_file_hash(
        &self,
        slug: &Slug,
        extension: &FileExtension,
    ) -> Result<Option<ContentHash>, ProjectionError> {
        let name = Self::agent_name(slug, extension);
        let (path, exists) = self.locate(&[&name], Kind::File)?;
        if !exists {
            return Ok(None);
        }
        let read = |source| ProjectionError::Read {
            entry: name.clone(),
            source,
        };
        // The size comes from metadata, so an oversized file is refused without being read.
        let length = fs::metadata(&path).map_err(read)?.len();
        if length > MAX_SKILL_BYTES as u64 {
            return Err(ProjectionError::TargetEntryOverLimit {
                entry: name,
                limit: TargetLimit::Bytes,
            });
        }
        let bytes = fs::read(&path).map_err(read)?;
        Ok(Some(content_hash(&bytes)))
    }

    /// Replaces the skill folder `<slug>` with `folder` atomically and returns the hash of the folder written.
    ///
    /// # Errors
    ///
    /// As [`Self::write_agent_file`].
    pub fn replace_skill_folder(&self, slug: &Slug, folder: &SkillFolder) -> Result<ContentHash, ProjectionError> {
        self.replace_skill_folder_observed(slug, folder, &NoFolderFaults)
    }

    /// As [`Self::replace_skill_folder`], reporting each step to `observer` so tests can interrupt it. Confinement is
    /// checked again once the new folder is built, immediately before it is installed.
    ///
    /// # Errors
    ///
    /// As [`Self::write_agent_file`].
    pub fn replace_skill_folder_observed(
        &self,
        slug: &Slug,
        folder: &SkillFolder,
        observer: &dyn FolderReplaceObserver,
    ) -> Result<ContentHash, ProjectionError> {
        let name = <&str>::from(slug);
        let (path, _) = self.locate(&[name], Kind::Folder)?;
        let guard = Recheck {
            target: self,
            name,
            inner: observer,
            failure: RefCell::new(None),
        };
        let result = replace_folder(&path, |staging| write_files(staging, folder), &guard);
        if let Some(error) = guard.failure.into_inner() {
            // The replacement was abandoned before anything was moved; clear the half-built sibling.
            let _ = recover_folder(&path);
            return Err(error);
        }
        result?;
        Ok(ContentHash::from(folder))
    }

    /// Removes the skill folder `<slug>`, reporting whether it existed. Any interrupted replacement is repaired first.
    ///
    /// # Errors
    ///
    /// As [`Self::write_agent_file`].
    pub fn remove_skill_folder(&self, slug: &Slug) -> Result<bool, ProjectionError> {
        let (path, _) = self.locate(&[<&str>::from(slug)], Kind::Folder)?;
        recover_folder(&path)?;
        Ok(remove_entry(&path)?)
    }

    /// Reads the skill folder `<slug>`, or `None` when absent.
    ///
    /// # Errors
    ///
    /// As [`Self::agent_file_hash`], [`ProjectionError::LinkBelowRoot`] or [`ProjectionError::UnsupportedEntry`] for a
    /// link or special file inside it, and [`ProjectionError::InvalidSkillPath`] for a file name that is not valid.
    pub fn read_skill_folder(&self, slug: &Slug) -> Result<Option<SkillFolder>, ProjectionError> {
        let name = <&str>::from(slug);
        let (path, exists) = self.locate(&[name], Kind::Folder)?;
        if !exists {
            return Ok(None);
        }
        let mut files = Vec::new();
        collect_files(&path, name, &mut files)?;
        SkillFolder::try_from(files).map(Some)
    }

    /// Repairs an interrupted replacement of the skill folder `<slug>`.
    ///
    /// # Errors
    ///
    /// As [`Self::write_agent_file`].
    pub fn recover_skill_folder(&self, slug: &Slug) -> Result<FolderRecovery, ProjectionError> {
        let (path, _) = self.locate(&[<&str>::from(slug)], Kind::Folder)?;
        Ok(recover_folder(&path)?)
    }
}

/// Writes every file of `folder` into the freshly created staging folder.
fn write_files(staging: &Path, folder: &SkillFolder) -> io::Result<()> {
    for (path, bytes) in folder.files() {
        let mut destination = staging.to_path_buf();
        destination.extend(<&str>::from(path).split('/'));
        write_file_atomically(&destination, bytes).map_err(io::Error::other)?;
    }
    Ok(())
}

/// A file found below a target skill folder, not yet read.
struct Found {
    path: PathBuf,
    inside: String,
}

/// What a skill folder in a target has used of the file-count and byte limits so far.
#[derive(Default)]
struct Budget {
    files: usize,
    bytes: u64,
}

/// Lists every regular file below `directory`, refusing links, reparse points, special files, and anything beyond the
/// same file-count, depth, and byte limits a skill definition has in a vault. Sizes come from the listing, so nothing
/// over a limit is read, and a managed index at any level is not part of the skill.
fn walk_files(
    directory: &Path,
    prefix: &str,
    entry: &str,
    depth: usize,
    budget: &mut Budget,
    found: &mut Vec<Found>,
) -> Result<(), ProjectionError> {
    let over = |limit| ProjectionError::TargetEntryOverLimit {
        entry: entry.to_owned(),
        limit,
    };
    if depth > MAX_SKILL_DEPTH {
        return Err(over(TargetLimit::Depth));
    }
    let read = |source| ProjectionError::Read {
        entry: entry.to_owned(),
        source,
    };
    for item in fs::read_dir(directory).map_err(read)? {
        let item = item.map_err(read)?;
        let name = item
            .file_name()
            .into_string()
            .map_err(|_| ProjectionError::UnsupportedEntry {
                entry: entry.to_owned(),
            })?;
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let metadata = item.path().symlink_metadata().map_err(read)?;
        refuse_links(&metadata, &format!("{entry}/{relative}"))?;
        if metadata.is_dir() {
            walk_files(&item.path(), &relative, entry, depth.saturating_add(1), budget, found)?;
        } else if metadata.is_file() {
            if is_managed_index(&name) {
                continue;
            }
            budget.files = budget.files.saturating_add(1);
            if budget.files > MAX_SKILL_FILES {
                return Err(over(TargetLimit::Files));
            }
            budget.bytes = budget.bytes.saturating_add(metadata.len());
            if budget.bytes > MAX_SKILL_BYTES as u64 {
                return Err(over(TargetLimit::Bytes));
            }
            found.push(Found {
                path: item.path(),
                inside: relative,
            });
        } else {
            return Err(ProjectionError::UnsupportedEntry {
                entry: format!("{entry}/{relative}"),
            });
        }
    }
    Ok(())
}

/// Reads every regular file below `directory` as [`walk_files`] lists them.
fn collect_files(
    directory: &Path,
    entry: &str,
    files: &mut Vec<(SkillFilePath, Vec<u8>)>,
) -> Result<(), ProjectionError> {
    let mut found = Vec::new();
    walk_files(directory, "", entry, 0, &mut Budget::default(), &mut found)?;
    for file in found {
        let bytes = fs::read(&file.path).map_err(|source| ProjectionError::Read {
            entry: entry.to_owned(),
            source,
        })?;
        files.push((SkillFilePath::try_from(file.inside.as_str())?, bytes));
    }
    Ok(())
}

/// Forwards each step to the caller's observer, then re-verifies confinement once the new folder is built. A
/// confinement failure is stashed (the observer can only return an I/O error) and surfaced by the caller.
struct Recheck<'a> {
    target: &'a ProjectionTarget,
    name: &'a str,
    inner: &'a dyn FolderReplaceObserver,
    failure: RefCell<Option<ProjectionError>>,
}

impl FolderReplaceObserver for Recheck<'_> {
    fn reached(&self, step: ReplaceStep) -> io::Result<()> {
        self.inner.reached(step)?;
        if step == ReplaceStep::NewFolderBuilt
            && let Err(error) = self.target.locate(&[self.name], Kind::Folder)
        {
            *self.failure.borrow_mut() = Some(error);
            return Err(io::Error::other("confinement check failed before install"));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "target_test.rs"]
mod tests;
