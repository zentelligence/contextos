//! A base, a canvas, or a diagram opens on the screen that owns its folder, inside that screen's navigation, never on a
//! page of its own; the generic vault route sends one there, refuses one no screen owns, and keeps serving a base's
//! view to the tabs that switch it in place. Real `contextos-mcp` sessions against real temporary vaults.

mod support;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use contextos_web::McpClientSet;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT: &str = "contract-fixture";

const CANVAS: &str =
    r#"{"nodes":[{"id":"n1","type":"text","x":0,"y":0,"width":200,"height":60,"text":"Node **one**"}],"edges":[]}"#;

const BASE: &str = "filters: file.inFolder(\"memory/wiki/records\")\nviews:\n  - type: table\n    name: \"Active\"\n    order:\n      - file.path\n  - type: table\n    name: \"All\"\n    order:\n      - file.path\n";

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

fn vault() -> Result<tempfile::TempDir, BoxError> {
    let vault = tempfile::tempdir()?;
    write(vault.path(), "index.md", "# Root\n")?;
    write(vault.path(), "memory/wiki/topic/plan.base", BASE)?;
    write(
        vault.path(),
        "memory/wiki/records/one.md",
        "---\ntitle: One\n---\n# One\n",
    )?;
    write(vault.path(), "memory/wiki/topic/board.canvas", CANVAS)?;
    write(vault.path(), "memory/wiki/topic/flow.mermaid", "graph TD\n  A --> B\n")?;
    write(vault.path(), "loose/stray.base", BASE)?;
    write(
        vault.path(),
        "memory/tasks.base",
        "filters: file.inFolder(\"memory/tasks\")\nviews:\n  - type: table\n    name: All\n",
    )?;
    write(
        vault.path(),
        "memory/wiki/topic/links.md",
        "# Links\n\n[[plan.base]] [[board.canvas]] [[flow.mermaid]] [[stray.base]] [[tasks.base]]\n",
    )?;
    Ok(vault)
}

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

/// The status, the `Location` or `HX-Redirect` header, and the body of a `GET`, as htmx when `htmx` is set.
async fn get(router: &Router, path: &str, htmx: bool) -> Result<(StatusCode, Option<String>, String), BoxError> {
    let mut request = Request::builder().uri(path);
    if htmx {
        request = request.header("hx-request", "true");
    }
    let response = router.clone().oneshot(request.body(Body::empty())?).await?;
    let status = response.status();
    let location = ["location", "hx-redirect"].iter().find_map(|name| {
        response
            .headers()
            .get(*name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    });
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, location, String::from_utf8_lossy(&body).into_owned()))
}

#[tokio::test]
async fn links_to_page_files_go_to_the_screen_that_owns_them_or_say_none_does() -> Result<(), BoxError> {
    let vault = vault()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;

    let (status, _, page) = get(&router, &format!("/{VAULT}/wiki/topic/links.md"), false).await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    for href in [
        "/contract-fixture/wiki/topic/plan.base",
        "/contract-fixture/wiki/topic/board.canvas",
        "/contract-fixture/wiki/topic/flow.mermaid",
        "/contract-fixture/tasks/",
    ] {
        assert!(page.contains(&format!("href=\"{href}\"")), "{href}: {page}");
    }
    assert!(
        page.contains("<span class=\"wikilink dead unrouted\" data-target=\"loose/stray.base\""),
        "{page}"
    );
    Ok(())
}

