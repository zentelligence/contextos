//! The counts the sidebar shows beside a destination come from the vault itself: real `contextos-mcp` sessions
//! against real temporary vault fixtures. No mocking, per `testing.md`.

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

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
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

/// The count badge of the sidebar link pointing at `href`, or `None` when that link carries no badge.
fn badge(body: &str, href: &str) -> Result<Option<String>, BoxError> {
    let anchor = format!("class=\"nav-link \" href=\"{href}\"");
    let start = body
        .find(&anchor)
        .or_else(|| body.find(&format!("class=\"nav-link active\" href=\"{href}\"")))
        .ok_or(format!("no sidebar link to {href}"))?;
    let link = &body[start..start + body[start..].find("</a>").ok_or("unterminated link")?];
    let marker = "<span class=\"nav-link-meta\">";
    Ok(link.find(marker).map(|at| {
        let rest = &link[at + marker.len()..];
        rest[..rest.find("</span>").unwrap_or(rest.len())].to_owned()
    }))
}

fn task(id: &str, state: &str) -> String {
    format!(
        "---\nid: {id}\ntype: task\ntitle: Title for {id}\nstate: {state}\ntask_type: action\nentity: acme\n---\n\n# {id}\n"
    )
}

const TEAM: &str = "\
[[member]]
slug = \"a\"
name = \"A\"
team = \"business\"
state = \"working\"

[[member]]
slug = \"b\"
name = \"B\"
team = \"Business\"
state = \"blocked\"

[[member]]
slug = \"c\"
name = \"C\"
team = \"personal\"
state = \"blocked\"

[[member]]
slug = \"d\"
name = \"D\"
type = \"human\"
team = \"business\"
state = \"blocked\"

[[member]]
slug = \"e\"
name = \"E\"
team = \"General\"
state = \"idle\"

[[member]]
slug = \"f\"
name = \"F\"
type = \"human\"
team = \"general\"
";

async fn populated() -> Result<(tempfile::TempDir, tempfile::TempDir, Router), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    write(vault.path(), "index.md", "# Root\n")?;
    write(vault.path(), "inbox/a.md", "# A\n")?;
    write(vault.path(), "inbox/sub/x.md", "# X\n")?;
    write(vault.path(), "processed/old.md", "# Old\n")?;
    write(vault.path(), "outbox/r1.md", "# R1\n")?;
    write(vault.path(), "outbox/r2.md", "# R2\n")?;
    write(vault.path(), ".contextos/team.toml", TEAM)?;
    write(vault.path(), "memory/tasks/t-001.md", &task("T-001", "doing"))?;
    write(vault.path(), "memory/tasks/t-002.md", &task("T-002", "next"))?;
    write(vault.path(), "memory/tasks/t-003.md", &task("T-003", "waiting"))?;
    write(vault.path(), "memory/tasks/t-004.md", &task("T-004", "done"))?;
    write(vault.path(), "memory/tasks/t-005.md", &task("T-005", "dropped"))?;
    let router = router_over(vault.path(), config.path()).await?;
    Ok((vault, config, router))
}

