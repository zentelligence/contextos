use contextos_core::ContentHash;

use crate::hash::content_hash;
use crate::{
    AgentDefinition, DefinitionKind, DefinitionRejection, DefinitionSource, ProjectionError, SkillDefinition, Slug,
    SourceVault,
};

/// Which part of a definition failed the projection contract, without the failing text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rejection {
    /// The frontmatter field, `body`, `definition`, `SKILL.md`, or `skill folder` that failed.
    pub field: &'static str,
    /// The rule it broke.
    pub reason: DefinitionRejection,
}

impl From<&ProjectionError> for Rejection {
    fn from(value: &ProjectionError) -> Self {
        match value {
            ProjectionError::InvalidDefinition { field, reason } => Self { field, reason: *reason },
            _ => Self {
                field: "skill folder",
                reason: DefinitionRejection::Malformed,
            },
        }
    }
}

/// What one vault says about a slug.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Candidate {
    /// The vault does not define the slug.
    Absent,
    /// The vault defines the slug, but not in a form that can be projected.
    Invalid {
        /// The vault's name.
        vault: String,
        /// The hash of the source, when its bytes could be taken as a whole.
        source_hash: Option<ContentHash>,
        /// What failed.
        rejection: Rejection,
    },
    /// The vault defines the slug validly.
    Valid {
        /// The vault's name.
        vault: String,
        /// The definition's path, relative to its vault.
        source_path: String,
        /// The content hash of the agent file, or the digest of the skill folder.
        source_hash: ContentHash,
    },
}

/// A parsed definition kept beside the candidate it came from.
#[derive(Clone, Debug)]
pub(crate) enum Definition {
    Agent(AgentDefinition),
    Skill(SkillDefinition),
}

/// Every vault's view of one slug, in configuration order, with the parsed definitions the valid ones hold.
#[derive(Debug)]
pub(crate) struct Gathered {
    pub(crate) candidates: Vec<Candidate>,
    definitions: Vec<Option<Definition>>,
}

impl Gathered {
    /// The definition of the first vault that defines the slug at all, when that definition is valid.
    pub(crate) fn winner(&self) -> Option<&Definition> {
        let index = self
            .candidates
            .iter()
            .position(|candidate| !matches!(candidate, Candidate::Absent))?;
        self.definitions.get(index)?.as_ref()
    }
}

/// The path of a definition relative to its vault.
pub(crate) fn source_path(kind: DefinitionKind, slug: &Slug) -> String {
    match kind {
        DefinitionKind::Agent => format!("registry/agents/{}.md", <&str>::from(slug)),
        DefinitionKind::Skill => format!("registry/skills/{}", <&str>::from(slug)),
    }
}

/// Reads `slug` from every vault in order. A read that fails stops the gathering: with one vault unreadable, which
/// vault wins cannot be known, so the caller leaves the slug as it is.
pub(crate) fn gather(
    source: &dyn DefinitionSource,
    vaults: &[SourceVault],
    kind: DefinitionKind,
    slug: &Slug,
) -> Result<Gathered, ProjectionError> {
    let mut candidates = Vec::with_capacity(vaults.len());
    let mut definitions = Vec::with_capacity(vaults.len());
    for vault in vaults {
        let (candidate, definition) = match kind {
            DefinitionKind::Agent => gather_agent(source, vault, slug)?,
            DefinitionKind::Skill => gather_skill(source, vault, slug)?,
        };
        candidates.push(candidate);
        definitions.push(definition);
    }
    Ok(Gathered {
        candidates,
        definitions,
    })
}

fn gather_agent(
    source: &dyn DefinitionSource,
    vault: &SourceVault,
    slug: &Slug,
) -> Result<(Candidate, Option<Definition>), ProjectionError> {
    let Some(bytes) = source.read_agent(vault, slug)? else {
        return Ok((Candidate::Absent, None));
    };
    let source_hash = content_hash(&bytes);
    Ok(match AgentDefinition::try_from((slug, bytes.as_slice())) {
        Ok(definition) => (
            Candidate::Valid {
                vault: vault.name.clone(),
                source_path: source_path(DefinitionKind::Agent, slug),
                source_hash,
            },
            Some(Definition::Agent(definition)),
        ),
        Err(error) => (
            Candidate::Invalid {
                vault: vault.name.clone(),
                source_hash: Some(source_hash),
                rejection: Rejection::from(&error),
            },
            None,
        ),
    })
}

fn gather_skill(
    source: &dyn DefinitionSource,
    vault: &SourceVault,
    slug: &Slug,
) -> Result<(Candidate, Option<Definition>), ProjectionError> {
    let folder = match source.read_skill(vault, slug) {
        Ok(Some(folder)) => folder,
        Ok(None) => return Ok((Candidate::Absent, None)),
        Err(error @ (ProjectionError::InvalidSkillFolder { .. } | ProjectionError::InvalidSkillPath { .. })) => {
            return Ok((
                Candidate::Invalid {
                    vault: vault.name.clone(),
                    source_hash: None,
                    rejection: Rejection::from(&error),
                },
                None,
            ));
        }
        Err(error) => return Err(error),
    };
    let source_hash = contextos_core::ContentHash::from(&folder);
    Ok(match SkillDefinition::try_from((slug.clone(), folder)) {
        Ok(definition) => (
            Candidate::Valid {
                vault: vault.name.clone(),
                source_path: source_path(DefinitionKind::Skill, slug),
                source_hash,
            },
            Some(Definition::Skill(definition)),
        ),
        Err(error) => (
            Candidate::Invalid {
                vault: vault.name.clone(),
                source_hash: Some(source_hash),
                rejection: Rejection::from(&error),
            },
            None,
        ),
    })
}