#[tokio::test]
async fn a_base_canvas_or_diagram_shows_inside_its_owning_screen_without_note_controls() -> Result<(), BoxError> {
    let vault = vault()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;

    for (file, shown) in [
        ("plan.base", "class=\"base-view\""),
        ("board.canvas", "<svg class=\"canvas-svg\""),
        ("flow.mermaid", "class=\"mermaid-diagram\""),
    ] {
        let (status, _, page) = get(&router, &format!("/{VAULT}/wiki/topic/{file}"), false).await?;
        assert_eq!(status, StatusCode::OK, "{file}: {page}");
        assert!(
            page.contains("class=\"wiki-screen"),
            "{file} is not inside the wiki screen: {page}"
        );
        assert!(page.contains(shown), "{file}: {page}");
        assert!(
            !page.contains("aria-label=\"Edit page\""),
            "{file} offers note editing: {page}"
        );
        assert!(
            !page.contains("notes-metadata-panel"),
            "{file} shows a note's metadata panel: {page}"
        );
    }
    let (_, _, board) = get(&router, &format!("/{VAULT}/wiki/topic/board.canvas"), false).await?;
    assert!(
        board.contains("<strong>one</strong>"),
        "a text node is rendered Markdown: {board}"
    );
    Ok(())
}

#[tokio::test]
async fn the_generic_route_sends_a_page_file_to_its_screen_or_refuses_it() -> Result<(), BoxError> {
    let vault = vault()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;

    for (file, screen) in [
        ("memory/wiki/topic/plan.base", "/contract-fixture/wiki/topic/plan.base"),
        (
            "memory/wiki/topic/board.canvas",
            "/contract-fixture/wiki/topic/board.canvas",
        ),
        (
            "memory/wiki/topic/flow.mermaid",
            "/contract-fixture/wiki/topic/flow.mermaid",
        ),
        ("memory/tasks.base", "/contract-fixture/tasks/"),
    ] {
        let (status, location, _) = get(&router, &format!("/{VAULT}/{file}"), false).await?;
        assert_eq!(status, StatusCode::SEE_OTHER, "{file}");
        assert_eq!(location.as_deref(), Some(screen), "{file}");
    }
    let (status, _, _) = get(&router, &format!("/{VAULT}/loose/stray.base"), false).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn a_base_views_tabs_still_switch_it_in_place() -> Result<(), BoxError> {
    let vault = vault()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;

    let (status, _, fragment) = get(&router, &format!("/{VAULT}/memory/wiki/topic/plan.base?view=All"), true).await?;
    assert_eq!(status, StatusCode::OK, "{fragment}");
    assert!(fragment.contains("class=\"base-view\""), "{fragment}");
    assert!(
        !fragment.contains("<html"),
        "an htmx view switch gets the view alone: {fragment}"
    );

    // A base in a folder whose name is a screen's address gets its view from that screen, the view alone.
    write(vault.path(), "notes/2026/plan.base", BASE)?;
    let (status, _, fragment) = get(&router, &format!("/{VAULT}/notes/2026/plan.base?view=All"), true).await?;
    assert_eq!(status, StatusCode::OK, "{fragment}");
    assert!(fragment.starts_with("<div class=\"base-view\""), "{fragment}");
    assert!(!fragment.contains("notes-screen"), "{fragment}");
    Ok(())
}

#[tokio::test]
async fn every_screen_that_owns_a_folder_shows_its_page_files() -> Result<(), BoxError> {
    let vault = vault()?;
    write(
        vault.path(),
        ".contextos/entities.toml",
        "[[entities]]\nslug = \"zenith\"\nname = \"Zenith\"\naccent = 1\n",
    )?;
    write(vault.path(), "zenith/projects/board.canvas", CANVAS)?;
    write(vault.path(), "memory/insights/f/flow.mermaid", "graph TD\n  A --> B\n")?;
    write(vault.path(), "notes/2026/board.canvas", CANVAS)?;
    // The Notes screen starts a first note in an empty notes folder, so this one has a note already.
    write(vault.path(), "notes/2026/one.md", "---\ntitle: One\n---\nText.\n")?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;

    for (url, screen, shown) in [
        (
            "/contract-fixture/entity/zenith/projects/board.canvas",
            "entity-docs-screen",
            "<svg class=\"canvas-svg\"",
        ),
        (
            "/contract-fixture/insights/f/flow.mermaid",
            "insights-screen",
            "class=\"mermaid-diagram\"",
        ),
        (
            "/contract-fixture/notes/2026/board.canvas",
            "notes-screen",
            "<svg class=\"canvas-svg\"",
        ),
    ] {
        let (status, _, page) = get(&router, url, false).await?;
        assert_eq!(status, StatusCode::OK, "{url}: {page}");
        assert!(page.contains(screen), "{url} is not on its screen: {page}");
        assert!(page.contains(shown), "{url}: {page}");
        assert!(
            !page.contains("notes-metadata-panel"),
            "{url} shows a note's metadata panel: {page}"
        );
        // The title bar's download asks the screen for the file as stored, whatever screen shares its folder's name.
        let link = page
            .split("class=\"icon-button title-download\" href=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .ok_or_else(|| format!("{url} offers no download: {page}"))?
            .replace("&amp;", "&");
        assert_eq!(link, format!("{url}?raw=1"));
        let (status, _, source) = get(&router, &link, false).await?;
        assert_eq!(status, StatusCode::OK, "{link}");
        assert!(!source.contains("<html"), "{link} is a page, not the file: {source}");
    }
    Ok(())
}

#[tokio::test]
async fn a_missing_page_file_is_not_found_on_every_screen() -> Result<(), BoxError> {
    let vault = vault()?;
    write(
        vault.path(),
        ".contextos/entities.toml",
        "[[entities]]\nslug = \"zenith\"\nname = \"Zenith\"\naccent = 1\n",
    )?;
    write(vault.path(), "zenith/projects/one.md", "# One\n")?;
    write(
        vault.path(),
        "memory/insights/f/first.md",
        "---\nstatus: proposed\n---\n# First\n",
    )?;
    write(vault.path(), "notes/2026/one.md", "---\ntitle: One\n---\nText.\n")?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;

    for url in [
        "/contract-fixture/wiki/absent.canvas",
        "/contract-fixture/entity/zenith/projects/absent.base",
        "/contract-fixture/insights/f/absent.mermaid",
    ] {
        let (status, _, page) = get(&router, url, false).await?;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{url} shows a page file that does not exist: {page}"
        );
    }
    // The Notes screen answers a note it cannot find with its most recent note, as it always has, never the missing
    // file.
    let (status, _, page) = get(&router, "/contract-fixture/notes/2026/absent.canvas", false).await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(!page.contains("absent.canvas"), "{page}");
    assert!(page.contains("Text."), "the most recent note is not shown: {page}");
    Ok(())
}

/// The status of a request with a JSON body.
async fn send(router: &Router, method: &str, path: &str, body: &str) -> Result<StatusCode, BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .header("hx-request", "true")
                .body(Body::from(body.to_owned()))?,
        )
        .await?;
    Ok(response.status())
}

