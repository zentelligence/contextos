//! Every list that links to vault notes sends each note to the screen that owns it, and a direct visit to an owned
//! note or folder through the generic vault route is redirected there, so a note always opens inside its own section's
//! navigation. Nothing else is shown through that route: a note or folder no section owns, and every folder's managed
//! `index.md`, is not found, and no list shows an `index.md`. Real `contextos-mcp` sessions against real temporary
//! vault fixtures, per `testing.md`.

mod support;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};

use contextos_web::McpClientSet;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";

/// A vault config with the text index and link graph on, which the search results view needs.
fn write_vault_config(dir: &Path, vault_dir: &Path) -> std::io::Result<std::path::PathBuf> {
    let path = dir.join("config.toml");
    #[allow(clippy::unnecessary_debug_formatting)]
    let vault_toml_value = format!("{vault_dir:?}");
    std::fs::write(
        &path,
        format!(
            "[[vault]]\npath = {vault_toml_value}\nname = \"{VAULT_NAME}\"\n\
             state_directory = \".contextos\"\n\
             [vault.search]\ntext = true\ngraph = true\nsemantic = false\n"
        ),
    )?;
    Ok(path)
}

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

/// A wiki note (owned by the Wiki screen under the default `memory/wiki` folder), a note no screen owns, and a
/// base whose rows are both.
fn vault() -> Result<tempfile::TempDir, BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(vault.path(), "index.md", "# Root\n")?;
    write(
        vault.path(),
        "memory/wiki/topic.md",
        "---\ntitle: Griffin topic\n---\n\nThe griffin wiki topic, linking [[x]].\n",
    )?;
    write(
        vault.path(),
        "loose/x.md",
        "---\ntitle: Griffin loose\n---\n\nA loose griffin note.\n",
    )?;
    write(
        vault.path(),
        "memory/wiki/everything.base",
        "filters: file.inFolder(\"memory/wiki\") || file.inFolder(\"loose\")\nviews:\n  - type: table\n    name: All\n    order:\n      - file.path\n",
    )?;
    Ok(vault)
}

async fn router_over(vault_dir: &Path, config_dir: &Path) -> Result<Router, BoxError> {
    let config_path = write_vault_config(config_dir, vault_dir)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(contextos_web::build_router(
        clients,
        Some(config_dir),
        &config_dir.join("web.toml"),
    ))
}

async fn get(router: &Router, path: &str) -> Result<(StatusCode, Option<String>, String), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let location = response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, location, String::from_utf8_lossy(&body).into_owned()))
}

const WIKI_SCREEN: &str = "href=\"/contract-fixture/wiki/topic.md\"";

#[tokio::test]
async fn a_base_view_links_each_row_to_the_screen_that_owns_it() -> Result<(), BoxError> {
    let vault = vault()?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault.path(), config_dir.path()).await?;

    let (status, _, page) = get(&router, &format!("/{VAULT_NAME}/wiki/everything.base")).await?;

    assert_eq!(status, StatusCode::OK);
    assert!(page.contains(WIKI_SCREEN), "{page}");
    assert!(
        !page.contains("href=\"/contract-fixture/memory/wiki/topic.md\""),
        "{page}"
    );
    assert!(!page.contains("href=\"/contract-fixture/loose/x.md\""), "{page}");
    assert!(page.contains("No screen shows this note yet."), "{page}");
    Ok(())
}

#[tokio::test]
async fn search_results_and_their_link_panels_link_to_the_screen_that_owns_each_note() -> Result<(), BoxError> {
    let vault = vault()?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault.path(), config_dir.path()).await?;

    let (status, _, page) = get(
        &router,
        &format!("/{VAULT_NAME}/search?q=griffin&mode=text&selected=loose/x.md"),
    )
    .await?;

    assert_eq!(status, StatusCode::OK);
    assert!(page.contains(WIKI_SCREEN), "{page}");
    assert!(
        !page.contains("href=\"/contract-fixture/memory/wiki/topic.md\""),
        "{page}"
    );
    assert!(!page.contains("href=\"/contract-fixture/loose/x.md\""), "{page}");
    Ok(())
}

