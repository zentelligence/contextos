//! The shell's page title bar: every navigation destination shows its own icon and name at the top of the main
//! content panel, with the un/favourite star at the far right; the sidebar itself carries no stars. Real
//! `contextos-mcp` sessions against real temporary vault fixtures. No mocking, per `testing.md`.

mod support;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use contextos_web::McpClientSet;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";

/// Every favouritable destination: the request path, the view identifier the star toggle patches, the page name,
/// and the icon class the sidebar shows for the same destination.
const DESTINATIONS: &[(&str, &str, &str, &str)] = &[
    ("", "dashboard", "Dashboard", "ic-layout-dashboard"),
    ("tasks/", "tasks", "Tasks", "ic-list-checks"),
    ("inbox/", "inbox", "Inbox", "ic-inbox"),
    ("outbox/", "outbox", "Outbox", "ic-send"),
    ("journal/", "journal", "Journal", "ic-book-open"),
    ("notes/", "notes", "Notes", "ic-file-text"),
    ("agents/", "agents", "Agents", "ic-agents"),
    ("agents/?team=general", "agents-general", "General agents", "ic-globe"),
    (
        "agents/?team=business",
        "agents-business",
        "Business agents",
        "ic-briefcase",
    ),
    (
        "agents/?team=personal",
        "agents-personal",
        "Personal agents",
        "ic-user-round",
    ),
    (
        "agents/?state=blocked",
        "agents-blocked",
        "Blocked agents",
        "ic-octagon-alert",
    ),
    ("wiki/", "wiki", "Wiki", "ic-book"),
    ("insights/", "insights", "Insights", "ic-sparkles"),
    ("apps/", "apps", "Apps", "ic-apps"),
    ("workflows", "workflows", "Workflows", "ic-jobs"),
];

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

/// `GET`s `path`, following redirects (the Journal lands on today's date), and returns the final status and body.
async fn get(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    let mut target = path.to_owned();
    for _ in 0..3 {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(&target).body(Body::empty())?)
            .await?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(header::LOCATION)
                .ok_or("redirect without a Location header")?;
            target = location.to_str()?.to_owned();
            continue;
        }
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        return Ok((status, String::from_utf8_lossy(&body).into_owned()));
    }
    Err(format!("{path}: too many redirects").into())
}

async fn patch_favourite(router: &Router, path: &str, favourite: bool) -> Result<StatusCode, BoxError> {
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
    Ok(response.status())
}

/// The sidebar's own markup: everything from `<aside` to its closing tag.
fn sidebar(body: &str) -> Result<&str, BoxError> {
    let start = body.find("<aside").ok_or("page has no sidebar")?;
    let end = body[start..].find("</aside>").ok_or("unterminated sidebar")?;
    Ok(&body[start..start + end])
}

/// The title bar's own markup: everything from its opening tag to its closing tag.
fn title_bar(body: &str) -> Result<&str, BoxError> {
    let start = body
        .find("<header class=\"page-title\"")
        .ok_or("page has no title bar")?;
    let end = body[start..].find("</header>").ok_or("unterminated title bar")?;
    Ok(&body[start..start + end])
}

async fn fixture() -> Result<(tempfile::TempDir, tempfile::TempDir, Router), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    std::fs::write(vault.path().join("index.md"), "# Root\n")?;
    std::fs::create_dir_all(vault.path().join("memory/events"))?;
    // The index note's own text is operator-authored content and is rendered as written, so the fixture carries no
    // heading of its own: the page name must come from the title bar alone.
    std::fs::write(
        vault.path().join("memory/events/index.md"),
        "Recent signals are listed here.\n",
    )?;
    std::fs::create_dir_all(vault.path().join("memory/wiki"))?;
    std::fs::write(vault.path().join("memory/wiki/some-note.md"), "Body of the note.\n")?;
    let router = router_over(vault.path(), config.path()).await?;
    Ok((vault, config, router))
}

#[tokio::test]
async fn every_destination_shows_its_icon_and_name_in_a_title_bar_at_the_top_of_the_content() -> Result<(), BoxError> {
    let (_vault, _config, router) = fixture().await?;
    for (path, _view, name, icon) in DESTINATIONS {
        let (status, body) = get(&router, &format!("/{VAULT_NAME}/{path}")).await?;
        assert_eq!(status, StatusCode::OK, "{path}");
        let bar = title_bar(&body).map_err(|error| format!("{path}: {error}"))?;
        assert!(bar.contains(&format!("class=\"ic {icon}\"")), "{path}: icon in {bar}");
        assert!(bar.contains(&format!("<h1>{name}</h1>")), "{path}: name in {bar}");
        let content = &body[body.find("<main class=\"content\">").ok_or("no main content")?..];
        assert!(
            content["<main class=\"content\">".len()..]
                .trim_start()
                .starts_with("<header class=\"page-title\""),
            "{path}: the title bar must be the first thing in the content panel"
        );
        // Markdown headings carry an id attribute, so count both spellings.
        let headings = body.matches("<h1>").count() + body.matches("<h1 ").count();
        assert_eq!(headings, 1, "{path}: the page name must appear once");
    }
    Ok(())
}

