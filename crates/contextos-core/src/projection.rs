use thiserror::Error;

use crate::VaultPath;

/// The names that lay out a vault's registry, shared by every part of the product that decides which paths are
/// definitions, so a completed write and a full refresh can never disagree.
pub mod registry_layout {
    /// The registry folder whose definitions are projected into agent hosts.
    pub const ROOT: &str = "registry";
    /// Folder of agent definitions, one `<name>.md` file each.
    pub const AGENTS: &str = "agents";
    /// Folder of skill definitions, one `<name>` folder each.
    pub const SKILLS: &str = "skills";
    /// Folder holding the global orientation instructions.
    pub const INSTRUCTIONS: &str = "instructions";
    /// The one file in `instructions` that is projected.
    pub const ORIENTATION_FILE: &str = "global.md";
    /// The managed folder index, which is never a definition.
    pub const MANAGED_INDEX: &str = "index.md";

    /// Whether a file name is the managed folder index, which is never part of a definition at any level.
    #[must_use]
    pub fn is_managed_index(name: &str) -> bool {
        name == MANAGED_INDEX
    }
    /// The extension that marks an agent definition file.
    pub const AGENT_EXTENSION: &str = ".md";
}

use registry_layout::{
    AGENT_EXTENSION, AGENTS, INSTRUCTIONS, ORIENTATION_FILE, ROOT as REGISTRY, SKILLS, is_managed_index,
};

/// Which part of the agent host projection a vault path belongs to.
///
/// Classification looks only at the vault-relative path, by whole `/`-separated components, matching the registry folder names and `global.md` without regard to case (and never the definition name), so look-alike
/// prefixes such as `registry/agentsx/a.md` never match. The name is the raw path component: validating it against the
/// definition grammar is the projection service's job, which warns about an invalid name, so an invalid name must still
/// be classified and routed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectionScope {
    /// `registry/agents/<name>.md`: one agent definition, named without its extension.
    Agent(String),
    /// `registry/skills/<name>` or any path below it: one skill definition folder.
    Skill(String),
    /// `registry/instructions/global.md`: the global instructions behind each host's orientation block.
    Orientation,
    /// A registry folder itself (`registry`, `registry/agents`, `registry/skills`, or `registry/instructions`), which
    /// can be moved or deleted wholesale and so affects every definition beneath it.
    AllDefinitions,
}

/// The path is not part of any projected definition.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("the path is not a projected agent host definition")]
pub struct NotProjected;

impl TryFrom<&VaultPath> for ProjectionScope {
    type Error = NotProjected;

    fn try_from(value: &VaultPath) -> Result<Self, Self::Error> {
        let portable = value.portable_relative();
        let components = portable.split('/').collect::<Vec<_>>();
        let (root, tail) = components.split_first().ok_or(NotProjected)?;
        let Some((folder, rest)) = tail.split_first() else {
            return if folds_to(root, REGISTRY) {
                Ok(Self::AllDefinitions)
            } else {
                Err(NotProjected)
            };
        };
        if !folds_to(root, REGISTRY) {
            return Err(NotProjected);
        }
        let kind = if folds_to(folder, AGENTS) {
            Folder::Agents
        } else if folds_to(folder, SKILLS) {
            Folder::Skills
        } else if folds_to(folder, INSTRUCTIONS) {
            Folder::Instructions
        } else {
            return Err(NotProjected);
        };
        match (kind, rest) {
            (_, []) => Ok(Self::AllDefinitions),
            (Folder::Agents, [file]) => match file.strip_suffix(AGENT_EXTENSION) {
                Some(name) if !name.is_empty() && !is_managed_index(file) => Ok(Self::Agent(name.to_owned())),
                _ => Err(NotProjected),
            },
            (Folder::Skills, [file]) if is_managed_index(file) => Err(NotProjected),
            (Folder::Skills, [name, inner @ ..]) if !inner.last().is_some_and(|last| is_managed_index(last)) => {
                Ok(Self::Skill((*name).to_owned()))
            }
            (Folder::Instructions, [file]) if is_orientation_file(file) => Ok(Self::Orientation),
            _ => Err(NotProjected),
        }
    }
}

/// The registry subfolders that hold projected definitions.
#[derive(Clone, Copy)]
enum Folder {
    Agents,
    Skills,
    Instructions,
}

/// Whether a path component names the given registry folder, ignoring case.
fn folds_to(component: &str, name: &str) -> bool {
    component.eq_ignore_ascii_case(name)
}

/// Whether the file name is `global.md`, folding the case of the stem only.
fn is_orientation_file(file: &str) -> bool {
    let stem = ORIENTATION_FILE
        .strip_suffix(AGENT_EXTENSION)
        .unwrap_or(ORIENTATION_FILE);
    file.strip_suffix(AGENT_EXTENSION)
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(stem))
}

#[cfg(test)]
#[path = "projection_test.rs"]
mod tests;
