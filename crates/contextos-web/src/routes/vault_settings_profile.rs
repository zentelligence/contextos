//! The Profile category of `/{{vault_name}}/settings/` (`vault_settings.rs`'s
//! own sibling, matching `vault_settings_dates.rs`'s identical "split out
//! once a section needs its own real form" precedent): the sidebar side
//! preference plus the optional entity-profile template path. No root-path
//! fields here at all - Identity, Operating, and Entities each sit at a
//! fixed, well-known location, not something an operator relocates.

use serde_json::{Map, Value};

use crate::inbox_outbox::sanitised_relative_path;
use crate::mcp_client::{McpCallError, McpClient};
use crate::panel_side::PanelSide;
use crate::profile_config::{self, PROFILE_TOML_PATH, ProfileConfig};
use crate::routes::vault::{html_response, unreachable};
use crate::routes::vault_settings::{
    FileInfoResult, SaveSectionBody, render_current, render_sections, write_toml_text,
};

use axum::response::Response;

async fn file_hash(client: &McpClient, vault_name: &str, path: &str) -> Option<String> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{path}")));
    match client.call_tool("fs_get_file_info".to_owned(), args).await {
        Ok(result) if result.is_error != Some(true) => result
            .into_typed::<FileInfoResult>()
            .ok()
            .and_then(|info| info.content_hash),
        _ => None,
    }
}

/// `"left"`/`"right"` (case-sensitive, matching the `<select>`'s own
/// `value` attributes this crate itself renders) into [`PanelSide`];
/// anything else is a validation error, matching
/// `vault_settings_dates::parse_date_structure_side`'s identical reasoning
/// for the equivalent-shaped field on Journal/Notes.
fn parse_panel_side(raw: &str) -> Result<PanelSide, String> {
    match raw {
        "left" => Ok(PanelSide::Left),
        "right" => Ok(PanelSide::Right),
        _ => Err(format!("side must be \"left\" or \"right\", not {raw:?}.")),
    }
}

/// The Profile section's own rendered state: real, structured field
/// values, never a raw TOML blob.
pub(crate) struct ProfileSectionState {
    pub(crate) side_is_left: bool,
    pub(crate) entity_template_path_value: String,
    pub(crate) expected_hash: Option<String>,
    pub(crate) error: Option<String>,
}

/// Reads the Profile section fresh: the configured file, or its typed
/// default when absent/malformed.
pub(crate) async fn read_profile_section(client: &McpClient, vault_name: &str) -> ProfileSectionState {
    let (config, expected_hash) = tokio::join!(
        profile_config::load_profile_config(client, vault_name),
        file_hash(client, vault_name, PROFILE_TOML_PATH),
    );
    ProfileSectionState {
        side_is_left: config.side == PanelSide::Left,
        entity_template_path_value: config.entity_template_path,
        expected_hash,
        error: None,
    }
}

/// Re-renders the Profile section with `error`, `submitted` preserved
/// (rather than reverted to disk) standing in for what would otherwise
/// come from a fresh read, and every other section read fresh from disk
/// unchanged, matching `vault_settings_knowledge`'s own identical shape.
async fn profile_error_response(
    client: &McpClient,
    vault_name: &str,
    submitted: ProfileSectionState,
    error: String,
) -> Response {
    let section = ProfileSectionState {
        error: Some(error),
        ..submitted
    };
    html_response(render_sections(client, vault_name, move |sections| sections.profile = section).await)
}

/// Saves the Profile section: validates `side` as `"left"`/`"right"` and
/// `entity_template_path` as an empty value (no template configured) or a
/// `/`-separated relative path with no traversal segment, before writing
/// anything.
pub(crate) async fn save_profile(client: &McpClient, vault_name: &str, body: &SaveSectionBody) -> Response {
    let expected_hash = body.expected_hash.clone();
    let submitted_side = body.date_structure_side.clone().unwrap_or_else(|| "right".to_owned());
    let submitted_template_path = body.entity_template_path.clone().unwrap_or_default();
    let submitted = ProfileSectionState {
        side_is_left: submitted_side == "left",
        entity_template_path_value: submitted_template_path.clone(),
        expected_hash: expected_hash.clone(),
        error: None,
    };

    let entity_template_path = if submitted_template_path.trim().is_empty() {
        String::new()
    } else {
        match sanitised_relative_path(&submitted_template_path) {
            Ok(segments) => segments.join("/"),
            Err(reason) => {
                return Box::pin(profile_error_response(
                    client,
                    vault_name,
                    submitted,
                    format!("entity_template_path {reason}."),
                ))
                .await;
            }
        }
    };
    let side = match parse_panel_side(&submitted_side) {
        Ok(side) => side,
        Err(reason) => return Box::pin(profile_error_response(client, vault_name, submitted, reason)).await,
    };

    let config = ProfileConfig {
        side,
        entity_template_path,
    };
    let Ok(content) = toml::to_string(&config) else {
        return Box::pin(profile_error_response(
            client,
            vault_name,
            submitted,
            "This configuration could not be saved.".to_owned(),
        ))
        .await;
    };
    match write_toml_text(client, vault_name, PROFILE_TOML_PATH, &content, expected_hash).await {
        Ok(true) => html_response(Box::pin(render_current(client, vault_name)).await),
        Ok(false) => {
            Box::pin(profile_error_response(
                client,
                vault_name,
                submitted,
                "The file could not be saved (it may have changed since you loaded this page).".to_owned(),
            ))
            .await
        }
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => unreachable(),
    }
}

#[cfg(test)]
#[path = "vault_settings_profile_test.rs"]
mod tests;
