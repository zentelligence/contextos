//! The feature routers `build_router_closing` merges: each feature's route table, kept apart from the start-up and
//! assembly code in [`crate::app`] so neither grows past what one reader can take in.

use axum::Router;
use axum::routing::{delete, get, post, put};

use crate::routes;
use crate::routes::apps::AppRoutesState;
use crate::routes::entity_docs::EntityDocsRoutesState;
use crate::routes::inbox_outbox::InboxOutboxRoutesState;
use crate::routes::insights::InsightsRoutesState;
use crate::routes::journal::JournalRoutesState;
use crate::routes::notes::NotesRoutesState;
use crate::routes::packages::PackagesRoutesState;
use crate::routes::profile::ProfileRoutesState;
use crate::routes::search::SearchRoutesState;
use crate::routes::settings::SettingsRoutesState;
use crate::routes::shared_workflows::SharedRoutesState;
use crate::routes::shell::ShellFavouritesRoutesState;
use crate::routes::vault::VaultRoutesState;
use crate::routes::vault_health::VaultHealthRoutesState;
use crate::routes::vault_management::VaultManagementRoutesState;
use crate::routes::wiki::WikiRoutesState;
use crate::routes::workflows::WorkflowRoutesState;

/// The header Favourites panel's own lazy-loaded data route. Extracted from [`build_router`] for the identical
/// reason as [`tasks_router`]; kept separate from the state-free `/shell/sidebar-toggle` route (registered on
/// `proxy_and_static` above) since this one needs an MCP session.
pub(crate) fn shell_favourites_router(state: ShellFavouritesRoutesState) -> Router {
    Router::new()
        .route("/shell/favourites", get(routes::shell::favourites_panel))
        .route("/shell/status", get(routes::shell::status))
        .route(
            "/{vault_name}/views/{id}",
            axum::routing::patch(routes::shell::set_view_favourite),
        )
        .route(
            "/{vault_name}/favourites/{*path}",
            axum::routing::patch(routes::shell::set_page_favourite),
        )
        .with_state(state)
}

/// The app registry's own routes. Extracted from
/// [`build_router`] for the identical reason as [`tasks_router`].
pub(crate) fn apps_router(state: AppRoutesState) -> Router {
    Router::new()
        .route("/{vault_name}/apps", get(routes::apps::list))
        .route("/{vault_name}/apps/", get(routes::apps::list))
        .route("/{vault_name}/apps/rescan", post(routes::apps::rescan))
        .route(
            "/{vault_name}/apps/{slug}",
            get(routes::apps::serve_root).patch(routes::apps::set_favourite),
        )
        .route(
            "/{vault_name}/apps/{slug}/",
            get(routes::apps::serve_root).patch(routes::apps::set_favourite),
        )
        .route("/{vault_name}/apps/{slug}/{*sub_path}", get(routes::apps::serve_path))
        .with_state(state)
}

