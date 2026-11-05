use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use axum::routing::{get, put};
use flate2::Compression;
use flate2::write::GzEncoder;
use std::fmt::Write as _;
use tower::ServiceExt as _;

use super::{PackagesRoutesState, create, shared_delete, shared_get, shared_list, shared_remove};

type BoxError = Box<dyn std::error::Error>;

fn router(store_root: &std::path::Path) -> Router {
    Router::new()
        .route("/api/v1/shared/packages", get(shared_list).post(create))
        .route(
            "/api/v1/shared/packages/{name}/versions/{version}",
            get(shared_get).delete(shared_delete),
        )
        .route(
            "/api/v1/shared/packages/{name}/versions/{version}/execute/remove",
            put(shared_remove),
        )
        .with_state(PackagesRoutesState::new(
            store_root.to_path_buf(),
            store_root.with_extension("jobs"),
            store_root.with_extension("state"),
        ))
}

fn build_bundle(name: &str, version: &str) -> Result<Vec<u8>, BoxError> {
    build_package(name, version, "", true)
}

fn needs(dependencies: &[(&str, &str)]) -> String {
    let mut text = String::new();
    for (dependency, requirement) in dependencies {
        let _ = write!(
            text,
            "[[dependencies]]\nname = {dependency:?}\nrequirement = {requirement:?}\n"
        );
    }
    text
}

/// A library exporting the module `m`, which needs each of `dependencies` as `(name, requirement)`.
fn build_library(name: &str, version: &str, dependencies: &[(&str, &str)]) -> Result<Vec<u8>, BoxError> {
    let needs = needs(dependencies);
    build_package(
        name,
        version,
        &format!("modules = [\"m.rhai\"]\nexports = [\"m.rhai\"]\n{needs}"),
        false,
    )
}

/// A package with a workflow that needs each of `dependencies` as `(name, requirement)`.
fn build_dependent(name: &str, version: &str, dependencies: &[(&str, &str)]) -> Result<Vec<u8>, BoxError> {
    let needs = needs(dependencies);
    build_package(name, version, &needs, true)
}

fn build_package(name: &str, version: &str, extra: &str, workflow: bool) -> Result<Vec<u8>, BoxError> {
    let (file, entry): (&str, &[u8]) = if workflow {
        ("sync.rhai", b"// sync workflow")
    } else {
        ("m.rhai", b"fn m() { 1 }")
    };
    let mut sketch = format!("name = {name:?}\nversion = {version:?}\ndigest = \"\"\n{extra}\n");
    if workflow {
        sketch.push_str("[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n");
    }
    let (manifest, files) = crate::release_fixture::sketch_release(&sketch, &[(file, entry)])?;
    let gzip = Vec::new();
    let encoder = GzEncoder::new(gzip, Compression::default());
    let mut builder = tar::Builder::new(encoder);
    let members = std::iter::once(("manifest.toml".to_owned(), manifest.into_bytes())).chain(files);
    for (member_name, contents) in members {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, member_name, contents.as_slice())?;
    }
    let encoder = builder.into_inner()?;
    Ok(encoder.finish()?)
}

/// Builds one `multipart/form-data` request over a single `bundle` field,
/// matching `tests/inbox_outbox.rs`'s own `post_upload` shape.
async fn post_bundle(router: &Router, bundle_bytes: &[u8]) -> Result<(StatusCode, Vec<u8>, Option<String>), BoxError> {
    const BOUNDARY: &str = "contract-test-boundary";
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"bundle\"; filename=\"bundle.tar.gz\"\r\n");
    body.extend_from_slice(b"Content-Type: application/gzip\r\n\r\n");
    body.extend_from_slice(bundle_bytes);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/shared/packages")
                .header("content-type", format!("multipart/form-data; boundary={BOUNDARY}"))
                .body(Body::from(body))?,
        )
        .await?;
    let status = response.status();
    let location = response
        .headers()
        .get(axum::http::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = to_bytes(response.into_body(), usize::MAX).await?.to_vec();
    Ok((status, bytes, location))
}

async fn get_path(router: &Router, path: &str) -> Result<(StatusCode, Vec<u8>), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?.to_vec();
    Ok((status, bytes))
}

