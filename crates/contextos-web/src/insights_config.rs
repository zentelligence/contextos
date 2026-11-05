//! `.contextos/insights.toml`: the Command Centre Insights screen's own
//! root folder and shard-picker side, read through an ordinary MCP tool
//! call, matching `wiki_config.rs`'s identical precedent. `contextos-web`
//! writes it only through its own `routes::vault_settings` Insights section,
//! validated before writing, never in response to an Insights screen action
//! itself.
//!
//! An absent or malformed file degrades to [`InsightsConfig::default`]: the
//! base scaffold's own `memory/insights/` directory.

use serde::{Deserialize, Serialize};

use crate::mcp_client::McpClient;
use crate::panel_side::PanelSide;
use crate::vault_content::read_full_text;

pub const INSIGHTS_TOML_PATH: &str = ".contextos/insights.toml";

/// `.contextos/insights.toml`'s full schema, so `routes::vault_settings`
/// edits it through a real per-field form (a plain path input, a select),
/// matching [`crate::wiki_config::WikiConfig`]'s identical "fixed small
/// field set, no raw-text editor" reasoning. `deny_unknown_fields`: an
/// unrecognised key is a schema violation, not a silently ignored typo
/// (`rust-quality.md`: "reject malformed or unknown input").
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct InsightsConfig {
    /// The sharded insight-note store's own top-level directory name,
    /// vault-root-relative.
    pub insights_dir: String,
    /// Which side of the split screen the shard picker renders on.
    pub shard_side: PanelSide,
    /// Which side the metadata panel (a selected note's own frontmatter, shown and edited as
    /// real fields) stacks above the shard-picker pane already on that side - independent of
    /// [`Self::shard_side`]: an operator may put the metadata panel on the same side as the
    /// shard picker, or the other one, matching
    /// [`crate::wiki_config::WikiConfig::metadata_panel_side`]'s identical reasoning.
    pub metadata_panel_side: PanelSide,
}

impl Default for InsightsConfig {
    fn default() -> Self {
        Self {
            insights_dir: "memory/insights".to_owned(),
            shard_side: PanelSide::default(),
            metadata_panel_side: PanelSide::default(),
        }
    }
}

/// Loads `vault_name`'s configured Insights settings. Never fails outright:
/// an absent file, an unreachable MCP session, or malformed TOML all
/// degrade to [`InsightsConfig::default`], matching
/// `wiki_config::load_wiki_config`'s identical contract.
pub async fn load_insights_config(client: &McpClient, vault_name: &str) -> InsightsConfig {
    let Ok(Some(read)) = read_full_text(client, &format!("{vault_name}://{INSIGHTS_TOML_PATH}")).await else {
        return InsightsConfig::default();
    };
    toml::from_str::<InsightsConfig>(&read).unwrap_or_default()
}

#[cfg(test)]
#[path = "insights_config_test.rs"]
mod tests;
