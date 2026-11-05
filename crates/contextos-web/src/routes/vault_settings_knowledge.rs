//! The Knowledge categories of `/{{vault_name}}/settings/`
//! (`vault_settings.rs`'s own sibling, matching `vault_settings_dates.rs`'s
//! identical "split out once a section needs its own real form" precedent):
//! Wiki, Insights, and Entity folders. Each edits its own
//! `.contextos/*.toml` through a real per-field form - never a raw TOML
//! textarea, matching every other small fixed-field-set category already in
//! this crate. Wiki and Insights each configure a root directory plus a
//! folder-side preference; Entity folders configures only the side
//! preference, since its own root is already fixed per entity by
//! `.contextos/entities.toml`'s own slug list.

use serde_json::{Map, Value};

use crate::entity_docs_config::{self, ENTITY_DOCS_TOML_PATH, EntityDocsConfig};
use crate::inbox_outbox::sanitised_relative_path;
use crate::insights_config::{self, INSIGHTS_TOML_PATH, InsightsConfig};
use crate::mcp_client::{McpCallError, McpClient};
use crate::panel_side::PanelSide;
use crate::routes::vault::{html_response, unreachable};
use crate::routes::vault_settings::{
    FileInfoResult, SaveSectionBody, render_current, render_sections, write_toml_text,
};
use crate::wiki_config::{self, WIKI_TOML_PATH, WikiConfig};

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
/// for the equivalent-shaped field on Journal/Notes. `field_name` names the
/// real submitted field this call is validating (`tree_side`, `shard_side`,
/// `folder_side`, or `metadata_panel_side` - every side field this module's
/// three sections and Wiki's own metadata panel each have) in the error
/// message, so a rejection always names the field an operator actually
/// submitted rather than a different section's own field name.
fn parse_panel_side(raw: &str, field_name: &str) -> Result<PanelSide, String> {
    match raw {
        "left" => Ok(PanelSide::Left),
        "right" => Ok(PanelSide::Right),
        _ => Err(format!("{field_name} must be \"left\" or \"right\", not {raw:?}.")),
    }
}

// ---------------------------------------------------------------------
// Wiki
// ---------------------------------------------------------------------

/// The Wiki section's own rendered state: real, structured field values,
/// never a raw TOML blob.
pub(crate) struct WikiSectionState {
    pub(crate) wiki_dir_value: String,
    pub(crate) tree_side_is_left: bool,
    pub(crate) metadata_panel_side_is_left: bool,
    pub(crate) expected_hash: Option<String>,
    pub(crate) error: Option<String>,
}

/// Reads the Wiki section fresh: the configured file, or its typed default
/// when absent/malformed.
pub(crate) async fn read_wiki_section(client: &McpClient, vault_name: &str) -> WikiSectionState {
    let (config, expected_hash) = tokio::join!(
        wiki_config::load_wiki_config(client, vault_name),
        file_hash(client, vault_name, WIKI_TOML_PATH),
    );
    WikiSectionState {
        wiki_dir_value: config.wiki_dir,
        tree_side_is_left: config.tree_side == PanelSide::Left,
        metadata_panel_side_is_left: config.metadata_panel_side == PanelSide::Left,
        expected_hash,
        error: None,
    }
}

/// Re-renders the Wiki section with `error`, `submitted` preserved (rather
/// than reverted to disk) standing in for what would otherwise come from a
/// fresh read, and every other section read fresh from disk unchanged,
/// matching `vault_settings_dates::notes_error_response`'s identical shape.
async fn wiki_error_response(
    client: &McpClient,
    vault_name: &str,
    submitted: WikiSectionState,
    error: String,
) -> Response {
    let section = WikiSectionState {
        error: Some(error),
        ..submitted
    };
    html_response(render_sections(client, vault_name, move |sections| sections.wiki = section).await)
}