/// A vault's workflows, the jobs under them, and the runs of each, in the order of that hierarchy and
/// the URL schema of `docs/http-routing-response-contract.md`: a job is changed with `PATCH` and `DELETE`, a run is
/// created with `POST .../runs`, and only the lifecycle commands that have no standard method are `PUT .../execute/{command}`.
/// Extracted from [`build_router`] for the identical reason as [`tasks_router`].
pub(crate) fn workflows_router(state: WorkflowRoutesState) -> Router {
    Router::new()
        .route("/{vault_name}/workflows", get(routes::workflows::list))
        .route("/{vault_name}/workflows/", get(routes::workflows::list))
        .route("/{vault_name}/packages", get(routes::vault_packages_page::page))
        .route(
            "/{vault_name}/packages/{name}/versions/{version}/execute/{command}",
            put(routes::vault_packages_page::execute),
        )
        .route(
            "/{vault_name}/workflows/{package}/{workflow_slug}",
            get(routes::workflows::detail),
        )
        .route(
            "/{vault_name}/workflows/{package}/{workflow_slug}/jobs/{slug}",
            get(routes::workflow_jobs::detail)
                .patch(routes::workflow_jobs::update)
                .delete(routes::workflow_jobs::delete),
        )
        .route(
            "/{vault_name}/workflows/{package}/{workflow_slug}/jobs/{slug}/confirm-delete",
            get(routes::workflow_jobs::confirm_delete),
        )
        .route(
            "/{vault_name}/workflows/{package}/{workflow_slug}/jobs/{slug}/execute/{command}",
            put(routes::workflow_jobs::execute),
        )
        .route(
            "/{vault_name}/workflows/{package}/{workflow_slug}/jobs/{slug}/runs",
            get(routes::workflow_jobs::list_runs).post(routes::workflow_jobs::start_run),
        )
        .route("/api/v1/{vault_name}/packages", get(routes::vault_packages_api::list))
        .route(
            "/api/v1/{vault_name}/packages/{name}/versions/{version}",
            get(routes::vault_packages_api::get_one).delete(routes::vault_packages_api::delete_one),
        )
        .route(
            "/api/v1/{vault_name}/packages/{name}/versions/{version}/execute/remove",
            put(routes::vault_packages_api::remove),
        )
        .route(
            "/api/v1/{vault_name}/workflows/{package}/{workflow_slug}/jobs/{slug}/execute/successor",
            put(routes::successor_api::vault_one),
        )
        .route(
            "/api/v1/{vault_name}/workflows/{package}/{workflow_slug}/jobs/execute/successor",
            post(routes::successor_api::vault_all),
        )
        .route(
            "/{vault_name}/workflows/{package}/{workflow_slug}/jobs/{slug}/runs/{run}/execute/cancel",
            put(routes::workflow_jobs::cancel_run),
        )
        .with_state(state)
}

/// The host's shared workflows, the jobs under them, and the runs of each, in the order of that hierarchy. They take no
/// vault: `/shared/workflows/{package}` stands where `/{vault}/workflows/{package}` does for a vault's workflows, and the shape beneath it is the same.
pub fn shared_router(state: SharedRoutesState) -> Router {
    Router::new()
        .route("/shared/workflows", get(routes::shared_workflows::list))
        .route("/shared/workflows/", get(routes::shared_workflows::list))
        .route("/shared/packages", get(routes::shared_packages::list))
        .route(
            "/shared/packages/{name}/versions/{version}/execute/{command}",
            put(routes::shared_packages::execute),
        )
        .route(
            "/shared/workflows/{package}/{workflow_slug}",
            get(routes::shared_workflows::workflow),
        )
        .route(
            "/shared/workflows/{package}/{workflow_slug}/jobs/{slug}",
            get(routes::shared_workflow_jobs::detail)
                .patch(routes::shared_workflow_jobs::update)
                .delete(routes::shared_workflow_jobs::delete),
        )
        .route(
            "/shared/workflows/{package}/{workflow_slug}/jobs/{slug}/confirm-delete",
            get(routes::shared_workflow_jobs::confirm_delete),
        )
        .route(
            "/shared/workflows/{package}/{workflow_slug}/jobs/{slug}/execute/{command}",
            put(routes::shared_workflow_jobs::execute),
        )
        .route(
            "/shared/workflows/{package}/{workflow_slug}/jobs/{slug}/runs",
            get(routes::shared_workflow_jobs::list_runs).post(routes::shared_workflow_jobs::start_run),
        )
        .route(
            "/api/v1/shared/workflows/{package}/{workflow_slug}/jobs/{slug}/execute/successor",
            put(routes::successor_api::shared_one),
        )
        .route(
            "/api/v1/shared/workflows/{package}/{workflow_slug}/jobs/execute/successor",
            post(routes::successor_api::shared_all),
        )
        .route(
            "/shared/workflows/{package}/{workflow_slug}/jobs/{slug}/runs/{run}/execute/cancel",
            put(routes::shared_workflow_jobs::cancel_run),
        )
        .with_state(state)
}

