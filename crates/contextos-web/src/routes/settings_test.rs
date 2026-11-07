use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt as _;

use super::*;
use crate::app::build_router;

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

#[test]
fn no_rotation_trigger_displays_as_never() {
    assert_eq!(log_rotation_display(&WebServerConfig::default()), "Never");
}

#[test]
fn a_size_trigger_alone_names_its_threshold() {
    let server = WebServerConfig {
        log_max_size_mb: Some(10),
        ..WebServerConfig::default()
    };
    assert_eq!(log_rotation_display(&server), "every 10 MB");
}

#[test]
fn a_daily_trigger_alone_reads_daily() {
    let server = WebServerConfig {
        log_rotate_daily: true,
        ..WebServerConfig::default()
    };
    assert_eq!(log_rotation_display(&server), "daily");
}

#[test]
fn both_triggers_are_joined_with_and() {
    let server = WebServerConfig {
        log_max_size_mb: Some(10),
        log_rotate_daily: true,
        ..WebServerConfig::default()
    };
    assert_eq!(log_rotation_display(&server), "every 10 MB and daily");
}

#[test]
fn no_retention_bound_displays_as_kept_indefinitely() {
    assert_eq!(log_retention_display(&WebServerConfig::default()), "Kept indefinitely");
}

#[test]
fn both_retention_bounds_are_joined_with_a_comma() {
    let server = WebServerConfig {
        log_retention_days: Some(30),
        log_retention_files: Some(10),
        ..WebServerConfig::default()
    };
    assert_eq!(log_retention_display(&server), "30 days, 10 files");
}

async fn router_with_web_toml(contents: &str) -> Result<(tempfile::TempDir, axum::Router), BoxError> {
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("web.toml"), contents)?;
    // No real `contextos-mcp` process for these unit tests: every case here
    // either never removes an `[[mcp_server]]` name (so the dependency
    // check is never reached) or deliberately exercises the
    // `mcp/server-not-configured` path for a removal with no live session
    // at all.
    let clients = Arc::new(McpClientSet::connect(&[]).await?);
    let router = build_router(clients, Some(dir.path()), &dir.path().join("web.toml"));
    Ok((dir, router))
}

async fn request(
    router: &axum::Router,
    method: Method,
    body: &str,
) -> Result<(StatusCode, serde_json::Value), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri("/server/settings/")
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))?,
        )
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let value = if status == StatusCode::OK {
        serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned())
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    Ok((status, value))
}

/// `DELETE /server/settings/mcp-servers/{name}`: `name` is the URL path segment,
/// never a request body field, matching what a real `hx-delete` button
/// actually sends (`docs/http-routing-response-contract.md`'s sub-entity
/// form; htmx's own `methodsThatUseUrlParams` default never puts a `DELETE`
/// request's parameters in its body).
async fn delete_mcp_server_request(
    router: &axum::Router,
    name: &str,
) -> Result<(StatusCode, serde_json::Value), BoxError> {
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
    let value = if status == StatusCode::OK {
        serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned())
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    Ok((status, value))
}

async fn get_body(router: &axum::Router) -> Result<String, BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().uri("/server/settings/").body(Body::empty())?)
        .await?;
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn get_renders_the_configured_mcp_server() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let response = router
        .clone()
        .oneshot(Request::builder().uri("/server/settings/").body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let body = String::from_utf8_lossy(&bytes);
    assert!(body.contains("contextos"));
    assert!(body.contains("127.0.0.1:7373"));
    Ok(())
}

