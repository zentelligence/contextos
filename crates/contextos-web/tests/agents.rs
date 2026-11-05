//! Command Centre Agents screen contract tests: real `contextos-mcp`
//! sessions against real temporary vault fixtures, exercising
//! `.contextos/team.toml` reading, domain grouping, and the `?state=`
//! filter end to end. No mocking, per `testing.md`.

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

const TEAM_TOML: &str = "\
[[member]]
slug = \"atlas\"
name = \"Atlas\"
domain = \"Finance\"
chief_of_staff = true
state = \"working\"

[[member]]
slug = \"vela\"
name = \"Vela\"
domain = \"Ops\"
state = \"blocked\"
";

#[tokio::test]
async fn an_absent_agents_file_renders_an_empty_list_not_an_error() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/agents/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("No agents are configured"));
    Ok(())
}

#[tokio::test]
async fn the_agents_screen_groups_by_domain_and_shows_each_agents_state() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", TEAM_TOML)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/agents/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Finance"));
    assert!(body.contains("Ops"));
    assert!(body.contains("Atlas"));
    assert!(body.contains("chief of staff"));
    assert!(body.contains("working"));
    assert!(body.contains("Vela"));
    assert!(body.contains("blocked"));
    Ok(())
}

#[tokio::test]
async fn the_state_query_parameter_filters_to_only_matching_agents() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", TEAM_TOML)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/agents/?state=blocked")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Vela"));
    assert!(!body.contains("Atlas"));
    Ok(())
}

#[tokio::test]
async fn the_agents_sidebar_heading_is_marked_active_only_on_the_agents_screen() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/agents/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(&format!(
        "class=\"nav-group-heading-link active\" href=\"/{VAULT_NAME}/agents/\""
    )));

    let (root_status, root_body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert_eq!(root_status, StatusCode::OK);
    assert!(root_body.contains(&format!(
        "class=\"nav-group-heading-link \" href=\"/{VAULT_NAME}/agents/\""
    )));
    Ok(())
}