#[tokio::test]
async fn the_inbox_count_is_the_pending_items_and_excludes_the_processed_tree() -> Result<(), BoxError> {
    let (_vault, _config, router) = populated().await?;
    let (_status, body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert_eq!(badge(&body, &format!("/{VAULT_NAME}/inbox/"))?.as_deref(), Some("2"));
    Ok(())
}

#[tokio::test]
async fn the_outbox_count_is_the_items_waiting_in_the_outbox() -> Result<(), BoxError> {
    let (_vault, _config, router) = populated().await?;
    let (_status, body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert_eq!(badge(&body, &format!("/{VAULT_NAME}/outbox/"))?.as_deref(), Some("2"));
    Ok(())
}

#[tokio::test]
async fn the_tasks_count_is_the_open_tasks_and_excludes_done_and_dropped_ones() -> Result<(), BoxError> {
    let (_vault, _config, router) = populated().await?;
    let (_status, body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert_eq!(badge(&body, &format!("/{VAULT_NAME}/tasks/"))?.as_deref(), Some("3"));
    Ok(())
}

#[tokio::test]
async fn each_agents_count_matches_the_agents_its_own_page_lists() -> Result<(), BoxError> {
    let (_vault, _config, router) = populated().await?;
    let (_status, body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert_eq!(
        badge(&body, &format!("/{VAULT_NAME}/agents/?team=general"))?.as_deref(),
        Some("1"),
        "a human member is not an agent, and the team matches case-insensitively"
    );
    assert_eq!(
        badge(&body, &format!("/{VAULT_NAME}/agents/?team=business"))?.as_deref(),
        Some("2"),
        "a human member is not an agent"
    );
    assert_eq!(
        badge(&body, &format!("/{VAULT_NAME}/agents/?team=personal"))?.as_deref(),
        Some("1")
    );
    assert_eq!(
        badge(&body, &format!("/{VAULT_NAME}/agents/?state=blocked"))?.as_deref(),
        Some("2")
    );
    Ok(())
}

#[tokio::test]
async fn signals_count_zero_when_no_event_needs_attention_and_lead_to_the_signals_screen() -> Result<(), BoxError> {
    let (_vault, _config, router) = populated().await?;
    let (_status, body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    // No event needs attention in this vault, so the count is a genuine zero.
    let entry = body
        .split("<li>")
        .find(|item| item.contains("<span class=\"nav-link-label\">Signals</span>"))
        .ok_or("no Signals entry in the sidebar")?;
    assert!(entry.contains("<span class=\"nav-link-meta\">0</span>"), "{entry}");
    assert!(entry.contains(&format!("href=\"/{VAULT_NAME}/signals/\"")), "{entry}");
    assert!(
        !entry.contains("status-dot"),
        "no attention dot while nothing needs attention: {entry}"
    );
    Ok(())
}

#[tokio::test]
async fn an_empty_vault_shows_genuine_zero_counts() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    write(vault.path(), "index.md", "# Root\n")?;
    let router = router_over(vault.path(), config.path()).await?;
    let (_status, body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    for href in [
        "inbox/",
        "outbox/",
        "tasks/",
        "agents/?team=general",
        "agents/?team=business",
        "agents/?team=personal",
        "agents/?state=blocked",
    ] {
        assert_eq!(
            badge(&body, &format!("/{VAULT_NAME}/{href}"))?.as_deref(),
            Some("0"),
            "{href}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_count_follows_the_vault_on_every_render() -> Result<(), BoxError> {
    let (vault, _config, router) = populated().await?;
    let (_status, before) = get(&router, &format!("/{VAULT_NAME}/notes/")).await?;
    assert_eq!(badge(&before, &format!("/{VAULT_NAME}/inbox/"))?.as_deref(), Some("2"));
    write(vault.path(), "inbox/b.md", "# B\n")?;
    let (_status, after) = get(&router, &format!("/{VAULT_NAME}/notes/")).await?;
    assert_eq!(badge(&after, &format!("/{VAULT_NAME}/inbox/"))?.as_deref(), Some("3"));
    Ok(())
}

#[tokio::test]
async fn a_destination_with_no_count_shows_no_badge() -> Result<(), BoxError> {
    let (_vault, _config, router) = populated().await?;
    let (_status, body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert_eq!(badge(&body, &format!("/{VAULT_NAME}/notes/"))?, None);
    Ok(())
}

#[tokio::test]
async fn a_malformed_team_file_shows_no_agent_badges_rather_than_a_false_zero() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    write(vault.path(), "index.md", "# Root\n")?;
    write(vault.path(), ".contextos/team.toml", "[[member\nslug = ")?;
    let router = router_over(vault.path(), config.path()).await?;
    let (_status, body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    for href in [
        "agents/?team=general",
        "agents/?team=business",
        "agents/?team=personal",
        "agents/?state=blocked",
    ] {
        assert_eq!(badge(&body, &format!("/{VAULT_NAME}/{href}"))?, None, "{href}");
    }
    Ok(())
}