/// htmx evaluates an `hx-vals="js:..."` expression as a bare
/// `Function(...)()` call (confirmed against the bundled `htmx.min.js`,
/// never `.call(element, ...)`), so `this` inside it is the global object,
/// not the row's own edit form: `this.dataset.name`/`this.querySelector(...)`
/// throw in a real browser, the request never fires, and an MCP server edit
/// silently does nothing. The per-row edit form must look its own element up
/// by the `id` it already carries (`mcp-server-edit-{{ loop.index0 }}`,
/// also the "Edit" toggle's own `data-target`), never `this`.
#[tokio::test]
async fn the_mcp_server_edit_form_reads_its_own_fields_by_id_not_this() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let body = get_body(&router).await?;

    assert!(body.contains("id=\"mcp-server-edit-0\""), "{body}");
    assert!(body.contains("document.getElementById('mcp-server-edit-0')"), "{body}");
    assert!(!body.contains("this.dataset"), "{body}");
    assert!(!body.contains("this.querySelector"), "{body}");
    Ok(())
}

/// A distinct, real bug from the `this`-inside-`hx-vals` class above, found 2026-09-20 while
/// investigating a silent "Add an MCP server (HTTP)" no-op: htmx's own `js:`-prefixed
/// `hx-vals` evaluator (`bn` in `htmx.min.js`) auto-wraps the expression in `{...}` whenever it
/// does not already start with `{` (the shorthand that lets `hx-vals="js:foo: bar()"` work
/// without its own braces) — but this wrapping mangles a bare top-level IIFE like
/// `js:(function(){...})()` into the syntactically invalid `{(function(){...})()}`, throwing a
/// `SyntaxError` synchronously during request-building, before any `XMLHttpRequest` is ever
/// created. The failure mode is silent: no network request, no error banner, `web.toml`
/// untouched, exactly the report that led here. Verified against real htmx 2.0.4/2.0.10 source
/// (`Function("return (" + e + ")")()`, confirmed via direct execution, not just static
/// reading). Every `hx-vals="js:..."` on this page must therefore have its own outer `{...}`
/// already, whatever it computes inside — an expression form this test enforces structurally
/// (`.trim().starts_with('{')`) rather than trusting a human reviewer to notice the one form
/// shaped differently from all its siblings.
#[tokio::test]
async fn every_js_hx_vals_expression_on_this_page_already_starts_with_a_brace() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let body = get_body(&router).await?;

    let mut checked = 0;
    for chunk in body.split("hx-vals=\"js:").skip(1) {
        let end = chunk.find('"').ok_or("unterminated hx-vals attribute")?;
        let expression = chunk[..end].trim();
        assert!(
            expression.starts_with('{'),
            "hx-vals=\"js:{expression}\" does not start with '{{': htmx's own auto-wrap will \
             mangle this into invalid JS and the request will silently never fire"
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "expected at least one js: hx-vals attribute on this page to check"
    );
    Ok(())
}

/// `/server/settings/` is an SSR+HTMX
/// surface, so a plain browser navigation (no `HX-Request` header) gets the
/// full page shell, htmx and its `json-enc` extension included, matching
/// `docs/http-routing-response-contract.md`'s "no HX-Request
/// -> full HTML page" contract.
#[tokio::test]
async fn get_without_hx_request_returns_the_full_page_shell_with_htmx_loaded() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let response = router
        .oneshot(Request::builder().uri("/server/settings/").body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let body = String::from_utf8_lossy(&bytes);
    assert!(body.contains("<html"));
    assert!(body.contains("/static/htmx.min.js"));
    assert!(body.contains("/static/htmx-ext-json-enc.min.js"));
    assert!(body.contains("hx-post=\"/server/settings/\""));
    assert!(body.contains("hx-delete=\"/server/settings/mcp-servers/contextos\""));
    // The HTTP-transport add form (endpoint + token_env), not just stdio.
    assert!(body.contains("id=\"mcp-server-add-http\""));
    assert!(body.contains("data-transport=\"http\""));
    // Every configured entry gets a per-row Edit toggle, not delete-only.
    assert!(body.contains("mcp-server-edit-toggle"));
    Ok(())
}