/// The Tasks screen's own routes: the dashboard (list and create), the
/// creation screen (`/tasks/new`, a static segment axum matches ahead of
/// the dynamic `/tasks/{id}` below - the same precedent `/apps/rescan`
/// already sets ahead of `/apps/{slug}`), and one task's own detail page
/// (read, plus every edit/close action rendered on it, each a dedicated
/// route `routes::tasks_detail` owns; creation lives in
/// `routes::tasks_create`). Extracted from [`build_router`] purely to keep
/// that function's own length within the workspace's lint bound; this
/// router carries no behaviour of its own beyond the route table.
pub(crate) fn tasks_router(state: crate::routes::tasks::TasksRoutesState) -> Router {
    Router::new()
        .route(
            "/{vault_name}/tasks",
            get(routes::tasks::list).post(routes::tasks_create::create),
        )
        .route(
            "/{vault_name}/tasks/",
            get(routes::tasks::list).post(routes::tasks_create::create),
        )
        .route("/{vault_name}/tasks/new", get(routes::tasks_create::new_task_form))
        .route("/{vault_name}/tasks/new/", get(routes::tasks_create::new_task_form))
        .route(
            "/{vault_name}/tasks/{id}",
            get(routes::tasks_detail::detail).patch(routes::tasks_detail::patch),
        )
        .route(
            "/{vault_name}/tasks/{id}/",
            get(routes::tasks_detail::detail).patch(routes::tasks_detail::patch),
        )
        .route("/{vault_name}/tasks/{id}/edit", get(routes::tasks_detail::edit_form))
        .route(
            "/{vault_name}/tasks/{id}/links/search",
            get(routes::tasks_links::search),
        )
        .route("/{vault_name}/alerts", get(routes::alerts::list))
        .route("/{vault_name}/alerts/summary", get(routes::alerts::summary))
        .with_state(state)
}

/// The vault's Overview and every file's own address.
pub(crate) fn vault_router(state: VaultRoutesState) -> Router {
    Router::new()
        .route("/", get(routes::vault::get_server_root))
        .route("/{vault_name}", get(routes::vault::get_root))
        .route("/{vault_name}/", get(routes::vault::get_root))
        // A file no screen shows, at its own vault path (`urls::VaultUrls::file`); a screen's own route wins.
        .route(
            "/{vault_name}/{*relative_path}",
            get(routes::vault::get_path)
                .post(routes::vault_mutations::mutate)
                .patch(routes::vault_mutations::mutate)
                .put(routes::vault_mutations::mutate)
                .delete(routes::vault_mutations::mutate),
        )
        .with_state(state)
}

/// `/settings/`'s own routes: the vault-independent `web.toml` configuration UI
/// (`routes::settings`), plus the `OAuth2` connect/callback/disconnect routes
/// (`routes::settings_oauth`) that extend its method contract for an
/// `oauth2`-mode `[[mcp_server]]` entry. Extracted from [`build_router`] for the identical reason
/// as [`tasks_router`].
pub(crate) fn settings_router(state: SettingsRoutesState) -> Router {
    Router::new()
        .route(
            "/settings",
            get(routes::settings::get)
                .post(routes::settings::mutate)
                .patch(routes::settings::mutate)
                .put(routes::settings::mutate),
        )
        .route(
            "/settings/",
            get(routes::settings::get)
                .post(routes::settings::mutate)
                .patch(routes::settings::mutate)
                .put(routes::settings::mutate),
        )
        .route(
            "/settings/mcp-servers/{name}",
            delete(routes::settings::delete_mcp_server),
        )
        .route(
            "/settings/mcp-servers/{name}/connection",
            get(routes::settings_connections::connection),
        )
        .route(
            "/settings/mcp-servers/{name}/reconnect",
            post(routes::settings_connections::reconnect),
        )
        .route(
            "/settings/mcp-servers/{name}/secrets/{field}",
            get(routes::settings_secrets::secret),
        )
        .route(
            "/settings/oauth/{server_name}/connect",
            post(routes::settings_oauth::connect).delete(routes::settings_oauth::disconnect),
        )
        .route(
            "/settings/oauth/{server_name}/callback",
            get(routes::settings_oauth::callback),
        )
        .route(
            "/settings/oauth/{server_name}/detect",
            post(routes::settings_oauth::detect),
        )
        .with_state(state)
}

