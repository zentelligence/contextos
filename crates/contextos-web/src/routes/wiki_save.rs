//! `PATCH /{{vault_name}}/wiki/{{*path}}` - saving an existing page's own body or
//! frontmatter, and its own `/rename` sub-path for the title action: split out of
//! `routes::wiki` (already close to this workspace's own file-size guidance) for the same
//! reason `routes::wiki_create` already split "New" out, matching that module's own precedent
//! of splitting a screen's own action into its own file rather than growing `routes::wiki`
//! further.
//!
//! Every rendering this module calls back into - `render_wiki_fragment` in particular - stays
//! in `routes::wiki`, since the one view every Wiki response renders through lives there; this
//! module only ever calls back into it, never the reverse.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::mcp_client::{McpCallError, McpClient};
use crate::metadata_panel;
use crate::routes::vault::{html_response, not_found, unreachable};
use crate::wiki::{self, LoadWikiPageError};
use crate::wiki_config::{self, WikiConfig};

use super::wiki::{WikiRoutesState, action_path, as_page_path, ensure_vault, render_wiki_fragment, sanitised_path};

/// `PATCH /{{vault_name}}/wiki/{{*path}}`'s own request body: structurally two
/// distinct shapes on the one endpoint, matching `routes::notes::SaveBody`'s identical
/// "content-replace or frontmatter-patch, distinguished by which field is present" contract.
/// `Content` replaces the body text verbatim, keeping frontmatter untouched
/// ([`wiki::save_page_body`]); `Metadata` is the metadata panel's own submitted form, keyed by
/// each editable frontmatter field's own real name ([`metadata_panel::metadata_field_views`]) -
/// [`metadata_panel::metadata_patch`] turns it back into the right JSON shape.
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
}

fn malformed_body() -> Response {
    html_response("<p class=\"hint error\">Malformed request.</p>".to_owned())
}

fn rename_error_message(error: &wiki::RenamePageError) -> String {
    match error {
        wiki::RenamePageError::EmptyTitle => "A page needs a title.".to_owned(),
        wiki::RenamePageError::Write(_) => "The vault could not be reached.".to_owned(),
        wiki::RenamePageError::Rejected => {
            "The page could not be renamed (a page with that title may already exist).".to_owned()
        }
    }
}

/// `PATCH /{{vault_name}}/wiki/{{*path}}/rename`: the rename
/// form's own action, updating the page's own title (and, when its slug
/// changes, its own filename) together ([`wiki::rename_page`]). Redirects
/// to the page's own current path on success, matching
/// `routes::insights::patch_rename`'s identical "the URL always names the
/// file's current identity" contract.
async fn patch_rename(
    client: &McpClient,
    vault_name: &str,
    config: &WikiConfig,
    relative_path: &str,
    body: &axum::body::Bytes,
) -> Response {
    let Ok(parsed) = serde_json::from_slice::<RenameBody>(body) else {
        return malformed_body();
    };
    match wiki::rename_page(client, vault_name, config, relative_path, &parsed.title).await {
        Ok(new_relative_path) => {
            let target = crate::urls::VaultUrls::from(vault_name).wiki(&new_relative_path);
            (StatusCode::OK, [("HX-Redirect", target)], String::new()).into_response()
        }
        Err(wiki::RenamePageError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. })) => {
            unreachable()
        }
        Err(error) => {
            let message = rename_error_message(&error);
            let fragment = Box::pin(render_wiki_fragment(
                client,
                vault_name,
                config,
                relative_path,
                Some(message),
            ))
            .await
            .unwrap_or_else(|| "<p>The Wiki screen could not be rendered.</p>".to_owned());
            html_response(fragment)
        }
    }
}

