//! The `[projection]` section of `config.toml`: which agent hosts the server keeps in step with the vaults' registry
//! definitions, where, and which model each tier of agent resolves to.
//!
//! This module holds only the file's shape and its defaults. Turning it into validated settings (home expansion,
//! absolute targets, the vault-overlap check) is [`crate::projection_setup`]'s job, because those need the loaded
//! configuration's vaults, home directory, and source.

use std::path::PathBuf;

use serde::Deserialize;

/// The default Claude Code agents folder.
const CLAUDE_AGENTS: &str = "~/.claude/agents";
/// The default Claude Code skills folder.
const CLAUDE_SKILLS: &str = "~/.claude/skills";
/// The default Claude Code global instructions file.
const CLAUDE_INSTRUCTIONS: &str = "~/.claude/CLAUDE.md";
/// The default Codex agents folder.
const CODEX_AGENTS: &str = "~/.codex/agents";
/// The default Codex skills folder.
const CODEX_SKILLS: &str = "~/.agents/skills";
/// The default Codex global instructions file.
const CODEX_INSTRUCTIONS: &str = "~/.codex/AGENTS.md";
/// The default MCP server name `ContextOS` tools are registered under in Claude Code.
const DEFAULT_MCP_SERVER: &str = "contextos";

/// `[projection]`.
///
/// `enabled` left out means "as the configuration's source decides": on for the configuration at the platform's default
/// location, off for one named by `--config` or `CONTEXTOS_MCP_CONFIG`. An explicit value always wins.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProjectionConfig {
    /// Whether projection runs at all.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// `[projection.claude]`.
    #[serde(default)]
    pub claude: ClaudeProjectionConfig,
    /// `[projection.codex]`.
    #[serde(default)]
    pub codex: CodexProjectionConfig,
}

/// `[projection.claude]`: Claude Code's targets and model mapping.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ClaudeProjectionConfig {
    /// Whether Claude Code is projected into.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// The folder agent files are written to.
    #[serde(default = "claude_agents")]
    pub agents: PathBuf,
    /// The folder skill folders are written to.
    #[serde(default = "claude_skills")]
    pub skills: PathBuf,
    /// The global instructions file the orientation block is kept in.
    #[serde(default = "claude_instructions")]
    pub instructions: PathBuf,
    /// The MCP server name a read-only agent's `ContextOS` tools are allowed under (`mcp__<name>__<tool>`).
    #[serde(default = "default_mcp_server")]
    pub mcp_server: String,
    /// The model each tier resolves to.
    #[serde(default)]
    pub tiers: ClaudeTiersConfig,
}

impl Default for ClaudeProjectionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            agents: claude_agents(),
            skills: claude_skills(),
            instructions: claude_instructions(),
            mcp_server: default_mcp_server(),
            tiers: ClaudeTiersConfig::default(),
        }
    }
}

/// `[projection.claude.tiers]`: the Claude Code model each tier resolves to.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ClaudeTiersConfig {
    /// The model for `fast` agents.
    #[serde(default = "claude_fast")]
    pub fast: String,
    /// The model for `balanced` agents.
    #[serde(default = "claude_balanced")]
    pub balanced: String,
    /// The model for `deep` agents.
    #[serde(default = "claude_deep")]
    pub deep: String,
}

impl Default for ClaudeTiersConfig {
    fn default() -> Self {
        Self {
            fast: claude_fast(),
            balanced: claude_balanced(),
            deep: claude_deep(),
        }
    }
}

/// `[projection.codex]`: Codex's targets and model mapping.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CodexProjectionConfig {
    /// Whether Codex is projected into.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// The folder agent files are written to.
    #[serde(default = "codex_agents")]
    pub agents: PathBuf,
    /// The folder skill folders are written to.
    #[serde(default = "codex_skills")]
    pub skills: PathBuf,
    /// The global instructions file the orientation block is kept in.
    #[serde(default = "codex_instructions")]
    pub instructions: PathBuf,
    /// The model and reasoning effort each tier resolves to.
    #[serde(default)]
    pub tiers: CodexTiersConfig,
}

impl Default for CodexProjectionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            agents: codex_agents(),
            skills: codex_skills(),
            instructions: codex_instructions(),
            tiers: CodexTiersConfig::default(),
        }
    }
}

/// `[projection.codex.tiers]`: what each tier resolves to for Codex.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CodexTiersConfig {
    /// `fast` agents; reasoning effort `low` unless set.
    #[serde(default)]
    pub fast: CodexTierConfig,
    /// `balanced` agents; reasoning effort `medium` unless set.
    #[serde(default)]
    pub balanced: CodexTierConfig,
    /// `deep` agents; reasoning effort `high` unless set.
    #[serde(default)]
    pub deep: CodexTierConfig,
}

/// One Codex tier: an optional model and an optional reasoning effort, each falling back to the tier's default.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CodexTierConfig {
    /// The model; left out, Codex uses its own default.
    #[serde(default)]
    pub model: Option<String>,
    /// The reasoning effort: `minimal`, `low`, `medium`, `high`, `xhigh`, or `max`.
    #[serde(default)]
    pub effort: Option<String>,
}

const fn default_true() -> bool {
    true
}

fn default_mcp_server() -> String {
    DEFAULT_MCP_SERVER.to_owned()
}

fn claude_agents() -> PathBuf {
    PathBuf::from(CLAUDE_AGENTS)
}

fn claude_skills() -> PathBuf {
    PathBuf::from(CLAUDE_SKILLS)
}

fn claude_instructions() -> PathBuf {
    PathBuf::from(CLAUDE_INSTRUCTIONS)
}

fn codex_agents() -> PathBuf {
    PathBuf::from(CODEX_AGENTS)
}

fn codex_skills() -> PathBuf {
    PathBuf::from(CODEX_SKILLS)
}

fn codex_instructions() -> PathBuf {
    PathBuf::from(CODEX_INSTRUCTIONS)
}

fn claude_fast() -> String {
    "haiku".to_owned()
}

fn claude_balanced() -> String {
    "sonnet".to_owned()
}

fn claude_deep() -> String {
    "opus".to_owned()
}
