//! `PATCH /{{vault_name}}/insights/{{*path}}` - saving an existing note's own body or
//! frontmatter, and its own `/rename` sub-path for the combined title/status action: split out
//! of `routes::insights` (already close to this workspace's own file-size guidance) for the
//! same reason `routes::wiki_save` already split out of `routes::wiki`, matching that module's
//! own precedent of splitting a screen's own action into its own file rather than growing
//! `routes::insights` further.
//!
//! Every rendering this module calls back into - `render_insights_fragment` in particular -
//! stays in `routes::insights`, since the one view every Insights response renders through
//! lives there; this module only ever calls back into it, never the reverse.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::insights::{self, LoadInsightError};
use crate::insights_config::{self, InsightsConfig};
use crate::mcp_client::{McpCallError, McpClient};
use crate::metadata_panel;
use crate::routes::vault::{html_response, not_found, unreachable};

use super::insights::{
    InsightsRoutesState, as_note_path, ensure_vault, note_action_path, render_insights_fragment, sanitised_path,
};

/// `PATCH /{{vault_name}}/insights/{{*path}}`'s own request body: structurally two
/// distinct shapes on the one endpoint, matching `routes::wiki_save::SaveBody`'s identical
/// "content-replace or frontmatter-patch, distinguished by which field is present" contract.
/// `Content` replaces the body text verbatim, keeping frontmatter (including `status`)
/// untouched ([`insights::save_note_body`]); `Metadata` is the metadata panel's own submitted
/// form, keyed by each editable frontmatter field's own real name
/// ([`metadata_panel::metadata_field_views`]) - [`metadata_panel::metadata_patch`] turns it
/// back into the right JSON shape.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SaveBody {
    Content {
        content: String,
        #[serde(default)]
        expected_hash: Option<String>,
    },
    Metadata {
        patch: std::collections::BTreeMap<String, String>,
        #[serde(default)]
        expected_hash: Option<String>,
    },
}

#[derive(Debug, Default, Deserialize)]
struct RenameBody {
    #[serde(default)]
    title: String,
    #[serde(default)]
    status: String,
}

fn malformed_body() -> Response {
    html_response("<p class=\"hint error\">Malformed request.</p>".to_owned())
}

fn rename_error_message(error: &insights::RenameNoteError) -> String {
    match error {
        insights::RenameNoteError::EmptyTitle => "A note needs a title.".to_owned(),
        insights::RenameNoteError::InvalidStatus => "Status must be one of proposed, adopted, or dismissed.".to_owned(),
        insights::RenameNoteError::Write(_) => "The vault could not be reached.".to_owned(),
        insights::RenameNoteError::Rejected => {
            "The note could not be renamed (a note with that title may already exist).".to_owned()
        }
    }
}

/// `PATCH /{{vault_name}}/insights/{{*path}}/rename`: the
/// rename-and-restatus form's own action, updating the note's own title
/// (and, when its slug changes, its own filename and shard) and `status`
/// together ([`insights::rename_note`]). Redirects to the note's own
/// current `path` on success, matching `routes::wiki_save::patch_rename`'s
/// identical "the URL always names the file's current identity" contract.
async fn patch_rename(
    client: &McpClient,
    vault_name: &str,
    config: &InsightsConfig,
    relative_path: &str,
    body: &axum::body::Bytes,
) -> Response {
    let Ok(parsed) = serde_json::from_slice::<RenameBody>(body) else {
        return malformed_body();
    };
    match insights::rename_note(client, vault_name, config, relative_path, &parsed.title, &parsed.status).await {
        Ok(new_relative_path) => {
            let target = crate::urls::VaultUrls::from(vault_name).insights(&new_relative_path);
            (StatusCode::OK, [("HX-Redirect", target)], String::new()).into_response()
        }
        Err(insights::RenameNoteError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. })) => {
            unreachable()
        }
        Err(error) => {
            let message = rename_error_message(&error);
            let fragment = Box::pin(render_insights_fragment(
                client,
                vault_name,
                config,
                relative_path,
                Some(message),
            ))
            .await
            .unwrap_or_else(|| "<p>The Insights screen could not be rendered.</p>".to_owned());
            html_response(fragment)
        }
    }
}