/// The `HX-Request: true` counterpart to the test above: an htmx-issued
/// request gets the bare fragment it swaps in, never the full page shell
/// again (`docs/http-routing-response-contract.md`'s "HTMX
/// Surface: HTML fragment only").
#[tokio::test]
async fn get_with_hx_request_returns_a_bare_fragment() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let response = router
        .oneshot(
            Request::builder()
                .uri("/server/settings/")
                .header("hx-request", "true")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let body = String::from_utf8_lossy(&bytes);
    assert!(!body.contains("<html"));
    assert!(body.contains("contextos"));
    Ok(())
}

#[tokio::test]
async fn post_adds_a_new_mcp_server_entry_and_a_following_get_reflects_it() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let (status, body) = request(
        &router,
        Method::POST,
        r#"{"transport":"stdio","name":"second","command":"other","args":[]}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let rendered = body.as_str().unwrap_or_default();
    assert!(rendered.contains("second"));
    // A write response is what htmx swaps into place: a bare fragment,
    // never a second full page shell nested inside the first.
    assert!(!rendered.contains("<html"));

    assert!(get_body(&router).await?.contains("second"));
    Ok(())
}

/// Regression: adding an `[[mcp_server]]` entry re-renders and swaps the *whole* `.settings`
/// fragment via `hx-swap="outerHTML"`, and `render()` always marks Appearance active in that
/// server-rendered markup (there is no "which pane was open" field on `SettingsTemplate`). An
/// operator on the "MCP servers" pane who adds a server was silently dropped back onto Appearance
/// even though the server list they were looking at had just changed underneath them. The fix is
/// client-side (a sessionStorage note the swapped-in script re-applies on load); this only pins
/// that the mechanism still ships in the fragment a mutation returns, not the browser behaviour
/// itself, which this Rust-level harness cannot exercise.
#[tokio::test]
async fn a_post_response_still_carries_the_active_pane_restore_script() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let (status, body) = request(
        &router,
        Method::POST,
        r#"{"transport":"stdio","name":"second","command":"other","args":[]}"#,
    )
    .await?;

    assert_eq!(status, StatusCode::OK, "{body:?}");
    let rendered = body.as_str().unwrap_or_default();
    assert!(
        rendered.contains("contextos-settings-active-pane") && rendered.contains("sessionStorage"),
        "{rendered}"
    );
    Ok(())
}

#[tokio::test]
async fn post_with_a_duplicate_name_is_rejected_and_web_toml_is_unchanged() -> Result<(), BoxError> {
    let (dir, router) = router_with_web_toml(BASE).await?;
    let before = std::fs::read_to_string(dir.path().join("web.toml"))?;

    let (status, body) = request(
        &router,
        Method::POST,
        r#"{"transport":"stdio","name":"contextos","command":"other","args":[]}"#,
    )
    .await?;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "settings/invalid-configuration");
    assert_eq!(std::fs::read_to_string(dir.path().join("web.toml"))?, before);
    Ok(())
}

#[tokio::test]
async fn post_with_a_malformed_body_is_a_400_and_web_toml_is_unchanged() -> Result<(), BoxError> {
    let (dir, router) = router_with_web_toml(BASE).await?;
    let before = std::fs::read_to_string(dir.path().join("web.toml"))?;

    let (status, body) = request(&router, Method::POST, "not json").await?;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "route/malformed-body");
    assert_eq!(std::fs::read_to_string(dir.path().join("web.toml"))?, before);
    Ok(())
}

#[tokio::test]
async fn a_fuzzed_body_naming_a_vault_field_never_touches_config_toml() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("web.toml"), BASE)?;
    std::fs::write(
        dir.path().join("config.toml"),
        "[[vault]]\npath = \"/some/real/vault\"\nname = \"real\"\n",
    )?;
    let before_config_toml = std::fs::read_to_string(dir.path().join("config.toml"))?;
    let clients = Arc::new(McpClientSet::connect(&[]).await?);
    let router = build_router(clients, Some(dir.path()), &dir.path().join("web.toml"));

    // A submission with no legitimate mapping to `web.toml`'s own schema at
    // all (a `vault` array, the shape `config.toml`'s `[[vault]]` blocks
    // take): rejected as malformed, since `McpServerConfig` has no `vault`
    // field and this crate never opens `config.toml` from any handler.
    let (status, _body) = request(&router, Method::POST, r#"{"vault":[{"path":"/etc","name":"evil"}]}"#).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    assert_eq!(
        std::fs::read_to_string(dir.path().join("config.toml"))?,
        before_config_toml,
        "config.toml must never be touched by a /server/settings/ submission"
    );
    Ok(())
}