async fn delete_path(router: &Router, path: &str) -> Result<StatusCode, BoxError> {
    Ok(delete_with_body(router, path).await?.0)
}

async fn delete_with_body(router: &Router, path: &str) -> Result<(StatusCode, Vec<u8>), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().method("DELETE").uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?.to_vec();
    Ok((status, bytes))
}

#[tokio::test]
async fn an_empty_store_lists_with_no_releases_and_no_ledger_yet() -> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let router = router(root.path());

    let (status, body) = get_path(&router, "/api/v1/shared/packages").await?;

    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["store"], "shared");
    assert_eq!(json["ledger"]["state"], "absent");
    assert_eq!(json["releases"], serde_json::json!([]));
    Ok(())
}

#[tokio::test]
async fn installing_a_bundle_returns_201_with_a_location_header_and_the_identity_body() -> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let router = router(root.path());
    let bundle = build_bundle("sync-crm", "1.0.0")?;

    let (status, body, location) = post_bundle(&router, &bundle).await?;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        location.as_deref(),
        Some("/api/v1/shared/packages/sync-crm/versions/1.0.0")
    );
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["name"], "sync-crm");
    assert_eq!(json["version"], "1.0.0");
    Ok(())
}

#[tokio::test]
async fn an_installed_package_is_listed_and_inspectable() -> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let router = router(root.path());
    post_bundle(&router, &build_bundle("sync-crm", "1.0.0")?).await?;

    let (list_status, list_body) = get_path(&router, "/api/v1/shared/packages").await?;
    assert_eq!(list_status, StatusCode::OK);
    let list_json: serde_json::Value = serde_json::from_slice(&list_body)?;
    assert_eq!(list_json["releases"].as_array().map(Vec::len), Some(1));

    let (inspect_status, inspect_body) = get_path(&router, "/api/v1/shared/packages/sync-crm/versions/1.0.0").await?;
    assert_eq!(inspect_status, StatusCode::OK);
    let inspect_json: serde_json::Value = serde_json::from_slice(&inspect_body)?;
    assert_eq!(inspect_json["workflows"][0]["workflow"], "sync");
    assert!(inspect_json["libraries"].is_array(), "{inspect_json}");
    assert!(
        inspect_json.get("dependencies").is_none(),
        "the field was renamed: {inspect_json}"
    );
    Ok(())
}

#[tokio::test]
async fn removing_an_installed_package_returns_204_and_it_is_no_longer_listed() -> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let router = router(root.path());
    post_bundle(&router, &build_bundle("sync-crm", "1.0.0")?).await?;

    let delete_status = delete_path(&router, "/api/v1/shared/packages/sync-crm/versions/1.0.0").await?;
    assert_eq!(delete_status, StatusCode::NO_CONTENT);

    let (get_status, get_body) = get_path(&router, "/api/v1/shared/packages/sync-crm/versions/1.0.0").await?;
    assert_eq!(get_status, StatusCode::NOT_FOUND);
    let json: serde_json::Value = serde_json::from_slice(&get_body)?;
    assert_eq!(json["error"]["code"], "packages/not-found");
    Ok(())
}

#[tokio::test]
async fn installing_other_content_under_an_installed_identity_returns_409_with_a_duplicate_identity_error()
-> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let router = router(root.path());
    post_bundle(&router, &build_bundle("sync-crm", "1.0.0")?).await?;

    let (status, body, _location) = post_bundle(&router, &build_library("sync-crm", "1.0.0", &[])?).await?;

    assert_eq!(status, StatusCode::CONFLICT);
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["error"]["code"], "packages/duplicate-identity");
    Ok(())
}

#[tokio::test]
async fn installing_a_bundle_whose_version_steps_less_than_its_interface_changes_need_returns_422_naming_the_change()
-> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let router = router(root.path());
    let day = "inputs = [{ name = \"day\", kind = \"string\" }]";
    post_bundle(&router, &crate::retention_fixture::archive_declaring("1.0.0", day)?).await?;

    let (status, body, _location) =
        post_bundle(&router, &crate::retention_fixture::archive_declaring("1.0.1", "")?).await?;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["error"]["code"], "packages/version-step-invalid");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("input day removed"),
        "{json}"
    );
    Ok(())
}

