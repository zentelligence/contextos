use std::path::PathBuf;

use contextos_core::registry_layout::{
    AGENT_EXTENSION, AGENTS, INSTRUCTIONS, ORIENTATION_FILE, ROOT, SKILLS, is_managed_index,
};
use contextos_core::{PathError, VaultChildInput, VaultPath, VaultPathInput, VaultRootId, VaultSet};
use contextos_fs::{
    AttachmentRequest, EntryKind, Filesystem, FsError, ListDirectoryRequest, ListDirectoryWithSizesRequest, SortBy,
};

/// The most files one skill folder may hold.
pub(crate) const MAX_SKILL_FILES: usize = 512;
/// The most folder levels one skill may nest.
pub(crate) const MAX_SKILL_DEPTH: usize = 8;
/// The most bytes one skill folder may hold in total.
pub(crate) const MAX_SKILL_BYTES: usize = 32 << 20;

use crate::{DefinitionName, DefinitionSource, ProjectionError, SkillFilePath, SkillFolder, Slug, SourceVault};

/// The production [`DefinitionSource`]: the configured vaults, read through the same root-confined filesystem
/// capability every vault read uses.
#[derive(Clone, Debug)]
pub struct VaultSetSource {
    roots: VaultSet,
    filesystem: Filesystem,
}

impl VaultSetSource {
    /// Reads definitions from `roots` through `filesystem`, which must be built over the same set.
    #[must_use]
    pub const fn new(roots: VaultSet, filesystem: Filesystem) -> Self {
        Self { roots, filesystem }
    }
}

impl VaultSetSource {
    /// Resolves `relative` in `vault`, refusing a path whose resolution differs from its spelling, which is what a
    /// link anywhere on it produces.
    fn resolve(&self, vault: &SourceVault, relative: &str) -> Result<VaultPath, ProjectionError> {
        let raw = format!("{}://{relative}", vault.name);
        let path = VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw: &raw,
        })
        .map_err(|source| {
            read_failure(
                vault,
                relative,
                FsError::PathValidation {
                    path: PathBuf::from(relative),
                    source,
                },
            )
        })?;
        Self::confirm_unlinked(vault, relative, &path)?;
        Ok(path)
    }

    /// Resolves the entry `name` directly inside `parent`, with the same link refusal as [`Self::resolve`].
    fn resolve_child(
        &self,
        vault: &SourceVault,
        parent: &VaultPath,
        name: &str,
    ) -> Result<(VaultPath, String), ProjectionError> {
        let relative = format!("{}/{name}", parent.portable_relative());
        let path = VaultPath::try_from(VaultChildInput {
            roots: &self.roots,
            parent,
            name,
        })
        .map_err(|source| match source {
            // A link out of the vault is refused as the link it is, as one that stays inside is: nothing is wrong
            // with reading it, and no retry would help.
            PathError::SymlinkEscape { .. } => ProjectionError::SourceLink {
                vault: vault.name.clone(),
                entry: relative.clone(),
            },
            source => read_failure(
                vault,
                &relative,
                FsError::PathValidation {
                    path: PathBuf::from(&relative),
                    source,
                },
            ),
        })?;
        Self::confirm_unlinked(vault, &relative, &path)?;
        Ok((path, relative))
    }

    fn confirm_unlinked(vault: &SourceVault, relative: &str, path: &VaultPath) -> Result<(), ProjectionError> {
        if path.portable_relative() == relative {
            Ok(())
        } else {
            Err(ProjectionError::SourceLink {
                vault: vault.name.clone(),
                entry: relative.to_owned(),
            })
        }
    }

    /// Lists the direct children of `relative`, or `None` when it does not exist.
    fn list(
        &self,
        vault: &SourceVault,
        directory: &VaultPath,
        relative: &str,
    ) -> Result<Option<Vec<contextos_fs::DirectoryEntry>>, ProjectionError> {
        match self.filesystem.list_directory(&ListDirectoryRequest {
            path: directory.clone(),
        }) {
            Ok(listing) => Ok(Some(listing.entries)),
            Err(FsError::NotFound { .. }) => Ok(None),
            Err(source) => Err(read_failure(vault, relative, source)),
        }
    }

    /// As [`Self::list`], with each file's size, so limits can be checked before anything is read.
    fn list_sized(
        &self,
        vault: &SourceVault,
        directory: &VaultPath,
        relative: &str,
    ) -> Result<Option<Vec<contextos_fs::DirectoryEntry>>, ProjectionError> {
        match self
            .filesystem
            .list_directory_with_sizes(&ListDirectoryWithSizesRequest {
                path: directory.clone(),
                sort_by: SortBy::Name,
            }) {
            Ok(listing) => Ok(Some(listing.entries)),
            Err(FsError::NotFound { .. }) => Ok(None),
            Err(source) => Err(read_failure(vault, relative, source)),
        }
    }

    fn read_file(
        &self,
        vault: &SourceVault,
        path: &VaultPath,
        relative: &str,
    ) -> Result<Option<Vec<u8>>, ProjectionError> {
        match self
            .filesystem
            .read_attachment(&AttachmentRequest { path: path.clone() })
        {
            Ok(attachment) => Ok(Some(attachment.bytes)),
            Err(FsError::NotFound { .. }) => Ok(None),
            Err(source) => Err(read_failure(vault, relative, source)),
        }
    }

    fn names(
        &self,
        vault: &SourceVault,
        folder: &str,
        wants: EntryKind,
    ) -> Result<Vec<DefinitionName>, ProjectionError> {
        let directory = self.resolve(vault, folder)?;
        let Some(entries) = self.list(vault, &directory, folder)? else {
            return Ok(Vec::new());
        };
        Ok(entries
            .iter()
            .filter(|entry| entry.kind == wants)
            .filter_map(|entry| match wants {
                EntryKind::File => entry
                    .name
                    .strip_suffix(AGENT_EXTENSION)
                    .filter(|stem| !stem.is_empty() && !is_managed_index(&entry.name)),
                EntryKind::Directory => Some(entry.name.as_str()),
            })
            .map(DefinitionName::from)
            .collect())
    }

    /// Lists every file below `directory`, refusing links and anything past the limits, from the listed sizes alone so
    /// that no file is read before the whole skill is known to fit. A managed index is not part of the skill.
    fn walk(
        &self,
        vault: &SourceVault,
        directory: &VaultPath,
        prefix: &str,
        depth: usize,
        found: &mut Vec<Listed>,
        total: &mut u64,
    ) -> Result<(), ProjectionError> {
        if depth > MAX_SKILL_DEPTH {
            return Err(ProjectionError::InvalidSkillFolder {
                reason: "it nests folders too deeply",
            });
        }
        let relative = directory.portable_relative();
        let Some(entries) = self.list_sized(vault, directory, &relative)? else {
            return Ok(());
        };
        for entry in entries {
            let is_directory = entry.kind == EntryKind::Directory;
            if !is_directory && is_managed_index(&entry.name) {
                continue;
            }
            let (path, relative) = self.resolve_child(vault, directory, &entry.name)?;
            let inside = if prefix.is_empty() {
                entry.name.clone()
            } else {
                format!("{prefix}/{}", entry.name)
            };
            if is_directory {
                self.walk(vault, &path, &inside, depth.saturating_add(1), found, total)?;
                continue;
            }
            if found.len() >= MAX_SKILL_FILES {
                return Err(ProjectionError::InvalidSkillFolder {
                    reason: "it holds too many files",
                });
            }
            *total = total.saturating_add(entry.size.unwrap_or(0));
            if *total > MAX_SKILL_BYTES as u64 {
                return Err(ProjectionError::InvalidSkillFolder {
                    reason: "it holds too many bytes",
                });
            }
            found.push(Listed { path, relative, inside });
        }
        Ok(())
    }
}

