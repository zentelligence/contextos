//! The entity folder screen's page shell: what it answers before showing anything, and the full page around its
//! content. Split from `routes::entity_docs` to keep each file within the line limit.

use std::sync::Arc;

use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};

use super::{
    EntityDocsRoutesState, EntityEntry, FileRequest, file_href, file_response, is_real_markdown_file, last_visited,
    redirect_response,
};
use crate::config;
use crate::entity_docs;
use crate::mcp_client::McpClient;
use crate::rendering::page;
use crate::rendering::shell::{self, ActiveScreen};
use crate::routes::vault::html_response;

/// A file an entity's folder page shows: the entity's name, the file's vault path, and its address here.
pub(super) struct ShownFile {
    pub(super) entity_name: String,
    pub(super) path: String,
    pub(super) href: String,
}

pub(super) async fn finish_response(
    state: &EntityDocsRoutesState,
    client: &McpClient,
    vault_name: &str,
    (fragment, shown): (String, Option<ShownFile>),
    breadcrumb_suffix: String,
    current_entity: Option<&str>,
    headers: HeaderMap,
) -> Response {
    if headers.contains_key("hx-request") {
        return html_response(fragment);
    }
    let web_config_path = Arc::clone(&state.web_config_path);
    let known_mcp_servers = state.clients.names();
    let (mut nav, appearance, mcp_status) = tokio::join!(
        Box::pin(shell::build_nav(
            client,
            ActiveScreen::Vault,
            Some(vault_name),
            Some(&breadcrumb_suffix),
            current_entity,
            None,
            &known_mcp_servers,
        )),
        async move {
            tokio::task::spawn_blocking(move || config::current_appearance(&web_config_path))
                .await
                .unwrap_or_default()
        },
        shell::mcp_status(&state.clients),
    );
    nav.appearance = appearance;
    nav.sidebar_wide = crate::routes::sidebar_wide_from_headers(&headers);
    nav.mcp_status = mcp_status;
    if let Some(shown) = shown {
        let screen = shell::ScreenTitle {
            icon: "folder",
            label: shown.entity_name,
        };
        shell::show_file(&mut nav, client, vault_name, screen, (&shown.path, &shown.href)).await;
    }
    html_response(page::render_page(&nav, "Entity folder", &fragment)).into_response()
}

/// What the entity folder screen answers before showing anything: a file in the entity's folder that the request asks
/// for as itself ([`file_response::before_screen`]), or, at the entity's own root, the operator's last-visited file.
/// The entity's sidebar link always opens its bare root, so this resumes the last-visited real file within this
/// entity's own tree, matching `routes::wiki::get_dispatch`'s identical contract and reasoning - re-validated against
/// the real tree here, never trusted blindly. Scoped per entity slug, since each configured entity tracks its own
/// last-visited file independently ([`last_visited::entity_last_visited`]'s own doc).
pub(super) async fn before_entity_screen(
    client: &McpClient,
    vault_name: &str,
    (entity, relative_path): (&EntityEntry, &str),
    file: &FileRequest,
    headers: &HeaderMap,
) -> Option<Response> {
    if !relative_path.is_empty() {
        return file_response::before_screen(client, vault_name, &format!("{}/{relative_path}", entity.slug), file)
            .await;
    }
    let last_visited_state = last_visited::read_from_headers(headers);
    let target = last_visited::entity_last_visited(&last_visited_state, vault_name, &entity.slug)?;
    let root = entity_docs::load_tree(client, vault_name, &entity.slug).await;
    is_real_markdown_file(&root, target)
        .then(|| redirect_response(&file_href(vault_name, &entity.slug, target), headers))
}
