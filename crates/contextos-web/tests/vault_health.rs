//! Vault health as the operator meets it: the Vault Health pane, the header and footer indicators, and the two-step
//! automatic repair, all driven through the real router against a real `contextos` session and a real temporary
//! vault. The checks themselves belong to the server; these tests prove the web UI reads them faithfully, shares one
//! assessment between surfaces, and never writes without an explicit, previewed confirmation.

mod support;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use contextos_web::McpClientSet;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT: &str = "health-fixture";

/// A vault with Git off, so the only automatic finding is the missing managed index and the run is deterministic.
// `Debug`, not `Display`, is the correct formatting for a TOML string value: it quotes and escapes the path, as
// `support::write_vault_config` also relies on.
#[allow(clippy::unnecessary_debug_formatting)]
async fn router_over(vault_dir: &Path, config_dir: &Path) -> Result<Router, BoxError> {
    let config_path = config_dir.join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            "[[vault]]\npath = {vault_dir:?}\nname = \"{VAULT}\"\nstate_directory = \".contextos\"\n\
             [vault.git]\nenabled = false\n[vault.search]\ntext = false\ngraph = false\n"
        ),
    )?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(contextos_web::build_router(
        clients,
        Some(config_dir),
        &config_dir.join("web.toml"),
    ))
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

async fn send(router: &Router, method: Method, path: &str, htmx: bool) -> Result<Reply, BoxError> {
    send_with(router, method, path, htmx, &[]).await
}

async fn send_with(
    router: &Router,
    method: Method,
    path: &str,
    htmx: bool,
    extra_headers: &[(&str, &str)],
) -> Result<Reply, BoxError> {
    let mut request = Request::builder().method(method).uri(path);
    if htmx {
        request = request.header("hx-request", "true");
    }
    for (name, value) in extra_headers {
        request = request.header(*name, *value);
    }
    let response = router.clone().oneshot(request.body(Body::empty())?).await?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok(Reply {
        status,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

async fn get_fragment(router: &Router, path: &str) -> Result<Reply, BoxError> {
    send(router, Method::GET, path, true).await
}

async fn post_fragment(router: &Router, path: &str) -> Result<Reply, BoxError> {
    send(router, Method::POST, path, true).await
}

/// The plan digest in the confirmation's apply button.
fn plan_in(body: &str) -> Result<String, BoxError> {
    let marker = "execute/repair?plan=";
    let start = body.find(marker).ok_or("the confirmation offers no apply button")? + marker.len();
    let end = body[start..].find('"').ok_or("unterminated apply URL")?;
    Ok(body[start..start + end].to_owned())
}

fn triggers_refresh(reply: &Reply) -> bool {
    reply
        .headers
        .get("hx-trigger")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("vault-health-changed"))
}

#[tokio::test]
async fn the_pane_reports_the_servers_findings_grouped_with_their_classification() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    std::fs::write(vault.path().join("broken.md"), "---\ntitle: Notes: bad\n---\nBody\n")?;
    let router = router_over(vault.path(), config.path()).await?;

    let reply = get_fragment(&router, &format!("/{VAULT}/health")).await?;

    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.contains("id=\"vault-health\""), "{}", reply.body);
    assert!(!reply.body.contains("<html"), "a fragment carries no page chrome");
    assert!(reply.body.contains("Frontmatter validity"), "{}", reply.body);
    assert!(reply.body.contains("broken.md"), "{}", reply.body);
    assert!(reply.body.contains("Needs you"), "{}", reply.body);
    assert!(reply.body.contains("Managed indexes"), "{}", reply.body);
    assert!(reply.body.contains("Fixable automatically"), "{}", reply.body);
    Ok(())
}

#[tokio::test]
async fn a_plain_browser_visit_is_taken_to_the_settings_pane() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;

    let reply = send(&router, Method::GET, &format!("/{VAULT}/health"), false).await?;

    assert!(reply.status.is_redirection(), "{}", reply.status);
    assert_eq!(
        reply.headers.get("location").and_then(|value| value.to_str().ok()),
        Some(format!("/{VAULT}/settings/#health").as_str())
    );
    Ok(())
}

#[tokio::test]
async fn an_unknown_vault_is_not_found_on_every_health_route() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;

    for (method, path) in [
        (Method::GET, "/nowhere/health"),
        (Method::GET, "/nowhere/health/summary?surface=header"),
        (Method::POST, "/nowhere/health/execute/refresh"),
        (Method::POST, "/nowhere/health/execute/repair-preview"),
        (Method::POST, "/nowhere/health/execute/repair?plan=x"),
    ] {
        let reply = send(&router, method.clone(), path, true).await?;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{method} {path}");
    }
    Ok(())
}

