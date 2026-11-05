//! Every page that shows a file carries, in its title bar, a link that downloads the file as it is stored (the page's
//! own address with `?raw=1`) and a star that favourites that file, so a file someone keeps returning to is one click
//! away from the favourites panel. Real `contextos-mcp` sessions against real temporary vaults.

mod support;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use contextos_web::McpClientSet;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT: &str = "contract-fixture";

const WIKI_PAGE: &str = "---\ntitle: Plan\n---\n# Plan\n\nThe plan.\n";
const INSIGHT: &str = "---\ntitle: First\n---\nAn insight.\n";
const NOTE: &str = "---\ntitle: One\n---\nA note.\n";
const ENTITY_FILE: &str = "---\ntitle: Offer\n---\nThe offer.\n";
const TASK: &str =
    "---\nid: T-001\ntype: task\ntitle: Ship it\nstate: active\ntask_type: action\nentity: zenith\n---\n\n# Ship it\n";
const JOURNAL_DAY: &str = "# Thursday\n\nA day.\n";

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

/// One file per screen that shows files: (the page's address, the file's vault path, what the file holds).
const FILE_PAGES: &[(&str, &str, &str)] = &[
    (
        "/contract-fixture/wiki/topic/plan.md",
        "memory/wiki/topic/plan.md",
        WIKI_PAGE,
    ),
    (
        "/contract-fixture/insights/f/first.md",
        "memory/insights/f/first.md",
        INSIGHT,
    ),
    ("/contract-fixture/notes/2026/one.md", "notes/2026/one.md", NOTE),
    (
        "/contract-fixture/entity/zenith/projects/offer.md",
        "zenith/projects/offer.md",
        ENTITY_FILE,
    ),
    ("/contract-fixture/tasks/t-001", "memory/tasks/t-001.md", TASK),
    (
        "/contract-fixture/journal/2026-01-15",
        "journal/2026/01/2026-01-15.md",
        JOURNAL_DAY,
    ),
];

async fn fixture() -> Result<(tempfile::TempDir, tempfile::TempDir, Router), BoxError> {
    let vault = tempfile::tempdir()?;
    write(vault.path(), "index.md", "# Root\n")?;
    write(
        vault.path(),
        ".contextos/entities.toml",
        "[[entities]]\nslug = \"zenith\"\nname = \"Zenith\"\naccent = 1\n",
    )?;
    write(
        vault.path(),
        "memory/tasks.base",
        "filters: file.inFolder(\"memory/tasks\")\nviews:\n  - type: table\n    name: All\n",
    )?;
    for (_, path, content) in FILE_PAGES {
        write(vault.path(), path, content)?;
    }
    let config = tempfile::tempdir()?;
    let config_path = support::write_vault_config(config.path(), vault.path())?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    let router = contextos_web::build_router(clients, Some(config.path()), &config.path().join("web.toml"));
    Ok((vault, config, router))
}

/// The status, the content type, and the body of a `GET`.
async fn get(router: &Router, path: &str) -> Result<(StatusCode, String, String), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, content_type, String::from_utf8_lossy(&body).into_owned()))
}

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

/// The title bar's own markup.
fn title_bar(body: &str) -> Result<&str, BoxError> {
    let start = body
        .find("<header class=\"page-title\"")
        .ok_or("page has no title bar")?;
    let end = body[start..].find("</header>").ok_or("unterminated title bar")?;
    Ok(&body[start..start + end])
}

/// The value of the first `attribute="..."` after `marker` in `markup`, entity-decoded.
fn attribute_after(markup: &str, marker: &str, attribute: &str) -> Option<String> {
    let rest = &markup[markup.find(marker)?..];
    let start = rest.find(&format!("{attribute}=\""))? + attribute.len() + 2;
    let value = rest[start..].split('"').next()?;
    Some(value.replace("&amp;", "&"))
}

#[tokio::test]
async fn every_file_page_downloads_the_file_it_shows_from_its_own_address() -> Result<(), BoxError> {
    let (_vault, _config, router) = fixture().await?;
    for (page, _, content) in FILE_PAGES {
        let (status, _, body) = get(&router, page).await?;
        assert_eq!(status, StatusCode::OK, "{page}: {body}");
        let bar = title_bar(&body)?;
        let download = attribute_after(bar, "title-download", "href").ok_or(format!("{page}: no download: {bar}"))?;
        assert_eq!(download, format!("{page}?raw=1"), "{page}");
        let download_at = bar.find("title-download").ok_or("no download")?;
        let star_at = bar.find("favourite-toggle").ok_or(format!("{page}: no star: {bar}"))?;
        assert!(
            download_at < star_at,
            "{page}: the download sits beside the star: {bar}"
        );

        let (status, content_type, served) = get(&router, &download).await?;
        assert_eq!(status, StatusCode::OK, "{download}");
        assert_eq!(&served, content, "{download} did not return the file as stored");
        assert!(content_type.starts_with("text/plain"), "{download}: {content_type}");
    }
    Ok(())
}

