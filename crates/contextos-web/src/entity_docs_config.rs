//! `.contextos/entity-docs.toml`: the Command Centre entity-folder
//! screen's own folder-list-pane side and metadata-panel side. Unlike
//! `wiki_config.rs`/`insights_config.rs`, there is no root-directory field
//! here: an entity's own top-level directory is already fixed by
//! `.contextos/entities.toml`'s own slug list, so this file configures
//! only the two remaining real preferences.
//!
//! An absent or malformed file degrades to [`EntityDocsConfig::default`].

use serde::{Deserialize, Serialize};

use crate::mcp_client::McpClient;
use crate::panel_side::PanelSide;
use crate::vault_content::read_full_text;

pub const ENTITY_DOCS_TOML_PATH: &str = ".contextos/entity-docs.toml";

/// `.contextos/entity-docs.toml`'s full schema.
/// `deny_unknown_fields`: an unrecognised key is a schema violation, not a
/// silently ignored typo (`rust-quality.md`: "reject malformed or unknown
/// input").
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct EntityDocsConfig {
    /// Which side of the split screen the folder-list pane renders on.
    pub folder_side: PanelSide,
    /// Which side the metadata panel (a selected file's own frontmatter, shown and edited as
    /// real fields) stacks above the folder-list pane already rendering on that side -
    /// independent of [`Self::folder_side`], matching
    /// [`crate::wiki_config::WikiConfig::metadata_panel_side`]'s identical reasoning.
    pub metadata_panel_side: PanelSide,
}

/// Loads `vault_name`'s configured entity-folder settings. Never fails
/// outright: an absent file, an unreachable MCP session, or malformed TOML
/// all degrade to [`EntityDocsConfig::default`], matching
/// `wiki_config::load_wiki_config`'s identical contract.
pub async fn load_entity_docs_config(client: &McpClient, vault_name: &str) -> EntityDocsConfig {
    let Ok(Some(read)) = read_full_text(client, &format!("{vault_name}://{ENTITY_DOCS_TOML_PATH}")).await else {
        return EntityDocsConfig::default();
    };
    toml::from_str::<EntityDocsConfig>(&read).unwrap_or_default()
}

#[cfg(test)]
#[path = "entity_docs_config_test.rs"]
mod tests;
