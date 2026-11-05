//! `PATCH /{{vault_name}}/entity/{{*path}}` - saving an existing markdown file's own
//! body or frontmatter, split out of `entity_docs_write.rs` for the same file-size reason that
//! module's own top-of-file doc already describes for its own split out of `entity_docs.rs`:
//! this route grew a second request shape once the metadata panel needed its own save path,
//! matching `routes::wiki_save`'s identical "PATCH gets its own file, split out of the screen's
//! main module" precedent. Unlike `routes::wiki_save`, there is no `/rename` sub-path here -
//! entity-folder files carry no rename action of their own (`entity_docs.rs`'s own module doc).
//!
//! Every rendering this module calls back into - [`render_entity_fragment`] in particular -
//! stays in `entity_docs.rs`, since the one view every response on this screen renders through
//! lives there; this module only ever calls back into it, never the reverse.

use super::{
    EntityDocsConfig, EntityDocsRoutesState, EntityEntry, McpCallError, McpClient, Path, Response, State,
    as_markdown_path, ensure_vault, entities, entity_by_slug, entity_docs, entity_docs_config, html_response,
    metadata_panel, not_found, render_entity_fragment, split_entity_path, unreachable,
};
use crate::entity_docs::{LoadEntityFileError, ReadEntityFileError};
use serde::Deserialize;

/// `PATCH /{{vault_name}}/entity/{{*path}}`'s own request body: structurally two
/// distinct shapes on the one endpoint, matching `routes::wiki_save::SaveBody`'s identical
/// "content-replace or frontmatter-patch, distinguished by which field is present" contract.
/// `Content` replaces the body text verbatim, keeping frontmatter untouched
/// ([`entity_docs::save_file_body`]); `Metadata` is the metadata panel's own submitted form,
/// keyed by each editable frontmatter field's own real name
/// ([`metadata_panel::metadata_field_views`]) - [`metadata_panel::metadata_patch`] turns it back
/// into the right JSON shape.
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

fn malformed_body() -> Response {
    html_response("<p class=\"hint error\">Malformed request.</p>".to_owned())
}

/// `PATCH /{{vault_name}}/entity/{{*path}}`: saves the markdown file named by `path`,
/// either its body ([`patch_body_content`]) or its frontmatter ([`patch_body_metadata`]) - never
/// both in one request, [`SaveBody`]'s own doc - matching `routes::wiki_save::patch_body`'s
/// identical dispatch shape without a `/rename` sub-path.
pub async fn patch_body(
    State(state): State<EntityDocsRoutesState>,
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
    let Some((entity_slug, relative_path)) = split_entity_path(&raw_path) else {
        return not_found();
    };
    let configured_entities = entities::load_entities(&client, &vault_name).await;
    let Some(entity) = entity_by_slug(&configured_entities, &entity_slug) else {
        return not_found();
    };
    let config = entity_docs_config::load_entity_docs_config(&client, &vault_name).await;

    let Some(file_path) = as_markdown_path(&relative_path).map(str::to_owned) else {
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
                entity,
                &file_path,
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
                entity,
                &file_path,
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
    config: &EntityDocsConfig,
    entity: &EntityEntry,
    relative_path: &str,
    content: &str,
    expected_hash: Option<String>,
) -> Response {
    match entity_docs::save_file_body(client, vault_name, &entity.slug, relative_path, content, expected_hash).await {
        Ok(true) => html_response(
            Box::pin(render_entity_fragment(
                client,
                vault_name,
                config,
                entity,
                relative_path,
                None,
            ))
            .await
            .unwrap_or_else(|| "<p>The entity folder screen could not be rendered.</p>".to_owned()),
        ),
        Ok(false) => html_response(
            Box::pin(render_entity_fragment(
                client,
                vault_name,
                config,
                entity,
                relative_path,
                Some("This file changed since it was last loaded; reload and try again.".to_owned()),
            ))
            .await
            .unwrap_or_else(|| "<p>The entity folder screen could not be rendered.</p>".to_owned()),
        ),
        Err(entity_docs::SaveEntityFileError::Load(ReadEntityFileError::NotFound)) => not_found(),
        Err(entity_docs::SaveEntityFileError::Malformed) => {
            let fragment = Box::pin(render_entity_fragment(
                client,
                vault_name,
                config,
                entity,
                relative_path,
                Some("This file's own frontmatter could not be read.".to_owned()),
            ))
            .await
            .unwrap_or_else(|| "<p>The entity folder screen could not be rendered.</p>".to_owned());
            html_response(fragment)
        }
        Err(
            entity_docs::SaveEntityFileError::Load(ReadEntityFileError::Write(
                McpCallError::Unreachable { .. } | McpCallError::Refused { .. },
            ))
            | entity_docs::SaveEntityFileError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }),
        ) => unreachable(),
    }
}

/// The metadata panel's own save: applies `submitted`'s edits to the selected file's
/// frontmatter, its body left untouched ([`entity_docs::save_file_metadata`]). Re-reads the
/// file's own current frontmatter first (never trusting a client-supplied shape,
/// [`metadata_panel::metadata_patch`]'s own doc, [`entity_docs::load_file_frontmatter`]'s own
/// "read fresh before patch" contract), matching `routes::wiki_save::patch_body_metadata`'s
/// identical shape.
async fn patch_body_metadata(
    client: &McpClient,
    vault_name: &str,
    config: &EntityDocsConfig,
    entity: &EntityEntry,
    relative_path: &str,
    submitted: std::collections::BTreeMap<String, String>,
    expected_hash: Option<String>,
) -> Response {
    let current = match entity_docs::load_file_frontmatter(client, vault_name, &entity.slug, relative_path).await {
        Ok(frontmatter) => frontmatter,
        Err(LoadEntityFileError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. })) => {
            return unreachable();
        }
        Err(LoadEntityFileError::NotFound) => return not_found(),
        Err(LoadEntityFileError::Malformed) => {
            let fragment = Box::pin(render_entity_fragment(
                client,
                vault_name,
                config,
                entity,
                relative_path,
                Some("This file's own frontmatter could not be read.".to_owned()),
            ))
            .await
            .unwrap_or_else(|| "<p>The entity folder screen could not be rendered.</p>".to_owned());
            return html_response(fragment);
        }
    };
    let patch = metadata_panel::metadata_patch(&current, submitted);

    match entity_docs::save_file_metadata(client, vault_name, &entity.slug, relative_path, patch, expected_hash).await {
        Ok(true) => html_response(
            Box::pin(render_entity_fragment(
                client,
                vault_name,
                config,
                entity,
                relative_path,
                None,
            ))
            .await
            .unwrap_or_else(|| "<p>The entity folder screen could not be rendered.</p>".to_owned()),
        ),
        Ok(false) => html_response(
            Box::pin(render_entity_fragment(
                client,
                vault_name,
                config,
                entity,
                relative_path,
                Some("This file changed since it was last loaded; reload and try again.".to_owned()),
            ))
            .await
            .unwrap_or_else(|| "<p>The entity folder screen could not be rendered.</p>".to_owned()),
        ),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => unreachable(),
    }
}
