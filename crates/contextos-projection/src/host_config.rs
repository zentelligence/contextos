use std::collections::BTreeSet;

use crate::{NameRejection, ProjectionError, Tier};

/// A concrete model identifier a host understands, such as `sonnet`: non-empty, with no whitespace or control
/// characters.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ModelName(String);

impl TryFrom<&str> for ModelName {
    type Error = ProjectionError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let reason = if value.is_empty() {
            Some(NameRejection::Empty)
        } else if value.chars().any(char::is_control) {
            Some(NameRejection::ControlCharacter)
        } else if value.chars().any(char::is_whitespace) {
            Some(NameRejection::InvalidCharacter)
        } else {
            None
        };
        match reason {
            Some(reason) => Err(ProjectionError::InvalidHostSetting { field: "model", reason }),
            None => Ok(Self(value.to_owned())),
        }
    }
}

impl<'a> From<&'a ModelName> for &'a str {
    fn from(value: &'a ModelName) -> Self {
        &value.0
    }
}

/// The Claude model each tier resolves to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeTiers {
    fast: ModelName,
    balanced: ModelName,
    deep: ModelName,
}

impl ClaudeTiers {
    /// Maps each tier to a model.
    #[must_use]
    pub const fn new(fast: ModelName, balanced: ModelName, deep: ModelName) -> Self {
        Self { fast, balanced, deep }
    }

    /// The model `tier` resolves to.
    #[must_use]
    pub const fn model(&self, tier: Tier) -> &ModelName {
        match tier {
            Tier::Fast => &self.fast,
            Tier::Balanced => &self.balanced,
            Tier::Deep => &self.deep,
        }
    }
}

impl Default for ClaudeTiers {
    fn default() -> Self {
        let name = |value: &str| ModelName(value.to_owned());
        Self::new(name("haiku"), name("sonnet"), name("opus"))
    }
}

/// How much reasoning Codex applies, as its `model_reasoning_effort` key spells it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ReasoningEffort {
    /// `minimal`.
    Minimal,
    /// `low`.
    Low,
    /// `medium`.
    Medium,
    /// `high`.
    High,
    /// `xhigh`.
    Xhigh,
    /// `max`.
    Max,
}

impl TryFrom<&str> for ReasoningEffort {
    type Error = ProjectionError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "minimal" => Ok(Self::Minimal),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::Xhigh),
            "max" => Ok(Self::Max),
            _ => Err(ProjectionError::InvalidHostSetting {
                field: "reasoning effort",
                reason: NameRejection::Unrecognised,
            }),
        }
    }
}

impl From<ReasoningEffort> for &'static str {
    fn from(value: ReasoningEffort) -> Self {
        match value {
            ReasoningEffort::Minimal => "minimal",
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
            ReasoningEffort::Xhigh => "xhigh",
            ReasoningEffort::Max => "max",
        }
    }
}

/// What one tier resolves to for Codex: an optional model (absent means the session's own) and a reasoning effort.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexTier {
    model: Option<ModelName>,
    effort: ReasoningEffort,
}

impl CodexTier {
    /// Builds a tier mapping.
    #[must_use]
    pub const fn new(model: Option<ModelName>, effort: ReasoningEffort) -> Self {
        Self { model, effort }
    }

    /// The model, when the tier names one.
    #[must_use]
    pub const fn model(&self) -> Option<&ModelName> {
        self.model.as_ref()
    }

    /// The reasoning effort.
    #[must_use]
    pub const fn effort(&self) -> ReasoningEffort {
        self.effort
    }
}

/// The Codex model and reasoning effort each tier resolves to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexTiers {
    fast: CodexTier,
    balanced: CodexTier,
    deep: CodexTier,
}

impl CodexTiers {
    /// Maps each tier.
    #[must_use]
    pub const fn new(fast: CodexTier, balanced: CodexTier, deep: CodexTier) -> Self {
        Self { fast, balanced, deep }
    }

    /// What `tier` resolves to.
    #[must_use]
    pub const fn tier(&self, tier: Tier) -> &CodexTier {
        match tier {
            Tier::Fast => &self.fast,
            Tier::Balanced => &self.balanced,
            Tier::Deep => &self.deep,
        }
    }
}

impl Default for CodexTiers {
    fn default() -> Self {
        Self::new(
            CodexTier::new(None, ReasoningEffort::Low),
            CodexTier::new(None, ReasoningEffort::Medium),
            CodexTier::new(None, ReasoningEffort::High),
        )
    }
}

/// The server name `ContextOS` tools are registered under unless configured otherwise.
const DEFAULT_SERVER: &str = "contextos";

/// The `ContextOS` tools a read-only Claude agent may use, and the MCP server name they are registered under.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadOnlyTools {
    server: String,
    tools: BTreeSet<String>,
}

impl ReadOnlyTools {
    /// The MCP server name.
    #[must_use]
    pub fn server(&self) -> &str {
        &self.server
    }

    /// The allowlist entries, `mcp__<server>__<tool>`, sorted by tool name.
    #[must_use]
    pub fn allowlist(&self) -> Vec<String> {
        self.tools
            .iter()
            .map(|tool| format!("mcp__{}__{tool}", self.server))
            .collect()
    }
}

/// Accepts an MCP server or tool name made of lowercase ASCII letters, digits, underscores, and hyphens.
fn validate_mcp_name(field: &'static str, value: &str) -> Result<(), ProjectionError> {
    let reason = if value.is_empty() {
        Some(NameRejection::Empty)
    } else if !value
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-')
    {
        Some(NameRejection::InvalidCharacter)
    } else {
        None
    };
    reason.map_or(Ok(()), |reason| {
        Err(ProjectionError::InvalidHostSetting { field, reason })
    })
}

impl Default for ReadOnlyTools {
    fn default() -> Self {
        Self {
            server: DEFAULT_SERVER.to_owned(),
            tools: BTreeSet::new(),
        }
    }
}

impl TryFrom<(&str, &[&str])> for ReadOnlyTools {
    type Error = ProjectionError;

    fn try_from((server, tools): (&str, &[&str])) -> Result<Self, Self::Error> {
        validate_mcp_name("server", server)?;
        let mut sorted = BTreeSet::new();
        for tool in tools {
            validate_mcp_name("tool", tool)?;
            sorted.insert((*tool).to_owned());
        }
        Ok(Self {
            server: server.to_owned(),
            tools: sorted,
        })
    }
}

#[cfg(test)]
#[path = "host_config_test.rs"]
mod tests;
