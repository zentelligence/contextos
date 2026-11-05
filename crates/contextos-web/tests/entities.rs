//! Command Centre entity switcher contract tests: real `contextos-mcp`
//! sessions against real temporary vault fixtures, exercising
//! `.contextos/entities.toml` seeding and the shell's `?entity=` query
//! parameter end to end. No mocking, per `testing.md`.

mod support;

use std::path::Path;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use std::sync::Arc;

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

async fn get(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
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

const INDEX_MARKDOWN: &str = "\
# Entities: Index

| File | Entity | Primary purpose |
| --- | --- | --- |
| [zenith](zenith.md) | Zenith | Consulting |
| [personal](personal.md) | Personal | Operator's own profile |

<!-- contextos:index:begin -->
| Item | Summary |
| --- | --- |
| [personal.md](personal.md) | Personal |
| [zenith.md](zenith.md) | Zenith |
<!-- contextos:index:end -->
";

#[tokio::test]
async fn no_switcher_renders_when_the_vault_has_no_entity_index_and_no_entities_toml() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("entity-switcher"));
    assert!(!vault_dir.path().join(".contextos/entities.toml").exists());
    Ok(())
}

#[tokio::test]
async fn visiting_the_vault_seeds_entities_toml_from_the_index_in_row_order() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/entities/index.md", INDEX_MARKDOWN)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("entity-switcher"));
    assert!(body.contains("<option value=\"zenith\">Zenith</option>"));
    assert!(body.contains("<option value=\"personal\">Personal</option>"));

    let seeded = std::fs::read_to_string(vault_dir.path().join(".contextos/entities.toml"))?;
    assert!(seeded.contains("slug = \"zenith\""));
    assert!(seeded.contains("slug = \"personal\""));
    let zenith_at = seeded.find("\"zenith\"").ok_or("zenith entry present")?;
    let personal_at = seeded.find("\"personal\"").ok_or("personal entry present")?;
    assert!(
        zenith_at < personal_at,
        "entities.toml should preserve the index's own row order"
    );
    Ok(())
}

#[tokio::test]
async fn an_existing_entities_toml_is_authoritative_and_is_never_reseeded_from_the_index() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/entities/index.md", INDEX_MARKDOWN)?;
    write(
        vault_dir.path(),
        ".contextos/entities.toml",
        "[[entities]]\nslug = \"family\"\nname = \"Family\"\naccent = 4\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<option value=\"family\">Family</option>"));
    assert!(!body.contains("<option value=\"zenith\""), "{body}");

    let unchanged = std::fs::read_to_string(vault_dir.path().join(".contextos/entities.toml"))?;
    assert!(unchanged.contains("family"));
    assert!(!unchanged.contains("zenith"));
    Ok(())
}

#[tokio::test]
async fn the_entity_query_parameter_selects_the_matching_option_on_the_vault_root() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/entities/index.md", INDEX_MARKDOWN)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/?entity=personal")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<option value=\"personal\" selected>Personal</option>"));
    assert!(body.contains("<option value=\"\">All entities</option>"));
    Ok(())
}

#[tokio::test]
async fn the_entity_selection_survives_a_page_file_link_through_the_generic_route() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    // A Canvas board is shown on the screen that owns its folder, so the generic route sends it there.
    write(
        vault_dir.path(),
        "memory/wiki/board.canvas",
        "{\"nodes\":[],\"edges\":[]}",
    )?;
    write(vault_dir.path(), "memory/entities/index.md", INDEX_MARKDOWN)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/{VAULT_NAME}/memory/wiki/board.canvas?entity=zenith"))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = response
        .headers()
        .get(axum::http::header::LOCATION)
        .and_then(|value| value.to_str().ok());
    assert_eq!(
        location,
        Some(format!("/{VAULT_NAME}/wiki/board.canvas?entity=zenith").as_str())
    );
    Ok(())
}