#[tokio::test]
async fn the_header_and_footer_indicators_agree_with_the_pane() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    std::fs::write(vault.path().join("broken.md"), "---\ntitle: Notes: bad\n---\nBody\n")?;
    let router = router_over(vault.path(), config.path()).await?;

    let pane = get_fragment(&router, &format!("/{VAULT}/health")).await?;
    let header = get_fragment(&router, &format!("/{VAULT}/health/summary?surface=header")).await?;
    let footer = get_fragment(&router, &format!("/{VAULT}/health/summary?surface=footer")).await?;

    // Two failing findings: the frontmatter one, and the missing index.
    assert!(pane.body.contains("2 checks need attention."), "{}", pane.body);
    for chip in [&header.body, &footer.body] {
        assert!(chip.contains("2 issues"), "{chip}");
        assert!(chip.contains("health-attention"), "{chip}");
        assert!(chip.contains(&format!("/{VAULT}/settings/#health")), "{chip}");
    }
    assert!(header.body.contains("header-icon-button"), "{}", header.body);
    assert!(footer.body.contains("status-bar-item"), "{}", footer.body);
    Ok(())
}

#[tokio::test]
async fn an_unknown_indicator_surface_is_rejected() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;

    let reply = get_fragment(&router, &format!("/{VAULT}/health/summary?surface=sidebar")).await?;

    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let missing = get_fragment(&router, &format!("/{VAULT}/health/summary")).await?;
    assert_eq!(missing.status, StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn one_assessment_serves_every_surface_until_an_explicit_refresh() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;
    let before = get_fragment(&router, &format!("/{VAULT}/health/summary?surface=header")).await?;
    assert!(
        before.body.contains("1 issue"),
        "only the missing index: {}",
        before.body
    );

    // A new fault appears on disk. Nothing has asked for a fresh assessment, so the surfaces still agree.
    std::fs::write(vault.path().join("broken.md"), "---\ntitle: Notes: bad\n---\nBody\n")?;
    let cached_header = get_fragment(&router, &format!("/{VAULT}/health/summary?surface=header")).await?;
    let cached_footer = get_fragment(&router, &format!("/{VAULT}/health/summary?surface=footer")).await?;
    assert!(cached_header.body.contains("1 issue"), "{}", cached_header.body);
    assert!(cached_footer.body.contains("1 issue"), "{}", cached_footer.body);

    let refreshed = post_fragment(&router, &format!("/{VAULT}/health/execute/refresh")).await?;

    assert_eq!(refreshed.status, StatusCode::OK);
    assert!(triggers_refresh(&refreshed), "{:?}", refreshed.headers);
    assert!(
        refreshed.body.contains("2 checks need attention."),
        "{}",
        refreshed.body
    );
    let after = get_fragment(&router, &format!("/{VAULT}/health/summary?surface=footer")).await?;
    assert!(after.body.contains("2 issues"), "{}", after.body);
    Ok(())
}

#[tokio::test]
async fn previewing_a_repair_lists_what_it_would_do_and_writes_nothing() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;

    let reply = post_fragment(&router, &format!("/{VAULT}/health/execute/repair-preview")).await?;

    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.contains("This will write to your vault."), "{}", reply.body);
    assert!(reply.body.contains("Nothing has been changed yet."), "{}", reply.body);
    assert!(reply.body.contains("Managed indexes"), "{}", reply.body);
    assert!(
        !triggers_refresh(&reply),
        "a preview changes nothing, so nothing needs refreshing"
    );
    assert!(!vault.path().join("index.md").exists(), "a preview must not write");
    Ok(())
}

#[tokio::test]
async fn a_confirmed_repair_writes_reports_and_refreshes_every_indicator() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;
    let preview = post_fragment(&router, &format!("/{VAULT}/health/execute/repair-preview")).await?;
    let plan = plan_in(&preview.body)?;

    let applied = post_fragment(&router, &format!("/{VAULT}/health/execute/repair?plan={plan}")).await?;

    assert_eq!(applied.status, StatusCode::OK);
    assert!(
        vault.path().join("index.md").exists(),
        "the confirmed repair must write"
    );
    assert!(applied.body.contains("Repaired 1 finding."), "{}", applied.body);
    assert!(triggers_refresh(&applied), "{:?}", applied.headers);
    assert!(applied.body.contains("Every check passed."), "{}", applied.body);
    // The repair's own report is the cached assessment: every surface now agrees without another scan.
    let chip = get_fragment(&router, &format!("/{VAULT}/health/summary?surface=header")).await?;
    assert!(chip.body.contains("health-ok"), "{}", chip.body);
    Ok(())
}

#[tokio::test]
async fn a_repair_whose_plan_no_longer_matches_is_not_run() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;

    let reply = post_fragment(
        &router,
        &format!("/{VAULT}/health/execute/repair?plan=not-what-was-reviewed"),
    )
    .await?;

    assert_eq!(reply.status, StatusCode::OK);
    assert!(
        reply.body.contains("has changed since you last looked"),
        "{}",
        reply.body
    );
    assert!(reply.body.contains("This will write to your vault."), "{}", reply.body);
    assert!(
        !vault.path().join("index.md").exists(),
        "nothing runs on a stale confirmation"
    );
    assert!(!triggers_refresh(&reply));
    Ok(())
}