#[tokio::test]
async fn patch_partially_updates_an_existing_mcp_server_entry() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let (status, body) = request(
        &router,
        Method::PATCH,
        r#"{"target":"mcp_server","name":"contextos","patch":{"command":"patched"}}"#,
    )
    .await?;

    assert_eq!(status, StatusCode::OK, "{body:?}");
    let rendered = body.as_str().unwrap_or_default();
    assert!(rendered.contains("patched"));
    Ok(())
}

/// The "auth for an MCP server" case (distinct from `contextos-web`'s own,
/// deliberately deferred, HTTP surface auth): an HTTP `[[mcp_server]]`
/// entry's `token_env` round-trips through `POST`, the read-only summary,
/// and the edit form's pre-filled value.
#[tokio::test]
async fn post_adds_an_http_mcp_server_with_a_token_env_and_it_is_visible_and_editable() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let (status, body) = request(
        &router,
        Method::POST,
        r#"{"transport":"http","name":"remote","endpoint":"http://127.0.0.1:9000","token_env":"REMOTE_TOKEN"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let rendered = body.as_str().unwrap_or_default();
    assert!(rendered.contains("token via $REMOTE_TOKEN"), "{rendered}");
    // The edit form pre-fills the current token_env so an operator can see
    // and change it, not just the read-only summary line.
    assert!(
        rendered.contains("value=\"REMOTE_TOKEN\""),
        "edit form did not pre-fill token_env: {rendered}"
    );
    Ok(())
}

#[tokio::test]
async fn patch_updates_an_http_mcp_servers_token_env() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(
        &(BASE.to_owned()
            + "\n[[mcp_server]]\nname = \"remote\"\ntransport = \"http\"\nendpoint = \"http://127.0.0.1:9000\"\n"),
    )
    .await?;

    let (status, body) = request(
        &router,
        Method::PATCH,
        r#"{"target":"mcp_server","name":"remote","patch":{"token_env":"REMOTE_TOKEN"}}"#,
    )
    .await?;

    assert_eq!(status, StatusCode::OK, "{body:?}");
    let rendered = body.as_str().unwrap_or_default();
    assert!(rendered.contains("token via $REMOTE_TOKEN"), "{rendered}");
    Ok(())
}

/// Slices the `<button>...</button>` block for one Appearance picker option
/// out of a rendered `/server/settings/` body, keyed by the exact `key`/`value`
/// pair its own `hx-vals` patch submits (e.g. `("theme", "dark")`) - so an
/// assertion about that option's own `selected` state cannot be satisfied
/// by a different option's markup.
fn appearance_option_block<'a>(body: &'a str, key: &str, value: &str) -> Result<&'a str, BoxError> {
    let marker = format!("\"{key}\":\"{value}\"");
    let start = body
        .find(&marker)
        .ok_or_else(|| format!("no {key}={value:?} option in {body}"))?;
    let before = &body[..start];
    let button_start = before.rfind("<button").unwrap_or(0);
    let rest = &body[button_start..];
    let end = rest
        .find("</button>")
        .map_or(rest.len(), |index| index + "</button>".len());
    Ok(&rest[..end])
}

#[tokio::test]
async fn patch_partially_updates_server_ui() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let (status, body) = request(&router, Method::PATCH, r#"{"target":"ui","patch":{"theme":"dark"}}"#).await?;

    assert_eq!(status, StatusCode::OK, "{body:?}");
    let rendered = body.as_str().unwrap_or_default();
    assert!(
        appearance_option_block(rendered, "theme", "dark")?.contains("selected"),
        "{rendered}"
    );
    assert!(
        !appearance_option_block(rendered, "theme", "blueprint")?.contains("selected"),
        "{rendered}"
    );
    assert!(
        !appearance_option_block(rendered, "theme", "light")?.contains("selected"),
        "{rendered}"
    );
    Ok(())
}

