//! App registration and serving contract tests: real `contextos-mcp`
//! sessions against real temporary vault fixtures, exercising `GET
//! /{{vault_name}}/apps/`, `GET /{{vault_name}}/apps/{{slug}}/...`,
//! `POST /{{vault_name}}/apps/rescan`, and `PATCH
//! /{{vault_name}}/apps/{{slug}}` (favouriting) end to end. No mocking, per
//! `testing.md`.

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use contextos_web::McpClientSet;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";

async fn router_over(vault_dir: &Path, config_dir: &Path) -> Result<Router, BoxError> {
    let config_path = support::write_vault_config(config_dir, vault_dir)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(contextos_web::build_router(
        clients,
        Some(config_dir),
        &config_dir.join("web.toml"),
    ))
}

fn write_two_vault_config(
    dir: &Path,
    vault_a: &Path,
    name_a: &str,
    vault_b: &Path,
    name_b: &str,
) -> std::io::Result<PathBuf> {
    let path = dir.join("config.toml");
    #[allow(clippy::unnecessary_debug_formatting)]
    let content = format!(
        "[[vault]]\npath = {vault_a:?}\nname = {name_a:?}\nstate_directory = \".contextos\"\n\
         [vault.search]\ntext = false\ngraph = false\n\n\
         [[vault]]\npath = {vault_b:?}\nname = {name_b:?}\nstate_directory = \".contextos\"\n\
         [vault.search]\ntext = false\ngraph = false\n"
    );
    std::fs::write(&path, content)?;
    Ok(path)
}

async fn router_over_two_vaults(
    vault_a: &Path,
    name_a: &str,
    vault_b: &Path,
    name_b: &str,
    config_dir: &Path,
) -> Result<Router, BoxError> {
    let config_path = write_two_vault_config(config_dir, vault_a, name_a, vault_b, name_b)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(contextos_web::build_router(
        clients,
        Some(config_dir),
        &config_dir.join("web.toml"),
    ))
}

async fn get(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

async fn post(router: &Router, path: &str) -> Result<StatusCode, BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().method("POST").uri(path).body(Body::empty())?)
        .await?;
    Ok(response.status())
}

/// `PATCH` with a `{"favourite": ...}` JSON body, matching the star toggle's own real request
/// shape (`hx-ext="json-enc"`, `docs/http-routing-response-contract.md`'s PATCH "submit partial
/// update" row): no test in this file sends the old, non-compliant plain POST any more.
async fn patch_favourite(router: &Router, path: &str, favourite: bool) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(path)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!("{{\"favourite\":{favourite}}}")))?,
        )
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

