use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt as _;

use super::*;
use crate::app::build_router;
use crate::credential_store::{CredentialRecord, CredentialStore, FsCredentialStore};
use crate::mcp_client::McpClientSet;

type BoxError = Box<dyn std::error::Error>;

const BASE: &str = r#"
[server]
bind = "127.0.0.1:7373"

[[mcp_server]]
name = "contextos"
transport = "stdio"
command = "contextos-mcp"
args = ["--config", "config.toml"]
"#;

const PASTED_ENTRY: &str = r#"
[[mcp_server]]
name = "remote"
transport = "http"
endpoint = "http://127.0.0.1:9000"

[mcp_server.auth]
mode = "bearer"
credential_ref = "remote-token"
"#;

const SECRET: &str = "pit-pasted-secret-value";

async fn router_with_web_toml(contents: &str) -> Result<(tempfile::TempDir, axum::Router), BoxError> {
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("web.toml"), contents)?;
    // No `[[mcp_server]]` here is ever connected: these cases only read and write web.toml and the credential store.
    let clients = Arc::new(McpClientSet::connect(&[]).await?);
    let router = build_router(clients, Some(dir.path()), &dir.path().join("web.toml"));
    Ok((dir, router))
}

fn store(dir: &tempfile::TempDir) -> FsCredentialStore {
    FsCredentialStore::new(dir.path().join("credentials"))
}

fn bearer(value: &str) -> CredentialRecord {
    CredentialRecord::Bearer {
        value: value.to_owned(),
        updated_at: "2026-10-27T00:00:00Z".to_owned(),
    }
}

struct Answer {
    status: StatusCode,
    cache_control: Option<String>,
    body: String,
}

async fn send(router: &axum::Router, request: Request<Body>) -> Result<Answer, BoxError> {
    let response = router.clone().oneshot(request).await?;
    let status = response.status();
    let cache_control = response
        .headers()
        .get("cache-control")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok(Answer {
        status,
        cache_control,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    })
}

async fn post(router: &axum::Router, body: &str) -> Result<Answer, BoxError> {
    send(
        router,
        Request::builder()
            .method(Method::POST)
            .uri("/server/settings/")
            .header("content-type", "application/json")
            .body(Body::from(body.to_owned()))?,
    )
    .await
}

async fn secret(router: &axum::Router, uri: &str) -> Result<Answer, BoxError> {
    send(
        router,
        Request::builder()
            .uri(uri)
            .header("hx-request", "true")
            .body(Body::empty())?,
    )
    .await
}

#[test]
fn every_secret_field_round_trips_through_its_route_segment() {
    for field in [
        SecretField::BearerToken,
        SecretField::BearerTokenVariable,
        SecretField::ClientSecret,
        SecretField::ClientSecretVariable,
    ] {
        assert_eq!(SecretField::try_from(field.segment()), Ok(field));
    }
    assert_eq!(SecretField::try_from("password"), Err(UnknownSecretField));
}

#[tokio::test]
async fn adding_an_http_server_with_a_pasted_token_keeps_the_token_out_of_web_toml() -> Result<(), BoxError> {
    let (dir, router) = router_with_web_toml(BASE).await?;

    let answer = post(
        &router,
        &format!(
            r#"{{"transport":"http","name":"remote","endpoint":"http://127.0.0.1:9000","auth":{{"mode":"bearer","credential_ref":"remote-token","credential_value":"{SECRET}"}}}}"#
        ),
    )
    .await?;

    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    let web_toml = std::fs::read_to_string(dir.path().join("web.toml"))?;
    assert!(web_toml.contains("credential_ref = \"remote-token\""), "{web_toml}");
    assert!(!web_toml.contains(SECRET), "{web_toml}");
    assert!(
        matches!(store(&dir).get("remote-token")?, Some(CredentialRecord::Bearer { value, .. }) if value == SECRET),
        "the pasted token is stored under its reference"
    );
    assert!(!answer.body.contains(SECRET), "the page never shows the token unasked");
    Ok(())
}