/// The vault-scoped `/{{vault_name}}/settings/`'s own route, distinct from
/// the vault-independent `/settings/` (`web.toml`) [`build_router`] already
/// registers. Extracted for the identical reason as [`tasks_router`].
pub(crate) fn vault_settings_router(state: crate::routes::vault_settings::VaultSettingsRoutesState) -> Router {
    Router::new()
        .route(
            "/{vault_name}/settings",
            get(routes::vault_settings::get)
                .patch(routes::vault_settings::mutate)
                .put(routes::vault_settings::mutate),
        )
        .route(
            "/{vault_name}/settings/",
            get(routes::vault_settings::get)
                .patch(routes::vault_settings::mutate)
                .put(routes::vault_settings::mutate),
        )
        .with_state(state)
}

/// The vault settings page's "Vault management" categories (`routes::vault_management`): each category is its own
/// resource beneath `/{vault_name}/settings/`, and everything beneath a category is that category's own path, so one
/// handler per shape takes every method and the category module decides what each method means there. Extracted for
/// the identical reason as [`tasks_router`].
pub(crate) fn vault_management_router(state: VaultManagementRoutesState) -> Router {
    let every_method = || {
        get(routes::vault_management::category_root)
            .post(routes::vault_management::category_root)
            .put(routes::vault_management::category_root)
            .patch(routes::vault_management::category_root)
    };
    let every_method_beneath = || {
        get(routes::vault_management::category_path)
            .post(routes::vault_management::category_path)
            .put(routes::vault_management::category_path)
            .patch(routes::vault_management::category_path)
    };
    Router::new()
        .route("/{vault_name}/settings/{category}", every_method())
        .route("/{vault_name}/settings/{category}/", every_method())
        .route("/{vault_name}/settings/{category}/{*rest}", every_method_beneath())
        .with_state(state)
}

/// Vault health: the pane, the compact indicators, and the refresh and two-step repair commands. Extracted for the
/// identical reason as [`tasks_router`].
pub(crate) fn vault_health_router(state: VaultHealthRoutesState) -> Router {
    Router::new()
        .route("/{vault_name}/health", get(routes::vault_health::get))
        .route("/{vault_name}/health/summary", get(routes::vault_health::summary))
        .route(
            "/{vault_name}/health/execute/refresh",
            post(routes::vault_health::refresh),
        )
        .route(
            "/{vault_name}/health/execute/repair-preview",
            post(routes::vault_health::repair_preview),
        )
        .route(
            "/{vault_name}/health/execute/repair",
            post(routes::vault_health::repair),
        )
        .with_state(state)
}

/// The Inbox/Outbox screens' own routes: `path` addresses a browsed folder
/// beneath the screen's own root directory
/// (`docs/http-routing-response-contract.md`'s own "the resource's path is
/// its identifier" model, matching `routes::wiki`'s identical shape) - never
/// a `?path=` query parameter standing in for it, and never repeating the
/// root directory's own name. `GET` on the bare root browses the Inbox
/// screen's own merged view (both `inbox_dir` and `processed_dir` at once)
/// or the Outbox screen's own `outbox_dir` root; `GET` on a path browses
/// that subfolder, the Inbox's processed tree living beneath the reserved
/// first segment `processed_dir` names.
/// `POST` on the Inbox wildcard dispatches to whichever action its own
/// trailing `/execute/upload`/`/execute/ingest` sub-path names
/// (`routes::inbox_outbox::inbox_action`'s own doc), matching
/// `routes::wiki`'s identical `/execute/new-folder` shape; Outbox has no
/// `POST` action of its own. Extracted from [`build_router`] for the
/// identical reason as [`tasks_router`].
/// The request body limit for a route that receives whole file content (the MCP proxy, which relays tool calls such
/// as `fs_write_file`, and the Inbox upload): the shared [`contextos_core::MAX_REQUEST_BODY_BYTES`], the same default
/// `contextos-mcp`'s HTTP transport applies, in place of axum's own 2 MB default, which would refuse any file over
/// 2 MB that the tools behind these routes accept.
pub(crate) fn file_carrying_body_limit() -> axum::extract::DefaultBodyLimit {
    axum::extract::DefaultBodyLimit::max(usize::try_from(contextos_core::MAX_REQUEST_BODY_BYTES).unwrap_or(usize::MAX))
}

