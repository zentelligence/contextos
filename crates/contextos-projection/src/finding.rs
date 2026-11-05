use std::fmt;

/// An agent host that definitions are projected into.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum HostKind {
    /// Claude Code.
    ClaudeCode,
    /// Codex.
    Codex,
}

impl From<HostKind> for &'static str {
    fn from(value: HostKind) -> Self {
        match value {
            HostKind::ClaudeCode => "claude",
            HostKind::Codex => "codex",
        }
    }
}

impl fmt::Display for HostKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(<&str>::from(*self))
    }
}

/// Which kind of definition something concerns.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DefinitionKind {
    /// A `registry/agents/<name>.md` file.
    Agent,
    /// A `registry/skills/<name>` folder.
    Skill,
}

impl From<DefinitionKind> for &'static str {
    fn from(value: DefinitionKind) -> Self {
        match value {
            DefinitionKind::Agent => "agent",
            DefinitionKind::Skill => "skill",
        }
    }
}

impl fmt::Display for DefinitionKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(<&str>::from(*self))
    }
}

/// What a finding says is wrong, spelled as the stable `doctor` kinds.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FindingKind {
    /// A recorded entry differs from what its source would now project, or a source has no projection.
    ProjectionStale,
    /// A recorded entry's bytes differ from the hash recorded when it was projected.
    ProjectionHandEdited,
    /// A recorded entry whose source definition, or whose vault, no longer exists.
    ProjectionOrphaned,
    /// A definition that fails the projection contract.
    ProjectionInvalidDefinition,
    /// A later vault's divergent definition of a projected slug.
    ProjectionShadowed,
    /// An entry the manifest does not record, at a slug a definition would project to.
    ProjectionUnownedCollision,
    /// An entry recorded as another configuration's.
    ProjectionForeignOwner,
    /// A read-only agent projected to Codex, which cannot enforce it.
    CodexReadOnlyUnenforced,
    /// A host's orientation block is missing, differs from the source, or sits in a malformed file.
    OrientationBlock,
    /// An `AGENTS.override.md` beside Codex's instructions file makes Codex ignore the instructions file.
    CodexOverrideHidesAgents,
}

impl From<FindingKind> for &'static str {
    fn from(value: FindingKind) -> Self {
        match value {
            FindingKind::ProjectionStale => "projection_stale",
            FindingKind::ProjectionHandEdited => "projection_hand_edited",
            FindingKind::ProjectionOrphaned => "projection_orphaned",
            FindingKind::ProjectionInvalidDefinition => "projection_invalid_definition",
            FindingKind::ProjectionShadowed => "projection_shadowed",
            FindingKind::ProjectionUnownedCollision => "projection_unowned_collision",
            FindingKind::ProjectionForeignOwner => "projection_foreign_owner",
            FindingKind::CodexReadOnlyUnenforced => "codex_read_only_unenforced",
            FindingKind::OrientationBlock => "orientation_block",
            FindingKind::CodexOverrideHidesAgents => "codex_override_hides_agents",
        }
    }
}

/// Whether the server may repair one particular finding itself.
///
/// A kind says whether findings of it can be repaired at all (this is its default, the classification a finding of the
/// kind starts from); a single finding may still need the operator, as a
/// malformed orientation block does, and that is recorded here, in the type, rather than inferred from a message.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Repair {
    /// The server repairs it without discarding anything the operator owns.
    Automatic,
    /// Only the operator can repair it.
    Manual,
}

impl From<FindingKind> for Repair {
    fn from(value: FindingKind) -> Self {
        match value {
            FindingKind::ProjectionStale
            | FindingKind::ProjectionHandEdited
            | FindingKind::ProjectionOrphaned
            | FindingKind::OrientationBlock => Self::Automatic,
            FindingKind::ProjectionInvalidDefinition
            | FindingKind::ProjectionShadowed
            | FindingKind::ProjectionUnownedCollision
            | FindingKind::ProjectionForeignOwner
            | FindingKind::CodexReadOnlyUnenforced
            | FindingKind::CodexOverrideHidesAgents => Self::Manual,
        }
    }
}

/// One thing `doctor` should say about the projection of a definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Finding {
    /// What is wrong.
    pub kind: FindingKind,
    /// The host concerned, when the finding is about one host's folder.
    pub host: Option<HostKind>,
    /// Whether an agent or a skill is concerned, or `None` for a finding about a host's instructions file.
    pub definition: Option<DefinitionKind>,
    /// The slug, or the raw file or folder name when it is not a valid slug.
    pub name: String,
    /// The source vault concerned, when one applies.
    pub vault: Option<String>,
    /// For a shadowed definition, the vault whose definition is shadowed.
    pub other_vault: Option<String>,
    /// Whether the server may repair this finding.
    pub repair: Repair,
    /// A sentence for the operator. Never definition content, and no paths outside a vault.
    pub message: String,
}

impl Finding {
    /// Whether the server may repair this finding itself.
    #[must_use]
    pub const fn auto_fixable(&self) -> bool {
        matches!(self.repair, Repair::Automatic)
    }
}

#[cfg(test)]
#[path = "finding_test.rs"]
mod tests;
