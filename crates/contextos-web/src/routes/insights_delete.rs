//! `DELETE /{{vault_name}}/insights/{{*path}}` - moving a note to trash: split out of `routes::insights`
//! for the same reason `routes::insights_save` is, so the screen's own module stays within the file-size limit.
//!
//! Every rendering this module calls back into stays in `routes::insights`; this module only ever calls back into
//! it, never the reverse.

use axum::extract::{Path, State};
use axum::response::Response;

use crate::insights;
use crate::insights_config;
use crate::mcp_client::McpCallError;
use crate::routes::vault::{html_response, not_found, unreachable};

use super::insights::{InsightsRoutesState, as_note_path, ensure_vault, render_insights_fragment, sanitised_path};

/// `DELETE /{{vault_name}}/insights/{{*path}}`: moves the named
/// note to trash ([`insights::delete_note`]). On success, re-renders the
/// shard-picker root fragment (no note requested, matching this screen's
/// own "browse the shards" empty state - unlike Notes, Insights has no
/// "most recently edited note" fallback to reselect instead) - the same
/// `hx-target="closest .insights-screen" hx-swap="outerHTML"` full-refresh
/// convention every other mutation on this screen already uses.
///
/// A malformed or missing `path`, or one naming a bare shard rather than a
/// real note, is a genuine `404` ([`as_note_path`]'s own contract, matching
/// every other action on this screen). A note that no longer exists, or
/// that `fs_delete_file` otherwise refuses to delete, re-renders that
/// note's own shard fragment with an error message in the same `error`
/// slot every other handler already uses - never a raw `404`, matching the
/// HTMX surface's own error-fragment rule.
pub async fn delete(
    State(state): State<InsightsRoutesState>,
    Path((vault_name, raw_path)): Path<(String, String)>,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let Some(path) = sanitised_path(&raw_path).and_then(|path| as_note_path(&path).map(str::to_owned)) else {
        return not_found();
    };

    let config = insights_config::load_insights_config(&client, &vault_name).await;
    match insights::delete_note(&client, &vault_name, &config, &path).await {
        Ok(()) => html_response(
            Box::pin(render_insights_fragment(&client, &vault_name, &config, "", None))
                .await
                .unwrap_or_else(|| "<p>The Insights screen could not be rendered.</p>".to_owned()),
        ),
        Err(insights::DeleteNoteError::NotFound) => {
            let shard = path.split('/').next().unwrap_or("");
            let fragment = Box::pin(render_insights_fragment(
                &client,
                &vault_name,
                &config,
                shard,
                Some("This note could not be found.".to_owned()),
            ))
            .await
            .unwrap_or_else(|| "<p>The Insights screen could not be rendered.</p>".to_owned());
            html_response(fragment)
        }
        Err(insights::DeleteNoteError::Rejected) => {
            let shard = path.split('/').next().unwrap_or("");
            let fragment = Box::pin(render_insights_fragment(
                &client,
                &vault_name,
                &config,
                shard,
                Some("This note could not be deleted.".to_owned()),
            ))
            .await
            .unwrap_or_else(|| "<p>The Insights screen could not be rendered.</p>".to_owned());
            html_response(fragment)
        }
        Err(insights::DeleteNoteError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. })) => {
            unreachable()
        }
    }
}