/// `<html data-theme>` lives in `page_shell.html`, outside the `.settings`
/// element the Appearance picker's own `hx-target` swaps, so a bare
/// fragment swap leaves the previous skin showing until some unrelated
/// navigation happens to reload the shell around it. `HX-Refresh: true`
/// tells htmx to discard the fragment and reload the page instead, so the
/// new skin actually shows the moment the operator picks it.
#[tokio::test]
async fn patching_the_theme_triggers_a_full_page_refresh() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::PATCH)
                .uri("/server/settings/")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"target":"ui","patch":{"theme":"dark"}}"#))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("HX-Refresh")
            .and_then(|value| value.to_str().ok()),
        Some("true"),
        "a theme change must trigger htmx's full-page refresh"
    );
    Ok(())
}

/// `system_name`/`logo` render in the header, and `font`/`size` render as
/// `<html>` attributes alongside `theme` - all outside `.settings`, all
/// needing the same refresh `theme` needs.
#[tokio::test]
async fn patching_font_size_system_name_or_logo_also_triggers_a_refresh() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    for patch in [
        r#"{"target":"ui","patch":{"font":"serif"}}"#,
        r#"{"target":"ui","patch":{"size":"large"}}"#,
        r#"{"target":"ui","patch":{"system_name":"Ops Desk"}}"#,
        r#"{"target":"ui","patch":{"logo":"custom-logo.png"}}"#,
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri("/server/settings/")
                    .header("content-type", "application/json")
                    .body(Body::from(patch))?,
            )
            .await?;

        assert_eq!(response.status(), StatusCode::OK, "{patch}");
        assert_eq!(
            response
                .headers()
                .get("HX-Refresh")
                .and_then(|value| value.to_str().ok()),
            Some("true"),
            "{patch} touches shell chrome and must trigger a refresh"
        );
    }
    Ok(())
}

/// A save that touches nothing the shell renders outside `.settings` (an
/// `[[mcp_server]]` edit, or a `[server.ui]` key with no wired rendering
/// effect anywhere) never needs a refresh: the swapped `.settings`
/// fragment alone already reflects it.
#[tokio::test]
async fn patching_an_mcp_server_or_an_inert_ui_key_never_triggers_a_refresh() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    for (patch, description) in [
        (
            r#"{"target":"mcp_server","name":"contextos","patch":{"args":["--config","other.toml"]}}"#,
            "an mcp_server patch",
        ),
        (
            r#"{"target":"ui","patch":{"custom_flag":"1"}}"#,
            "an unenumerated ui key with no rendering effect",
        ),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri("/server/settings/")
                    .header("content-type", "application/json")
                    .body(Body::from(patch))?,
            )
            .await?;

        assert_eq!(response.status(), StatusCode::OK, "{description}");
        assert!(
            response.headers().get("HX-Refresh").is_none(),
            "{description} touches nothing outside `.settings` and must not trigger a refresh"
        );
    }
    Ok(())
}

/// The sharpest part of the raw key-value form this replaced: a vault with
/// no `[server.ui]` set at all must still show the real, currently-
/// effective default selected on every Appearance picker, never a blank
/// or unselected control.
#[tokio::test]
async fn appearance_pickers_pre_select_the_real_defaults_when_server_ui_is_unset() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let body = get_body(&router).await?;

    assert!(
        appearance_option_block(&body, "theme", "blueprint")?.contains("selected"),
        "{body}"
    );
    assert!(
        appearance_option_block(&body, "size", "normal")?.contains("selected"),
        "{body}"
    );
    assert!(
        appearance_option_block(&body, "font", "sans")?.contains("selected"),
        "{body}"
    );
    Ok(())
}