pub(crate) fn inbox_outbox_router(state: InboxOutboxRoutesState) -> Router {
    Router::new()
        .route("/{vault_name}/inbox", get(routes::inbox_outbox::inbox_get))
        .route("/{vault_name}/inbox/", get(routes::inbox_outbox::inbox_get))
        .route(
            "/{vault_name}/inbox/{*path}",
            get(routes::inbox_outbox::inbox_get_path)
                .post(routes::inbox_outbox::inbox_action)
                .layer(file_carrying_body_limit()),
        )
        .route("/{vault_name}/outbox", get(routes::inbox_outbox::outbox_get))
        .route("/{vault_name}/outbox/", get(routes::inbox_outbox::outbox_get))
        .route(
            "/{vault_name}/outbox/{*path}",
            get(routes::inbox_outbox::outbox_get_path),
        )
        .with_state(state)
}

/// The Journal screen's own routes: `date` addresses a real day directly
/// (`docs/http-routing-response-contract.md`'s own "the resource's path is
/// its identifier" model), never a `?date=` query parameter standing in for
/// it. The bare root has no "pick one" resource of its own, since every real
/// day is its own single-entry resource, so it redirects to today's date path.
/// A day's sections are its sub-resources, addressed by their place in the
/// entry: `GET` shows one, `GET .../edit` loads its edit form, and `PATCH`
/// saves it. Extracted from [`build_router`] for the identical reason as
/// [`tasks_router`].
pub(crate) fn journal_router(state: JournalRoutesState) -> Router {
    Router::new()
        .route("/{vault_name}/journal", get(routes::journal::get_root))
        .route("/{vault_name}/journal/", get(routes::journal::get_root))
        .route("/{vault_name}/journal/{date}", get(routes::journal::get))
        .route(
            "/{vault_name}/journal/{date}/sections/{index}",
            get(routes::journal::get_section).patch(routes::journal::patch_section),
        )
        .route(
            "/{vault_name}/journal/{date}/sections/{index}/edit",
            get(routes::journal::get_section_edit),
        )
        .with_state(state)
}

/// The Notes screen's own routes: `path` addresses a real note directly
/// (`docs/http-routing-response-contract.md`'s own "the resource's path is its
/// identifier" model, `routes::wiki`'s identical shape) - never a `?note=` query
/// parameter standing in for it. `GET` on the bare root selects the most recent note, or
/// auto-creates a first one; `GET` on a note's own path selects that note, or (its own
/// reserved `/confirm-delete` sub-path, dispatched inside `routes::notes::get_path` itself)
/// loads its own delete confirmation; `GET .../new` loads the "New" form. `POST` on the
/// bare root is that form's own submit action (`note_create`) - Notes has no `create_path`
/// counterpart, since a new note's own path is the operator's own choice from the form
/// itself, never "inside" an operator-named existing path the way a Wiki page is. `PATCH`
/// on a note's own path saves its edited body or frontmatter, or (its own `/rename`
/// sub-path) renames it. `DELETE` on a note's own path moves it to trash; the bare root is
/// never a deletable target of its own. Extracted from [`build_router`] for the identical
/// reason as [`tasks_router`].
pub(crate) fn notes_router(state: NotesRoutesState) -> Router {
    Router::new()
        .route(
            "/{vault_name}/notes",
            get(routes::notes::get_root)
                .post(routes::notes::create_root)
                .delete(routes::notes::delete_root),
        )
        .route(
            "/{vault_name}/notes/",
            get(routes::notes::get_root)
                .post(routes::notes::create_root)
                .delete(routes::notes::delete_root),
        )
        .route(
            "/{vault_name}/notes/{*path}",
            get(routes::notes::get_path)
                .patch(routes::notes::patch_body)
                .delete(routes::notes::delete_path),
        )
        .with_state(state)
}

