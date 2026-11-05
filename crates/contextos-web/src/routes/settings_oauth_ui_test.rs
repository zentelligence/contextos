//! The per-entry `mcp_server` auth UI: banner, mode sections, headers editor, pasted secrets.
//! Nested under `routes::settings::tests` (this file's own parent module, not a sibling of it)
//! purely to keep `settings_test.rs` itself under this repository's per-file line limit; every
//! helper referenced here (`router_with_web_toml`, `get_body`, `request`, `BASE`, `BoxError`) is
//! `settings_test.rs`'s own, reached via `super::*` exactly as any other test in that file reaches
//! them.

use super::*;

async fn get_body_with_query(router: &axum::Router, query: &str) -> Result<String, BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/settings/?{query}"))
                .body(Body::empty())?,
        )
        .await?;
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn no_query_parameters_render_no_oauth_banner() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let body = get_body(&router).await?;

    assert!(!body.contains("settings-banner"), "{body}");
    Ok(())
}

#[tokio::test]
async fn oauth_connected_renders_a_success_banner() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let body = get_body_with_query(&router, "oauth_connected=remote").await?;

    assert!(body.contains("Connected to remote."), "{body}");
    Ok(())
}

#[tokio::test]
async fn each_known_oauth_error_reason_renders_its_own_sentence_and_an_unknown_one_falls_back() -> Result<(), BoxError>
{
    let (_dir, router) = router_with_web_toml(BASE).await?;

    for (reason, expected_fragment) in [
        ("invalid_state", "could not be verified"),
        ("missing_parameters", "did not return the expected"),
        ("server_removed", "no longer configured"),
        ("client_secret_missing", "stored client secret could not be read"),
        ("token_exchange_failed", "rejected the token exchange"),
        // A provider-supplied `error` value passed straight through, or any other reason this
        // page has not seen before: never a rendering failure, just the generic fallback.
        ("access_denied", "connection failed"),
    ] {
        let body = get_body_with_query(&router, &format!("oauth_error={reason}&server=remote")).await?;
        assert!(
            body.contains("Could not connect remote:") && body.contains(expected_fragment),
            "{reason}: {body}"
        );
    }
    Ok(())
}

const HTTP_ENTRY: &str =
    "\n[[mcp_server]]\nname = \"remote\"\ntransport = \"http\"\nendpoint = \"http://127.0.0.1:9000\"\n";

