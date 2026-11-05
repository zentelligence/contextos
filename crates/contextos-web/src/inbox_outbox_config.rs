//! `.contextos/inbox-outbox.toml`: the Command Centre Inbox/Outbox screens'
//! own folder-naming and knowledge-curator-agent configuration. Read
//! through an ordinary MCP tool call, like every other `.contextos/`-rooted
//! file (`agents.rs`/`tasks_config.rs`'s identical precedent); this crate
//! writes it only through its own `routes::vault_settings` section,
//! validated before writing, never in response to an Inbox/Outbox screen
//! action itself.
//!
//! An absent or malformed file degrades to [`InboxOutboxConfig::default`]:
//! the base scaffold's own `inbox/`/`outbox/`/`processed/` directory names
//! and the knowledge curator agent's own default slug, `"karla"`. Named
//! `inbox-outbox.toml`, not `inbox.toml`: it configures both screens
//! equally (`outbox_dir` included), so a name reading as Inbox-only would
//! misrepresent its own scope.

use serde::{Deserialize, Serialize};

use crate::mcp_client::McpClient;
use crate::vault_content::read_full_text;

pub const INBOX_OUTBOX_TOML_PATH: &str = ".contextos/inbox-outbox.toml";

/// `.contextos/inbox-outbox.toml`'s full schema: four fixed scalar fields, never a variable-length structure, so
/// `routes::vault_settings` edits it through a real per-field form (a `curator_agent` select sourced from
/// `.contextos/team.toml`'s own agent entries, three plain directory-name inputs), not a raw TOML textarea — unlike
/// `.contextos/tasks.toml`'s genuinely open-ended `[[stage]]` list, a form here cannot let an operator submit a
/// value this file could not already represent, and it rules out an unregistered `curator_agent` structurally
/// rather than catching it after a parse. `Serialize` round-trips a validated form submission straight back into
/// this file's own standard TOML text. `deny_unknown_fields`: an unrecognised key is a schema violation, not a
/// silently ignored typo (`rust-quality.md`: "reject malformed or unknown input").
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct InboxOutboxConfig {
    /// The task `assignee` an Inbox "Ingest" action stamps onto the task record it creates: the knowledge curator
    /// agent's own slug. Must name an agent slug already present in `.contextos/team.toml`, checked both when this
    /// file is saved (`routes::vault_settings`) and again at the moment an Ingest action actually runs
    /// (`routes::inbox_outbox`), since the configured team can change between the two; this crate never writes
    /// `team.toml` itself, so an unregistered curator is the operator's own configuration to fix there, not
    /// something this screen repairs on their behalf.
    pub curator_agent: String,
    /// `inbox/`'s own directory name, vault-root-relative.
    pub inbox_dir: String,
    /// `outbox/`'s own directory name, vault-root-relative.
    pub outbox_dir: String,
    /// The "handled" marker directory's own name: a top-level sibling of
    /// `inbox_dir`, not nested inside it, matching a plain, uniform
    /// top-level scaffold layout rather than a directory hidden a level
    /// deeper.
    pub processed_dir: String,
}

impl Default for InboxOutboxConfig {
    fn default() -> Self {
        Self {
            curator_agent: "karla".to_owned(),
            inbox_dir: "inbox".to_owned(),
            outbox_dir: "outbox".to_owned(),
            processed_dir: "processed".to_owned(),
        }
    }
}

/// Loads `vault_name`'s configured Inbox/Outbox settings. Never fails
/// outright: an absent file, an unreachable MCP session, or malformed TOML
/// all degrade to [`InboxOutboxConfig::default`], matching `agents.rs`'s
/// `load_agents`/`tasks_config.rs`'s `load_tasks_config` identical contract.
pub async fn load_inbox_outbox_config(client: &McpClient, vault_name: &str) -> InboxOutboxConfig {
    let Ok(Some(read)) = read_full_text(client, &format!("{vault_name}://{INBOX_OUTBOX_TOML_PATH}")).await else {
        return InboxOutboxConfig::default();
    };
    toml::from_str::<InboxOutboxConfig>(&read).unwrap_or_default()
}

#[cfg(test)]
#[path = "inbox_outbox_config_test.rs"]
mod tests;