fn write_app(
    vault_dir: &Path,
    dir_name: &str,
    manifest_toml: &str,
    entry_name: &str,
    entry_content: &str,
) -> std::io::Result<()> {
    write(
        vault_dir,
        &format!("registry/apps/{dir_name}/manifest.toml"),
        manifest_toml,
    )?;
    if !entry_name.is_empty() {
        write(
            vault_dir,
            &format!("registry/apps/{dir_name}/{entry_name}"),
            entry_content,
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Registration and serving
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_valid_spa_manifest_registers_and_serves() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write_app(
        vault_dir.path(),
        "task-register",
        r#"
            name = "Task Register Dashboard"
            kind = "spa"
            entry = "index.html"
            mcp_servers = ["contextos"]
        "#,
        "index.html",
        "<html><body>Task Register</body></html>",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/apps/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Task Register Dashboard"));
    assert!(body.contains(&format!("/{VAULT_NAME}/apps/task-register/")));

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/apps/task-register/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Task Register"));
    // A `spa` app's entry is served raw and unwrapped, never composed into
    // the shell the way a `js` app's entry is (shell composition applies
    // only to `js`).
    assert!(!body.contains("shell-body"));
    Ok(())
}

#[tokio::test]
async fn the_apps_listing_and_a_registered_apps_root_render_identically_without_a_trailing_slash()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write_app(
        vault_dir.path(),
        "task-register",
        r#"
            name = "Task Register Dashboard"
            kind = "spa"
            entry = "index.html"
        "#,
        "index.html",
        "<html><body>Task Register</body></html>",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status_slash, body_slash) = get(&router, &format!("/{VAULT_NAME}/apps/")).await?;
    let (status_bare, body_bare) = get(&router, &format!("/{VAULT_NAME}/apps")).await?;
    assert_eq!(status_slash, StatusCode::OK);
    assert_eq!(status_bare, StatusCode::OK);
    assert_eq!(body_slash, body_bare);

    let (status_slash, body_slash) = get(&router, &format!("/{VAULT_NAME}/apps/task-register/")).await?;
    let (status_bare, body_bare) = get(&router, &format!("/{VAULT_NAME}/apps/task-register")).await?;
    assert_eq!(status_slash, StatusCode::OK);
    assert_eq!(status_bare, StatusCode::OK);
    assert_eq!(body_slash, body_bare);
    Ok(())
}

#[tokio::test]
async fn a_spa_apps_sub_path_falls_back_to_its_own_entry_file_spa_routing() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write_app(
        vault_dir.path(),
        "task-register",
        r#"
            name = "Task Register Dashboard"
            kind = "spa"
            entry = "index.html"
        "#,
        "index.html",
        "<html><body>SPA shell</body></html>",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/apps/task-register/some/client/route")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("SPA shell"));
    Ok(())
}

#[tokio::test]
async fn a_schema_violation_fails_registration_without_blocking_other_apps() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write_app(
        vault_dir.path(),
        "broken",
        r#"
            name = "Broken App"
            kind = "not-a-real-kind"
            entry = "index.html"
        "#,
        "index.html",
        "<html></html>",
    )?;
    write_app(
        vault_dir.path(),
        "healthy",
        r#"
            name = "Healthy App"
            kind = "spa"
            entry = "index.html"
        "#,
        "index.html",
        "<html><body>Healthy</body></html>",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/apps/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Healthy App"));
    assert!(!body.contains("Broken App"));

    let (status, _) = get(&router, &format!("/{VAULT_NAME}/apps/broken/")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn an_mcp_servers_entry_absent_from_web_toml_fails_registration() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write_app(
        vault_dir.path(),
        "needs-unknown-server",
        r#"
            name = "Needs Unknown Server"
            kind = "spa"
            entry = "index.html"
            mcp_servers = ["not-configured-anywhere"]
        "#,
        "index.html",
        "<html></html>",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/apps/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("Needs Unknown Server"));
    Ok(())
}

#[tokio::test]
async fn a_missing_entry_file_fails_registration() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write_app(
        vault_dir.path(),
        "no-entry",
        r#"
            name = "No Entry"
            kind = "spa"
            entry = "index.html"
        "#,
        "",
        "",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/apps/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("No Entry"));
    Ok(())
}

// ---------------------------------------------------------------------
// `js`-kind: embed composition
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_js_kind_apps_entry_document_composes_within_the_shell() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write_app(
        vault_dir.path(),
        "task-widget",
        r#"
            name = "Task Widget"
            kind = "js"
            entry = "index.html"
        "#,
        "index.html",
        "<div id=\"app\">Task Widget content</div>",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/apps/task-widget/")).await?;
    assert_eq!(status, StatusCode::OK);
    // The app's own entry markup is inlined into the shell...
    assert!(body.contains("Task Widget content"));
    // ...alongside the shell's own chrome, which the app's raw entry
    // document (just a bare <div>, no shell markup of its own) does not
    // itself contain -- proving composition happened rather than a raw
    // passthrough of the fetched document.
    assert!(body.contains("shell-body"));
    Ok(())
}

#[tokio::test]
async fn a_js_kind_apps_composed_entry_is_exempt_from_the_shells_prose_width_cap() -> Result<(), BoxError> {
    // `.content > *{max-width:72ch}` (`contextos-web.css`) caps a prose
    // reading width onto every direct child of the shell's content region,
    // for ordinary vault-note markdown. A composed `kind = "js"` app is not
    // prose -- it is arbitrary application UI (a kanban board, a form) that
    // needs the full content width, exactly like the shell's own
    // `card-grid`/`base-view`/`apps-list` content types are already
    // exempted from this cap. Without an equivalent exemption, an app's
    // wide layout (e.g. multiple flex/grid columns) is squeezed into ~72
    // characters and its overflow is left to whatever the app's own CSS
    // happens to do with it, which most naive app layouts render as
    // visually truncated content rather than a usable scroll area.
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write_app(
        vault_dir.path(),
        "wide-widget",
        r#"
            name = "Wide Widget"
            kind = "js"
            entry = "index.html"
        "#,
        "index.html",
        "<div id=\"app\">Wide Widget content</div>",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/apps/wide-widget/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("<div class=\"app-embed\"><div id=\"app\">Wide Widget content</div></div>"),
        "composed entry document must be wrapped in the width-cap-exempt app-embed container: {body}"
    );
    Ok(())
}

#[tokio::test]
async fn a_js_kind_apps_own_non_html_asset_still_passes_through_raw() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write_app(
        vault_dir.path(),
        "task-widget",
        r#"
            name = "Task Widget"
            kind = "js"
            entry = "index.html"
        "#,
        "index.html",
        "<div id=\"app\"></div>",
    )?;
    write(
        vault_dir.path(),
        "registry/apps/task-widget/app.js",
        "console.log('task widget');",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/apps/task-widget/app.js")).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "console.log('task widget');");
    // Not composed into the shell: a raw JS asset carries none of the
    // shell's own chrome.
    assert!(!body.contains("shell-body"));
    Ok(())
}

#[tokio::test]
async fn a_js_kind_apps_unmatched_sub_path_404s_instead_of_falling_back_to_its_entry() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write_app(
        vault_dir.path(),
        "task-widget",
        r#"
            name = "Task Widget"
            kind = "js"
            entry = "index.html"
        "#,
        "index.html",
        "<div id=\"app\">Task Widget content</div>",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = get(&router, &format!("/{VAULT_NAME}/apps/task-widget/no/such/path")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

// ---------------------------------------------------------------------
// `target` field removed from the schema
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_manifest_declaring_the_removed_target_field_fails_registration_without_blocking_other_apps()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write_app(
        vault_dir.path(),
        "old-schema",
        r#"
            name = "Old Schema App"
            kind = "js"
            entry = "index.html"
            target = "_blank"
        "#,
        "index.html",
        "<html></html>",
    )?;
    write_app(
        vault_dir.path(),
        "healthy",
        r#"
            name = "Healthy App"
            kind = "js"
            entry = "index.html"
        "#,
        "index.html",
        "<html><body>Healthy</body></html>",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/apps/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Healthy App"));
    assert!(!body.contains("Old Schema App"));
    Ok(())
}

// ---------------------------------------------------------------------
// `htmx`-kind: registers, listed as not-yet-supported, distinct 404
// ---------------------------------------------------------------------

#[tokio::test]
async fn an_htmx_kind_app_is_listed_not_yet_supported_and_returns_a_distinct_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write_app(
        vault_dir.path(),
        "live-widget",
        r#"
            name = "Live Widget"
            kind = "htmx"
            entry = "widget.html"
        "#,
        "widget.html",
        "<div>widget</div>",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/apps/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Live Widget"));
    assert!(body.contains("not yet supported"));

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/apps/live-widget/")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("app/not-yet-supported"));

    // Distinct from a plain unregistered slug's 404 body.
    let (status, body) = get(&router, &format!("/{VAULT_NAME}/apps/does-not-exist/")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("route/not-found"));
    Ok(())
}

// ---------------------------------------------------------------------
// GET /{{vault_name}}/apps/ listing
// ---------------------------------------------------------------------

#[tokio::test]
async fn the_listing_route_reports_no_apps_for_an_empty_registry() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = get(&router, &format!("/{VAULT_NAME}/apps/")).await?;
    assert_eq!(status, StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn an_unconfigured_vault_name_is_a_404_for_the_apps_listing() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _) = get(&router, "/does-not-exist/apps/").await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

// ---------------------------------------------------------------------
// Cross-vault isolation
// ---------------------------------------------------------------------

#[tokio::test]
async fn two_vaults_registering_the_same_slug_serve_independently() -> Result<(), BoxError> {
    let vault_a = tempfile::tempdir()?;
    let vault_b = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_a.path(), "index.md", "# Vault A\n")?;
    write(vault_b.path(), "index.md", "# Vault B\n")?;
    write_app(
        vault_a.path(),
        "widget",
        r#"
            name = "Widget A"
            kind = "spa"
            entry = "index.html"
        "#,
        "index.html",
        "<html><body>Widget from vault A</body></html>",
    )?;
    write_app(
        vault_b.path(),
        "widget",
        r#"
            name = "Widget B"
            kind = "spa"
            entry = "index.html"
        "#,
        "index.html",
        "<html><body>Widget from vault B</body></html>",
    )?;
    let router =
        router_over_two_vaults(vault_a.path(), "vault-a", vault_b.path(), "vault-b", config_dir.path()).await?;

    let (status, body) = get(&router, "/vault-a/apps/widget/").await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Widget from vault A"));

    let (status, body) = get(&router, "/vault-b/apps/widget/").await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Widget from vault B"));
    Ok(())
}

// ---------------------------------------------------------------------
// Rescan
// ---------------------------------------------------------------------

#[tokio::test]
async fn rescan_discovers_an_app_added_after_the_first_request() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/apps/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("Freshly Added"));

    write_app(
        vault_dir.path(),
        "freshly-added",
        r#"
            name = "Freshly Added"
            kind = "spa"
            entry = "index.html"
        "#,
        "index.html",
        "<html></html>",
    )?;

    let status = post(&router, &format!("/{VAULT_NAME}/apps/rescan")).await?;
    assert_eq!(status, StatusCode::SEE_OTHER);

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/apps/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Freshly Added"));
    Ok(())
}

// ---------------------------------------------------------------------
// Favouriting
// ---------------------------------------------------------------------

#[tokio::test]
async fn toggling_favourite_marks_the_list_cards_star_pressed() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write_app(
        vault_dir.path(),
        "task-register",
        r#"
            name = "Task Register Dashboard"
            kind = "spa"
            entry = "index.html"
        "#,
        "index.html",
        "<html></html>",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_status, body) = get(&router, &format!("/{VAULT_NAME}/apps/")).await?;
    assert!(body.contains("aria-pressed=\"false\""));

    let (status, body) = patch_favourite(&router, &format!("/{VAULT_NAME}/apps/task-register"), true).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("aria-pressed=\"true\""));

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/apps/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("aria-pressed=\"true\""));
    Ok(())
}