#[tokio::test]
async fn adding_an_http_server_with_a_token_variable_writes_only_its_name() -> Result<(), BoxError> {
    let (dir, router) = router_with_web_toml(BASE).await?;

    let answer = post(
        &router,
        r#"{"transport":"http","name":"remote","endpoint":"http://127.0.0.1:9000","auth":{"mode":"bearer","token_env":"REMOTE_TOKEN"}}"#,
    )
    .await?;

    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    let web_toml = std::fs::read_to_string(dir.path().join("web.toml"))?;
    assert!(web_toml.contains("token_env = \"REMOTE_TOKEN\""), "{web_toml}");
    assert!(
        !dir.path().join("credentials").exists(),
        "nothing is stored for a variable"
    );
    assert!(answer.body.contains("token via $REMOTE_TOKEN"), "{}", answer.body);
    Ok(())
}

#[tokio::test]
async fn a_pasted_token_with_no_reference_to_store_it_under_is_refused_and_nothing_is_written() -> Result<(), BoxError>
{
    let (dir, router) = router_with_web_toml(BASE).await?;

    let answer = post(
        &router,
        &format!(
            r#"{{"transport":"http","name":"remote","endpoint":"http://127.0.0.1:9000","auth":{{"mode":"bearer","credential_value":"{SECRET}"}}}}"#
        ),
    )
    .await?;

    assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{}", answer.body);
    assert!(!answer.body.contains(SECRET), "{}", answer.body);
    assert_eq!(std::fs::read_to_string(dir.path().join("web.toml"))?, BASE);
    assert!(!dir.path().join("credentials").exists());
    Ok(())
}