/// `PATCH /{{vault_name}}/insights/{{*path}}`: saves the note named by `path`,
/// either its body ([`patch_body_content`]) or its frontmatter ([`patch_body_metadata`]) -
/// never both in one request, [`SaveBody`]'s own doc - or, when `path` ends in `/rename`,
/// dispatches [`patch_rename`] instead, matching `routes::wiki_save::patch_body`'s own
/// identical suffix-dispatch shape.
pub async fn patch_body(
    State(state): State<InsightsRoutesState>,
    Path((vault_name, raw_path)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let Some(path) = sanitised_path(&raw_path) else {
        return not_found();
    };
    let config = insights_config::load_insights_config(&client, &vault_name).await;

    if let Some(note_path) = note_action_path(&path, "/rename") {
        return Box::pin(patch_rename(&client, &vault_name, &config, note_path, &body)).await;
    }
    let Some(path) = as_note_path(&path).map(str::to_owned) else {
        return not_found();
    };
    let Ok(parsed) = serde_json::from_slice::<SaveBody>(&body) else {
        return malformed_body();
    };

    match parsed {
        SaveBody::Content { content, expected_hash } => {
            Box::pin(patch_body_content(
                &client,
                &vault_name,
                &config,
                &path,
                &content,
                expected_hash,
            ))
            .await
        }
        SaveBody::Metadata { patch, expected_hash } => {
            Box::pin(patch_body_metadata(
                &client,
                &vault_name,
                &config,
                &path,
                patch,
                expected_hash,
            ))
            .await
        }
    }
}

async fn patch_body_content(
    client: &McpClient,
    vault_name: &str,
    config: &InsightsConfig,
    relative_path: &str,
    content: &str,
    expected_hash: Option<String>,
) -> Response {
    match insights::save_note_body(client, vault_name, config, relative_path, content, expected_hash).await {
        Ok(true) => html_response(
            Box::pin(render_insights_fragment(
                client,
                vault_name,
                config,
                relative_path,
                None,
            ))
            .await
            .unwrap_or_else(|| "<p>The Insights screen could not be rendered.</p>".to_owned()),
        ),
        Ok(false) => html_response(
            Box::pin(render_insights_fragment(
                client,
                vault_name,
                config,
                relative_path,
                Some("This note changed since it was last loaded; reload and try again.".to_owned()),
            ))
            .await
            .unwrap_or_else(|| "<p>The Insights screen could not be rendered.</p>".to_owned()),
        ),
        Err(insights::SaveNoteError::Load(insights::ReadInsightError::NotFound)) => not_found(),
        Err(insights::SaveNoteError::Malformed) => {
            let fragment = Box::pin(render_insights_fragment(
                client,
                vault_name,
                config,
                relative_path,
                Some("This note's own frontmatter could not be read.".to_owned()),
            ))
            .await
            .unwrap_or_else(|| "<p>The Insights screen could not be rendered.</p>".to_owned());
            html_response(fragment)
        }
        Err(
            insights::SaveNoteError::Load(insights::ReadInsightError::Write(
                McpCallError::Unreachable { .. } | McpCallError::Refused { .. },
            ))
            | insights::SaveNoteError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }),
        ) => unreachable(),
    }
}

/// The metadata panel's own save: applies `submitted`'s edits to the selected note's
/// frontmatter, its body (including `status`) left untouched
/// ([`insights::save_note_metadata`]). Re-reads the note's own current frontmatter first
/// (never trusting a client-supplied shape, [`metadata_panel::metadata_patch`]'s own doc,
/// [`insights::load_note_frontmatter`]'s own "read fresh before patch" contract), matching
/// `routes::wiki_save::patch_body_metadata`'s identical shape.
async fn patch_body_metadata(
    client: &McpClient,
    vault_name: &str,
    config: &InsightsConfig,
    relative_path: &str,
    submitted: std::collections::BTreeMap<String, String>,
    expected_hash: Option<String>,
) -> Response {
    let current = match insights::load_note_frontmatter(client, vault_name, config, relative_path).await {
        Ok(frontmatter) => frontmatter,
        Err(LoadInsightError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. })) => {
            return unreachable();
        }
        Err(LoadInsightError::NotFound) => return not_found(),
        Err(LoadInsightError::Malformed) => {
            let fragment = Box::pin(render_insights_fragment(
                client,
                vault_name,
                config,
                relative_path,
                Some("This note's own frontmatter could not be read.".to_owned()),
            ))
            .await
            .unwrap_or_else(|| "<p>The Insights screen could not be rendered.</p>".to_owned());
            return html_response(fragment);
        }
    };
    let patch = metadata_panel::metadata_patch(&current, submitted);

    match insights::save_note_metadata(client, vault_name, config, relative_path, patch, expected_hash).await {
        Ok(true) => html_response(
            Box::pin(render_insights_fragment(
                client,
                vault_name,
                config,
                relative_path,
                None,
            ))
            .await
            .unwrap_or_else(|| "<p>The Insights screen could not be rendered.</p>".to_owned()),
        ),
        Ok(false) => html_response(
            Box::pin(render_insights_fragment(
                client,
                vault_name,
                config,
                relative_path,
                Some("This note changed since it was last loaded; reload and try again.".to_owned()),
            ))
            .await
            .unwrap_or_else(|| "<p>The Insights screen could not be rendered.</p>".to_owned()),
        ),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => unreachable(),
    }
}

#[cfg(test)]
#[path = "insights_save_test.rs"]
mod tests;