/// A file found by walking a skill folder, not yet read.
struct Listed {
    path: VaultPath,
    relative: String,
    inside: String,
}

/// The vault-relative path of a folder directly inside the registry.
fn registry_folder(name: &str) -> String {
    format!("{ROOT}/{name}")
}

fn read_failure(vault: &SourceVault, entry: &str, source: FsError) -> ProjectionError {
    ProjectionError::SourceRead {
        vault: vault.name.clone(),
        entry: entry.to_owned(),
        source: Box::new(source),
    }
}

impl DefinitionSource for VaultSetSource {
    fn vaults(&self) -> Vec<SourceVault> {
        self.roots
            .iter()
            .enumerate()
            .filter_map(|(index, root)| {
                Some(SourceVault {
                    id: VaultRootId::try_from(index).ok()?,
                    name: root.name().to_owned(),
                })
            })
            .collect()
    }

    fn agent_names(&self, vault: &SourceVault) -> Result<Vec<DefinitionName>, ProjectionError> {
        self.names(vault, &registry_folder(AGENTS), EntryKind::File)
    }

    fn read_agent(&self, vault: &SourceVault, slug: &Slug) -> Result<Option<Vec<u8>>, ProjectionError> {
        let directory = self.resolve(vault, &registry_folder(AGENTS))?;
        let name = format!("{}{AGENT_EXTENSION}", <&str>::from(slug));
        let (path, relative) = self.resolve_child(vault, &directory, &name)?;
        self.read_file(vault, &path, &relative)
    }

    fn read_orientation(&self, vault: &SourceVault) -> Result<Option<Vec<u8>>, ProjectionError> {
        let directory = self.resolve(vault, &registry_folder(INSTRUCTIONS))?;
        let (path, relative) = self.resolve_child(vault, &directory, ORIENTATION_FILE)?;
        self.read_file(vault, &path, &relative)
    }

    fn skill_names(&self, vault: &SourceVault) -> Result<Vec<DefinitionName>, ProjectionError> {
        self.names(vault, &registry_folder(SKILLS), EntryKind::Directory)
    }

    fn read_skill(&self, vault: &SourceVault, slug: &Slug) -> Result<Option<SkillFolder>, ProjectionError> {
        let parent = self.resolve(vault, &registry_folder(SKILLS))?;
        let (directory, relative) = self.resolve_child(vault, &parent, <&str>::from(slug))?;
        if self.list(vault, &directory, &relative)?.is_none() {
            return Ok(None);
        }
        let mut listed = Vec::new();
        let mut total = 0;
        self.walk(vault, &directory, "", 0, &mut listed, &mut total)?;
        let mut files = Vec::new();
        let mut read_total = 0_usize;
        for file in listed {
            let Some(bytes) = self.read_file(vault, &file.path, &file.relative)? else {
                continue;
            };
            read_total = read_total.saturating_add(bytes.len());
            if read_total > MAX_SKILL_BYTES {
                return Err(ProjectionError::InvalidSkillFolder {
                    reason: "it holds too many bytes",
                });
            }
            files.push((SkillFilePath::try_from(file.inside.as_str())?, bytes));
        }
        SkillFolder::try_from(files).map(Some)
    }
}

#[cfg(test)]
#[path = "vault_source_test.rs"]
mod tests;
