use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use contextos_core::{ContentHash, ResolvedPath};
use contextos_fs::host_file::replace_file_atomically;

use crate::{
    AgentDefinition, ClaudeAgentFile, ClaudeAgentInput, ClaudeTiers, CodexAgentFile, CodexAgentInput, CodexTiers,
    FileExtension, HostKind, ManifestLocation, ProjectionError, ProjectionTarget, ReadOnlyTools, TargetRootRejection,
};

/// One host folder the server projects into, with the ownership record that goes with it.
#[derive(Clone, Debug)]
pub struct HostFolder {
    target: ProjectionTarget,
    manifest: ManifestLocation,
}

impl HostFolder {
    /// A folder and the place its ownership manifest lives.
    #[must_use]
    pub const fn new(target: ProjectionTarget, manifest: ManifestLocation) -> Self {
        Self { target, manifest }
    }

    pub(crate) const fn target(&self) -> &ProjectionTarget {
        &self.target
    }

    pub(crate) const fn manifest(&self) -> &ManifestLocation {
        &self.manifest
    }
}

/// How an agent definition becomes a host's native agent file, which also says which host it is.
#[derive(Clone, Debug)]
pub enum HostRender {
    /// Claude Code: `<slug>.md` with YAML frontmatter.
    Claude {
        /// The model each tier resolves to.
        tiers: ClaudeTiers,
        /// The `ContextOS` tools a read-only agent may use.
        read_only_tools: ReadOnlyTools,
    },
    /// Codex: `<slug>.toml`.
    Codex {
        /// The model and reasoning effort each tier resolves to.
        tiers: CodexTiers,
    },
}

impl From<&HostRender> for HostKind {
    fn from(value: &HostRender) -> Self {
        match value {
            HostRender::Claude { .. } => Self::ClaudeCode,
            HostRender::Codex { .. } => Self::Codex,
        }
    }
}

/// An agent file as rendered for one host.
pub(crate) struct RenderedAgent {
    pub(crate) bytes: Vec<u8>,
    pub(crate) hash: ContentHash,
}

impl HostRender {
    /// The extension of the host's agent files.
    pub(crate) fn extension(&self) -> Result<FileExtension, ProjectionError> {
        FileExtension::try_from(match self {
            Self::Claude { .. } => "md",
            Self::Codex { .. } => "toml",
        })
    }

    /// Renders `definition` in the host's native format.
    pub(crate) fn render(&self, definition: &AgentDefinition) -> Result<RenderedAgent, ProjectionError> {
        match self {
            Self::Claude { tiers, read_only_tools } => {
                let file = ClaudeAgentFile::try_from(ClaudeAgentInput {
                    definition,
                    tiers,
                    read_only_tools,
                })?;
                Ok(RenderedAgent {
                    hash: ContentHash::from(&file),
                    bytes: file.into(),
                })
            }
            Self::Codex { tiers } => {
                let file = CodexAgentFile::try_from(CodexAgentInput { definition, tiers })?;
                Ok(RenderedAgent {
                    hash: ContentHash::from(&file),
                    bytes: file.into(),
                })
            }
        }
    }
}

/// The most bytes of a host's global instructions file that are read before the file is refused.
const MAX_INSTRUCTIONS_BYTES: u64 = 8 << 20;

/// A host's global instructions file (for example `~/.claude/CLAUDE.md`), which may or may not exist yet.
///
/// The path is absolute and resolved once, here, so that an operator-made link to the file (a dotfiles repository, say)
/// is written through to the file it names and the link itself survives. The configured path is kept too, because
/// Codex looks for its override file beside the configured one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionsFile {
    path: PathBuf,
    resolved: PathBuf,
}

impl TryFrom<&Path> for InstructionsFile {
    type Error = ProjectionError;

    fn try_from(value: &Path) -> Result<Self, Self::Error> {
        let resolved = resolve_instructions(value)?;
        Ok(Self {
            path: value.to_path_buf(),
            resolved,
        })
    }
}

/// Where the instructions file at `value` really is: an operator-made link to the file is followed, anything else
/// resolves beside its folder. The one resolution policy is used, so a file that does not exist yet is placed by where
/// its folder really is.
fn resolve_instructions(value: &Path) -> Result<PathBuf, ProjectionError> {
    let reject = |reason| ProjectionError::TargetRootRejected { reason };
    if !value.is_absolute() {
        return Err(reject(TargetRootRejection::NotAbsolute));
    }
    let name = value
        .file_name()
        .ok_or_else(|| reject(TargetRootRejection::NoFileName))?;
    let unresolvable = |source| ProjectionError::TargetRootUnresolvable { source };
    let beside_parent = || {
        value
            .parent()
            .map_or_else(
                || Ok(PathBuf::new()),
                |parent| ResolvedPath::try_from(parent).map(|resolved| <&Path>::from(&resolved).to_path_buf()),
            )
            .map(|parent| parent.join(name))
            .map_err(unresolvable)
    };
    let resolved = match fs::symlink_metadata(value) {
        Ok(metadata) if metadata.file_type().is_symlink() => dunce::canonicalize(value).map_err(unresolvable)?,
        Ok(metadata) if !metadata.is_file() => return Err(reject(TargetRootRejection::NotFile)),
        Ok(_) => beside_parent()?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => beside_parent()?,
        Err(error) => return Err(unresolvable(error)),
    };
    if resolved.is_dir() {
        return Err(reject(TargetRootRejection::NotFile));
    }
    Ok(resolved)
}