#[tokio::test]
async fn the_title_bar_carries_the_favourite_star_and_the_sidebar_carries_none() -> Result<(), BoxError> {
    let (_vault, _config, router) = fixture().await?;
    for (path, view, name, _icon) in DESTINATIONS {
        let (_status, body) = get(&router, &format!("/{VAULT_NAME}/{path}")).await?;
        let bar = title_bar(&body).map_err(|error| format!("{path}: {error}"))?;
        assert!(
            bar.contains(&format!("hx-patch=\"/{VAULT_NAME}/views/{view}\"")),
            "{path}: star patches its own view in {bar}"
        );
        assert!(bar.contains("favourite-toggle"), "{path}");
        assert!(bar.contains(&format!("Add {name} to favourites")), "{path}");
        let side = sidebar(&body)?;
        assert!(!side.contains("favourite-toggle"), "{path}: sidebar must carry no star");
        assert!(
            !side.contains("app-favourite-form"),
            "{path}: sidebar must carry no star form"
        );
        assert!(!side.contains("ic-star"), "{path}: sidebar must carry no star icon");
    }
    Ok(())
}

#[tokio::test]
async fn the_star_reflects_the_stored_favourite_state_and_toggles_it_off_again() -> Result<(), BoxError> {
    let (_vault, _config, router) = fixture().await?;
    let (_status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;
    assert!(title_bar(&body)?.contains("aria-pressed=\"false\""));

    assert_eq!(
        patch_favourite(&router, &format!("/{VAULT_NAME}/views/tasks"), true).await?,
        StatusCode::OK
    );
    let (_status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;
    let bar = title_bar(&body)?;
    assert!(bar.contains("aria-pressed=\"true\""));
    assert!(
        bar.contains("hx-vals='{\"favourite\": false}'"),
        "a pressed star must submit false: {bar}"
    );

    let (_status, other) = get(&router, &format!("/{VAULT_NAME}/inbox/")).await?;
    assert!(
        title_bar(&other)?.contains("aria-pressed=\"false\""),
        "favouriting one view must not favourite another"
    );

    assert_eq!(
        patch_favourite(&router, &format!("/{VAULT_NAME}/views/tasks"), false).await?,
        StatusCode::OK
    );
    let (_status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;
    assert!(title_bar(&body)?.contains("aria-pressed=\"false\""));
    Ok(())
}

#[tokio::test]
async fn an_agents_filter_that_matches_no_named_view_shows_the_plain_agents_title() -> Result<(), BoxError> {
    let (_vault, _config, router) = fixture().await?;
    for query in [
        "?team=business&state=blocked",
        "?state=working",
        "?team=other",
        "?entity=acme",
    ] {
        let (_status, body) = get(&router, &format!("/{VAULT_NAME}/agents/{query}")).await?;
        let bar = title_bar(&body).map_err(|error| format!("{query}: {error}"))?;
        assert!(bar.contains("<h1>Agents</h1>"), "{query}: {bar}");
        assert!(
            bar.contains(&format!("hx-patch=\"/{VAULT_NAME}/views/agents\"")),
            "{query}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_favouritable_agents_filter_keeps_its_own_title_alongside_the_entity_selection() -> Result<(), BoxError> {
    let (_vault, _config, router) = fixture().await?;
    let (_status, body) = get(&router, &format!("/{VAULT_NAME}/agents/?team=business&entity=acme")).await?;
    let bar = title_bar(&body)?;
    assert!(bar.contains("<h1>Business agents</h1>"), "{bar}");
    Ok(())
}

#[tokio::test]
async fn pages_that_are_not_navigation_destinations_show_no_title_bar() -> Result<(), BoxError> {
    let (_vault, _config, router) = fixture().await?;
    for path in ["settings/", "profile/", "search?q=root&mode=text", "tasks/new"] {
        let (status, body) = get(&router, &format!("/{VAULT_NAME}/{path}")).await?;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        assert!(!body.contains("class=\"page-title\""), "{path}");
    }
    Ok(())
}

#[tokio::test]
async fn a_file_within_a_split_pane_screen_keeps_that_screens_name_and_its_star_favourites_the_file()
-> Result<(), BoxError> {
    let (_vault, _config, router) = fixture().await?;
    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/some-note.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let bar = title_bar(&body)?;
    assert!(bar.contains("<h1>Wiki</h1>"), "{bar}");
    assert!(
        bar.contains(&format!(
            "hx-patch=\"/{VAULT_NAME}/favourites/memory/wiki/some-note.md\""
        )),
        "{bar}"
    );
    assert!(!bar.contains("/views/wiki"), "{bar}");
    Ok(())
}

#[tokio::test]
async fn an_htmx_fragment_request_returns_the_content_without_the_shell_title_bar() -> Result<(), BoxError> {
    let (_vault, _config, router) = fixture().await?;
    for path in ["tasks/", "agents/", "apps/", "workflows", "inbox/", "outbox/", "notes/"] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/{VAULT_NAME}/{path}"))
                    .header("hx-request", "true")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let body = String::from_utf8_lossy(&to_bytes(response.into_body(), usize::MAX).await?).into_owned();
        assert!(!body.contains("page-title"), "{path}: fragments carry no title bar");
        assert!(!body.contains("<h1"), "{path}: the page name is the title bar's alone");
    }
    Ok(())
}

/// The title bar's action controls sit immediately before the star, at the far right, and the page's own content
/// carries no second copy.
fn assert_action_in_title_bar_before_the_star(body: &str, needle: &str, label: &str) -> Result<(), BoxError> {
    let bar = title_bar(body)?;
    let action = bar
        .find(needle)
        .ok_or(format!("{label}: not in the title bar: {bar}"))?;
    let star = bar.find("favourite-toggle").ok_or("no star")?;
    assert!(action < star, "{label}: the action must come before the star: {bar}");
    let bar_start = body.find("<header class=\"page-title\"").ok_or("no title bar")?;
    let content = &body[bar_start + bar.len()..];
    assert!(
        !content.contains(needle),
        "{label}: the page content must not repeat the action"
    );
    Ok(())
}

#[tokio::test]
async fn the_inbox_upload_action_sits_in_the_title_bar_next_to_the_star() -> Result<(), BoxError> {
    let (_vault, _config, router) = fixture().await?;
    let (_status, body) = get(&router, &format!("/{VAULT_NAME}/inbox/")).await?;
    assert_action_in_title_bar_before_the_star(&body, "inbox-upload-input", "inbox upload")?;
    assert!(
        title_bar(&body)?.contains(&format!("hx-post=\"/{VAULT_NAME}/inbox/execute/upload\"")),
        "{}",
        title_bar(&body)?
    );
    assert!(
        title_bar(&body)?.contains("hx-target=\".inbox-screen\""),
        "the upload must swap the screen it no longer sits inside"
    );
    Ok(())
}

#[tokio::test]
async fn the_new_task_action_sits_in_the_title_bar_next_to_the_star() -> Result<(), BoxError> {
    let (_vault, _config, router) = fixture().await?;
    let (_status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;
    assert_action_in_title_bar_before_the_star(&body, &format!("href=\"/{VAULT_NAME}/tasks/new\""), "new task")
}

#[tokio::test]
async fn the_new_note_action_sits_in_the_title_bar_next_to_the_star() -> Result<(), BoxError> {
    let (_vault, _config, router) = fixture().await?;
    let (_status, body) = get(&router, &format!("/{VAULT_NAME}/notes/")).await?;
    assert_action_in_title_bar_before_the_star(&body, &format!("hx-get=\"/{VAULT_NAME}/notes/new\""), "new note")
}

#[tokio::test]
async fn the_apps_rescan_action_sits_in_the_title_bar_next_to_the_star() -> Result<(), BoxError> {
    let (_vault, _config, router) = fixture().await?;
    let (_status, body) = get(&router, &format!("/{VAULT_NAME}/apps/")).await?;
    assert_action_in_title_bar_before_the_star(&body, &format!("action=\"/{VAULT_NAME}/apps/rescan\""), "rescan")
}

#[tokio::test]
async fn a_destination_with_no_action_has_a_title_bar_holding_only_the_star() -> Result<(), BoxError> {
    let (_vault, _config, router) = fixture().await?;
    let (_status, body) = get(&router, &format!("/{VAULT_NAME}/outbox/")).await?;
    let bar = title_bar(&body)?;
    assert!(!bar.contains("<form action") && !bar.contains("class=\"btn"), "{bar}");
    Ok(())
}

#[tokio::test]
async fn the_inbox_upload_action_targets_the_folder_being_browsed_and_is_absent_from_the_processed_tree()
-> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    std::fs::write(vault.path().join("index.md"), "# Root\n")?;
    std::fs::create_dir_all(vault.path().join("inbox/2026-09"))?;
    std::fs::write(vault.path().join("inbox/2026-09/receipt.md"), "# Receipt\n")?;
    std::fs::create_dir_all(vault.path().join("processed/2026-08"))?;
    std::fs::write(vault.path().join("processed/2026-08/old.md"), "# Old\n")?;
    let router = router_over(vault.path(), config.path()).await?;

    let (_status, body) = get(&router, &format!("/{VAULT_NAME}/inbox/2026-09")).await?;
    assert!(
        title_bar(&body)?.contains(&format!("hx-post=\"/{VAULT_NAME}/inbox/2026-09/execute/upload\"")),
        "{}",
        title_bar(&body)?
    );

    let (_status, body) = get(&router, &format!("/{VAULT_NAME}/inbox/processed/2026-08")).await?;
    assert!(
        !title_bar(&body)?.contains("inbox-upload-input"),
        "{}",
        title_bar(&body)?
    );
    Ok(())
}