/// `PATCH /{{vault_name}}/wiki/{{*path}}`: saves the page named by `path`, either its
/// body ([`patch_body_content`]) or its frontmatter ([`patch_body_metadata`]) - never both in
/// one request, [`SaveBody`]'s own doc - or, when `path` ends in `/rename`, dispatches
/// [`patch_rename`] instead, matching `routes::wiki_create::create_dispatch`'s own identical
/// suffix-dispatch shape.
pub async fn patch_body(
    State(state): State<WikiRoutesState>,
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
    let config = wiki_config::load_wiki_config(&client, &vault_name).await;

    if let Some(page_path_str) = action_path(&path, "/rename").and_then(as_page_path) {
        return Box::pin(patch_rename(&client, &vault_name, &config, page_path_str, &body)).await;
    }
    let Some(path) = as_page_path(&path).map(str::to_owned) else {
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
    config: &WikiConfig,
    relative_path: &str,
    content: &str,
    expected_hash: Option<String>,
) -> Response {
    match wiki::save_page_body(client, vault_name, config, relative_path, content, expected_hash).await {
        Ok(true) => html_response(
            Box::pin(render_wiki_fragment(client, vault_name, config, relative_path, None))
                .await
                .unwrap_or_else(|| "<p>The Wiki screen could not be rendered.</p>".to_owned()),
        ),
        Ok(false) => html_response(
            Box::pin(render_wiki_fragment(
                client,
                vault_name,
                config,
                relative_path,
                Some("This page changed since it was last loaded; reload and try again.".to_owned()),
            ))
            .await
            .unwrap_or_else(|| "<p>The Wiki screen could not be rendered.</p>".to_owned()),
        ),
        Err(wiki::SavePageError::Load(wiki::ReadWikiPageError::NotFound)) => not_found(),
        Err(wiki::SavePageError::Malformed) => {
            let fragment = Box::pin(render_wiki_fragment(
                client,
                vault_name,
                config,
                relative_path,
                Some("This page's own frontmatter could not be read.".to_owned()),
            ))
            .await
            .unwrap_or_else(|| "<p>The Wiki screen could not be rendered.</p>".to_owned());
            html_response(fragment)
        }
        Err(
            wiki::SavePageError::Load(wiki::ReadWikiPageError::Write(
                McpCallError::Unreachable { .. } | McpCallError::Refused { .. },
            ))
            | wiki::SavePageError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }),
        ) => unreachable(),
    }
}

/// The metadata panel's own save: applies `submitted`'s edits to the selected page's
/// frontmatter, its body left untouched ([`wiki::save_page_metadata`]). Re-reads the page's
/// own current frontmatter first (never trusting a client-supplied shape,
/// [`metadata_panel::metadata_patch`]'s own doc, [`wiki::load_page_frontmatter`]'s own "read
/// fresh before patch" contract), matching `routes::notes::patch_body_metadata`'s identical
/// shape.
async fn patch_body_metadata(
    client: &McpClient,
    vault_name: &str,
    config: &WikiConfig,
    relative_path: &str,
    submitted: std::collections::BTreeMap<String, String>,
    expected_hash: Option<String>,
) -> Response {
    let current = match wiki::load_page_frontmatter(client, vault_name, config, relative_path).await {
        Ok(frontmatter) => frontmatter,
        Err(LoadWikiPageError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. })) => {
            return unreachable();
        }
        Err(LoadWikiPageError::NotFound) => return not_found(),
        Err(LoadWikiPageError::Malformed) => {
            let fragment = Box::pin(render_wiki_fragment(
                client,
                vault_name,
                config,
                relative_path,
                Some("This page's own frontmatter could not be read.".to_owned()),
            ))
            .await
            .unwrap_or_else(|| "<p>The Wiki screen could not be rendered.</p>".to_owned());
            return html_response(fragment);
        }
    };
    let patch = metadata_panel::metadata_patch(&current, submitted);

    match wiki::save_page_metadata(client, vault_name, config, relative_path, patch, expected_hash).await {
        Ok(true) => html_response(
            Box::pin(render_wiki_fragment(client, vault_name, config, relative_path, None))
                .await
                .unwrap_or_else(|| "<p>The Wiki screen could not be rendered.</p>".to_owned()),
        ),
        Ok(false) => html_response(
            Box::pin(render_wiki_fragment(
                client,
                vault_name,
                config,
                relative_path,
                Some("This page changed since it was last loaded; reload and try again.".to_owned()),
            ))
            .await
            .unwrap_or_else(|| "<p>The Wiki screen could not be rendered.</p>".to_owned()),
        ),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => unreachable(),
    }
}