/// The Appearance pane's own dedicated `System name` textbox (distinct from
/// the generic `[server.ui]` free-form editor `patch_partially_updates_
/// server_ui` exercises): a vault with no `system_name` set at all must
/// still show the real, currently-effective default ("Command Centre")
/// pre-filled, matching every other Appearance control's own "show the
/// real default, never a blank" rule.
#[tokio::test]
async fn the_system_name_field_pre_fills_the_real_default_when_unset() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let body = get_body(&router).await?;

    assert!(
        body.contains("data-ui-key=\"system_name\" value=\"Command Centre\""),
        "{body}"
    );
    // Promoted to its own control: no longer duplicated in the generic
    // free-form `[server.ui]` editor.
    assert!(!body.contains("<label>system_name<input"), "{body}");
    Ok(())
}

#[tokio::test]
async fn patch_updates_the_system_name_and_it_is_reflected_and_persisted() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let (status, body) = request(
        &router,
        Method::PATCH,
        r#"{"target":"ui","patch":{"system_name":"Ops Desk"}}"#,
    )
    .await?;

    assert_eq!(status, StatusCode::OK, "{body:?}");
    let rendered = body.as_str().unwrap_or_default();
    assert!(
        rendered.contains("data-ui-key=\"system_name\" value=\"Ops Desk\""),
        "{rendered}"
    );

    let after = get_body(&router).await?;
    assert!(
        after.contains("data-ui-key=\"system_name\" value=\"Ops Desk\""),
        "{after}"
    );
    Ok(())
}

#[tokio::test]
async fn patch_updates_font_size_and_font_selection_independently() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let (status, body) = request(
        &router,
        Method::PATCH,
        r#"{"target":"ui","patch":{"size":"large","font":"serif"}}"#,
    )
    .await?;

    assert_eq!(status, StatusCode::OK, "{body:?}");
    let rendered = body.as_str().unwrap_or_default();
    assert!(
        appearance_option_block(rendered, "size", "large")?.contains("selected"),
        "{rendered}"
    );
    assert!(
        !appearance_option_block(rendered, "size", "normal")?.contains("selected"),
        "{rendered}"
    );
    assert!(
        appearance_option_block(rendered, "font", "serif")?.contains("selected"),
        "{rendered}"
    );
    assert!(
        !appearance_option_block(rendered, "font", "sans")?.contains("selected"),
        "{rendered}"
    );
    // Independent of the theme axis, still at its own default.
    assert!(
        appearance_option_block(rendered, "theme", "blueprint")?.contains("selected"),
        "{rendered}"
    );
    Ok(())
}

/// An unrecognised or stale `theme`/`size`/`font` value (for example a
/// pre-rename `"grey"`/`"white"`/`"blue"` string left over in a hand-edited
/// `web.toml`) is never a failed render: the picker falls back to that
/// axis's real default rather than erroring or leaving nothing selected,
/// exactly as `contextos-tokens.css`'s own "no matching rule" fallback
/// already does visually.
#[tokio::test]
async fn an_unrecognised_theme_value_falls_back_to_the_default_selection() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let (status, body) = request(&router, Method::PATCH, r#"{"target":"ui","patch":{"theme":"grey"}}"#).await?;

    assert_eq!(status, StatusCode::OK, "{body:?}");
    let rendered = body.as_str().unwrap_or_default();
    assert!(
        appearance_option_block(rendered, "theme", "blueprint")?.contains("selected"),
        "{rendered}"
    );
    Ok(())
}

#[tokio::test]
async fn patch_on_an_unknown_mcp_server_name_is_a_404() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let (status, body) = request(
        &router,
        Method::PATCH,
        r#"{"target":"mcp_server","name":"does-not-exist","patch":{"command":"x"}}"#,
    )
    .await?;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "settings/unknown-mcp-server");
    Ok(())
}

