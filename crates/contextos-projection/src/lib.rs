#![forbid(unsafe_code)]

//! Agent host projection: keeping agent hosts' own folders in step with a vault's registry definitions.
//!
//! Definitions from a vault are projected into an agent host's own folders (for example `~/.claude/agents`). This crate
//! owns what makes such writes safe: validated names ([`Slug`], [`SkillFilePath`], [`FileExtension`]), a confined
//! write surface ([`ProjectionTarget`]) that accepts nothing but those validated names, and the ownership record
//! ([`OwnershipManifest`]) that says which entries the server may touch. Parsed definitions ([`AgentDefinition`],
//! [`SkillDefinition`]) and their conversion into each host's native agent format ([`ClaudeAgentFile`],
//! [`CodexAgentFile`]) live here too, as do the definition source port ([`DefinitionSource`]), the pure planner that
//! settles precedence across vaults ([`Plan`]), and the service that keeps hosts in step ([`ProjectionService`]).

mod candidate;
mod claude_agent;
mod codex_agent;
mod definition;
mod device_name;
mod error;
mod finding;
mod folder;
mod hash;
mod host;
mod host_config;
mod instructions;
mod instructions_observer;
mod manifest;
mod memory_source;
mod name;
mod orientation;
mod plan;
mod report;
mod run;
mod service;
mod skill_path;
mod slug;
mod source;
mod target;
mod tier;
mod vault_source;

pub use candidate::{Candidate, Rejection};
pub use claude_agent::{ClaudeAgentFile, ClaudeAgentInput};
pub use codex_agent::{CodexAgentFile, CodexAgentInput};
pub use definition::{AgentDefinition, SkillDefinition};
pub use error::{
    BlockRejection, DefinitionRejection, ManifestRejection, NameRejection, ProjectionError, TargetLimit,
    TargetRootRejection,
};
pub use finding::{DefinitionKind, Finding, FindingKind, HostKind, Repair};
pub use folder::SkillFolder;
pub use host::{HostFolder, HostProjection, HostRender, InstructionsFile};
pub use host_config::{ClaudeTiers, CodexTier, CodexTiers, ModelName, ReadOnlyTools, ReasoningEffort};
pub use instructions_observer::{InstructionsObserver, InstructionsStep};
pub use manifest::{ConfigurationId, ManifestEntry, ManifestLocation, OwnershipManifest, TargetKey};
pub use memory_source::MemorySource;
pub use name::FileExtension;
pub use orientation::OrientationBlock;
pub use plan::{Action, Mode, Plan, PlanFinding, PlanInput, Recording, TargetState};
pub use report::{Failure, ProjectionReport};
pub use service::ProjectionService;
pub use skill_path::SkillFilePath;
pub use slug::Slug;
pub use source::{DefinitionName, DefinitionSource, SourceVault};
pub use target::ProjectionTarget;
pub use tier::{Access, Tier};
pub use vault_source::VaultSetSource;
