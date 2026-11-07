//! `/api/v1/server/packages`: the host's package store as a JSON API
//! (`docs/http-routing-response-contract.md`): install a bundle, list the store, inspect a release, and remove one.
//! `DELETE .../{name}/versions/{version}` is the guarded removal, and `PUT .../execute/remove` with `force` in its body is
//! the forced one. A vault's store has the same surface under `/api/v1/{vault}/packages`
//! ([`crate::routes::vault_packages_api`]), and both answer with the documents the `packages` commands print under
//! `--format json`.
//!
//! The host store never touches a vault or an MCP client, so [`PackagesRoutesState`] carries only the store's own paths,
//! resolved once at startup (`app.rs::build_router`) and never re-derived per request, so a test can inject a temporary
//! root instead of this store ever touching the operator's real home directory (`testing.md`). Blocking filesystem work
//! runs on a blocking thread, never on an async executor thread.

use std::path::PathBuf;
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::{CONTENT_TYPE, LOCATION};
use axum::response::{IntoResponse, Response};
use contextos_packages::PackagesError;
use contextos_workflows::package::{PackageError, PackageIdentity};
use serde::{Deserialize, Serialize};

use crate::package_keep::ReasonView;
use crate::package_op_error::PackageOpError;
use crate::package_standing::{ReleaseStanding, StoreStanding};
use crate::package_store::{self, Installed, MAX_BUNDLE_BYTES, PackageStoreError};
use crate::package_store_report::{ReleaseJson, RemovalJson, RemovalReport, StandingJson};
use crate::package_stores::SharedPackages;
use crate::saved_jobs::SavedJobs;
use crate::shared_state::SharedStateStore;
use crate::store_retention::RetentionWarning;

/// Shared state every `/api/v1/server/packages` handler needs: the package store's own paths, resolved once (see this
/// module's own doc).
#[derive(Clone)]
pub struct PackagesRoutesState {
    store_root: Arc<PathBuf>,
    /// Where the saved shared jobs and their state are, which say what keeps a release.
    jobs: Arc<SavedJobs>,
}

impl PackagesRoutesState {
    #[must_use]
    pub fn new(store_root: PathBuf, jobs_root: PathBuf, state_root: PathBuf) -> Self {
        Self {
            store_root: Arc::new(store_root),
            jobs: Arc::new(SavedJobs {
                jobs_root,
                state: SharedStateStore::new(state_root),
            }),
        }
    }

    fn packages(&self) -> SharedPackages {
        SharedPackages {
            store_root: self.store_root.as_ref().clone(),
            jobs: self.jobs.as_ref().clone(),
        }
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: String,
    /// Every reason that refused a removal, for a release in use.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    reasons: Vec<ReasonView>,
}

pub(super) fn error_response(status: StatusCode, code: &'static str, message: impl Into<String>) -> Response {
    detailed_response(status, code, message.into(), Vec::new())
}

fn detailed_response(status: StatusCode, code: &'static str, message: String, reasons: Vec<ReasonView>) -> Response {
    (
        status,
        Json(ErrorBody {
            error: ErrorDetail { code, message, reasons },
        }),
    )
        .into_response()
}

fn internal_error_response() -> Response {
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "packages/internal-error",
        "an internal error occurred",
    )
}

/// A successful answer: the document a command prints under `--format json`.
pub(super) fn json_response(document: String) -> Response {
    (StatusCode::OK, [(CONTENT_TYPE, "application/json")], document).into_response()
}