#[tokio::test]
async fn setting_favourite_false_after_true_returns_to_unfavourited() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write_app(
        vault_dir.path(),
        "task-register",
        r#"
            name = "Task Register Dashboard"
            kind = "spa"
            entry = "index.html"
        "#,
        "index.html",
        "<html></html>",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    patch_favourite(&router, &format!("/{VAULT_NAME}/apps/task-register"), true).await?;
    patch_favourite(&router, &format!("/{VAULT_NAME}/apps/task-register"), false).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/apps/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("aria-pressed=\"false\""));
    Ok(())
}

#[tokio::test]
async fn setting_favourite_for_an_unregistered_slug_404s() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = patch_favourite(&router, &format!("/{VAULT_NAME}/apps/no-such-app"), true).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn a_favourited_app_is_linked_from_the_sidebars_apps_group_on_every_page() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write_app(
        vault_dir.path(),
        "task-register",
        r#"
            name = "Task Register Dashboard"
            kind = "spa"
            entry = "index.html"
        "#,
        "index.html",
        "<html></html>",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_status, body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert!(
        !body.contains("Task Register Dashboard"),
        "an unfavourited app must not appear in the sidebar"
    );

    let (status, _body) = patch_favourite(&router, &format!("/{VAULT_NAME}/apps/task-register"), true).await?;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Task Register Dashboard"));
    assert!(body.contains(&format!("/{VAULT_NAME}/apps/task-register/\" target=\"_blank\"")));
    Ok(())
}