/// The Wiki screen's own routes: `path` addresses a real folder or page
/// directly (`docs/http-routing-response-contract.md`'s own "the resource's
/// path is its identifier" model). `GET` browses a folder or reads a page,
/// or (via the contract's own reserved `/edit`/`/confirm-delete` sub-paths,
/// dispatched inside `routes::wiki::get_path` itself) loads a page's own
/// edit form or a page's/folder's own delete confirmation; `POST` on a
/// folder creates a page inside it (`note_create`), or (its own
/// `/execute/new-folder` sub-path) a new, empty subfolder
/// (`fs_create_directory`); `PATCH` on a page saves its edited body, or (its
/// own `/rename` sub-path) updates its title; `DELETE` on a page or an
/// empty folder moves it to trash. Extracted from [`build_router`] for the
/// identical reason as [`tasks_router`].
pub(crate) fn wiki_router(state: WikiRoutesState) -> Router {
    Router::new()
        .route(
            "/{vault_name}/wiki",
            get(routes::wiki::get_root)
                .post(routes::wiki_create::create_root)
                .delete(routes::wiki_delete::delete_root),
        )
        .route(
            "/{vault_name}/wiki/",
            get(routes::wiki::get_root)
                .post(routes::wiki_create::create_root)
                .delete(routes::wiki_delete::delete_root),
        )
        .route(
            "/{vault_name}/wiki/{*path}",
            get(routes::wiki::get_path)
                .post(routes::wiki_create::create_path)
                .patch(routes::wiki_save::patch_body)
                .delete(routes::wiki_delete::delete_path),
        )
        .with_state(state)
}

/// The Insights screen's own routes: `path` addresses a real shard or note
/// directly, matching `routes::wiki`'s identical "the resource's path is
/// its identifier" model. `GET` browses a shard, reads a note, or - via the
/// contract's own reserved `/edit`/`/confirm-delete` sub-paths, dispatched
/// inside `routes::insights::get_path` itself since axum resolves the
/// identical `{*path}` wildcard for all three - loads that note's own edit
/// form or delete confirmation; `POST` at the root creates a note
/// (`note_create`, its own shard derived from the title); `PATCH` on a note
/// saves either its edited body or its frontmatter (the metadata panel's own submitted form -
/// `routes::insights_save`), or (its own `/rename` sub-path, dispatched the same way) updates
/// its title/status together; `DELETE` on a note moves it to trash. Extracted from
/// [`build_router`] for the identical reason as [`tasks_router`].
pub(crate) fn insights_router(state: InsightsRoutesState) -> Router {
    Router::new()
        .route(
            "/{vault_name}/insights",
            get(routes::insights::get_root).post(routes::insights_create::create),
        )
        .route(
            "/{vault_name}/insights/",
            get(routes::insights::get_root).post(routes::insights_create::create),
        )
        .route(
            "/{vault_name}/insights/{*path}",
            get(routes::insights::get_path)
                .patch(routes::insights_save::patch_body)
                .delete(routes::insights_delete::delete),
        )
        .with_state(state)
}

/// The entity-folder screen's own routes: `path`'s first segment addresses
/// a configured entity directly, the remainder its own real folder or file
/// within it, matching `routes::wiki`'s identical "the resource's path is
/// its identifier" model. `GET` browses a folder or reads a markdown file,
/// or (via the contract's own reserved `/edit`/`/confirm-delete` sub-paths,
/// dispatched inside `routes::entity_docs::get_path` itself) loads a file's
/// own edit form or a file's/folder's own delete confirmation; `POST` on a
/// folder creates a markdown file inside it (`note_create`), or (its own
/// `/execute/new-folder` sub-path) a new, empty subfolder
/// (`fs_create_directory`) - reachable anywhere in the tree, including the
/// entity's own root, since entity folders carries no minimum-depth floor;
/// `PATCH` on a markdown file saves its edited body; `DELETE` on a file
/// (markdown or not) or an empty folder moves it to trash. The bare entity-
/// less root has no resource of its own, so it stays `GET`-only (redirect to
/// the vault's first configured entity, or a plain prompt). Extracted from
/// [`build_router`] for the identical reason as [`tasks_router`].
pub(crate) fn entity_docs_router(state: EntityDocsRoutesState) -> Router {
    Router::new()
        .route("/{vault_name}/entity", get(routes::entity_docs::get_root))
        .route("/{vault_name}/entity/", get(routes::entity_docs::get_root))
        .route(
            "/{vault_name}/entity/{*path}",
            get(routes::entity_docs::get_path)
                .post(routes::entity_docs::create_path)
                .patch(routes::entity_docs::patch_body)
                .delete(routes::entity_docs::delete_path),
        )
        .with_state(state)
}