#[tokio::test]
async fn installing_the_same_bundle_again_is_201_and_changes_nothing() -> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let router = router(root.path());
    let bundle = build_bundle("sync-crm", "1.0.0")?;
    post_bundle(&router, &bundle).await?;

    let (status, _body, _location) = post_bundle(&router, &bundle).await?;

    assert_eq!(status, StatusCode::CREATED);
    let (_status, listing) = get_path(&router, "/api/v1/shared/packages").await?;
    assert_eq!(json_of(&listing)?["releases"].as_array().map(Vec::len), Some(1));
    Ok(())
}

#[tokio::test]
async fn an_invalid_version_segment_returns_400_before_touching_the_store() -> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let router = router(root.path());

    let (status, body) = get_path(&router, "/api/v1/shared/packages/sync-crm/versions/not-a-version").await?;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["error"]["code"], "packages/invalid-version");
    Ok(())
}

#[tokio::test]
async fn a_multipart_request_with_no_field_is_rejected_as_a_missing_bundle() -> Result<(), BoxError> {
    const BOUNDARY: &str = "empty-boundary";
    let root = tempfile::tempdir()?;
    let router = router(root.path());

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/shared/packages")
                .header("content-type", format!("multipart/form-data; boundary={BOUNDARY}"))
                .body(Body::from(format!("--{BOUNDARY}--\r\n")))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["error"]["code"], "packages/missing-bundle");
    Ok(())
}

#[tokio::test]
async fn a_tampered_bundle_is_rejected_as_an_invalid_bundle() -> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let router = router(root.path());
    let mut bundle = build_bundle("sync-crm", "1.0.0")?;
    let middle = bundle.len() / 2;
    bundle[middle] ^= 0xFF;

    let (status, body, _location) = post_bundle(&router, &bundle).await?;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["error"]["code"], "packages/invalid-bundle");
    Ok(())
}

#[tokio::test]
async fn a_name_that_could_leave_the_store_is_refused_with_400_and_nothing_outside_it_is_touched()
-> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let store = root.path().join("store");
    let victim = root.path().join("victim").join("1.0.0");
    std::fs::create_dir_all(&store)?;
    std::fs::create_dir_all(&victim)?;
    std::fs::write(victim.join("manifest.toml"), "not a package")?;
    let router = router(&store);

    for path in [
        "/api/v1/shared/packages/..%2Fvictim/versions/1.0.0",
        "/api/v1/shared/packages/..%5Cvictim/versions/1.0.0",
        "/api/v1/shared/packages/Victim/versions/1.0.0",
    ] {
        let deleted = delete_path(&router, path).await?;
        let (read, body) = get_path(&router, path).await?;

        assert_eq!(deleted, StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(read, StatusCode::BAD_REQUEST, "{path}");
        let json: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(json["error"]["code"], "packages/invalid-name", "{path}");
    }
    assert!(
        victim.join("manifest.toml").exists(),
        "a folder outside the store was removed"
    );
    Ok(())
}

#[tokio::test]
async fn a_bundle_whose_manifest_names_a_path_is_refused_and_nothing_is_installed_outside_the_store()
-> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let store = root.path().join("store");
    std::fs::create_dir_all(&store)?;
    let router = router(&store);

    let (status, _body, _location) = post_bundle(&router, &build_bundle("../escape", "1.0.0")?).await?;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        !root.path().join("escape").exists(),
        "a release was placed outside the store"
    );
    Ok(())
}

fn json_of(body: &[u8]) -> Result<serde_json::Value, BoxError> {
    Ok(serde_json::from_slice(body)?)
}

async fn put_json(router: &Router, path: &str, body: &str) -> Result<(StatusCode, Vec<u8>), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))?,
        )
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?.to_vec();
    Ok((status, bytes))
}

const NIGHTLY: &str = "name = \"J\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";

