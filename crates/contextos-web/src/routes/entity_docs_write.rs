//! `POST`/`DELETE /{{vault_name}}/entity/{{*path}}`: the
//! entity-folder screen's own create and delete routes, plus the `GET
//! .../confirm-delete` fragment that always precedes a real delete, split
//! out of `entity_docs.rs` purely to keep that file under this workspace's
//! own 1000-line/50kB cap - every item here reaches back into
//! `entity_docs.rs`'s own private state, path helpers, and rendering
//! functions as a plain child module's ordinary visibility already allows,
//! rather than widening any of those items' own visibility to `pub(crate)`.
//! The `PATCH` route lives in its own sibling, `entity_docs_metadata.rs`,
//! split out for the identical file-size reason once it grew a second
//! request shape for the metadata panel.
//!
//! [`CreatingView`] and [`render_new_form_fragment`] - `GET .../new`'s own
//! pre-filled create-file form state and rendering - live here rather than
//! in `entity_docs.rs` for the identical reason: that file was already at
//! this workspace's own 1000-line cap once the `/new` dispatch wiring
//! itself was added, so the form's own rendering moved out instead, the
//! same file-size discipline `routes::wiki_create`'s own top-of-file doc
//! describes for splitting a screen's own creation action out of its main
//! module. Unlike `routes::wiki_create` (whose own `render_new_form_fragment`
//! stays in `routes::wiki`, since only that file's `GET` dispatch needs it),
//! `routes::entity_docs::get_dispatch_for_entity` reaches back into this
//! module (`write::render_new_form_fragment`) for its own `GET .../new`
//! rendering too - `entity_docs.rs` had no room left to keep it, and this
//! module already owns every other create-side concern.

use super::{
    Deserialize, EntityDocsConfig, EntityDocsRoutesState, EntityDocsTemplate, EntityEntry, HeaderValue, McpCallError,
    McpClient, PanelSide, Path, Response, State, StatusCode, TargetKind, dir_href, ensure_vault, entities,
    entity_by_slug, entity_docs, entity_docs_config, entity_layout_class, entity_view, file_href, html_response,
    new_folder_action_path, new_folder_href, new_page_href, not_found, render_created_file, render_entity_fragment,
    resolve_target, split_entity_path, unreachable,
};
use askama::Template;
use axum::response::IntoResponse;

// ---------------------------------------------------------------------
// POST /{{vault_name}}/entity/{{*path}} - the "New" file action,
// and its own "/execute/new-folder" sub-path for "New folder"
// ---------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct CreateBody {
    /// The dedicated `/new` form's own submitted title - the only field it
    /// carries, matching `routes::wiki_create::CreateBody`'s own "no
    /// server-synthesised fallback" contract. Absent or blank/whitespace-only
    /// alike stay [`entity_docs::CreateEntityFileError::EmptyTitle`]; the
    /// form itself is always pre-filled with [`entity_docs::default_title`],
    /// so an operator only ever submits nothing here by clearing that field
    /// or via a malformed request, never through the header "New" button,
    /// which loads that form rather than posting directly
    /// (`new_page_href`'s own doc).
    #[serde(default)]
    title: String,
}

/// `GET .../new`'s own pre-filled create-file form state, matching
/// `routes::wiki::CreatingView`'s identical shape - `parent_label` is the
/// entity's own name at that entity's own root (a bare `EntityFolder`
/// root's own `label` is its slug, not a display name fit for the
/// operator), and the target folder's own real label otherwise.
pub(super) struct CreatingView {
    pub(super) parent_label: String,
    pub(super) default_title: String,
}

#[derive(Debug, Default, Deserialize)]
struct CreateFolderBody {
    #[serde(default)]
    name: String,
}

fn create_error_message(error: &entity_docs::CreateEntityFileError) -> String {
    match error {
        entity_docs::CreateEntityFileError::EmptyTitle => "A file needs a title.".to_owned(),
        entity_docs::CreateEntityFileError::Write(_) => "The vault could not be reached.".to_owned(),
        entity_docs::CreateEntityFileError::Rejected => {
            "The file could not be created (a file with that name may already exist).".to_owned()
        }
    }
}