/// The answer to a store operation that was refused or failed, by the code it has. A release in use, a ledger that cannot
/// be used, and a store another operation holds are conflicts, a release that is not there is not found, a name or version
/// that is wrong is the caller's, and a store that could not be read or written is the server's: logged, and reported only
/// as a generic `500`, never leaking a path or internal detail (`security.md`).
pub(super) fn refusal_response(error: &PackageOpError) -> Response {
    let message = error.to_string();
    match error {
        PackageOpError::InUse { reasons, .. } => detailed_response(
            StatusCode::CONFLICT,
            error.code(),
            message,
            reasons.iter().map(ReasonView::from).collect(),
        ),
        PackageOpError::Name { .. }
        | PackageOpError::Bundle(_)
        | PackageOpError::BundleTooLarge { .. }
        | PackageOpError::FolderTooLarge { .. } => error_response(StatusCode::BAD_REQUEST, error.code(), message),
        PackageOpError::Packages(error) => packages_error_response(error, message),
        PackageOpError::Manifest(PackageError::DuplicateIdentity { .. }) => {
            error_response(StatusCode::CONFLICT, error.code(), message)
        }
        PackageOpError::Manifest(_) | PackageOpError::Unresolvable { .. } => {
            error_response(StatusCode::UNPROCESSABLE_ENTITY, error.code(), message)
        }
        // The state file's path is the operator's, not the caller's: the code says what to fix, the log says where.
        PackageOpError::KeepUnreadable(_) => {
            tracing::error!(error = %message, code = error.code(), "what keeps releases could not be read");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                error.code(),
                "what keeps releases could not be read, so nothing was changed; see the server log, fix or restore the job state, and try again",
            )
        }
        PackageOpError::ConfigInvalid { .. }
        | PackageOpError::NotConnected { .. }
        | PackageOpError::Unreachable(_)
        | PackageOpError::Vault(_)
        | PackageOpError::StoreIo(_)
        | PackageOpError::Release(_)
        | PackageOpError::Host { .. } => {
            tracing::error!(error = %message, code = error.code(), "package store operation failed");
            internal_error_response()
        }
    }
}

/// Maps a [`PackageStoreError`] from an install to this surface's own documented status code and error code. A
/// storage-integrity failure this store's own install-time validation should have already prevented
/// (`AppDataUnavailable`, `CorruptInstalledManifest`, `Io`) is logged at `ERROR` and reported to the caller only as a
/// generic `500`.
fn store_error_response(error: &PackageStoreError) -> Response {
    let message = error.to_string();
    match error {
        PackageStoreError::BundleTooLarge { .. } => {
            error_response(StatusCode::BAD_REQUEST, "packages/bundle-too-large", message)
        }
        // The HTTP route installs only uploaded bundles; a folder install is the command line's.
        PackageStoreError::FolderTooLarge { .. } => {
            error_response(StatusCode::BAD_REQUEST, "packages/folder-too-large", message)
        }
        PackageStoreError::StoreLocked => error_response(StatusCode::CONFLICT, "packages/store-locked", message),
        PackageStoreError::Unresolvable { .. } => error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "packages/dependency-unsatisfied",
            message,
        ),
        PackageStoreError::Bundle(_) => error_response(StatusCode::BAD_REQUEST, "packages/invalid-bundle", message),
        PackageStoreError::Package(PackageError::DuplicateIdentity { .. }) => {
            error_response(StatusCode::CONFLICT, "packages/duplicate-identity", message)
        }
        PackageStoreError::Package(_) => {
            error_response(StatusCode::UNPROCESSABLE_ENTITY, "packages/invalid-manifest", message)
        }
        PackageStoreError::NotFound { .. } => error_response(StatusCode::NOT_FOUND, "packages/not-found", message),
        PackageStoreError::Packages(error) => packages_error_response(error, message),
        PackageStoreError::AppDataUnavailable
        | PackageStoreError::ReleaseDamaged { .. }
        | PackageStoreError::ReleaseUnverifiable { .. }
        | PackageStoreError::ReleaseChangedWhileRead { .. }
        // Reading a run's snapshot is not something this surface does; a failure of one never reaches it.
        | PackageStoreError::NoSuchWorkflow { .. }
        | PackageStoreError::WorkflowInvalid { .. }
        | PackageStoreError::EntryUnreadable { .. }
        | PackageStoreError::ModuleUnreadable(_)
        | PackageStoreError::State(_)
        | PackageStoreError::Io(_) => {
            tracing::error!(error = %message, "package store operation failed");
            internal_error_response()
        }
    }
}