/// A saved job `nightly` of package `acme`, workflow `sync`, whose file names the release `version`.
fn save_job(store: &std::path::Path, version: &str) -> Result<(), BoxError> {
    let directory = store.with_extension("jobs").join("acme").join("sync").join("jobs");
    std::fs::create_dir_all(&directory)?;
    std::fs::write(directory.join("nightly.toml"), NIGHTLY.replace("1.0.0", version))?;
    Ok(())
}

#[tokio::test]
async fn the_listing_shows_each_releases_exports_and_the_workflow_releases_that_hold_a_library() -> Result<(), BoxError>
{
    let root = tempfile::tempdir()?;
    let router = router(root.path());
    post_bundle(&router, &build_library("acme-text", "1.2.0", &[])?).await?;
    post_bundle(&router, &build_dependent("acme", "1.0.0", &[("acme-text", "^1")])?).await?;

    let (status, body) = get_path(&router, "/api/v1/shared/packages").await?;

    assert_eq!(status, StatusCode::OK);
    let json = json_of(&body)?;
    let releases = json["releases"].as_array().ok_or("no releases")?;
    let acme = releases
        .iter()
        .find(|release| release["name"] == "acme")
        .ok_or("no acme")?;
    let text = releases
        .iter()
        .find(|release| release["name"] == "acme-text")
        .ok_or("no acme-text")?;
    assert_eq!(text["exports"], serde_json::json!(["m.rhai"]));
    assert_eq!(
        acme["libraries"],
        serde_json::json!([{"name": "acme-text", "requirement": "^1"}])
    );
    assert_eq!(acme["workflows"][0]["kind"], "future");
    let reasons: Vec<&str> = text["kept_by"]
        .as_array()
        .ok_or("no reasons")?
        .iter()
        .filter_map(|reason| reason["reason"].as_str())
        .collect();
    assert_eq!(reasons, ["chosen", "newest-release"]);
    assert_eq!(text["condition"], "whole");
    Ok(())
}

#[tokio::test]
async fn a_release_that_cannot_be_read_is_listed_for_itself_and_the_others_are_listed() -> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let router = router(root.path());
    post_bundle(&router, &build_bundle("sync-crm", "1.0.0")?).await?;
    post_bundle(&router, &build_library("broken", "1.0.0", &[])?).await?;
    std::fs::write(
        root.path().join("broken").join("1.0.0").join("manifest.toml"),
        "this is not a manifest",
    )?;

    let (status, body) = get_path(&router, "/api/v1/shared/packages").await?;

    assert_eq!(status, StatusCode::OK);
    let json = json_of(&body)?;
    let releases = json["releases"].as_array().ok_or("no releases")?;
    assert_eq!(releases.len(), 2);
    let broken = releases
        .iter()
        .find(|release| release["name"] == "broken")
        .ok_or("no broken")?;
    assert_eq!(broken["condition"], "damaged");
    assert!(
        broken["problems"]
            .as_array()
            .is_some_and(|problems| !problems.is_empty()),
        "{json}"
    );
    let whole = releases
        .iter()
        .find(|release| release["name"] == "sync-crm")
        .ok_or("no sync-crm")?;
    assert_eq!(whole["condition"], "whole");
    Ok(())
}

#[tokio::test]
async fn installing_a_package_whose_library_is_not_installed_returns_422_naming_it_and_installs_nothing()
-> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let router = router(root.path());

    let (status, body, _) = post_bundle(&router, &build_dependent("acme", "1.0.0", &[("acme-text", "^1")])?).await?;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let json = json_of(&body)?;
    assert_eq!(json["error"]["code"], "packages/dependency-unsatisfied");
    assert!(
        json["error"]["message"]
            .as_str()
            .is_some_and(|text| text.contains("acme-text")),
        "{json}"
    );
    assert!(!root.path().join("acme").exists());
    Ok(())
}