fn create_folder_error_message(error: &entity_docs::CreateFolderError) -> String {
    match error {
        entity_docs::CreateFolderError::EmptyName => "A folder needs a name.".to_owned(),
        entity_docs::CreateFolderError::Write(_) => "The vault could not be reached.".to_owned(),
        entity_docs::CreateFolderError::Rejected => "The folder could not be created.".to_owned(),
    }
}

/// `POST /{{vault_name}}/entity/{{*path}}/execute/new-folder`:
/// creates a new, empty folder inside the folder named by `parent_dir`
/// ([`entity_docs::create_folder`]) - matching
/// `routes::wiki_create::create_folder_dispatch`'s identical contract, with no
/// minimum-depth floor of its own.
async fn create_folder_dispatch(
    client: &McpClient,
    vault_name: &str,
    config: &EntityDocsConfig,
    entity: &EntityEntry,
    parent_dir: &str,
    body: &axum::body::Bytes,
) -> Response {
    let Ok(parsed) = serde_json::from_slice::<CreateFolderBody>(body) else {
        return html_response("<p class=\"hint error\">Malformed request.</p>".to_owned());
    };
    match entity_docs::create_folder(client, vault_name, &entity.slug, parent_dir, &parsed.name).await {
        Ok(relative_path) => {
            let target = dir_href(vault_name, &entity.slug, &relative_path);
            (StatusCode::OK, [("HX-Redirect", target)], String::new()).into_response()
        }
        Err(error) => {
            let message = create_folder_error_message(&error);
            let fragment = Box::pin(render_entity_fragment(
                client,
                vault_name,
                config,
                entity,
                parent_dir,
                Some(message),
            ))
            .await
            .unwrap_or_else(|| "<p>The entity folder screen could not be rendered.</p>".to_owned());
            html_response(fragment)
        }
    }
}

/// `GET .../new`'s own dedicated create-file form fragment, and
/// [`create_dispatch`]'s own `POST`-error re-render target for every
/// [`entity_docs::CreateEntityFileError`] variant - sharing this one
/// function keeps both paths identical, matching
/// `routes::wiki::render_new_form_fragment`'s identical reasoning. Unlike
/// Wiki, `parent_dir` carries no minimum-depth gate to fail here: any real
/// folder resolves to a usable form, the entity's own root included.
///
/// `error`, when present, is the create form's own validation or rejection
/// message alongside the operator's own just-submitted (and rejected)
/// title, preserved rather than lost - matching every other form-error path
/// in this crate's own "redisplay what was actually submitted" contract.
///
/// `None` only when `parent_dir` itself no longer resolves to a real folder
/// at all (a stale or tampered link, or an operator-edited URL) - the
/// caller turns that into a real `404`, matching
/// [`render_entity_fragment`]'s own "unresolvable path" contract.
pub(super) async fn render_new_form_fragment(
    client: &McpClient,
    vault_name: &str,
    config: &EntityDocsConfig,
    entity: &EntityEntry,
    parent_dir: &str,
    default_title: String,
    error: Option<String>,
) -> Option<String> {
    let folder_side_is_left = config.folder_side == PanelSide::Left;
    let metadata_panel_side_is_left = config.metadata_panel_side == PanelSide::Left;
    let root = entity_docs::load_tree(client, vault_name, &entity.slug).await;
    let folder = entity_docs::find_folder(&root, parent_dir)?;
    let tree_events = entity_view::flatten(vault_name, &entity.slug, &root, &folder.relative_path, None);
    let parent_label = if folder.relative_path.is_empty() {
        entity.name.clone()
    } else {
        folder.label.clone()
    };

    Some(
        EntityDocsTemplate {
            urls: crate::urls::VaultUrls::from(vault_name),
            entity_slug: entity.slug.clone(),
            folder_side_is_left,
            metadata_panel_side_is_left,
            layout_class: entity_layout_class(folder_side_is_left, metadata_panel_side_is_left, false),
            has_entities: true,
            entity_name: entity.name.clone(),
            tree_events,
            current_dir_href: dir_href(vault_name, &entity.slug, &folder.relative_path),
            new_folder_href: new_folder_href(vault_name, &entity.slug, &folder.relative_path),
            new_page_href: new_page_href(vault_name, &entity.slug, &folder.relative_path),
            browsing_folder: None,
            selected: None,
            editing: None,
            creating: Some(CreatingView {
                parent_label,
                default_title,
            }),
            error,
        }
        .render()
        .unwrap_or_else(|_| "<p>The entity folder screen could not be rendered.</p>".to_owned()),
    )
}

