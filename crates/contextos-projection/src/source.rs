use contextos_core::VaultRootId;

use crate::{NameRejection, ProjectionError, SkillFolder, Slug};

/// A configured vault as definitions are looked up in it: its place in configuration order and its name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceVault {
    /// The vault's identity, which is also its position in configuration order.
    pub id: VaultRootId,
    /// The vault's configured name.
    pub name: String,
}

/// The name of a definition found in a vault, which may not be usable as a slug.
///
/// A name that fails slug validation is still reported, so the operator can be told about it; it is never dropped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DefinitionName {
    /// A name that is a valid slug.
    Valid(Slug),
    /// A name that fails slug validation.
    Invalid {
        /// The file stem or folder name as found.
        raw: String,
        /// The rule it broke.
        reason: NameRejection,
    },
}

impl From<&str> for DefinitionName {
    fn from(value: &str) -> Self {
        match Slug::try_from(value) {
            Ok(slug) => Self::Valid(slug),
            Err(ProjectionError::InvalidSlug { reason }) => Self::Invalid {
                raw: value.to_owned(),
                reason,
            },
            Err(_) => Self::Invalid {
                raw: value.to_owned(),
                reason: NameRejection::InvalidCharacter,
            },
        }
    }
}

/// Where definitions are read from: the configured vaults, in configuration order.
///
/// Reads never follow a link and never leave the vault. A failure to read is a typed error, never an empty answer,
/// so a vault that cannot be read is never mistaken for one that defines nothing.
pub trait DefinitionSource: Send + Sync {
    /// The configured vaults in configuration order.
    fn vaults(&self) -> Vec<SourceVault>;

    /// The agent definitions in `vault`: `registry/agents/*.md` except `index.md`, named without the extension.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the folder exists but cannot be listed safely.
    fn agent_names(&self, vault: &SourceVault) -> Result<Vec<DefinitionName>, ProjectionError>;

    /// The bytes of `registry/agents/<slug>.md` in `vault`, or `None` when it does not exist.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the file exists but cannot be read safely.
    fn read_agent(&self, vault: &SourceVault, slug: &Slug) -> Result<Option<Vec<u8>>, ProjectionError>;

    /// The skill definitions in `vault`: the folders under `registry/skills`.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the folder exists but cannot be listed safely.
    fn skill_names(&self, vault: &SourceVault) -> Result<Vec<DefinitionName>, ProjectionError>;

    /// The files of `registry/skills/<slug>` in `vault`, or `None` when the folder does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::InvalidSkillFolder`] or [`ProjectionError::InvalidSkillPath`] when the folder's
    /// files cannot form a skill, and another typed error when it cannot be read safely.
    fn read_skill(&self, vault: &SourceVault, slug: &Slug) -> Result<Option<SkillFolder>, ProjectionError>;

    /// The bytes of `registry/instructions/global.md` in `vault`, or `None` when it does not exist.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the file exists but cannot be read safely.
    fn read_orientation(&self, vault: &SourceVault) -> Result<Option<Vec<u8>>, ProjectionError>;
}
