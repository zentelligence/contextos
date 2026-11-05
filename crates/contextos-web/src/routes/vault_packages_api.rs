//! `/api/v1/{vault}/packages`: a vault's package store as a JSON API, the same surface the host's has under `shared`
//! ([`crate::routes::packages`]). Each handler checks that the primary server serves the vault, then calls the one code
//! the `packages` commands call.

use axum::body::Bytes;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::response::Response;

use crate::package_stores::VaultPackagesOf;
use crate::routes::packages::{
    NoQuery, deleted_response, force_of, query_rejection, release_response, removed_response, standing_response,
};
use crate::routes::successor_api::reach_vault;
use crate::routes::workflows::WorkflowRoutesState;

/// `GET /api/v1/{vault}/packages`: the store, with its ledger, each release and what keeps it, what was removed, and what
/// unfinished operations left.
pub async fn list(State(state): State<WorkflowRoutesState>, Path(vault): Path<String>) -> Response {
    let client = match reach_vault(&state, &vault).await {
        Ok(client) => client,
        Err(response) => return *response,
    };
    standing_response(
        VaultPackagesOf {
            client: &client,
            vault: &vault,
        }
        .standing()
        .await,
    )
}

/// `GET /api/v1/{vault}/packages/{name}/versions/{version}`: one release in full.
pub async fn get_one(
    State(state): State<WorkflowRoutesState>,
    Path((vault, name, version)): Path<(String, String, String)>,
) -> Response {
    let client = match reach_vault(&state, &vault).await {
        Ok(client) => client,
        Err(response) => return *response,
    };
    release_response(
        VaultPackagesOf {
            client: &client,
            vault: &vault,
        }
        .release(&name, &version)
        .await,
    )
}

/// `DELETE /api/v1/{vault}/packages/{name}/versions/{version}`: the guarded removal. A release a rule keeps is refused with
/// `409` `packages/in-use`. A removal that finished cleanly is `204`; one with something it could not finish, or after which
/// retention removed further releases, is `200` with the removal as a document.
pub async fn delete_one(
    State(state): State<WorkflowRoutesState>,
    Path((vault, name, version)): Path<(String, String, String)>,
    query: Result<Query<NoQuery>, QueryRejection>,
) -> Response {
    if let Err(rejection) = query {
        return query_rejection(&rejection);
    }
    let client = match reach_vault(&state, &vault).await {
        Ok(client) => client,
        Err(response) => return *response,
    };
    deleted_response(
        VaultPackagesOf {
            client: &client,
            vault: &vault,
        }
        .remove(&name, &version, false)
        .await,
    )
}

/// `PUT /api/v1/{vault}/packages/{name}/versions/{version}/execute/remove`: the removal asked for as a command, with
/// `{"force": true}` in the body to override the guard.
pub async fn remove(
    State(state): State<WorkflowRoutesState>,
    Path((vault, name, version)): Path<(String, String, String)>,
    body: Bytes,
) -> Response {
    let force = match force_of(&body) {
        Ok(force) => force,
        Err(response) => return *response,
    };
    let client = match reach_vault(&state, &vault).await {
        Ok(client) => client,
        Err(response) => return *response,
    };
    removed_response(
        VaultPackagesOf {
            client: &client,
            vault: &vault,
        }
        .remove(&name, &version, force)
        .await,
    )
}