/// The engine's own refusals, under the engine's stable code: a ledger that cannot be used or rebuilt is a conflict with
/// the store's state, a manifest the model refuses is unprocessable, and a name, version, or size that is wrong is the
/// caller's. A store that could not be read or written is the server's, and is not described further.
fn packages_error_response(error: &PackagesError, message: String) -> Response {
    let code = error.code();
    match error {
        PackagesError::LedgerMissing
        | PackagesError::LedgerUnreadable { .. }
        | PackagesError::LedgerPresent
        | PackagesError::ReleaseIncomplete { .. }
        | PackagesError::DuplicateIdentity { .. }
        | PackagesError::StoreLocked => error_response(StatusCode::CONFLICT, code, message),
        PackagesError::InvalidManifest(_) | PackagesError::VersionStepInvalid(_) => {
            error_response(StatusCode::UNPROCESSABLE_ENTITY, code, message)
        }
        PackagesError::InvalidName { .. }
        | PackagesError::InvalidVersion { .. }
        | PackagesError::InvalidStorePath { .. }
        | PackagesError::LimitExceeded { .. } => error_response(StatusCode::BAD_REQUEST, code, message),
        PackagesError::NotFound { .. } => error_response(StatusCode::NOT_FOUND, code, message),
        PackagesError::Store(_) => {
            tracing::error!(error = %message, "package store operation failed");
            internal_error_response()
        }
    }
}

/// The answer to a read of a store.
pub(super) fn standing_response(result: Result<StoreStanding, PackageOpError>) -> Response {
    match result {
        Ok(standing) => json_response(StandingJson(&standing).to_string()),
        Err(error) => refusal_response(&error),
    }
}

/// The answer to a read of one release.
pub(super) fn release_response(result: Result<ReleaseStanding, PackageOpError>) -> Response {
    match result {
        Ok(release) => json_response(ReleaseJson(&release).to_string()),
        Err(error) => refusal_response(&error),
    }
}

/// The answer to a guarded removal: `204` when it went through with nothing to say, otherwise `200` with the removal as a
/// document, because the caller has to be told what could not be finished after it, or which releases retention removed
/// as well. This is the one exception to `204` for a `DELETE` in `docs/http-routing-response-contract.md`.
pub(super) fn deleted_response(result: Result<RemovalReport, PackageOpError>) -> Response {
    match result {
        Ok(report) if report.warnings.is_empty() && report.retention_removed.is_empty() => {
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(report) => json_response(RemovalJson(&report).to_string()),
        Err(error) => refusal_response(&error),
    }
}

/// The answer to a removal asked for as a command: always the removal as a document.
pub(super) fn removed_response(result: Result<RemovalReport, PackageOpError>) -> Response {
    match result {
        Ok(report) => json_response(RemovalJson(&report).to_string()),
        Err(error) => refusal_response(&error),
    }
}

/// A `DELETE` takes no query: an option such as the old `?force=true` is refused rather than ignored, since ignoring it would
/// remove under the guard what the caller meant to force.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoQuery {}

/// The answer to a query string a `DELETE` was given, when it was not empty.
pub(super) fn query_rejection(rejection: &QueryRejection) -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        "packages/invalid-query",
        format!("the query is not understood: {}", rejection.body_text()),
    )
}

/// The body of `PUT .../execute/remove`.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveBody {
    #[serde(default)]
    force: bool,
}

/// Whether the body of `PUT .../execute/remove` asks for the guard to be overridden. An empty body asks for the guarded
/// removal.
pub(super) fn force_of(body: &Bytes) -> Result<bool, Box<Response>> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(RemoveBody::default().force);
    }
    serde_json::from_slice::<RemoveBody>(body)
        .map(|parsed| parsed.force)
        .map_err(|error| {
            Box::new(error_response(
                StatusCode::BAD_REQUEST,
                "packages/invalid-request",
                format!("the body is not understood: {error}"),
            ))
        })
}

#[derive(Serialize)]
struct IdentityBody {
    name: String,
    version: String,
    digest: String,
}

impl From<PackageIdentity> for IdentityBody {
    fn from(identity: PackageIdentity) -> Self {
        Self {
            name: identity.name.to_string(),
            version: identity.version.to_string(),
            digest: identity.digest.as_str().to_owned(),
        }
    }
}

#[derive(Serialize)]
struct InstalledBody {
    #[serde(flatten)]
    identity: IdentityBody,
    /// What went wrong after the release was installed, which does not undo it. Absent when nothing did.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<WarningBody>,
    /// Present and true when retention removed the release again at once, because nothing keeps it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    removed_by_retention: bool,
}