#[tokio::test]
async fn a_repair_without_a_plan_is_rejected() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;

    let reply = post_fragment(&router, &format!("/{VAULT}/health/execute/repair")).await?;

    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert!(!vault.path().join("index.md").exists());
    Ok(())
}

#[tokio::test]
async fn a_repair_can_only_be_asked_for_by_post() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;

    for path in ["execute/repair?plan=x", "execute/repair-preview", "execute/refresh"] {
        let reply = get_fragment(&router, &format!("/{VAULT}/health/{path}")).await?;
        assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED, "GET {path}");
    }
    assert!(!vault.path().join("index.md").exists());
    Ok(())
}

#[tokio::test]
async fn every_vault_page_carries_both_indicators_and_the_settings_pane_loads_the_report() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;

    let page = send(&router, Method::GET, &format!("/{VAULT}/settings/"), false).await?;

    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body
            .contains(&format!("hx-get=\"/{VAULT}/health/summary?surface=header\"")),
        "no header indicator"
    );
    assert!(
        page.body
            .contains(&format!("hx-get=\"/{VAULT}/health/summary?surface=footer\"")),
        "no footer indicator"
    );
    assert!(
        page.body.contains("data-pane=\"health\""),
        "no Vault health entry in the settings menu"
    );
    assert!(page.body.contains("id=\"pane-health\""), "no Vault health pane");
    assert!(
        page.body.contains(&format!("hx-get=\"/{VAULT}/health\"")),
        "the pane does not load the report"
    );
    Ok(())
}

#[tokio::test]
async fn on_a_page_outside_any_vault_the_indicators_follow_the_vault_the_header_actions_target() -> Result<(), BoxError>
{
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;
    // The vault-independent page reads `web.toml`, so it must exist for this page to render at all.
    std::fs::write(config.path().join("web.toml"), "")?;

    let page = send(&router, Method::GET, "/server/settings/", false).await?;

    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body.contains("aria-label=\"Vault settings\"")
            && page.body.contains(&format!("href=\"/{VAULT}/settings/\"")),
        "the header's vault settings icon targets the default vault"
    );
    assert!(
        page.body
            .contains(&format!("hx-get=\"/{VAULT}/health/summary?surface=header\"")),
        "the health icon must target the same vault"
    );
    Ok(())
}

#[tokio::test]
async fn a_request_from_another_site_can_neither_repair_preview_nor_refresh() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;
    let preview = post_fragment(&router, &format!("/{VAULT}/health/execute/repair-preview")).await?;
    let plan = plan_in(&preview.body)?;

    for headers in [
        vec![("sec-fetch-site", "cross-site")],
        vec![("host", "localhost:8080"), ("origin", "https://evil.example")],
    ] {
        for path in [
            format!("/{VAULT}/health/execute/repair?plan={plan}"),
            format!("/{VAULT}/health/execute/repair-preview"),
            format!("/{VAULT}/health/execute/refresh"),
        ] {
            let reply = send_with(&router, Method::POST, &path, true, &headers).await?;
            assert_eq!(reply.status, StatusCode::FORBIDDEN, "{path} with {headers:?}");
        }
    }
    assert!(
        !vault.path().join("index.md").exists(),
        "a cross-site request must not write"
    );
    Ok(())
}

#[tokio::test]
async fn a_same_origin_browser_request_is_allowed() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;

    let reply = send_with(
        &router,
        Method::POST,
        &format!("/{VAULT}/health/execute/refresh"),
        true,
        &[("sec-fetch-site", "same-origin")],
    )
    .await?;

    assert_eq!(reply.status, StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn a_plan_that_no_longer_matches_because_a_finding_went_away_is_sent_back_for_review() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;
    let preview = post_fragment(&router, &format!("/{VAULT}/health/execute/repair-preview")).await?;
    let reviewed = plan_in(&preview.body)?;
    // Someone else repairs the vault first, so the plan the operator reviewed no longer describes anything.
    let first = post_fragment(&router, &format!("/{VAULT}/health/execute/repair?plan={reviewed}")).await?;
    assert!(first.body.contains("Repaired 1 finding."), "{}", first.body);
    let written = std::fs::read(vault.path().join("index.md"))?;

    let late = post_fragment(&router, &format!("/{VAULT}/health/execute/repair?plan={reviewed}")).await?;

    assert_eq!(late.status, StatusCode::OK);
    assert!(late.body.contains("has changed since you last looked"), "{}", late.body);
    assert!(
        late.body.contains("Nothing can be repaired automatically right now."),
        "{}",
        late.body
    );
    assert!(!triggers_refresh(&late), "a refused repair changes nothing");
    assert_eq!(
        std::fs::read(vault.path().join("index.md"))?,
        written,
        "the vault must be untouched"
    );
    Ok(())
}