#[tokio::test]
async fn the_add_form_offers_a_token_variable_or_a_pasted_token_side_by_side() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let page = secret(&router, "/server/settings/").await?;

    assert!(page.body.contains(r#"id="mcp-server-add-token-env""#), "{}", page.body);
    assert!(
        page.body.contains(r#"id="mcp-server-add-token-paste""#),
        "{}",
        page.body
    );
    assert!(!page.body.contains("never the token itself"), "{}", page.body);
    Ok(())
}

#[tokio::test]
async fn the_settings_page_shows_a_stored_token_obscured_with_a_show_control() -> Result<(), BoxError> {
    let (dir, router) = router_with_web_toml(&(BASE.to_owned() + PASTED_ENTRY)).await?;
    store(&dir).set("remote-token", &bearer(SECRET))?;

    let page = secret(&router, "/server/settings/").await?;

    assert!(!page.body.contains(SECRET), "{}", page.body);
    assert!(
        page.body
            .contains(r#"hx-get="/server/settings/mcp-servers/remote/secrets/bearer-token""#),
        "{}",
        page.body
    );
    Ok(())
}

#[tokio::test]
async fn revealing_a_stored_token_answers_with_its_value_and_is_never_cached() -> Result<(), BoxError> {
    let (dir, router) = router_with_web_toml(&(BASE.to_owned() + PASTED_ENTRY)).await?;
    store(&dir).set("remote-token", &bearer(SECRET))?;

    let answer = secret(&router, "/server/settings/mcp-servers/remote/secrets/bearer-token").await?;

    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert!(answer.body.contains(SECRET), "{}", answer.body);
    assert!(
        answer
            .body
            .contains("/server/settings/mcp-servers/remote/secrets/bearer-token?view=obscured"),
        "the revealed value offers Hide: {}",
        answer.body
    );
    assert_eq!(answer.cache_control.as_deref(), Some("no-store"));
    Ok(())
}

#[tokio::test]
async fn hiding_a_token_answers_the_obscured_control_without_its_value() -> Result<(), BoxError> {
    let (dir, router) = router_with_web_toml(&(BASE.to_owned() + PASTED_ENTRY)).await?;
    store(&dir).set("remote-token", &bearer(SECRET))?;

    let answer = secret(
        &router,
        "/server/settings/mcp-servers/remote/secrets/bearer-token?view=obscured",
    )
    .await?;

    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert!(!answer.body.contains(SECRET), "{}", answer.body);
    assert!(
        answer
            .body
            .contains(r#"hx-get="/server/settings/mcp-servers/remote/secrets/bearer-token""#),
        "{}",
        answer.body
    );
    Ok(())
}

#[tokio::test]
async fn revealing_a_token_needs_the_pages_own_request() -> Result<(), BoxError> {
    let (dir, router) = router_with_web_toml(&(BASE.to_owned() + PASTED_ENTRY)).await?;
    store(&dir).set("remote-token", &bearer(SECRET))?;

    let answer = send(
        &router,
        Request::builder()
            .uri("/server/settings/mcp-servers/remote/secrets/bearer-token")
            .body(Body::empty())?,
    )
    .await?;

    assert_eq!(answer.status, StatusCode::FORBIDDEN);
    assert!(answer.body.contains("settings/not-from-page"), "{}", answer.body);
    assert!(!answer.body.contains(SECRET));
    Ok(())
}

#[tokio::test]
async fn revealing_a_token_variable_shows_its_current_value() -> Result<(), BoxError> {
    // Cargo sets CARGO_PKG_NAME for every test binary it runs, and a test cannot set a variable of its own without
    // `unsafe`, so this names one that is always present.
    let (_dir, router) = router_with_web_toml(
        &(BASE.to_owned()
            + "\n[[mcp_server]]\nname = \"remote\"\ntransport = \"http\"\nendpoint = \"http://127.0.0.1:9000\"\n\
               [mcp_server.auth]\nmode = \"bearer\"\ntoken_env = \"CARGO_PKG_NAME\"\n"),
    )
    .await?;

    let answer = secret(
        &router,
        "/server/settings/mcp-servers/remote/secrets/bearer-token-variable",
    )
    .await?;

    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert!(answer.body.contains(env!("CARGO_PKG_NAME")), "{}", answer.body);
    assert_eq!(answer.cache_control.as_deref(), Some("no-store"));
    Ok(())
}

#[tokio::test]
async fn revealing_an_unset_token_variable_says_it_is_not_set() -> Result<(), BoxError> {
    let variable = "CONTEXTOS_WEB_TEST_UNSET_REVEAL_VARIABLE";
    assert!(
        std::env::var_os(variable).is_none(),
        "test precondition: {variable} is unset"
    );
    let (_dir, router) = router_with_web_toml(&format!(
        "{BASE}\n[[mcp_server]]\nname = \"remote\"\ntransport = \"http\"\nendpoint = \"http://127.0.0.1:9000\"\n\
         token_env = \"{variable}\"\n"
    ))
    .await?;

    let answer = secret(
        &router,
        "/server/settings/mcp-servers/remote/secrets/bearer-token-variable",
    )
    .await?;

    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert!(answer.body.contains("is not set"), "{}", answer.body);
    Ok(())
}

#[tokio::test]
async fn revealing_an_oauth_client_secret_answers_with_its_value() -> Result<(), BoxError> {
    let (dir, router) = router_with_web_toml(
        &(BASE.to_owned()
            + "\n[[mcp_server]]\nname = \"remote\"\ntransport = \"http\"\nendpoint = \"http://127.0.0.1:9000\"\n\
               [mcp_server.auth]\nmode = \"oauth2\"\nclient_id = \"abc\"\nclient_secret_ref = \"remote-client-secret\"\n\
               authorization_endpoint = \"https://provider.invalid/authorize\"\n\
               token_endpoint = \"https://provider.invalid/token\"\nscopes = [\"mcp\"]\n\
               credential_ref = \"remote-oauth-token\"\n"),
    )
    .await?;
    store(&dir).set(
        "remote-client-secret",
        &CredentialRecord::Oauth2ClientSecret {
            value: SECRET.to_owned(),
            updated_at: "2026-10-27T00:00:00Z".to_owned(),
        },
    )?;

    let page = secret(&router, "/server/settings/").await?;
    let answer = secret(&router, "/server/settings/mcp-servers/remote/secrets/client-secret").await?;

    assert!(!page.body.contains(SECRET), "{}", page.body);
    assert!(
        page.body
            .contains(r#"hx-get="/server/settings/mcp-servers/remote/secrets/client-secret""#),
        "{}",
        page.body
    );
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert!(answer.body.contains(SECRET), "{}", answer.body);
    Ok(())
}

#[tokio::test]
async fn revealing_a_secret_the_entry_does_not_have_is_a_404() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(&(BASE.to_owned() + PASTED_ENTRY)).await?;

    let not_stored = secret(&router, "/server/settings/mcp-servers/remote/secrets/bearer-token").await?;
    let no_variable = secret(
        &router,
        "/server/settings/mcp-servers/remote/secrets/bearer-token-variable",
    )
    .await?;
    let unknown_field = secret(&router, "/server/settings/mcp-servers/remote/secrets/password").await?;
    let unknown_server = secret(&router, "/server/settings/mcp-servers/elsewhere/secrets/bearer-token").await?;

    assert_eq!(not_stored.status, StatusCode::NOT_FOUND);
    assert!(
        not_stored.body.contains("settings/secret-not-configured"),
        "{}",
        not_stored.body
    );
    assert_eq!(no_variable.status, StatusCode::NOT_FOUND);
    assert!(
        no_variable.body.contains("settings/secret-not-configured"),
        "{}",
        no_variable.body
    );
    assert_eq!(unknown_field.status, StatusCode::NOT_FOUND);
    assert!(
        unknown_field.body.contains("settings/unknown-secret"),
        "{}",
        unknown_field.body
    );
    assert_eq!(unknown_server.status, StatusCode::NOT_FOUND);
    assert!(
        unknown_server.body.contains("settings/unknown-mcp-server"),
        "{}",
        unknown_server.body
    );
    Ok(())
}

#[tokio::test]
async fn show_and_hide_never_carry_the_edit_forms_values() -> Result<(), BoxError> {
    // Each control sits inside the edit form, whose `hx-vals` builds the whole patch, typed-in secrets included.
    // htmx merges every ancestor's `hx-vals` into a request, so each button must stop that or a GET would carry
    // the form's contents in its URL.
    let (dir, router) = router_with_web_toml(&(BASE.to_owned() + PASTED_ENTRY)).await?;
    store(&dir).set("remote-token", &bearer(SECRET))?;

    let page = secret(&router, "/server/settings/").await?;
    let revealed = secret(&router, "/server/settings/mcp-servers/remote/secrets/bearer-token").await?;

    for (name, html) in [("page", &page.body), ("revealed", &revealed.body)] {
        let buttons = html
            .matches("/server/settings/mcp-servers/remote/secrets/bearer-token")
            .count();
        assert!(buttons > 0, "{name}: {html}");
        assert_eq!(
            html.matches(r#"hx-vals="unset""#).count(),
            buttons,
            "{name}: every secret control stops inheriting hx-vals: {html}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn revealing_a_token_from_a_page_naming_another_host_is_refused() -> Result<(), BoxError> {
    let (dir, router) = router_with_web_toml(&(BASE.to_owned() + PASTED_ENTRY)).await?;
    store(&dir).set("remote-token", &bearer(SECRET))?;

    let answer = send(
        &router,
        Request::builder()
            .uri("/server/settings/mcp-servers/remote/secrets/bearer-token")
            .header("hx-request", "true")
            .header("host", "attacker.example:7373")
            .body(Body::empty())?,
    )
    .await?;

    assert_eq!(answer.status, StatusCode::FORBIDDEN);
    assert!(answer.body.contains("route/host-not-allowed"), "{}", answer.body);
    assert!(!answer.body.contains(SECRET));
    Ok(())
}

#[tokio::test]
async fn adding_a_server_with_both_a_token_variable_and_a_pasted_token_is_refused_naming_both() -> Result<(), BoxError>
{
    let (dir, router) = router_with_web_toml(BASE).await?;

    let answer = post(
        &router,
        &format!(
            r#"{{"transport":"http","name":"remote","endpoint":"http://127.0.0.1:9000","auth":{{"mode":"bearer","token_env":"REMOTE_TOKEN","credential_ref":"remote-token","credential_value":"{SECRET}"}}}}"#
        ),
    )
    .await?;

    assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{}", answer.body);
    assert!(
        answer.body.contains("settings/pasted-secret-refused"),
        "{}",
        answer.body
    );
    assert!(answer.body.contains("token_env"), "{}", answer.body);
    assert!(!answer.body.contains(SECRET), "{}", answer.body);
    assert_eq!(std::fs::read_to_string(dir.path().join("web.toml"))?, BASE);
    Ok(())
}

#[tokio::test]
async fn a_pasted_token_the_credential_store_cannot_write_is_reported_not_hidden() -> Result<(), BoxError> {
    let (dir, router) = router_with_web_toml(BASE).await?;
    // A file where the credential store's folder belongs makes every write to it fail.
    std::fs::write(dir.path().join("credentials"), b"not a folder")?;

    let answer = post(
        &router,
        &format!(
            r#"{{"transport":"http","name":"remote","endpoint":"http://127.0.0.1:9000","auth":{{"mode":"bearer","credential_ref":"remote-token","credential_value":"{SECRET}"}}}}"#
        ),
    )
    .await?;

    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert!(answer.body.contains("could not be stored"), "{}", answer.body);
    assert!(!answer.body.contains(SECRET), "{}", answer.body);
    Ok(())
}

#[tokio::test]
async fn an_unreadable_credential_store_record_is_reported_by_the_reveal() -> Result<(), BoxError> {
    let (dir, router) = router_with_web_toml(&(BASE.to_owned() + PASTED_ENTRY)).await?;
    std::fs::create_dir_all(dir.path().join("credentials"))?;
    std::fs::write(dir.path().join("credentials").join("remote-token.json"), b"{ not json")?;

    let answer = secret(&router, "/server/settings/mcp-servers/remote/secrets/bearer-token").await?;

    assert_eq!(answer.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        answer.body.contains("settings/credential-store-unreadable"),
        "{}",
        answer.body
    );
    Ok(())
}

#[tokio::test]
async fn a_record_of_another_kind_under_the_reference_is_not_revealed() -> Result<(), BoxError> {
    let (dir, router) = router_with_web_toml(&(BASE.to_owned() + PASTED_ENTRY)).await?;
    store(&dir).set(
        "remote-token",
        &CredentialRecord::Oauth2ClientSecret {
            value: SECRET.to_owned(),
            updated_at: "2026-10-27T00:00:00Z".to_owned(),
        },
    )?;

    let answer = secret(&router, "/server/settings/mcp-servers/remote/secrets/bearer-token").await?;

    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert!(
        answer.body.contains("settings/secret-not-configured"),
        "{}",
        answer.body
    );
    assert!(!answer.body.contains(SECRET));
    Ok(())
}

#[test]
fn debug_output_of_a_revealed_secret_never_shows_its_value() {
    let mut control = SecretControl::obscured("remote", SecretField::BearerToken);
    control.revealed = Some(Revealed::Value(SECRET.to_owned()));

    assert!(!format!("{control:?}").contains(SECRET));
}

#[tokio::test]
async fn a_client_secret_variable_is_shown_in_its_field_and_revealed_on_request() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(
        &(BASE.to_owned()
            + "\n[[mcp_server]]\nname = \"remote\"\ntransport = \"http\"\nendpoint = \"http://127.0.0.1:9000\"\n\
               [mcp_server.auth]\nmode = \"oauth2\"\nclient_id = \"abc\"\nclient_secret_env = \"CARGO_PKG_NAME\"\n\
               authorization_endpoint = \"https://provider.invalid/authorize\"\n\
               token_endpoint = \"https://provider.invalid/token\"\nscopes = [\"mcp\"]\n\
               credential_ref = \"remote-oauth-token\"\n"),
    )
    .await?;

    let page = secret(&router, "/server/settings/").await?;
    let answer = secret(
        &router,
        "/server/settings/mcp-servers/remote/secrets/client-secret-variable",
    )
    .await?;

    assert!(
        page.body
            .contains(r#"data-field="auth_oauth_client_secret_env" value="CARGO_PKG_NAME""#),
        "{}",
        page.body
    );
    assert!(
        page.body
            .contains(r#"hx-get="/server/settings/mcp-servers/remote/secrets/client-secret-variable""#),
        "{}",
        page.body
    );
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert!(answer.body.contains(env!("CARGO_PKG_NAME")), "{}", answer.body);
    Ok(())
}
