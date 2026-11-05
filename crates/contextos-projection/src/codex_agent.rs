use contextos_core::ContentHash;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{Access, AgentDefinition, CodexTiers, ProjectionError};

/// The first line of a read-only agent's instructions. Codex inherits its sandbox and tools from the session, so the
/// agent is told in words what its disabled features already withhold.
const READ_ONLY_NOTICE: &str = "This agent is read-only: it must not modify files or call tools that change state.";

/// The documented keys of a Codex agent file, in the order they are written. The `features` table comes last because
/// TOML requires every plain key to precede the first table.
#[derive(Serialize)]
struct CodexFile<'a> {
    name: &'a str,
    description: &'a str,
    developer_instructions: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    model_reasoning_effort: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    features: Option<Features>,
}

/// The capabilities a read-only agent loses.
#[derive(Serialize)]
struct Features {
    shell_tool: bool,
    apps: bool,
    plugins: bool,
}

/// What a Codex agent file is rendered from.
#[derive(Clone, Copy, Debug)]
pub struct CodexAgentInput<'a> {
    /// The definition to project.
    pub definition: &'a AgentDefinition,
    /// The tier-to-model-and-effort mapping.
    pub tiers: &'a CodexTiers,
}

/// The bytes of a Codex agent file (`<slug>.toml`), identical for identical input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexAgentFile(Vec<u8>);

impl TryFrom<CodexAgentInput<'_>> for CodexAgentFile {
    type Error = ProjectionError;

    fn try_from(value: CodexAgentInput<'_>) -> Result<Self, Self::Error> {
        let CodexAgentInput { definition, tiers } = value;
        let tier = tiers.tier(definition.tier());
        let read_only = *definition.access() == Access::ReadOnly;
        let developer_instructions = if read_only {
            format!("{READ_ONLY_NOTICE}\n\n{}", definition.body())
        } else {
            definition.body().to_owned()
        };
        let file = CodexFile {
            name: <&str>::from(definition.slug()),
            description: definition.description(),
            developer_instructions: &developer_instructions,
            model: tier.model().map(<&str>::from),
            model_reasoning_effort: tier.effort().into(),
            features: read_only.then_some(Features {
                shell_tool: false,
                apps: false,
                plugins: false,
            }),
        };
        let rendered = toml::to_string(&file).map_err(|source| ProjectionError::RenderToml { source })?;
        Ok(Self(rendered.into_bytes()))
    }
}

impl AsRef<[u8]> for CodexAgentFile {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl From<CodexAgentFile> for Vec<u8> {
    fn from(value: CodexAgentFile) -> Self {
        value.0
    }
}

impl From<&CodexAgentFile> for ContentHash {
    fn from(value: &CodexAgentFile) -> Self {
        Self::from(<[u8; 32]>::from(Sha256::digest(&value.0)))
    }
}

#[cfg(test)]
#[path = "codex_agent_test.rs"]
mod tests;
