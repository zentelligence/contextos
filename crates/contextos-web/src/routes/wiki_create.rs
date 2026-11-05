//! `POST /{{vault_name}}/wiki/{{*path}}` - the "New" page action, and its own
//! `/execute/new-folder` sub-path for "New folder": creating a page (through `note_create`) and
//! creating an empty folder (`fs_create_directory`) both live here rather than growing
//! `routes::wiki` (already close to this workspace's own file-size guidance) further, matching
//! `routes::tasks_create`'s identical precedent of splitting a screen's own creation action into
//! its own module.
//!
//! The dedicated `GET .../new` form itself - and every rendering shared with it - stays in
//! `routes::wiki`, since the one view every Wiki response renders through lives there; this module
//! only ever calls back into it (`render_new_form_fragment`, `render_wiki_fragment`,
//! `render_created_page`), never the reverse.

use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::mcp_client::McpClient;
use crate::routes::vault::{html_response, not_found};
use crate::wiki;
use crate::wiki_config::{self, WikiConfig};

use super::wiki::{
    WikiRoutesState, create_error_message, dir_href, ensure_vault, new_folder_action_path, render_created_page,
    render_new_form_fragment, render_wiki_fragment, sanitised_path,
};

#[derive(Debug, Default, Deserialize)]
struct CreateBody {
    /// The dedicated `/new` form's own submitted title - the only field it carries, matching
    /// `routes::notes::CreateBody`'s own corrected "no server-synthesised fallback" contract.
    /// Absent or blank/whitespace-only alike stay `CreateWikiPageError::EmptyTitle`; the form
    /// itself is always pre-filled with `wiki::default_title`, so an operator only ever submits
    /// nothing here by clearing that field or via a malformed request, never through the header
    /// "New" button, which loads that form rather than posting directly
    /// (`routes::wiki::new_page_href`'s own doc).
    #[serde(default)]
    title: String,
}

#[derive(Debug, Default, Deserialize)]
struct CreateFolderBody {
    #[serde(default)]
    name: String,
}

fn create_folder_error_message(error: &wiki::CreateFolderError) -> String {
    match error {
        wiki::CreateFolderError::EmptyName => "A folder needs a name.".to_owned(),
        wiki::CreateFolderError::Write(_) => "The vault could not be reached.".to_owned(),
        wiki::CreateFolderError::Rejected => "The folder could not be created.".to_owned(),
    }
}

/// `POST /{{vault_name}}/wiki/{{*path}}/execute/new-folder`:
/// creates a new, empty folder inside the folder named by `parent_dir`
/// (`wiki::create_folder`) - never gated by `wiki::MIN_CREATE_DEPTH` (that
/// rule is `wiki::create_page`'s own, for pages alone).
async fn create_folder_dispatch(
    client: &McpClient,
    vault_name: &str,
    config: &WikiConfig,
    parent_dir: &str,
    body: &axum::body::Bytes,
) -> Response {
    let Ok(parsed) = serde_json::from_slice::<CreateFolderBody>(body) else {
        return html_response("<p class=\"hint error\">Malformed request.</p>".to_owned());
    };
    match wiki::create_folder(client, vault_name, config, parent_dir, &parsed.name).await {
        Ok(relative_path) => {
            let target = dir_href(vault_name, &relative_path);
            (StatusCode::OK, [("HX-Redirect", target)], String::new()).into_response()
        }
        Err(error) => {
            let message = create_folder_error_message(&error);
            let fragment = Box::pin(render_wiki_fragment(
                client,
                vault_name,
                config,
                parent_dir,
                Some(message),
            ))
            .await
            .unwrap_or_else(|| "<p>The Wiki screen could not be rendered.</p>".to_owned());
            html_response(fragment)
        }
    }
}

/// `POST /{{vault_name}}/wiki/{{*path}}`: creates a page inside
/// the folder named by `path`, always through `wiki::create_page`
/// (`note_create`) - or, when `path` ends in `/execute/new-folder`,
/// dispatches [`create_folder_dispatch`] instead: the two share this one
/// route table entry since both are a `POST` against the identical
/// `/{{vault_name}}/wiki/{{*path}}` wildcard, matching
/// `routes::wiki::get_dispatch`'s own identical suffix-dispatch shape.
async fn create_dispatch(
    state: WikiRoutesState,
    vault_name: String,
    raw_parent_dir: String,
    body: axum::body::Bytes,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let Some(path) = sanitised_path(&raw_parent_dir) else {
        return not_found();
    };
    let config = wiki_config::load_wiki_config(&client, &vault_name).await;

    if let Some(parent_dir) = new_folder_action_path(&path) {
        return Box::pin(create_folder_dispatch(&client, &vault_name, &config, parent_dir, &body)).await;
    }

    let Ok(parsed) = serde_json::from_slice::<CreateBody>(&body) else {
        return html_response("<p class=\"hint error\">Malformed request.</p>".to_owned());
    };
    let title = parsed.title;
    match wiki::create_page(&client, &vault_name, &config, &path, &title).await {
        Ok(relative_path) => {
            let target = crate::urls::VaultUrls::from(&*vault_name).wiki(&relative_path);
            let fragment = Box::pin(render_created_page(&client, &vault_name, &config, &relative_path))
                .await
                .unwrap_or_else(|| "<p>The Wiki screen could not be rendered.</p>".to_owned());
            let mut response = html_response(fragment);
            if let Ok(value) = HeaderValue::from_str(&target) {
                response.headers_mut().insert("HX-Push-Url", value);
            }
            response
        }
        // Every failure re-renders the `/new` form itself, never the plain folder-browse
        // fragment: the header "New" button always loads that dedicated form first
        // (`routes::wiki::new_page_href`'s own doc) rather than posting directly, so every real
        // submission this handler ever sees already came from it - matching
        // `routes::notes::create_root`'s identical "any failure keeps the operator on their own
        // form" contract, the operator's own submitted title preserved rather than lost.
        Err(error) => {
            let message = create_error_message(&error);
            let fragment = Box::pin(render_new_form_fragment(
                &client,
                &vault_name,
                &config,
                &path,
                title,
                Some(message),
            ))
            .await
            .unwrap_or_else(|| "<p>The Wiki screen could not be rendered.</p>".to_owned());
            html_response(fragment)
        }
    }
}

pub async fn create_root(
    State(state): State<WikiRoutesState>,
    Path(vault_name): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    create_dispatch(state, vault_name, String::new(), body).await
}

pub async fn create_path(
    State(state): State<WikiRoutesState>,
    Path((vault_name, parent_dir)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    create_dispatch(state, vault_name, parent_dir, body).await
}