/// Saves the Wiki section: validates `wiki_dir` as a `/`-separated relative
/// path with no traversal segment (unlike `notes_dir`/`journal_dir`, this
/// one may genuinely nest - `memory/wiki` in the shipped default) and
/// `tree_side` as `"left"`/`"right"`, before writing anything. As with every
/// other real-form category, there is no "invalid TOML" failure mode here:
/// the operator submits already-validated-shape fields, not free text.
pub(crate) async fn save_wiki(client: &McpClient, vault_name: &str, body: &SaveSectionBody) -> Response {
    let expected_hash = body.expected_hash.clone();
    let submitted_side = body.date_structure_side.clone().unwrap_or_else(|| "right".to_owned());
    let submitted_metadata_side = body.metadata_panel_side.clone().unwrap_or_else(|| "right".to_owned());
    let submitted = WikiSectionState {
        wiki_dir_value: body.wiki_dir.clone().unwrap_or_default(),
        tree_side_is_left: submitted_side == "left",
        metadata_panel_side_is_left: submitted_metadata_side == "left",
        expected_hash: expected_hash.clone(),
        error: None,
    };

    let wiki_dir = match sanitised_relative_path(&submitted.wiki_dir_value) {
        Ok(segments) if !segments.is_empty() => segments.join("/"),
        Ok(_) => {
            return Box::pin(wiki_error_response(
                client,
                vault_name,
                submitted,
                "wiki_dir must not be empty.".to_owned(),
            ))
            .await;
        }
        Err(reason) => {
            return Box::pin(wiki_error_response(
                client,
                vault_name,
                submitted,
                format!("wiki_dir {reason}."),
            ))
            .await;
        }
    };
    let tree_side = match parse_panel_side(&submitted_side, "tree_side") {
        Ok(side) => side,
        Err(reason) => return Box::pin(wiki_error_response(client, vault_name, submitted, reason)).await,
    };
    let metadata_panel_side = match parse_panel_side(&submitted_metadata_side, "metadata_panel_side") {
        Ok(side) => side,
        Err(reason) => return Box::pin(wiki_error_response(client, vault_name, submitted, reason)).await,
    };

    let config = WikiConfig {
        wiki_dir,
        tree_side,
        metadata_panel_side,
    };
    let Ok(content) = toml::to_string(&config) else {
        return Box::pin(wiki_error_response(
            client,
            vault_name,
            submitted,
            "This configuration could not be saved.".to_owned(),
        ))
        .await;
    };
    match write_toml_text(client, vault_name, WIKI_TOML_PATH, &content, expected_hash).await {
        Ok(true) => html_response(Box::pin(render_current(client, vault_name)).await),
        Ok(false) => {
            Box::pin(wiki_error_response(
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

// ---------------------------------------------------------------------
// Insights
// ---------------------------------------------------------------------

/// The Insights section's own rendered state: real, structured field
/// values, never a raw TOML blob.
pub(crate) struct InsightsSectionState {
    pub(crate) insights_dir_value: String,
    pub(crate) shard_side_is_left: bool,
    pub(crate) metadata_panel_side_is_left: bool,
    pub(crate) expected_hash: Option<String>,
    pub(crate) error: Option<String>,
}

/// Reads the Insights section fresh: the configured file, or its typed
/// default when absent/malformed.
pub(crate) async fn read_insights_section(client: &McpClient, vault_name: &str) -> InsightsSectionState {
    let (config, expected_hash) = tokio::join!(
        insights_config::load_insights_config(client, vault_name),
        file_hash(client, vault_name, INSIGHTS_TOML_PATH),
    );
    InsightsSectionState {
        insights_dir_value: config.insights_dir,
        shard_side_is_left: config.shard_side == PanelSide::Left,
        metadata_panel_side_is_left: config.metadata_panel_side == PanelSide::Left,
        expected_hash,
        error: None,
    }
}

/// Re-renders the Insights section with `error`, `submitted` preserved
/// (rather than reverted to disk) standing in for what would otherwise
/// come from a fresh read, and every other section read fresh from disk
/// unchanged, matching [`wiki_error_response`]'s identical shape.
async fn insights_error_response(
    client: &McpClient,
    vault_name: &str,
    submitted: InsightsSectionState,
    error: String,
) -> Response {
    let section = InsightsSectionState {
        error: Some(error),
        ..submitted
    };
    html_response(render_sections(client, vault_name, move |sections| sections.insights = section).await)
}

/// Saves the Insights section: validates `insights_dir` as a `/`-separated relative path with
/// no traversal segment and `shard_side`/`metadata_panel_side` as `"left"`/`"right"`, before
/// writing anything, matching [`save_wiki`]'s identical reasoning.
pub(crate) async fn save_insights(client: &McpClient, vault_name: &str, body: &SaveSectionBody) -> Response {
    let expected_hash = body.expected_hash.clone();
    let submitted_side = body.date_structure_side.clone().unwrap_or_else(|| "right".to_owned());
    let submitted_metadata_side = body.metadata_panel_side.clone().unwrap_or_else(|| "right".to_owned());
    let submitted = InsightsSectionState {
        insights_dir_value: body.insights_dir.clone().unwrap_or_default(),
        shard_side_is_left: submitted_side == "left",
        metadata_panel_side_is_left: submitted_metadata_side == "left",
        expected_hash: expected_hash.clone(),
        error: None,
    };

    let insights_dir = match sanitised_relative_path(&submitted.insights_dir_value) {
        Ok(segments) if !segments.is_empty() => segments.join("/"),
        Ok(_) => {
            return Box::pin(insights_error_response(
                client,
                vault_name,
                submitted,
                "insights_dir must not be empty.".to_owned(),
            ))
            .await;
        }
        Err(reason) => {
            return Box::pin(insights_error_response(
                client,
                vault_name,
                submitted,
                format!("insights_dir {reason}."),
            ))
            .await;
        }
    };
    let shard_side = match parse_panel_side(&submitted_side, "shard_side") {
        Ok(side) => side,
        Err(reason) => return Box::pin(insights_error_response(client, vault_name, submitted, reason)).await,
    };
    let metadata_panel_side = match parse_panel_side(&submitted_metadata_side, "metadata_panel_side") {
        Ok(side) => side,
        Err(reason) => return Box::pin(insights_error_response(client, vault_name, submitted, reason)).await,
    };

    let config = InsightsConfig {
        insights_dir,
        shard_side,
        metadata_panel_side,
    };
    let Ok(content) = toml::to_string(&config) else {
        return Box::pin(insights_error_response(
            client,
            vault_name,
            submitted,
            "This configuration could not be saved.".to_owned(),
        ))
        .await;
    };
    match write_toml_text(client, vault_name, INSIGHTS_TOML_PATH, &content, expected_hash).await {
        Ok(true) => html_response(Box::pin(render_current(client, vault_name)).await),
        Ok(false) => {
            Box::pin(insights_error_response(
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

// ---------------------------------------------------------------------
// Entity folders
// ---------------------------------------------------------------------

/// The Entity folders section's own rendered state: real, structured
/// field values, never a raw TOML blob. No directory field here: an
/// entity's own root is already fixed by `.contextos/entities.toml`'s
/// own slug list, not something this category configures.
pub(crate) struct EntityDocsSectionState {
    pub(crate) folder_side_is_left: bool,
    pub(crate) metadata_panel_side_is_left: bool,
    pub(crate) expected_hash: Option<String>,
    pub(crate) error: Option<String>,
}

/// Reads the Entity folders section fresh: the configured file, or its
/// typed default when absent/malformed.
pub(crate) async fn read_entity_docs_section(client: &McpClient, vault_name: &str) -> EntityDocsSectionState {
    let (config, expected_hash) = tokio::join!(
        entity_docs_config::load_entity_docs_config(client, vault_name),
        file_hash(client, vault_name, ENTITY_DOCS_TOML_PATH),
    );
    EntityDocsSectionState {
        folder_side_is_left: config.folder_side == PanelSide::Left,
        metadata_panel_side_is_left: config.metadata_panel_side == PanelSide::Left,
        expected_hash,
        error: None,
    }
}

/// Re-renders the Entity folders section with `error`, `submitted`
/// preserved (rather than reverted to disk) standing in for what would
/// otherwise come from a fresh read, and every other section read fresh
/// from disk unchanged, matching [`wiki_error_response`]'s identical
/// shape.
async fn entity_docs_error_response(
    client: &McpClient,
    vault_name: &str,
    submitted: EntityDocsSectionState,
    error: String,
) -> Response {
    let section = EntityDocsSectionState {
        error: Some(error),
        ..submitted
    };
    html_response(render_sections(client, vault_name, move |sections| sections.entity = section).await)
}

/// Saves the Entity folders section: validates `folder_side` and
/// `metadata_panel_side` as `"left"`/`"right"`, before writing anything.
/// There is no directory field to validate - see [`EntityDocsSectionState`]'s
/// own doc for why.
pub(crate) async fn save_entity_docs(client: &McpClient, vault_name: &str, body: &SaveSectionBody) -> Response {
    let expected_hash = body.expected_hash.clone();
    let submitted_side = body.date_structure_side.clone().unwrap_or_else(|| "right".to_owned());
    let submitted_metadata_side = body.metadata_panel_side.clone().unwrap_or_else(|| "right".to_owned());
    let submitted = EntityDocsSectionState {
        folder_side_is_left: submitted_side == "left",
        metadata_panel_side_is_left: submitted_metadata_side == "left",
        expected_hash: expected_hash.clone(),
        error: None,
    };

    let folder_side = match parse_panel_side(&submitted_side, "folder_side") {
        Ok(side) => side,
        Err(reason) => return Box::pin(entity_docs_error_response(client, vault_name, submitted, reason)).await,
    };
    let metadata_panel_side = match parse_panel_side(&submitted_metadata_side, "metadata_panel_side") {
        Ok(side) => side,
        Err(reason) => return Box::pin(entity_docs_error_response(client, vault_name, submitted, reason)).await,
    };

    let config = EntityDocsConfig {
        folder_side,
        metadata_panel_side,
    };
    let Ok(content) = toml::to_string(&config) else {
        return Box::pin(entity_docs_error_response(
            client,
            vault_name,
            submitted,
            "This configuration could not be saved.".to_owned(),
        ))
        .await;
    };
    match write_toml_text(client, vault_name, ENTITY_DOCS_TOML_PATH, &content, expected_hash).await {
        Ok(true) => html_response(Box::pin(render_current(client, vault_name)).await),
        Ok(false) => {
            Box::pin(entity_docs_error_response(
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
#[path = "vault_settings_knowledge_test.rs"]
mod tests;