#[tokio::test]
async fn deleting_a_release_a_saved_job_names_returns_409_in_use_naming_the_job_and_removes_nothing()
-> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let router = router(root.path());
    post_bundle(&router, &build_dependent("acme", "1.0.0", &[])?).await?;
    save_job(root.path(), "1.0.0")?;

    let (status, body) = delete_with_body(&router, "/api/v1/shared/packages/acme/versions/1.0.0").await?;

    assert_eq!(status, StatusCode::CONFLICT);
    let json = json_of(&body)?;
    assert_eq!(json["error"]["code"], "packages/in-use");
    assert!(
        json["error"]["message"]
            .as_str()
            .is_some_and(|text| text.contains("job nightly of sync names it")),
        "{json}"
    );
    assert_eq!(json["error"]["reasons"][0]["job"], "nightly");
    assert!(root.path().join("acme").join("1.0.0").is_dir());
    Ok(())
}

#[tokio::test]
async fn the_forced_removal_removes_it_and_names_what_was_overridden_and_the_jobs_the_next_pass_moves()
-> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let router = router(root.path());
    post_bundle(&router, &build_dependent("acme", "1.0.0", &[])?).await?;
    save_job(root.path(), "1.0.0")?;
    // The job is recorded as enabled, so the next validation pass has a job to move.
    std::fs::create_dir_all(root.path().with_extension("state"))?;
    std::fs::write(
        root.path().with_extension("state").join("shared-jobs.toml"),
        "[acme.sync]\ncurrent = \"1.0.0\"\n\n[acme.sync.nightly]\nlifecycle = \"enabled\"\nrelease = \"1.0.0\"\n",
    )?;

    let (status, body) = put_json(
        &router,
        "/api/v1/shared/packages/acme/versions/1.0.0/execute/remove",
        r#"{"force":true}"#,
    )
    .await?;

    assert_eq!(status, StatusCode::OK);
    let json = json_of(&body)?;
    assert_eq!(json["cause"], "forced");
    assert_eq!(json["store"], "shared");
    assert!(
        json["overridden"]
            .as_array()
            .is_some_and(|reasons| reasons.iter().any(|reason| reason["reason"] == "current")),
        "{json}"
    );
    assert_eq!(json["affected_jobs"][0]["job"], "nightly");
    assert_eq!(json["affected_jobs"][0]["lifecycle"], "enabled");
    assert!(!root.path().join("acme").join("1.0.0").exists());
    Ok(())
}

#[tokio::test]
async fn the_remove_command_without_force_is_the_guarded_removal() -> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let router = router(root.path());
    post_bundle(&router, &build_dependent("acme", "1.0.0", &[])?).await?;
    save_job(root.path(), "1.0.0")?;

    let (empty, _) = put_json(
        &router,
        "/api/v1/shared/packages/acme/versions/1.0.0/execute/remove",
        "",
    )
    .await?;
    let (explicit, _) = put_json(
        &router,
        "/api/v1/shared/packages/acme/versions/1.0.0/execute/remove",
        r#"{"force":false}"#,
    )
    .await?;

    assert_eq!((empty, explicit), (StatusCode::CONFLICT, StatusCode::CONFLICT));
    assert!(root.path().join("acme").join("1.0.0").is_dir());
    Ok(())
}

#[tokio::test]
async fn a_remove_body_with_an_unknown_or_malformed_field_is_a_400_and_removes_nothing() -> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let router = router(root.path());
    post_bundle(&router, &build_bundle("acme", "1.0.0")?).await?;
    let path = "/api/v1/shared/packages/acme/versions/1.0.0/execute/remove";

    for body in [r#"{"force":"yes"}"#, r#"{"hard":true}"#, "not json"] {
        let (status, answer) = put_json(&router, path, body).await?;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(json_of(&answer)?["error"]["code"], "packages/invalid-request", "{body}");
    }
    assert!(root.path().join("acme").join("1.0.0").is_dir());
    Ok(())
}

#[tokio::test]
async fn deleting_a_release_no_rule_keeps_is_204_and_a_query_string_is_a_400_with_a_json_error() -> Result<(), BoxError>
{
    let root = tempfile::tempdir()?;
    let router = router(root.path());
    post_bundle(&router, &build_bundle("acme", "1.0.0")?).await?;

    let (forced_by_query, body) =
        delete_with_body(&router, "/api/v1/shared/packages/acme/versions/1.0.0?force=true").await?;
    let (removed, empty) = delete_with_body(&router, "/api/v1/shared/packages/acme/versions/1.0.0").await?;

    assert_eq!(forced_by_query, StatusCode::BAD_REQUEST);
    assert_eq!(json_of(&body)?["error"]["code"], "packages/invalid-query");
    assert_eq!(removed, StatusCode::NO_CONTENT);
    assert!(empty.is_empty(), "a 204 carries no body");
    Ok(())
}

