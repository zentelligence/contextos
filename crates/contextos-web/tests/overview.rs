//! The vault Overview: Bridge (the chief of staff), System status, Task lifecycle with its Attention tile, Needs a
//! call from you, the Alert rail, and Inputs, every panel built from what the vault records. Real `contextos` sessions
//! over real temporary vaults.

mod support;
mod tasks_common;

use support::events::EventSpec;
use tasks_common::*;

const TEAM: &str = "[[member]]\nslug = \"atlas\"\nname = \"Atlas\"\nchief_of_staff = true\nstate = \"working\"\n\n\
                    [[member]]\nslug = \"vela\"\nname = \"Vela\"\n";

fn task(id: &str, state: &str, assignee: &str) -> String {
    format!(
        "---\nid: {id}\ntype: task\ntitle: Title for {id}\nstate: {state}\ntask_type: action\nentity: acme\n\
         assignee: {assignee}\n---\n\n# Title for {id}\n"
    )
}

async fn vault() -> Result<(tempfile::TempDir, tempfile::TempDir, Router), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    let root = vault_dir.path();
    write(root, "index.md", "# Root\n")?;
    write(root, ".contextos/team.toml", TEAM)?;
    write(root, "memory/tasks/t-1.md", &task("T-1", "next", "atlas"))?;
    write(root, "memory/tasks/t-2.md", &task("T-2", "doing", "atlas"))?;
    write(root, "memory/tasks/t-3.md", &task("T-3", "done", "atlas"))?;
    write(root, "memory/tasks/t-4.md", &task("T-4", "next", "vela"))?;
    write(root, "inbox/one.md", "# One\n")?;
    write(root, "inbox/two.md", "# Two\n")?;
    EventSpec {
        kind: "task.claim-lapsed",
        severity: "warning",
        at: "2026-10-28T03:00:00Z",
        subject: Some("[[t-4|T-4]]"),
        subject_id: Some("T-4"),
        subject_path: Some("memory/tasks/t-4.md"),
        subject_base: Some("memory/tasks.base"),
        ..EventSpec::default()
    }
    .write_to(root, "lapsed")?;
    EventSpec {
        kind: "import.completed",
        severity: "info",
        at: "2026-10-28T04:00:00Z",
        fields: vec![("source", "bank")],
        ..EventSpec::default()
    }
    .write_to(root, "import")?;
    let router = router_over(root, config_dir.path()).await?;
    Ok((vault_dir, config_dir, router))
}

async fn fragment(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    let response = tower::ServiceExt::oneshot(
        router.clone(),
        Request::builder()
            .uri(path)
            .header("hx-request", "true")
            .body(Body::empty())?,
    )
    .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

#[tokio::test]
async fn the_overview_loads_every_panel_from_the_vault_and_shows_no_placeholder() -> Result<(), BoxError> {
    let (_vault, _config, router) = vault().await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/")).await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    let overview = body.split("vault-overview").nth(1).unwrap_or_default();
    for region in [
        format!("/{VAULT_NAME}/overview/bridge"),
        format!("/{VAULT_NAME}/overview/status"),
        format!("/{VAULT_NAME}/overview/inputs"),
        format!("/{VAULT_NAME}/signals/attention"),
        format!("/{VAULT_NAME}/alerts"),
    ] {
        assert!(
            overview.contains(&format!("hx-get=\"{region}\"")),
            "{region}: {overview}"
        );
    }
    assert!(!overview.contains("Not yet available"), "{overview}");
    assert!(
        !body.contains("task-stage-tile-disabled"),
        "the Attention tile is live: {body}"
    );
    Ok(())
}

#[tokio::test]
async fn the_attention_tile_counts_the_tasks_with_an_open_warning_or_error() -> Result<(), BoxError> {
    let (_vault, _config, router) = vault().await?;

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;

    let tile = body
        .split("task-stage-tile-attention")
        .nth(1)
        .unwrap_or_default()
        .split("</a>")
        .next()
        .unwrap_or_default();
    assert!(tile.contains(">1<") && tile.contains("Attention"), "{tile}");
    Ok(())
}

#[tokio::test]
async fn the_bridge_shows_the_chief_of_staff_and_what_is_assigned_to_them() -> Result<(), BoxError> {
    let (_vault, _config, router) = vault().await?;

    let (status, body) = fragment(&router, &format!("/{VAULT_NAME}/overview/bridge")).await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Atlas") && body.contains("working"), "{body}");
    assert!(
        body.contains("<dt>Open tasks</dt><dd>2</dd>"),
        "two open tasks, not the closed one: {body}"
    );
    assert!(body.contains("Open decisions"), "{body}");
    assert!(!body.contains("Vela"), "{body}");
    Ok(())
}

#[tokio::test]
async fn the_system_status_counts_the_last_day_of_events_by_severity() -> Result<(), BoxError> {
    let (_vault, _config, router) = vault().await?;

    let (status, body) = fragment(&router, &format!("/{VAULT_NAME}/overview/status")).await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("MCP servers") && body.contains("1 of 1"), "{body}");
    assert!(body.contains("Vault health"), "{body}");
    assert!(body.contains("Live stream"), "{body}");
    assert!(body.contains("Events"), "{body}");
    Ok(())
}

#[tokio::test]
async fn the_inputs_list_the_inbox_and_each_source_that_records_events() -> Result<(), BoxError> {
    let (_vault, _config, router) = vault().await?;

    let (status, body) = fragment(&router, &format!("/{VAULT_NAME}/overview/inputs")).await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Inbox") && body.contains("2 waiting"), "{body}");
    assert!(body.contains("import.completed"), "{body}");
    assert!(
        !body.contains("task.claim-lapsed"),
        "a kind the server fires is not an input: {body}"
    );
    Ok(())
}