#[tokio::test]
async fn bearer_mode_pre_fills_its_own_token_env_field() -> Result<(), BoxError> {
    let toml =
        BASE.to_owned() + HTTP_ENTRY + "\n[mcp_server.auth]\nmode = \"bearer\"\ntoken_env = \"REMOTE_BEARER_TOKEN\"\n";
    let (_dir, router) = router_with_web_toml(&toml).await?;

    let body = get_body(&router).await?;

    assert!(
        body.contains("data-field=\"auth_bearer_token_env\" value=\"REMOTE_BEARER_TOKEN\""),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn a_bearer_paste_field_is_always_blank_even_when_a_secret_is_already_stored() -> Result<(), BoxError> {
    let toml =
        BASE.to_owned() + HTTP_ENTRY + "\n[mcp_server.auth]\nmode = \"bearer\"\ncredential_ref = \"remote-token\"\n";
    let (dir, router) = router_with_web_toml(&toml).await?;
    std::fs::create_dir_all(dir.path().join("credentials"))?;
    std::fs::write(
        dir.path().join("credentials").join("remote-token.json"),
        r#"{"kind":"bearer","value":"super-secret-token","updated_at":"2026-01-01T00:00:00Z"}"#,
    )?;

    let body = get_body(&router).await?;

    assert!(!body.contains("super-secret-token"), "{body}");
    assert!(
        body.contains("data-field=\"auth_bearer_credential_paste\" value=\"\""),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn a_configured_indicator_appears_only_once_a_credential_record_actually_exists() -> Result<(), BoxError> {
    let toml =
        BASE.to_owned() + HTTP_ENTRY + "\n[mcp_server.auth]\nmode = \"bearer\"\ncredential_ref = \"remote-token\"\n";
    let (dir, router) = router_with_web_toml(&toml).await?;

    let before = get_body(&router).await?;
    assert!(!before.contains("Pasted token:"), "{before}");

    std::fs::create_dir_all(dir.path().join("credentials"))?;
    std::fs::write(
        dir.path().join("credentials").join("remote-token.json"),
        r#"{"kind":"bearer","value":"super-secret-token","updated_at":"2026-01-01T00:00:00Z"}"#,
    )?;

    let after = get_body(&router).await?;
    assert!(after.contains("Pasted token:"), "{after}");
    assert!(
        after.contains("/settings/mcp-servers/remote/secrets/bearer-token"),
        "{after}"
    );
    assert!(
        !after.contains("super-secret-token"),
        "the indicator never shows the token: {after}"
    );
    Ok(())
}

const OAUTH2_ENTRY: &str = "\n[mcp_server.auth]\nmode = \"oauth2\"\nauthorization_endpoint = \"https://example.invalid/authorize\"\ntoken_endpoint = \"https://example.invalid/token\"\nscopes = [\"a\"]\ncredential_ref = \"remote-oauth-token\"\n";

#[tokio::test]
async fn oauth2_mode_shows_connect_with_no_token_and_reconnect_disconnect_with_one() -> Result<(), BoxError> {
    let toml = BASE.to_owned() + HTTP_ENTRY + OAUTH2_ENTRY;
    let (dir, router) = router_with_web_toml(&toml).await?;

    let before = get_body(&router).await?;
    assert!(before.contains(">Connect<"), "{before}");
    assert!(!before.contains(">Sign in again<"), "{before}");
    assert!(!before.contains(">Disconnect<"), "{before}");

    std::fs::create_dir_all(dir.path().join("credentials"))?;
    std::fs::write(
        dir.path().join("credentials").join("remote-oauth-token.json"),
        r#"{"kind":"oauth2_token","access_token":"tok","refresh_token":null,"expires_at":"2026-01-01T00:00:00Z","obtained_at":"2025-12-01T00:00:00Z"}"#,
    )?;

    let after = get_body(&router).await?;
    assert!(after.contains(">Sign in again<"), "{after}");
    assert!(after.contains(">Disconnect<"), "{after}");
    assert!(!after.contains(">Connect<"), "{after}");
    Ok(())
}

#[tokio::test]
async fn the_headers_editor_shows_an_existing_header_as_a_visible_row() -> Result<(), BoxError> {
    let toml = BASE.to_owned() + HTTP_ENTRY + "\n[mcp_server.headers]\nlocationId = \"abc123\"\n";
    let (_dir, router) = router_with_web_toml(&toml).await?;

    let body = get_body(&router).await?;

    assert!(
        body.contains("data-header-key=\"locationId\" value=\"abc123\""),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn patching_bearer_auth_with_a_pasted_secret_writes_it_to_the_credential_store_never_to_web_toml()
-> Result<(), BoxError> {
    let toml = BASE.to_owned() + HTTP_ENTRY;
    let (dir, router) = router_with_web_toml(&toml).await?;

    let (status, body) = request(
        &router,
        Method::PATCH,
        r#"{"target":"mcp_server","name":"remote","patch":{"auth":{"mode":"bearer","credential_ref":"remote-token","credential_value":"super-secret-token"}}}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body:?}");

    let on_disk = std::fs::read_to_string(dir.path().join("web.toml"))?;
    assert!(!on_disk.contains("super-secret-token"), "{on_disk}");
    assert!(on_disk.contains("remote-token"), "{on_disk}");

    let stored = std::fs::read_to_string(dir.path().join("credentials").join("remote-token.json"))?;
    assert!(stored.contains("super-secret-token"), "{stored}");
    assert!(stored.contains("\"kind\": \"bearer\""), "{stored}");
    Ok(())
}

#[tokio::test]
async fn patching_oauth2_client_secret_with_a_pasted_value_writes_it_to_the_credential_store_never_to_web_toml()
-> Result<(), BoxError> {
    let toml = BASE.to_owned() + HTTP_ENTRY;
    let (dir, router) = router_with_web_toml(&toml).await?;

    let (status, body) = request(
        &router,
        Method::PATCH,
        r#"{"target":"mcp_server","name":"remote","patch":{"auth":{"mode":"oauth2","client_id":"abc","client_secret_ref":"remote-client-secret","client_secret_value":"super-secret-client-secret","authorization_endpoint":"https://example.invalid/authorize","token_endpoint":"https://example.invalid/token","scopes":["a"],"credential_ref":"remote-oauth-token"}}}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body:?}");

    let on_disk = std::fs::read_to_string(dir.path().join("web.toml"))?;
    assert!(!on_disk.contains("super-secret-client-secret"), "{on_disk}");
    assert!(on_disk.contains("remote-client-secret"), "{on_disk}");

    let stored = std::fs::read_to_string(dir.path().join("credentials").join("remote-client-secret.json"))?;
    assert!(stored.contains("super-secret-client-secret"), "{stored}");
    assert!(stored.contains("\"kind\": \"oauth2_client_secret\""), "{stored}");
    Ok(())
}

/// The exact incident this whole page already carries a regression test for
/// (`the_mcp_server_edit_form_reads_its_own_fields_by_id_not_this`), re-checked against every new
/// `hx-vals`/`hx-post`/`hx-delete` this task's own auth/headers UI adds: a bare `this` inside a
/// `js:` expression is the global object, not the form, so every new addition must look its own
/// element up by `id` (`document.getElementById(...)`) exactly like the pattern it extends.
#[tokio::test]
async fn the_new_auth_and_headers_ui_never_uses_this_inside_a_js_hx_vals_expression() -> Result<(), BoxError> {
    let toml = BASE.to_owned() + HTTP_ENTRY;
    let (_dir, router) = router_with_web_toml(&toml).await?;

    let body = get_body(&router).await?;

    assert!(body.contains("data-field=\"auth_mode\""), "{body}");
    assert!(
        body.contains("data-header-key") || body.contains("data-field=\"header_new_key\""),
        "{body}"
    );
    assert!(!body.contains("(this)"), "{body}");
    Ok(())
}

/// htmx merges `hx-vals` from every ancestor into a request, and `hx-disinherit` does not stop that. Were the edit
/// form's own patch (which includes anything typed into a paste field) on the `<form>` element, Detect, Connect,
/// and the secret controls inside it would all send it. It belongs on the Save button, which has no descendants.
#[tokio::test]
async fn the_edit_forms_patch_sits_on_its_save_button_so_no_control_inside_inherits_it() -> Result<(), BoxError> {
    let toml = BASE.to_owned() + HTTP_ENTRY + OAUTH2_ENTRY;
    let (_dir, router) = router_with_web_toml(&toml).await?;

    let body = get_body(&router).await?;

    let mut forms = 0;
    for (start, _) in body.match_indices("<form\n              class=\"mcp-server-edit\"") {
        let opening = &body[start..start + body[start..].find('>').ok_or("unterminated form tag")?];
        assert!(!opening.contains("hx-vals"), "{opening}");
        assert!(!opening.contains("hx-patch"), "{opening}");
        forms += 1;
    }
    assert_eq!(forms, 2, "both entries' edit forms were checked: {body}");
    assert_eq!(
        body.matches("class=\"btn primary small mcp-server-save\"").count(),
        2,
        "{body}"
    );
    assert!(body.contains("hx-post=\"/settings/oauth/remote/detect\""), "{body}");
    Ok(())
}