#[tokio::test]
async fn the_star_on_a_file_page_favourites_that_file_and_the_panel_opens_it() -> Result<(), BoxError> {
    let (vault, _config, router) = fixture().await?;
    for (page, path, _) in FILE_PAGES {
        let (_, _, body) = get(&router, page).await?;
        let bar = title_bar(&body)?;
        let action = attribute_after(bar, "<form", "hx-patch").ok_or(format!("{page}: no star: {bar}"))?;
        assert_eq!(action, format!("/{VAULT}/favourites/{path}"), "{page}");
        assert!(bar.contains("aria-pressed=\"false\""), "{page}: {bar}");

        let (status, toggled) = patch_favourite(&router, &action, true).await?;
        assert_eq!(status, StatusCode::OK, "{action}: {toggled}");
        assert!(toggled.contains("aria-pressed=\"true\""), "{toggled}");
        let (_, _, body) = get(&router, page).await?;
        assert!(title_bar(&body)?.contains("aria-pressed=\"true\""), "{page}");
    }
    let stored = std::fs::read_to_string(vault.path().join(".contextos/views.toml"))?;
    for (_, path, _) in FILE_PAGES {
        assert!(stored.contains(path), "{path} is not stored: {stored}");
    }

    let (status, _, panel) = get(&router, &format!("/shell/favourites?vault={VAULT}")).await?;
    assert_eq!(status, StatusCode::OK, "{panel}");
    assert!(panel.contains("Favourite pages"), "{panel}");
    for (page, _, _) in FILE_PAGES {
        assert!(
            panel.contains(&format!("href=\"{page}\"")),
            "{page} is not in the panel: {panel}"
        );
    }

    let (wiki_page, _, _) = FILE_PAGES[0];
    let (_, _, body) = get(&router, wiki_page).await?;
    let action = attribute_after(title_bar(&body)?, "<form", "hx-patch").ok_or("no star")?;
    let (status, _) = patch_favourite(&router, &action, false).await?;
    assert_eq!(status, StatusCode::OK);
    let (_, _, panel) = get(&router, &format!("/shell/favourites?vault={VAULT}")).await?;
    assert!(!panel.contains(&format!("href=\"{wiki_page}\"")), "{panel}");
    Ok(())
}

#[tokio::test]
async fn only_a_file_that_exists_can_be_favourited() -> Result<(), BoxError> {
    let (vault, _config, router) = fixture().await?;
    for path in [
        "memory/wiki/missing.md",
        "memory/wiki",
        "../outside.md",
        "memory/wiki/../../x.md",
    ] {
        let (status, body) = patch_favourite(&router, &format!("/{VAULT}/favourites/{path}"), true).await?;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
    }
    assert!(!vault.path().join(".contextos/views.toml").exists());
    Ok(())
}

#[tokio::test]
async fn a_screens_own_page_keeps_the_star_that_favourites_the_screen() -> Result<(), BoxError> {
    let (_vault, _config, router) = fixture().await?;
    let (status, _, body) = get(&router, &format!("/{VAULT}/wiki/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let bar = title_bar(&body)?;
    assert_eq!(
        attribute_after(bar, "<form", "hx-patch").as_deref(),
        Some("/contract-fixture/views/wiki"),
        "{bar}"
    );
    assert!(!bar.contains("title-download"), "a screen is not a file: {bar}");
    Ok(())
}

#[tokio::test]
async fn a_file_that_is_not_a_page_opens_from_its_screen_as_itself() -> Result<(), BoxError> {
    let (vault, _config, router) = fixture().await?;
    write(vault.path(), "memory/wiki/topic/report.pdf", "%PDF-1.4\n")?;
    write(vault.path(), "zenith/projects/photo.png", "png")?;

    let (_, _, tree) = get(&router, &format!("/{VAULT}/wiki/topic/")).await?;
    assert!(
        tree.contains("href=\"/contract-fixture/wiki/topic/report.pdf\""),
        "{tree}"
    );
    for (url, content_type, content) in [
        (
            "/contract-fixture/wiki/topic/report.pdf",
            "application/pdf",
            "%PDF-1.4\n",
        ),
        ("/contract-fixture/entity/zenith/projects/photo.png", "image/png", "png"),
    ] {
        let (status, served_type, served) = get(&router, url).await?;
        assert_eq!(status, StatusCode::OK, "{url}: {served}");
        assert_eq!(served_type, content_type, "{url}");
        assert_eq!(served, content, "{url}");
    }
    Ok(())
}

#[tokio::test]
async fn a_favourite_whose_file_is_gone_can_still_be_removed() -> Result<(), BoxError> {
    let (vault, _config, router) = fixture().await?;
    let (status, _) = patch_favourite(&router, &format!("/{VAULT}/favourites/notes/2026/one.md"), true).await?;
    assert_eq!(status, StatusCode::OK);
    std::fs::remove_file(vault.path().join("notes/2026/one.md"))?;

    let (status, body) = patch_favourite(&router, &format!("/{VAULT}/favourites/notes/2026/one.md"), false).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = std::fs::read_to_string(vault.path().join(".contextos/views.toml"))?;
    assert!(!stored.contains("notes/2026/one.md"), "{stored}");
    Ok(())
}

#[tokio::test]
async fn a_notes_address_naming_a_missing_file_keeps_the_notes_title_bar() -> Result<(), BoxError> {
    let (_vault, _config, router) = fixture().await?;
    let (status, _, body) = get(&router, &format!("/{VAULT}/notes/2026/absent.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let bar = title_bar(&body)?;
    assert!(
        !bar.contains("title-download"),
        "no file of this address is shown: {bar}"
    );
    assert_eq!(
        attribute_after(bar, "<form", "hx-patch").as_deref(),
        Some("/contract-fixture/views/notes"),
        "{bar}"
    );
    Ok(())
}

#[tokio::test]
async fn a_raw_value_other_than_one_is_refused_on_every_file_page() -> Result<(), BoxError> {
    let (_vault, _config, router) = fixture().await?;
    for (page, _, _) in FILE_PAGES {
        let (status, _, body) = get(&router, &format!("{page}?raw=true")).await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{page}: {body}");
        assert!(body.contains("route/malformed-query"), "{page}: {body}");
    }
    let (status, _, _) = get(&router, &format!("/{VAULT}/memory/tasks.base?raw=yes")).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    Ok(())
}
