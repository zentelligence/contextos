//! `.contextos/apps.toml`: per-app favourite state for the Command Centre Apps screen's own list-card star toggle
//! and the shared header Favourites panel's "Favourite apps" section. Read and written through the ordinary
//! `fs_attach_file`/`fs_write_file` MCP calls, mirroring `.contextos/jobs.toml`'s identical `[apps.<slug>]`
//! map-of-tables shape (`contextos_workflows::state::JobsState`); no schema change to
//! `registry/apps/{{slug}}/manifest.toml` itself, since favouriting is vault-side operator state, not an app's own
//! declared identity.
//!
//! `routes::apps`'s own favourite-toggle route is the sole writer of this file. An absent file, an unreachable MCP
//! session, or malformed TOML all degrade to no apps favourited, matching `inbox_outbox_config`'s own "never fail
//! outright" contract for a `.contextos/`-rooted config.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::mcp_client::{McpCallError, McpClient};
use crate::vault_content::read_full_text;

pub const APPS_TOML_PATH: &str = ".contextos/apps.toml";

/// `.contextos/apps.toml`'s full contents: a per-slug favourite flag only, no other per-app state lives here today.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppsFavouritesState {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub apps: BTreeMap<String, AppFavouriteEntry>,
}

/// `[apps.{{slug}}]`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppFavouriteEntry {
    pub favourite: bool,
}

impl AppsFavouritesState {
    /// Whether `slug` is favourited: `false` for a slug with no entry at all, the identical default an entry's own
    /// `favourite` field takes.
    #[must_use]
    pub fn is_favourite(&self, slug: &str) -> bool {
        self.apps.get(slug).is_some_and(|entry| entry.favourite)
    }
}

/// Loads `vault_name`'s configured app favourites. Never fails outright: an absent file, an unreachable MCP
/// session, or malformed TOML all degrade to [`AppsFavouritesState::default`] (no apps favourited).
pub async fn load_apps_favourites(client: &McpClient, vault_name: &str) -> AppsFavouritesState {
    let Ok(Some(read)) = read_full_text(client, &format!("{vault_name}://{APPS_TOML_PATH}")).await else {
        return AppsFavouritesState::default();
    };
    toml::from_str(&read).unwrap_or_default()
}

#[derive(Debug, thiserror::Error)]
pub enum SetFavouriteError {
    #[error(transparent)]
    Mcp(#[from] McpCallError),
    #[error("apps.toml could not be serialised: {0}")]
    Serialize(#[from] toml::ser::Error),
}

/// Sets `slug`'s own favourite flag and persists the result, returning the full state the caller's own re-render
/// (the list card and every favourited-app sidebar/header-panel surface) needs. Un-favouriting removes `slug`'s
/// own entry entirely rather than writing `favourite = false`: a bare, empty `[apps]` table for a vault with no
/// favourites, not a file that only ever grows.
///
/// This reads the current file, mutates the one slug in memory, then writes the whole file back (`force: true`,
/// matching `scheduler.rs`'s identical read-modify-write contract for `.contextos/jobs.toml`): this route is the
/// sole writer of `.contextos/apps.toml`, so a concurrent external editor is not a scenario this guards against.
///
/// # Errors
///
/// Returns [`SetFavouriteError::Mcp`] when the MCP transport itself fails, and
/// [`SetFavouriteError::Serialize`] on the (practically unreachable) case that the in-memory state fails to
/// serialise to TOML.
pub async fn set_app_favourite(
    client: &McpClient,
    vault_name: &str,
    slug: &str,
    favourite: bool,
) -> Result<AppsFavouritesState, SetFavouriteError> {
    let mut state = load_apps_favourites(client, vault_name).await;
    if favourite {
        state
            .apps
            .insert(slug.to_owned(), AppFavouriteEntry { favourite: true });
    } else {
        state.apps.remove(slug);
    }
    let content = toml::to_string(&state).map_err(SetFavouriteError::Serialize)?;
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://{APPS_TOML_PATH}")),
    );
    args.insert("content".to_owned(), Value::String(content));
    args.insert("force".to_owned(), Value::Bool(true));
    client.call_tool("fs_write_file".to_owned(), args).await?;
    Ok(state)
}

#[cfg(test)]
#[path = "apps_favourites_test.rs"]
mod tests;