#[tokio::test]
async fn deleting_a_release_that_is_not_there_is_404_and_a_ledger_that_cannot_be_read_is_409() -> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let router = router(root.path());
    post_bundle(&router, &build_bundle("acme", "1.0.0")?).await?;

    let (missing, body) = delete_with_body(&router, "/api/v1/shared/packages/acme/versions/9.0.0").await?;
    std::fs::write(root.path().join("ledger.toml"), "this is not a ledger")?;
    let (unreadable, answer) = delete_with_body(&router, "/api/v1/shared/packages/acme/versions/1.0.0").await?;

    assert_eq!(missing, StatusCode::NOT_FOUND);
    assert_eq!(json_of(&body)?["error"]["code"], "packages/not-found");
    assert_eq!(unreadable, StatusCode::CONFLICT);
    assert_eq!(json_of(&answer)?["error"]["code"], "packages/ledger-unreadable");
    assert!(root.path().join("acme").join("1.0.0").is_dir());
    Ok(())
}

#[tokio::test]
async fn a_state_file_that_cannot_be_read_is_a_500_with_its_own_code_and_no_path() -> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let router = router(root.path());
    post_bundle(&router, &build_bundle("acme", "1.0.0")?).await?;
    std::fs::create_dir_all(root.path().with_extension("state"))?;
    std::fs::write(
        root.path().with_extension("state").join("shared-jobs.toml"),
        "this is = not [valid",
    )?;

    let (listed, body) = get_path(&router, "/api/v1/shared/packages").await?;

    assert_eq!(listed, StatusCode::INTERNAL_SERVER_ERROR);
    let json = json_of(&body)?;
    assert_eq!(json["error"]["code"], "packages/state-unreadable");
    let message = json["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("fix or restore the job state"), "{message}");
    assert!(
        !message.contains("shared-jobs.toml") && !message.contains("not [valid"),
        "{message}"
    );
    Ok(())
}

#[tokio::test]
async fn an_install_whose_retention_cannot_finish_still_returns_201_with_the_warning() -> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let router = router(root.path());
    post_bundle(&router, &build_bundle("acme", "1.0.0")?).await?;
    // A file where the jobs folder belongs, so what names the installed releases cannot be read.
    std::fs::write(root.path().with_extension("jobs"), b"not a folder")?;

    let (status, body, _) = post_bundle(&router, &build_bundle("acme", "1.1.0")?).await?;

    assert_eq!(status, StatusCode::CREATED);
    let json = json_of(&body)?;
    assert_eq!(json["warnings"].as_array().map(Vec::len), Some(1), "{json}");
    assert_eq!(json["warnings"][0]["code"], "packages/retention-failed");
    assert!(
        json["warnings"][0]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty())
    );
    Ok(())
}

#[tokio::test]
async fn an_install_with_nothing_to_warn_of_has_no_warnings_field() -> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let router = router(root.path());

    let (_, body, _) = post_bundle(&router, &build_bundle("acme", "1.0.0")?).await?;

    assert!(json_of(&body)?.get("warnings").is_none());
    Ok(())
}

fn removal_report(
    warnings: Vec<crate::store_retention::RetentionWarning>,
    retention_removed: Vec<contextos_workflows::ReleaseRef>,
) -> Result<crate::package_store_report::RemovalReport, BoxError> {
    let release = contextos_workflows::ReleaseRef {
        name: contextos_workflows::PackageName::try_from("acme")?,
        version: semver::Version::new(1, 0, 0),
    };
    Ok(crate::package_store_report::RemovalReport {
        store: "shared".to_owned(),
        removal: crate::package_removal::Removal {
            release,
            cause: contextos_packages::RecordedCause::ByRequest,
            overridden: Vec::new(),
            affected_jobs: Vec::new(),
        },
        warnings,
        retention_removed,
    })
}

