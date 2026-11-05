use std::fmt::{self, Display, Formatter};

use contextos_projection::FindingKind;
use rmcp::schemars;
use serde::Serialize;

/// The stable, machine-readable category of a `doctor` finding.
///
/// Clients group and route findings by this code, so a variant's serialised name never changes once shipped. The
/// human-readable `subject` shown by the CLI is derived from it through [`Display`], except for the vault access
/// check, whose subject also names the vault path.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DoctorKind {
    Configuration,
    VaultAccess,
    ManagedIndexes,
    GitRecovery,
    UncommittedChanges,
    SemanticSearch,
    Frontmatter,
    DuplicateIds,
    RecordConformance,
    TeamConfig,
    FenceVocabulary,
    LinkIntegrity,
    TextIndexStore,
    LinkGraphStore,
    VectorStore,
    // The projection kinds below are described in plain comments, not rustdoc: a documented variant makes the
    // advertised schema a `oneOf`, which Cowork's tool registry rejects.
    //
    // A projected agent or skill differs from what its source definition now produces, or is missing.
    ProjectionStale,
    // A projected file or folder was edited outside `ContextOS`.
    ProjectionHandEdited,
    // A projected entry whose source definition no longer exists.
    ProjectionOrphaned,
    // A definition that fails the projection contract.
    ProjectionInvalidDefinition,
    // A later vault's divergent definition of a slug an earlier vault already projects.
    ProjectionShadowed,
    // A file or folder `ContextOS` did not project sits at a slug a definition projects to.
    ProjectionUnownedCollision,
    // An entry recorded as projected by another configuration.
    ProjectionForeignOwner,
    // The orientation block in a host's global instructions is missing, differs, or sits in a malformed file.
    OrientationBlock,
    // A read-only agent projected to Codex, which cannot enforce it.
    CodexReadOnlyUnenforced,
    // An `AGENTS.override.md` makes Codex ignore the global instructions.
    CodexOverrideHidesAgents,
    // Another process held the host-level projection lock, so projection was not assessed.
    ProjectionBusy,
    // The host-level projection lock could not be opened, so projection was not assessed.
    ProjectionLockFailed,
    // An event kind definition in the vault's kind registry cannot be used.
    EventKindInvalid,
    // One event kind is defined in more than one file.
    EventKindDuplicate,
    // An event note has no readable time, so the retention sweep keeps it.
    EventUnreadable,
    // The kind registry holds more definition files than are read, so some kinds cannot be recorded.
    EventKindTruncated,
    // A base other than `memory/events.base` declares an event kind registry, which has no effect there.
    EventKindMisplaced,
}

impl From<FindingKind> for DoctorKind {
    fn from(value: FindingKind) -> Self {
        match value {
            FindingKind::ProjectionStale => Self::ProjectionStale,
            FindingKind::ProjectionHandEdited => Self::ProjectionHandEdited,
            FindingKind::ProjectionOrphaned => Self::ProjectionOrphaned,
            FindingKind::ProjectionInvalidDefinition => Self::ProjectionInvalidDefinition,
            FindingKind::ProjectionShadowed => Self::ProjectionShadowed,
            FindingKind::ProjectionUnownedCollision => Self::ProjectionUnownedCollision,
            FindingKind::ProjectionForeignOwner => Self::ProjectionForeignOwner,
            FindingKind::CodexReadOnlyUnenforced => Self::CodexReadOnlyUnenforced,
            FindingKind::OrientationBlock => Self::OrientationBlock,
            FindingKind::CodexOverrideHidesAgents => Self::CodexOverrideHidesAgents,
        }
    }
}

impl Display for DoctorKind {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Configuration => "Configuration",
            Self::VaultAccess => "Vault",
            Self::ManagedIndexes => "Managed indexes",
            Self::GitRecovery => "Git recovery",
            Self::UncommittedChanges => "Uncommitted MCP changes",
            Self::SemanticSearch => "Semantic search",
            Self::Frontmatter => "Frontmatter validity",
            Self::DuplicateIds => "Duplicate id detection",
            Self::RecordConformance => "Record conformance",
            Self::TeamConfig => "Team configuration",
            Self::FenceVocabulary => "Fence vocabulary",
            Self::LinkIntegrity => "Link integrity",
            Self::TextIndexStore => "Text index store",
            Self::LinkGraphStore => "Link graph store",
            Self::VectorStore => "Vector store",
            Self::ProjectionStale => "Projection stale",
            Self::ProjectionHandEdited => "Projection hand edited",
            Self::ProjectionOrphaned => "Projection orphaned",
            Self::ProjectionInvalidDefinition => "Projection invalid definition",
            Self::ProjectionShadowed => "Projection shadowed",
            Self::ProjectionUnownedCollision => "Projection unowned collision",
            Self::ProjectionForeignOwner => "Projection foreign owner",
            Self::OrientationBlock => "Orientation block",
            Self::CodexReadOnlyUnenforced => "Codex read-only unenforced",
            Self::CodexOverrideHidesAgents => "Codex override hides instructions",
            Self::ProjectionBusy => "Projection busy",
            Self::ProjectionLockFailed => "Projection lock failed",
            Self::EventKindInvalid => "Event kind definitions",
            Self::EventKindDuplicate => "Event kind duplicates",
            Self::EventUnreadable => "Event notes",
            Self::EventKindTruncated => "Event kind registry size",
            Self::EventKindMisplaced => "Event kind registry placement",
        })
    }
}

#[cfg(test)]
#[path = "doctor_kind_test.rs"]
mod tests;