// ---------------------------------------------------------------------
// GET /{{vault_name}}/entity/{{*path}}/confirm-delete
// ---------------------------------------------------------------------

/// `GET /{{vault_name}}/entity/{{*path}}/confirm-delete`'s own
/// fragment (the contract's reserved `/confirm-delete` sub-path), matching
/// `routes::wiki::WikiConfirmDeleteTemplate`'s identical shape - `kind`
/// names whichever real thing `path` resolved to ("file" or "folder").
#[derive(Template)]
#[template(path = "entity_docs_confirm_delete.html")]
struct EntityDocsConfirmDeleteTemplate {
    /// Every URL the template links to within the vault.
    urls: crate::urls::VaultUrls,
    entity_slug: String,
    kind: &'static str,
    label: String,
    path: String,
}

/// `GET /{{vault_name}}/entity/{{*path}}/confirm-delete`: names
/// the real file or folder `target` resolves to before confirming
/// ([`resolve_target`]), matching `routes::wiki::render_confirm_delete_response`'s
/// identical contract - a `target` naming neither falls back to its own
/// parent folder's own browse fragment with an error message.
pub(super) async fn render_confirm_delete_response(
    client: &McpClient,
    vault_name: &str,
    config: &EntityDocsConfig,
    entity: &EntityEntry,
    target: &str,
) -> Response {
    let root = entity_docs::load_tree(client, vault_name, &entity.slug).await;
    let Some((kind, label)) = resolve_target(&root, target) else {
        let (parent_dir, _) = target.rsplit_once('/').unwrap_or(("", target));
        let fragment = Box::pin(render_entity_fragment(
            client,
            vault_name,
            config,
            entity,
            parent_dir,
            Some("This could not be found.".to_owned()),
        ))
        .await
        .unwrap_or_else(|| "<p>The entity folder screen could not be rendered.</p>".to_owned());
        return html_response(fragment);
    };
    html_response(
        EntityDocsConfirmDeleteTemplate {
            urls: crate::urls::VaultUrls::from(vault_name),
            entity_slug: entity.slug.clone(),
            kind: match kind {
                TargetKind::Folder => "folder",
                TargetKind::File => "file",
            },
            label,
            path: (target).to_owned(),
        }
        .render()
        .unwrap_or_else(|_| "<p>The delete confirmation could not be rendered.</p>".to_owned()),
    )
}

/// `POST /{{vault_name}}/entity/{{*path}}`: creates a markdown
/// file inside the folder named by `relative_path`, always through
/// [`entity_docs::create_file`] (`note_create`) - or, when `relative_path`
/// ends in `/execute/new-folder`, dispatches [`create_folder_dispatch`]
/// instead, matching `routes::wiki_create::create_dispatch`'s identical
/// suffix-dispatch shape. On success, lands directly in the new file's own
/// edit form ([`render_created_file`]) via `HX-Push-Url` rather than an
/// `HX-Redirect` full navigation to its own (empty) read view - matching
/// `routes::wiki_create::create_dispatch`'s identical "new" contract.
async fn create_dispatch(
    state: EntityDocsRoutesState,
    vault_name: String,
    raw_path: String,
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

    if let Some(parent_dir) = new_folder_action_path(&relative_path) {
        return Box::pin(create_folder_dispatch(
            &client,
            &vault_name,
            &config,
            entity,
            parent_dir,
            &body,
        ))
        .await;
    }

    let Ok(parsed) = serde_json::from_slice::<CreateBody>(&body) else {
        return html_response("<p class=\"hint error\">Malformed request.</p>".to_owned());
    };
    let title = parsed.title;
    match entity_docs::create_file(&client, &vault_name, &entity.slug, &relative_path, &title).await {
        Ok(created_relative_path) => {
            let target = file_href(&vault_name, &entity.slug, &created_relative_path);
            let fragment = Box::pin(render_created_file(
                &client,
                &vault_name,
                &config,
                entity,
                &created_relative_path,
            ))
            .await
            .unwrap_or_else(|| "<p>The entity folder screen could not be rendered.</p>".to_owned());
            let mut response = html_response(fragment);
            if let Ok(value) = HeaderValue::from_str(&target) {
                response.headers_mut().insert("HX-Push-Url", value);
            }
            response
        }
        // Every failure re-renders the `/new` form itself, never the plain folder-browse
        // fragment: the header "New" button always loads that dedicated form first
        // (`new_page_href`'s own doc) rather than posting directly, so every real submission this
        // handler ever sees already came from it - matching
        // `routes::wiki_create::create_dispatch`'s identical "any failure keeps the operator on
        // their own form" contract, the operator's own submitted title preserved rather than lost.
        Err(error) => {
            let message = create_error_message(&error);
            let fragment = Box::pin(render_new_form_fragment(
                &client,
                &vault_name,
                &config,
                entity,
                &relative_path,
                title,
                Some(message),
            ))
            .await
            .unwrap_or_else(|| "<p>The entity folder screen could not be rendered.</p>".to_owned());
            html_response(fragment)
        }
    }
}