#[derive(Serialize)]
struct WarningBody {
    code: &'static str,
    message: String,
}

impl From<&RetentionWarning> for WarningBody {
    fn from(warning: &RetentionWarning) -> Self {
        Self {
            code: RetentionWarning::CODE,
            message: warning.message().to_owned(),
        }
    }
}

impl From<Installed> for InstalledBody {
    fn from(installed: Installed) -> Self {
        Self {
            identity: installed.identity.into(),
            warnings: installed.warnings.iter().map(WarningBody::from).collect(),
            removed_by_retention: installed.removed_by_retention,
        }
    }
}

/// `GET /api/v1/server/packages`: the store, with its ledger, each release and what keeps it, what was removed, and what
/// unfinished operations left. A release that cannot be read is listed for itself and does not stop the others.
pub async fn shared_list(State(state): State<PackagesRoutesState>) -> Response {
    standing_response(state.packages().standing().await)
}

/// `POST /api/v1/server/packages`: verifies and installs a package bundle
/// submitted as a single multipart field's own raw bytes (a `.tar.gz`
/// archive), matching `routes::inbox_outbox_upload`'s identical
/// multipart-field pattern.
pub async fn create(State(state): State<PackagesRoutesState>, mut multipart: Multipart) -> Response {
    let field = match multipart.next_field().await {
        Ok(Some(field)) => field,
        Ok(None) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "packages/missing-bundle",
                "no bundle file was submitted",
            );
        }
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "packages/malformed-upload",
                "the upload request was malformed",
            );
        }
    };
    let Ok(bundle_bytes) = field.bytes().await else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "packages/malformed-upload",
            "the upload request was malformed",
        );
    };
    if bundle_bytes.len() > MAX_BUNDLE_BYTES {
        return error_response(
            StatusCode::BAD_REQUEST,
            "packages/bundle-too-large",
            format!(
                "the bundle is {} bytes, larger than the {MAX_BUNDLE_BYTES} byte limit",
                bundle_bytes.len()
            ),
        );
    }

    let outcome =
        tokio::task::spawn_blocking(move || package_store::install(&state.store_root, &state.jobs, &bundle_bytes))
            .await;
    match outcome {
        Ok(Ok(installed)) => {
            let location = format!(
                "/api/v1/server/packages/{}/versions/{}",
                installed.identity.name, installed.identity.version
            );
            (
                StatusCode::CREATED,
                [(LOCATION, location)],
                Json(InstalledBody::from(installed)),
            )
                .into_response()
        }
        Ok(Err(error)) => store_error_response(&error),
        Err(_join_error) => internal_error_response(),
    }
}

/// `GET /api/v1/server/packages/{name}/versions/{version}`: one release in full.
pub async fn shared_get(
    State(state): State<PackagesRoutesState>,
    Path((name, version)): Path<(String, String)>,
) -> Response {
    release_response(state.packages().release(&name, &version).await)
}

/// `DELETE /api/v1/server/packages/{name}/versions/{version}`: the guarded removal. A release a rule keeps is refused with
/// `409` `packages/in-use`, naming the jobs and releases that hold it. A removal that finished cleanly is `204`; one with
/// something it could not finish afterwards, or after which retention removed further releases, is `200` with the
/// removal as a document.
pub async fn shared_delete(
    State(state): State<PackagesRoutesState>,
    Path((name, version)): Path<(String, String)>,
    query: Result<Query<NoQuery>, QueryRejection>,
) -> Response {
    if let Err(rejection) = query {
        return query_rejection(&rejection);
    }
    deleted_response(state.packages().remove(&name, &version, false).await)
}

/// `PUT /api/v1/server/packages/{name}/versions/{version}/execute/remove`: the removal asked for as a command, with
/// `{"force": true}` in the body to override the guard. The answer names what was overridden and the jobs the next
/// validation pass will move.
pub async fn shared_remove(
    State(state): State<PackagesRoutesState>,
    Path((name, version)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    let force = match force_of(&body) {
        Ok(force) => force,
        Err(response) => return *response,
    };
    removed_response(state.packages().remove(&name, &version, force).await)
}

#[cfg(test)]
#[path = "packages_test.rs"]
mod tests;