#[tokio::test]
async fn put_fully_replaces_an_existing_entry_keeping_its_name() -> Result<(), BoxError> {
    // Keeping the same `name` on both sides of the replacement means no
    // `[[mcp_server]]` name is removed, so this test (deliberately, no live
    // MCP session) never reaches the registered-app-dependency check;
    // `tests/settings_routes.rs`'s
    // `put_renaming_an_entry_with_no_dependent_apps_succeeds` covers the
    // rename case against a real session.
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let (status, body) = request(
        &router,
        Method::PUT,
        r#"{"current_name":"contextos","transport":"stdio","name":"contextos","command":"full-replacement","args":[]}"#,
    )
    .await?;

    assert_eq!(status, StatusCode::OK, "{body:?}");
    let rendered = body.as_str().unwrap_or_default();
    assert!(rendered.contains("full-replacement"));
    Ok(())
}

#[tokio::test]
async fn put_replacing_an_unknown_entry_is_a_404_and_web_toml_is_unchanged() -> Result<(), BoxError> {
    let (dir, router) = router_with_web_toml(BASE).await?;
    let before = std::fs::read_to_string(dir.path().join("web.toml"))?;

    let (status, body) = request(
        &router,
        Method::PUT,
        r#"{"current_name":"does-not-exist","transport":"stdio","name":"x","command":"y","args":[]}"#,
    )
    .await?;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "settings/unknown-mcp-server");
    assert_eq!(std::fs::read_to_string(dir.path().join("web.toml"))?, before);
    Ok(())
}

#[tokio::test]
async fn delete_removing_the_only_mcp_server_with_no_live_session_fails_closed() -> Result<(), BoxError> {
    let (dir, router) = router_with_web_toml(BASE).await?;
    let before = std::fs::read_to_string(dir.path().join("web.toml"))?;

    // Removing `contextos` would remove a name that was present before the
    // edit; with no live MCP session at all (`McpClientSet::connect(&[])`
    // in this test's own setup) the registered-app-dependency check cannot
    // run, so the edit is rejected as unreachable rather than silently
    // skipping the check.
    let (status, body) = delete_mcp_server_request(&router, "contextos").await?;

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"], "mcp/unreachable");
    assert_eq!(std::fs::read_to_string(dir.path().join("web.toml"))?, before);
    Ok(())
}

#[tokio::test]
async fn delete_on_an_unknown_name_is_a_404() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let (status, body) = delete_mcp_server_request(&router, "does-not-exist").await?;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "settings/unknown-mcp-server");
    Ok(())
}

#[tokio::test]
async fn delete_at_the_old_body_addressed_settings_root_is_rejected_not_silently_ignored() -> Result<(), BoxError> {
    // The route this replaces (`DELETE /server/settings/` with `{"name": ...}` in
    // the body) must no longer exist at all: leaving it registered
    // alongside the new path-addressed route would let a stale client keep
    // hitting the exact no-op this fix removes. `/server/settings/` still answers
    // `GET`/`POST`/`PATCH`/`PUT`, so a `DELETE` there is a documented method
    // rejection, not a 404.
    let (_dir, router) = router_with_web_toml(BASE).await?;

    let (status, _body) = request(&router, Method::DELETE, r#"{"name":"contextos"}"#).await?;

    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    Ok(())
}

#[tokio::test]
async fn get_returns_a_validation_error_when_web_toml_cannot_be_read() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    // No web.toml written at all.
    let clients = Arc::new(McpClientSet::connect(&[]).await?);
    let router = build_router(clients, Some(dir.path()), &dir.path().join("web.toml"));

    let response = router
        .oneshot(Request::builder().uri("/server/settings/").body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    Ok(())
}

// The per-entry mcp_server auth UI (banner, mode sections, headers editor, pasted secrets) is
// tested in `settings_oauth_ui_test.rs`, nested here purely to keep this file under this
// repository's per-file line limit; see that file's own module doc for why `super::*` there
// reaches every helper defined above in this file.
#[path = "settings_oauth_ui_test.rs"]
mod oauth_ui;