/// The Profile screen's own routes: Identity and Operating each address a
/// real file directly under their own fixed root; Entities addresses
/// either a configured entity's own profile page (creating it, and its own
/// top-level folder, via a dedicated provision `POST`) or a real file
/// matching no configured entity. Every group's own root is registered
/// bare and with a trailing slash for the identical reason `build_router`'s
/// own "collection root" comment already gives. Extracted from
/// [`build_router`] for the identical reason as [`tasks_router`].
/// The Command Centre search results view: one route serves the full page, an HTMX fragment
/// request (`routes::search::get`'s own `hx-request` check), and the link-panels-only partial
/// swap a result's own selection link issues via `hx-select`.
pub(crate) fn search_router(state: SearchRoutesState) -> Router {
    Router::new()
        .route("/{vault_name}/search", get(routes::search::get))
        .with_state(state)
}

pub(crate) fn profile_router(state: ProfileRoutesState) -> Router {
    Router::new()
        .route("/{vault_name}/profile", get(routes::profile::get_root))
        .route("/{vault_name}/profile/", get(routes::profile::get_root))
        .route(
            "/{vault_name}/profile/identity",
            get(routes::profile::get_root).post(routes::profile::create_identity),
        )
        .route(
            "/{vault_name}/profile/identity/",
            get(routes::profile::get_root).post(routes::profile::create_identity),
        )
        .route(
            "/{vault_name}/profile/identity/{filename}",
            get(routes::profile::get_identity_file).patch(routes::profile::patch_identity),
        )
        .route(
            "/{vault_name}/profile/identity/{filename}/edit",
            get(routes::profile::get_identity_edit),
        )
        .route(
            "/{vault_name}/profile/operating",
            get(routes::profile::get_root).post(routes::profile::create_operating),
        )
        .route(
            "/{vault_name}/profile/operating/",
            get(routes::profile::get_root).post(routes::profile::create_operating),
        )
        .route(
            "/{vault_name}/profile/operating/{filename}",
            get(routes::profile::get_operating_file).patch(routes::profile::patch_operating),
        )
        .route(
            "/{vault_name}/profile/operating/{filename}/edit",
            get(routes::profile::get_operating_edit),
        )
        .route("/{vault_name}/profile/entities", get(routes::profile::get_root))
        .route("/{vault_name}/profile/entities/", get(routes::profile::get_root))
        .route(
            "/{vault_name}/profile/entities/{slug}",
            get(routes::profile::get_entity)
                .patch(routes::profile::patch_entity)
                .post(routes::profile::post_provision_entity),
        )
        .route(
            "/{vault_name}/profile/entities/{slug}/edit",
            get(routes::profile::get_entity_edit),
        )
        .with_state(state)
}

/// The host's package store as a JSON API (`docs/http-routing-response-contract.md`'s `/api/v1` surface), under `shared`
/// where a vault's store has its name. `{name}` addresses a package by its own name; `versions/{version}` addresses one
/// specific installed version under it, matching that contract's own sub-entity nesting shape: `DELETE` is the guarded
/// removal and `PUT .../execute/remove` the forced one. Extracted from [`build_router`] for the identical reason as
/// [`tasks_router`].
pub(crate) fn packages_router(state: PackagesRoutesState) -> Router {
    Router::new()
        .route(
            "/api/v1/shared/packages",
            get(routes::packages::shared_list).post(routes::packages::create),
        )
        .route(
            "/api/v1/shared/packages/{name}/versions/{version}",
            get(routes::packages::shared_get).delete(routes::packages::shared_delete),
        )
        .route(
            "/api/v1/shared/packages/{name}/versions/{version}/execute/remove",
            put(routes::packages::shared_remove),
        )
        .with_state(state)
}
