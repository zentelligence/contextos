//! Credential-store garbage collection on `[[mcp_server]]` removal/replacement: a real
//! `contextos-mcp` session backs the primary server (the registered-app-dependency check a
//! `DELETE`/name-removing `PUT` runs needs one, per `settings_routes.rs`'s identical precedent),
//! while the entry actually under test is an HTTP `[mcp_server.auth]` entry that is never itself
//! connected — exactly the state a freshly hand-edited or not-yet-restarted `web.toml` entry is
//! already in today, and irrelevant to the dependency check, which only reads registered apps'
//! own manifests. No mocking of the credential store itself, per `testing.md`.

mod support;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use contextos_web::McpClientSet;
use contextos_web::credential_store::{CredentialRecord, CredentialStore, FsCredentialStore};
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

/// `web.toml` naming one real, connectable `contextos-mcp` stdio session (`"contextos"`, the
/// primary server the dependency check runs its `vault_info` call against) plus whatever
/// additional `[[mcp_server]]` text `extra_entries_toml` supplies verbatim: never connected by
/// [`McpClientSet::connect`] itself, matching a freshly hand-edited or not-yet-restarted entry.
async fn router_with_unconnected_extra_entries(
    vault_dir: &Path,
    dir: &Path,
    extra_entries_toml: &str,
) -> Result<(Router, std::path::PathBuf, Arc<FsCredentialStore>), BoxError> {
    let config_path = support::write_vault_config(dir, vault_dir)?;
    let contextos_binary = support::contextos_mcp_binary()?;
    #[allow(clippy::unnecessary_debug_formatting)]
    let config_path_value = format!("{config_path:?}");
    let command_value = contextos_binary.to_string_lossy();
    let web_config_path = dir.join("web.toml");
    std::fs::write(
        &web_config_path,
        format!(
            "[[mcp_server]]\nname = \"contextos\"\ntransport = \"stdio\"\ncommand = {command_value:?}\n\
             args = [\"--config\", {config_path_value}]\n\n{extra_entries_toml}"
        ),
    )?;
    let contextos_entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[contextos_entry]).await?);
    let router = contextos_web::build_router(clients, Some(dir), &web_config_path);
    let credential_store = Arc::new(FsCredentialStore::new(dir.join("credentials")));
    Ok((router, web_config_path, credential_store))
}

async fn delete(router: &Router, name: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/server/settings/mcp-servers/{name}"))
                .body(Body::empty())?,
        )
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}

async fn put(router: &Router, body: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri("/server/settings/")
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))?,
        )
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}

const BEARER_ENTRY: &str = "[[mcp_server]]\nname = \"bearer-server\"\ntransport = \"http\"\n\
     endpoint = \"https://services.example-provider.invalid/mcp/\"\n\n\
     [mcp_server.auth]\nmode = \"bearer\"\ncredential_ref = \"bearer-server-token\"\n";

const OAUTH2_ENTRY: &str = "[[mcp_server]]\nname = \"hosted-oauth\"\ntransport = \"http\"\n\
     endpoint = \"https://mcp.example-provider.invalid/mcp\"\n\n\
     [mcp_server.auth]\nmode = \"oauth2\"\nclient_id = \"some-client-id\"\n\
     client_secret_ref = \"hosted-oauth-client-secret\"\n\
     authorization_endpoint = \"https://example-provider.invalid/authorize\"\n\
     token_endpoint = \"https://example-provider.invalid/token\"\n\
     scopes = [\"mcp.tools.call\"]\ncredential_ref = \"hosted-oauth-token\"\n";

#[tokio::test]
async fn deleting_a_bearer_mode_entry_removes_its_credential_record() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let (router, _web_config_path, store) =
        router_with_unconnected_extra_entries(vault_dir.path(), dir.path(), BEARER_ENTRY).await?;
    store.set(
        "bearer-server-token",
        &CredentialRecord::Bearer {
            value: "some-pasted-token".to_owned(),
            updated_at: "2026-01-01T00:00:00Z".to_owned(),
        },
    )?;

    let (status, body) = delete(&router, "bearer-server").await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(store.get("bearer-server-token")?.is_none());
    Ok(())
}