pub async fn create_path(
    State(state): State<EntityDocsRoutesState>,
    Path((vault_name, raw_path)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    create_dispatch(state, vault_name, raw_path, body).await
}

// ---------------------------------------------------------------------
// PATCH /{{vault_name}}/entity/{{*path}} - `routes::entity_docs_metadata`
// ---------------------------------------------------------------------

// ---------------------------------------------------------------------
// DELETE /{{vault_name}}/entity/{{*path}}
// ---------------------------------------------------------------------

fn delete_error_message(error: &entity_docs::DeleteEntityPathError) -> String {
    match error {
        entity_docs::DeleteEntityPathError::NotFound => "This could not be found.".to_owned(),
        entity_docs::DeleteEntityPathError::NotEmpty => {
            "This folder still has files or subfolders and must be emptied first.".to_owned()
        }
        entity_docs::DeleteEntityPathError::Write(_) => "The vault could not be reached.".to_owned(),
        entity_docs::DeleteEntityPathError::Rejected => "This could not be deleted.".to_owned(),
    }
}

/// `DELETE /{{vault_name}}/entity/{{*path}}`: moves the named file
/// or empty folder to trash ([`entity_docs::delete_file`]/
/// [`entity_docs::delete_folder`], chosen by [`resolve_target`]), matching
/// `routes::wiki_delete::delete_path`'s identical contract. A malformed or missing
/// `path`, an entity's own root itself, or a `path` naming neither a real
/// file nor a real folder is a genuine `404`. A folder that still has real
/// content in it re-renders that same folder with an actionable error
/// message, never a client-side recursive walk.
pub async fn delete_path(
    State(state): State<EntityDocsRoutesState>,
    Path((vault_name, raw_path)): Path<(String, String)>,
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
    if relative_path.is_empty() {
        // An entity's own root itself is never a deletable target.
        return not_found();
    }
    let configured_entities = entities::load_entities(&client, &vault_name).await;
    let Some(entity) = entity_by_slug(&configured_entities, &entity_slug) else {
        return not_found();
    };
    let config = entity_docs_config::load_entity_docs_config(&client, &vault_name).await;

    let root = entity_docs::load_tree(&client, &vault_name, &entity.slug).await;
    let Some((kind, _)) = resolve_target(&root, &relative_path) else {
        return not_found();
    };
    let (parent_dir, _) = relative_path.rsplit_once('/').unwrap_or(("", &relative_path));
    let parent_dir = parent_dir.to_owned();

    let outcome = match kind {
        TargetKind::Folder => entity_docs::delete_folder(&client, &vault_name, &entity.slug, &relative_path).await,
        TargetKind::File => entity_docs::delete_file(&client, &vault_name, &entity.slug, &relative_path).await,
    };
    match outcome {
        Ok(()) => html_response(
            Box::pin(render_entity_fragment(
                &client,
                &vault_name,
                &config,
                entity,
                &parent_dir,
                None,
            ))
            .await
            .unwrap_or_else(|| "<p>The entity folder screen could not be rendered.</p>".to_owned()),
        ),
        Err(entity_docs::DeleteEntityPathError::Write(
            McpCallError::Unreachable { .. } | McpCallError::Refused { .. },
        )) => unreachable(),
        Err(error) => {
            let message = delete_error_message(&error);
            let fragment = Box::pin(render_entity_fragment(
                &client,
                &vault_name,
                &config,
                entity,
                &relative_path,
                Some(message),
            ))
            .await
            .unwrap_or_else(|| "<p>The entity folder screen could not be rendered.</p>".to_owned());
            html_response(fragment)
        }
    }
}