async fn body_of(response: axum::response::Response) -> Result<(StatusCode, Vec<u8>), BoxError> {
    let status = response.status();
    Ok((status, to_bytes(response.into_body(), usize::MAX).await?.to_vec()))
}

#[tokio::test]
async fn a_delete_that_finished_with_nothing_to_report_answers_204_with_no_body() -> Result<(), BoxError> {
    let (status, body) = body_of(super::deleted_response(Ok(removal_report(Vec::new(), Vec::new())?))).await?;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(body, Vec::<u8>::new());
    Ok(())
}

#[tokio::test]
async fn a_delete_after_which_retention_removed_more_answers_200_with_the_removal_as_a_document() -> Result<(), BoxError>
{
    let gone = contextos_workflows::ReleaseRef {
        name: contextos_workflows::PackageName::try_from("text")?,
        version: semver::Version::new(1, 0, 0),
    };

    let (status, body) = body_of(super::deleted_response(Ok(removal_report(Vec::new(), vec![gone])?))).await?;

    assert_eq!(status, StatusCode::OK);
    let json = json_of(&body)?;
    assert_eq!(json["name"], "acme");
    assert_eq!(json["retention_removed"][0]["name"], "text");
    assert_eq!(json["retention_removed"][0]["version"], "1.0.0");
    Ok(())
}

#[tokio::test]
async fn a_delete_whose_retention_could_not_finish_answers_200_with_the_warning() -> Result<(), BoxError> {
    let warning = crate::store_retention::RetentionWarning::raised_for_report("the jobs folder could not be listed");

    let (status, body) = body_of(super::deleted_response(Ok(removal_report(vec![warning], Vec::new())?))).await?;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_of(&body)?["warnings"][0]["code"], "packages/retention-failed");
    Ok(())
}

#[tokio::test]
async fn deleting_a_future_release_that_alone_kept_a_library_release_answers_200_naming_what_retention_removed()
-> Result<(), BoxError> {
    use crate::retention_fixture::{archive_needing, library_archive, put_without_retention};
    let root = tempfile::tempdir()?;
    for archive in [
        library_archive("text", "1.0.0")?,
        library_archive("text", "1.1.0")?,
        archive_needing("1.2.0", "text", "^1")?,
    ] {
        put_without_retention(root.path(), &archive)?;
    }
    std::fs::create_dir_all(root.path().with_extension("state"))?;
    std::fs::write(
        root.path().with_extension("state").join("shared-jobs.toml"),
        "[crm.sync]\nfuture = \"1.2.0\"\n\n[crm.sync.\"1.2.0\".lock.text]\nversion = \"1.0.0\"\ndigest = \"sha256:aaaa\"\n",
    )?;
    let router = router(root.path());

    let (status, body) = delete_with_body(&router, "/api/v1/shared/packages/crm/versions/1.2.0").await?;

    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let json = json_of(&body)?;
    assert_eq!(json["retention_removed"][0]["name"], "text");
    assert_eq!(json["retention_removed"][0]["version"], "1.0.0");
    assert!(!root.path().join("text").join("1.0.0").exists());
    assert!(!root.path().join("crm").join("1.2.0").exists());
    Ok(())
}

#[tokio::test]
async fn a_delete_while_another_operation_holds_the_store_is_409_store_locked() -> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let router = router(root.path());
    post_bundle(&router, &build_bundle("acme", "1.0.0")?).await?;
    let store = crate::host_store::HostStore::new(root.path());
    let held = store.lock()?;

    let (status, body) = delete_with_body(&router, "/api/v1/shared/packages/acme/versions/1.0.0").await?;
    let (forced, answer) = put_json(
        &router,
        "/api/v1/shared/packages/acme/versions/1.0.0/execute/remove",
        r#"{"force":true}"#,
    )
    .await?;

    drop(held);
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(json_of(&body)?["error"]["code"], "packages/store-locked");
    assert_eq!(forced, StatusCode::CONFLICT);
    assert_eq!(json_of(&answer)?["error"]["code"], "packages/store-locked");
    assert!(root.path().join("acme").join("1.0.0").is_dir());
    Ok(())
}