impl InstructionsFile {
    #[cfg(test)]
    pub(crate) fn resolved(&self) -> &Path {
        &self.resolved
    }

    /// The file beside the configured one that makes Codex ignore it.
    pub(crate) fn override_file(&self) -> PathBuf {
        self.path.with_file_name("AGENTS.override.md")
    }

    /// Refuses the file when it no longer resolves where it did when the server started: an operator link followed at
    /// start is fine, but a path re-linked since could now lead into a vault or another host's files.
    fn confirm_unchanged(&self) -> Result<(), ProjectionError> {
        if resolve_instructions(&self.path)? == self.resolved {
            Ok(())
        } else {
            Err(ProjectionError::TargetRootRejected {
                reason: TargetRootRejection::Relinked,
            })
        }
    }

    /// The file's bytes, or `None` when it does not exist yet.
    pub(crate) fn read(&self) -> Result<Option<Vec<u8>>, ProjectionError> {
        self.confirm_unchanged()?;
        let read_failed = |source| ProjectionError::Read {
            entry: "the instructions file".to_owned(),
            source,
        };
        // A link followed at start may now lead to a pipe, socket, or device, and opening a pipe would block while the
        // projection lock is held, so the kind is checked before anything is opened.
        match fs::metadata(&self.resolved) {
            Ok(metadata) if !metadata.is_file() => {
                return Err(ProjectionError::TargetRootRejected {
                    reason: TargetRootRejection::NotFile,
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(read_failed(error)),
        }
        let file = match fs::File::open(&self.resolved) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(read_failed(error)),
        };
        let mut bytes = Vec::new();
        file.take(MAX_INSTRUCTIONS_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(read_failed)?;
        if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_INSTRUCTIONS_BYTES) {
            return Err(read_failed(io::Error::new(
                io::ErrorKind::InvalidData,
                "the instructions file is larger than the limit",
            )));
        }
        Ok(Some(bytes))
    }

    /// Replaces the file's bytes atomically, in the folder it already sits in, keeping its permissions. Never creates
    /// a folder, so a host that is not installed is not given a home.
    pub(crate) fn write(&self, bytes: &[u8]) -> Result<(), ProjectionError> {
        self.confirm_unchanged()?;
        replace_file_atomically(&self.resolved, bytes)?;
        Ok(())
    }
}

impl<'a> From<&'a InstructionsFile> for &'a Path {
    fn from(value: &'a InstructionsFile) -> Self {
        &value.path
    }
}

/// One enabled agent host: where its agents and skills go, and how its agent files are rendered.
#[derive(Clone, Debug)]
pub struct HostProjection {
    agents: Option<HostFolder>,
    skills: Option<HostFolder>,
    render: HostRender,
    instructions: Option<InstructionsFile>,
}

impl HostProjection {
    /// An enabled host.
    #[must_use]
    pub const fn new(agents: HostFolder, skills: HostFolder, render: HostRender) -> Self {
        Self {
            agents: Some(agents),
            skills: Some(skills),
            render,
            instructions: None,
        }
    }

    /// An installed host that is only being read (by `doctor`), one of whose folders may not exist. A folder given
    /// as `None` has nothing projected into it to assess, so its entries are left unassessed; the host's other
    /// folder and its orientation block are assessed as usual, and nothing is created.
    #[must_use]
    pub const fn assessed(agents: Option<HostFolder>, skills: Option<HostFolder>, render: HostRender) -> Self {
        Self {
            agents,
            skills,
            render,
            instructions: None,
        }
    }

    /// The same host, also keeping its global instructions file's orientation block in step with the vaults.
    #[must_use]
    pub fn with_instructions(self, instructions: InstructionsFile) -> Self {
        Self {
            instructions: Some(instructions),
            ..self
        }
    }

    /// Which host this is.
    #[must_use]
    pub fn kind(&self) -> HostKind {
        HostKind::from(&self.render)
    }

    /// The host's global instructions file, when its orientation block is kept.
    #[must_use]
    pub const fn instructions(&self) -> Option<&InstructionsFile> {
        self.instructions.as_ref()
    }

    pub(crate) const fn agents(&self) -> Option<&HostFolder> {
        self.agents.as_ref()
    }

    pub(crate) const fn skills(&self) -> Option<&HostFolder> {
        self.skills.as_ref()
    }

    pub(crate) const fn render(&self) -> &HostRender {
        &self.render
    }
}

#[cfg(test)]
#[path = "host_test.rs"]
mod tests;