#[tokio::test]
async fn the_notes_screen_never_writes_a_base_canvas_or_diagram() -> Result<(), BoxError> {
    let vault = vault()?;
    write(vault.path(), "notes/2026/one.md", "---\ntitle: One\n---\nText.\n")?;
    write(vault.path(), "notes/2026/board.canvas", CANVAS)?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;

    for (method, url, body) in [
        (
            "PATCH",
            "/contract-fixture/notes/2026/board.canvas",
            r##"{"content":"# Overwritten\n"}"##,
        ),
        (
            "PATCH",
            "/contract-fixture/notes/2026/board.canvas/rename",
            r#"{"new_name":"renamed"}"#,
        ),
        ("DELETE", "/contract-fixture/notes/2026/board.canvas", ""),
        (
            "POST",
            "/contract-fixture/notes",
            r##"{"path":"2026/made.canvas","content":"# Made\n"}"##,
        ),
    ] {
        send(&router, method, url, body).await?;
        assert_eq!(
            std::fs::read_to_string(vault.path().join("notes/2026/board.canvas"))?,
            CANVAS,
            "{method} {url} changed the canvas"
        );
        for written in [
            "notes/2026/renamed.md",
            "notes/2026/made.canvas",
            "notes/2026/board.canvas.md",
        ] {
            assert!(!vault.path().join(written).exists(), "{method} {url} wrote {written}");
        }
    }
    Ok(())
}