#[tokio::test]
async fn a_direct_visit_to_an_owned_note_is_redirected_to_its_screen() -> Result<(), BoxError> {
    let vault = vault()?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault.path(), config_dir.path()).await?;

    let (status, location, _) = get(&router, &format!("/{VAULT_NAME}/memory/wiki/topic.md")).await?;

    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location.as_deref(), Some("/contract-fixture/wiki/topic.md"));
    Ok(())
}

#[tokio::test]
async fn a_note_no_section_owns_is_not_shown_through_the_vault_route() -> Result<(), BoxError> {
    let vault = vault()?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault.path(), config_dir.path()).await?;

    let (status, location, page) = get(&router, &format!("/{VAULT_NAME}/loose/x.md")).await?;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(location, None);
    assert!(!page.contains("A loose griffin note."), "{page}");
    Ok(())
}
#[tokio::test]
async fn a_managed_index_note_is_never_shown_anywhere() -> Result<(), BoxError> {
    let vault = vault()?;
    write(
        vault.path(),
        "memory/wiki/index.md",
        "---\ntitle: Griffin index\n---\n\nThe managed griffin index.\n",
    )?;
    write(vault.path(), "loose/index.md", "# Loose griffin index\n")?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault.path(), config_dir.path()).await?;

    for path in ["memory/wiki/index.md", "loose/index.md"] {
        let (status, location, _) = get(&router, &format!("/{VAULT_NAME}/{path}")).await?;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(location, None, "{path}");
    }
    let (_, _, results) = get(&router, &format!("/{VAULT_NAME}/search?q=griffin&mode=text")).await?;
    assert!(!results.contains("index.md"), "{results}");
    let (_, _, base) = get(&router, &format!("/{VAULT_NAME}/wiki/everything.base")).await?;
    assert!(!base.contains("index.md"), "{base}");
    Ok(())
}

#[tokio::test]
async fn a_folder_opens_in_the_section_that_owns_it_and_no_other_folder_is_shown() -> Result<(), BoxError> {
    let vault = vault()?;
    write(vault.path(), "memory/wiki/topics/deep.md", "# Deep\n")?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault.path(), config_dir.path()).await?;

    let (status, location, _) = get(&router, &format!("/{VAULT_NAME}/memory/wiki/topics/")).await?;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location.as_deref(), Some("/contract-fixture/wiki/"));

    let (status, location, _) = get(&router, &format!("/{VAULT_NAME}/loose/")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(location, None);
    Ok(())
}

#[tokio::test]
async fn a_decisions_register_record_opens_on_its_vault_settings_page() -> Result<(), BoxError> {
    let vault = vault()?;
    write(
        vault.path(),
        "memory/decisions/vd-001.md",
        "---\nid: VD-001\ntype: decision\n---\n\nA griffin decision.\n",
    )?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault.path(), config_dir.path()).await?;

    let (status, location, _) = get(&router, &format!("/{VAULT_NAME}/memory/decisions/vd-001.md")).await?;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location.as_deref(), Some("/contract-fixture/settings/decisions/vd-001"));

    let (_, _, results) = get(&router, &format!("/{VAULT_NAME}/search?q=griffin&mode=text")).await?;
    assert!(
        results.contains("href=\"/contract-fixture/settings/decisions/vd-001\""),
        "{results}"
    );
    Ok(())
}

#[tokio::test]
async fn signals_are_listed_in_the_sidebar_and_lead_to_the_signals_screen() -> Result<(), BoxError> {
    let vault = vault()?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault.path(), config_dir.path()).await?;

    let (_, _, page) = get(&router, &format!("/{VAULT_NAME}/search")).await?;

    assert!(page.contains(&format!("href=\"/{VAULT_NAME}/signals/\"")), "{page}");
    assert!(!page.contains("No screen shows signals yet."), "{page}");
    assert!(!page.contains("href=\"/contract-fixture/memory/events/\""), "{page}");
    Ok(())
}
