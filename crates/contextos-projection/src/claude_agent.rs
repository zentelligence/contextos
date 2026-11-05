use contextos_core::ContentHash;

use contextos_obsidian::{FrontmatterDocument, FrontmatterInput};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::{Access, AgentDefinition, ClaudeTiers, ProjectionError, ReadOnlyTools};

/// The Claude Code tools a read-only agent may always use; `ContextOS` tools follow them.
const BASE_READ_ONLY_TOOLS: [&str; 5] = ["Read", "Grep", "Glob", "WebFetch", "WebSearch"];

/// What a Claude Code agent file is rendered from.
#[derive(Clone, Copy, Debug)]
pub struct ClaudeAgentInput<'a> {
    /// The definition to project.
    pub definition: &'a AgentDefinition,
    /// The tier-to-model mapping.
    pub tiers: &'a ClaudeTiers,
    /// The `ContextOS` tools a read-only agent may use.
    pub read_only_tools: &'a ReadOnlyTools,
}

/// The bytes of a Claude Code agent file (`<slug>.md`), identical for identical input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeAgentFile(Vec<u8>);

impl TryFrom<ClaudeAgentInput<'_>> for ClaudeAgentFile {
    type Error = ProjectionError;

    fn try_from(value: ClaudeAgentInput<'_>) -> Result<Self, Self::Error> {
        let ClaudeAgentInput {
            definition,
            tiers,
            read_only_tools,
        } = value;
        let mut frontmatter = Map::new();
        let mut put = |key: &str, text: String| frontmatter.insert(key.to_owned(), Value::String(text));
        put("name", <&str>::from(definition.slug()).to_owned());
        put("description", definition.description().to_owned());
        put("model", <&str>::from(tiers.model(definition.tier())).to_owned());
        if *definition.access() == Access::ReadOnly {
            let allowed = BASE_READ_ONLY_TOOLS
                .iter()
                .map(|tool| (*tool).to_owned())
                .chain(read_only_tools.allowlist());
            put("tools", allowed.collect::<Vec<_>>().join(", "));
        }
        let document = FrontmatterDocument::from(FrontmatterInput {
            frontmatter,
            body: format!("\n{}\n", definition.body()),
        });
        let rendered = String::try_from(&document).map_err(|source| ProjectionError::RenderYaml { source })?;
        Ok(Self(rendered.into_bytes()))
    }
}

impl AsRef<[u8]> for ClaudeAgentFile {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl From<ClaudeAgentFile> for Vec<u8> {
    fn from(value: ClaudeAgentFile) -> Self {
        value.0
    }
}

impl From<&ClaudeAgentFile> for ContentHash {
    fn from(value: &ClaudeAgentFile) -> Self {
        Self::from(<[u8; 32]>::from(Sha256::digest(&value.0)))
    }
}

#[cfg(test)]
#[path = "claude_agent_test.rs"]
mod tests;