#[tokio::test]
async fn deleting_an_oauth2_mode_entry_removes_both_its_token_and_client_secret_records() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let (router, _web_config_path, store) =
        router_with_unconnected_extra_entries(vault_dir.path(), dir.path(), OAUTH2_ENTRY).await?;
    store.set(
        "hosted-oauth-token",
        &CredentialRecord::Oauth2Token {
            access_token: "some-access-token".to_owned(),
            refresh_token: Some("some-refresh-token".to_owned()),
            expires_at: "2026-01-01T00:00:00Z".to_owned(),
            obtained_at: "2026-01-01T00:00:00Z".to_owned(),
        },
    )?;
    store.set(
        "hosted-oauth-client-secret",
        &CredentialRecord::Oauth2ClientSecret {
            value: "some-client-secret".to_owned(),
            updated_at: "2026-01-01T00:00:00Z".to_owned(),
        },
    )?;

    let (status, body) = delete(&router, "hosted-oauth").await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(store.get("hosted-oauth-token")?.is_none());
    assert!(store.get("hosted-oauth-client-secret")?.is_none());
    Ok(())
}

#[tokio::test]
async fn a_put_replacing_an_entrys_credential_ref_removes_only_the_old_reference() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let (router, _web_config_path, store) =
        router_with_unconnected_extra_entries(vault_dir.path(), dir.path(), BEARER_ENTRY).await?;
    store.set(
        "bearer-server-token",
        &CredentialRecord::Bearer {
            value: "old-token".to_owned(),
            updated_at: "2026-01-01T00:00:00Z".to_owned(),
        },
    )?;

    // Keeps the same `name` (so this is not a removal at all: `dependent_apps` never runs), but
    // changes `credential_ref` to a value nothing has ever populated.
    let (status, body) = put(
        &router,
        r#"{
            "current_name": "bearer-server",
            "transport": "http",
            "name": "bearer-server",
            "endpoint": "https://services.example-provider.invalid/mcp/",
            "auth": {"mode": "bearer", "credential_ref": "bearer-server-token-v2"}
        }"#,
    )
    .await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        store.get("bearer-server-token")?.is_none(),
        "the old, now-unreferenced credential must be garbage-collected"
    );
    assert!(
        store.get("bearer-server-token-v2")?.is_none(),
        "the new reference was never populated by this edit and must not be conjured into existence"
    );
    Ok(())
}

#[tokio::test]
async fn a_put_keeping_the_same_credential_ref_does_not_delete_it() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let (router, _web_config_path, store) =
        router_with_unconnected_extra_entries(vault_dir.path(), dir.path(), BEARER_ENTRY).await?;
    let record = CredentialRecord::Bearer {
        value: "still-valid".to_owned(),
        updated_at: "2026-01-01T00:00:00Z".to_owned(),
    };
    store.set("bearer-server-token", &record)?;

    // Same `credential_ref`, but a different `endpoint`: proves the diff is keyed on the
    // reference itself, not on whether the surrounding entry changed at all.
    let (status, body) = put(
        &router,
        r#"{
            "current_name": "bearer-server",
            "transport": "http",
            "name": "bearer-server",
            "endpoint": "https://services.example-provider.invalid/mcp/v2/",
            "auth": {"mode": "bearer", "credential_ref": "bearer-server-token"}
        }"#,
    )
    .await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        store.get("bearer-server-token")?,
        Some(record),
        "a credential_ref that still appears in the after document must never be deleted"
    );
    Ok(())
}

#[tokio::test]
async fn a_rejected_edit_deletes_no_credential_at_all() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let (router, web_config_path, store) =
        router_with_unconnected_extra_entries(vault_dir.path(), dir.path(), BEARER_ENTRY).await?;
    let record = CredentialRecord::Bearer {
        value: "still-valid".to_owned(),
        updated_at: "2026-01-01T00:00:00Z".to_owned(),
    };
    store.set("bearer-server-token", &record)?;
    let before = std::fs::read_to_string(&web_config_path)?;

    // A duplicate `name` ("contextos" already exists): rejected before anything is persisted.
    let (status, body) = put(
        &router,
        r#"{
            "current_name": "bearer-server",
            "transport": "http",
            "name": "contextos",
            "endpoint": "https://services.example-provider.invalid/mcp/",
            "auth": {"mode": "bearer", "credential_ref": "some-other-ref"}
        }"#,
    )
    .await?;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(std::fs::read_to_string(&web_config_path)?, before);
    assert_eq!(
        store.get("bearer-server-token")?,
        Some(record),
        "a rejected edit must delete nothing, since web.toml itself was never written"
    );
    Ok(())
}
