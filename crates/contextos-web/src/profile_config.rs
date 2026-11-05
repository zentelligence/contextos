//! `.contextos/profile.toml`: the Command Centre Profile screen's own
//! sidebar side and optional entity-profile template path. Unlike
//! `wiki_config.rs`/`insights_config.rs`, there is no root-directory field
//! for any of its three groups: Identity, Operating, and Entities each sit
//! at a fixed, well-known location (`memory/identity`, `memory/operating`,
//! `memory/entities`), not an operator-relocatable content tree, mirroring
//! `entity_docs_config.rs`'s identical "a fixed concept does not need a
//! second configurable location" reasoning.
//!
//! An absent or malformed file degrades to [`ProfileConfig::default`].

use serde::{Deserialize, Serialize};

use crate::mcp_client::McpClient;
use crate::panel_side::PanelSide;
use crate::vault_content::read_full_text;

pub const PROFILE_TOML_PATH: &str = ".contextos/profile.toml";

/// Identity's own fixed root, vault-relative.
pub const IDENTITY_DIR: &str = "memory/identity";
/// Operating's own fixed root, vault-relative.
pub const OPERATING_DIR: &str = "memory/operating";
/// Entities' own fixed root, vault-relative.
pub const ENTITIES_DIR: &str = "memory/entities";

/// `.contextos/profile.toml`'s full schema. `deny_unknown_fields`: an
/// unrecognised key is a schema violation, not a silently ignored typo
/// (`rust-quality.md`: "reject malformed or unknown input").
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProfileConfig {
    /// Which side of the split screen the sidebar renders on.
    pub side: PanelSide,
    /// A Templater-subset template's own vault-relative path
    /// (`notes_template::render_new_note`'s identical rendering, reused not
    /// duplicated), read to seed a brand-new entity profile page's own
    /// frontmatter and initial body. Empty (the default) means no template
    /// is configured: creation falls back to a minimal `type`/`title`
    /// frontmatter plus a bare heading. Identity and Operating creation
    /// never consult this field - both stay template-free, since each is a
    /// small closed set of one-off documents rather than a repeated
    /// per-entity pattern.
    pub entity_template_path: String,
}

/// Loads `vault_name`'s configured Profile settings. Never fails outright:
/// an absent file, an unreachable MCP session, or malformed TOML all
/// degrade to [`ProfileConfig::default`], matching
/// `entity_docs_config::load_entity_docs_config`'s identical contract.
pub async fn load_profile_config(client: &McpClient, vault_name: &str) -> ProfileConfig {
    let Ok(Some(read)) = read_full_text(client, &format!("{vault_name}://{PROFILE_TOML_PATH}")).await else {
        return ProfileConfig::default();
    };
    toml::from_str::<ProfileConfig>(&read).unwrap_or_default()
}

#[cfg(test)]
#[path = "profile_config_test.rs"]
mod tests;
