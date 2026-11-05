//! `DELETE /{{vault_name}}/wiki/{{*path}}` - moving a page or an empty folder to trash: split out of
//! `routes::wiki` for the same reason `routes::wiki_save` is, so the screen's own module stays within the file-size
//! limit.
//!
//! Every rendering this module calls back into stays in `routes::wiki`; this module only ever calls back into it,
//! never the reverse.

use axum::extract::{Path, State};
use axum::response::Response;

use crate::mcp_client::McpCallError;
use crate::routes::vault::{html_response, not_found, unreachable};
use crate::{wiki, wiki_config};

use super::wiki::{TargetKind, WikiRoutesState, ensure_vault, render_wiki_fragment, resolve_target, sanitised_path};

fn delete_error_message(error: &wiki::DeleteWikiPathError) -> String {
    match error {
        wiki::DeleteWikiPathError::NotFound => "This could not be found.".to_owned(),
        wiki::DeleteWikiPathError::NotEmpty => {
            "This folder still has pages or subfolders and must be emptied first.".to_owned()
        }
        wiki::DeleteWikiPathError::Write(_) => "The vault could not be reached.".to_owned(),
        wiki::DeleteWikiPathError::Rejected => "This could not be deleted.".to_owned(),
    }
}

/// `DELETE /{{vault_name}}/wiki/{{*path}}`: moves the named page
/// or empty folder to trash ([`wiki::delete_page`]/[`wiki::delete_folder`],
/// chosen by [`resolve_target`]). On success, re-renders the deleted item's
/// own parent folder ([`render_wiki_fragment`]) - the same
/// `hx-target="closest .wiki-screen" hx-swap="outerHTML"` full-refresh
/// convention every other mutation on this screen already uses.
///
/// A malformed or missing `path`, the Wiki root itself, or a `path` naming
/// neither a real page nor a real folder is a genuine `404`. A folder that
/// still has real content in it re-renders that same folder with an
/// actionable error message, never a client-side recursive walk
/// (`fs_delete_file` only ever trash-deletes a file or an *empty*
/// directory - there is no recursive-delete primitive on this MCP server).
pub async fn delete_path(
    State(state): State<WikiRoutesState>,
    Path((vault_name, raw_path)): Path<(String, String)>,
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
    if path.is_empty() {
        // The Wiki root itself is never a deletable target.
        return not_found();
    }
    let config = wiki_config::load_wiki_config(&client, &vault_name).await;
    let root = wiki::load_tree(&client, &vault_name, &config).await;
    let Some((kind, _)) = resolve_target(&root, &path) else {
        return not_found();
    };
    let (parent_dir, _) = path.rsplit_once('/').unwrap_or(("", &path));
    let parent_dir = parent_dir.to_owned();

    let outcome = match kind {
        TargetKind::Folder => wiki::delete_folder(&client, &vault_name, &config, &path).await,
        TargetKind::Page => wiki::delete_page(&client, &vault_name, &config, &path).await,
    };
    match outcome {
        Ok(()) => html_response(
            Box::pin(render_wiki_fragment(&client, &vault_name, &config, &parent_dir, None))
                .await
                .unwrap_or_else(|| "<p>The Wiki screen could not be rendered.</p>".to_owned()),
        ),
        Err(wiki::DeleteWikiPathError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. })) => {
            unreachable()
        }
        Err(error) => {
            let message = delete_error_message(&error);
            let fragment = Box::pin(render_wiki_fragment(
                &client,
                &vault_name,
                &config,
                &path,
                Some(message),
            ))
            .await
            .unwrap_or_else(|| "<p>The Wiki screen could not be rendered.</p>".to_owned());
            html_response(fragment)
        }
    }
}

/// `DELETE /{{vault_name}}/wiki`/`DELETE /{{vault_name}}/wiki/`:
/// the Wiki root's own bare routes carry no `{{*path}}` wildcard segment
/// (axum's own wildcard cannot match zero path segments, `build_router`'s
/// own "collection root" comment), so a `DELETE` against the root is never
/// dispatched to [`delete_path`] at all - this always answers the same real
/// `404` [`delete_path`] itself gives an empty `path`, matching "the Wiki
/// root itself is never a deletable target" regardless of which of the two
/// routes actually matched.
pub async fn delete_root(State(state): State<WikiRoutesState>, Path(vault_name): Path<String>) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    not_found()
}
