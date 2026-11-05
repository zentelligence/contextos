//! `.contextos/wiki.toml`: the Command Centre Wiki screen's own root folder, navigation-panel
//! side, and metadata-panel side, read through an ordinary MCP tool call, like every other
//! `.contextos/`-rooted file (`notes_config.rs`'s identical precedent). `contextos-web` writes
//! it only through its own `routes::vault_settings` Wiki section, validated before writing,
//! never in response to a Wiki screen action itself.
//!
//! An absent or malformed file degrades to [`WikiConfig::default`]: the
//! base scaffold's own `memory/wiki/` directory.

use serde::{Deserialize, Serialize};

use crate::mcp_client::McpClient;
use crate::panel_side::PanelSide;
use crate::vault_content::read_full_text;

pub const WIKI_TOML_PATH: &str = ".contextos/wiki.toml";

/// `.contextos/wiki.toml`'s full schema, so `routes::vault_settings` edits
/// it through a real per-field form (a plain path input, a select),
/// matching [`crate::notes_config::NotesConfig`]'s identical "fixed small
/// field set, no raw-text editor" reasoning. `deny_unknown_fields`: an
/// unrecognised key is a schema violation, not a silently ignored typo
/// (`rust-quality.md`: "reject malformed or unknown input").
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WikiConfig {
    /// The compiled wiki's own top-level directory name, vault-root-relative.
    pub wiki_dir: String,
    /// Which side of the split screen the folder-tree sidebar renders on.
    pub tree_side: PanelSide,
    /// Which side the metadata panel (a selected page's own frontmatter, shown and edited as
    /// real fields) stacks above the folder-tree pane that already renders on that side -
    /// independent of [`Self::tree_side`]: an operator may put the metadata panel on the same
    /// side as the folder tree, or the other one, matching
    /// [`crate::notes_config::NotesConfig::metadata_panel_side`]'s identical reasoning.
    pub metadata_panel_side: PanelSide,
}

impl Default for WikiConfig {
    fn default() -> Self {
        Self {
            wiki_dir: "memory/wiki".to_owned(),
            tree_side: PanelSide::default(),
            metadata_panel_side: PanelSide::default(),
        }
    }
}

/// Loads `vault_name`'s configured Wiki settings. Never fails outright: an
/// absent file, an unreachable MCP session, or malformed TOML all degrade
/// to [`WikiConfig::default`], matching `notes_config::load_notes_config`'s
/// identical contract.
pub async fn load_wiki_config(client: &McpClient, vault_name: &str) -> WikiConfig {
    let Ok(Some(read)) = read_full_text(client, &format!("{vault_name}://{WIKI_TOML_PATH}")).await else {
        return WikiConfig::default();
    };
    toml::from_str::<WikiConfig>(&read).unwrap_or_default()
}

#[cfg(test)]
#[path = "wiki_config_test.rs"]
mod tests;
