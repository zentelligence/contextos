//! The alerts the top-bar bell and the Overview's Alert rail list: open tasks past their due date, open questions
//! (operational question tasks and the vault's register questions), and the warnings and errors nothing has cleared,
//! most recent first. Real `contextos` sessions over real temporary vaults.

mod support;
mod tasks_common;

use support::events::EventSpec;
use tasks_common::*;

fn task(id: &str, task_type: &str, state: &str, due: Option<&str>, updated: &str) -> String {
    let due_line = due.map_or_else(String::new, |due| format!("due: {due}\n"));
    format!(
        "---\nid: {id}\ntype: task\ntitle: Title for {id}\nstate: {state}\ntask_type: {task_type}\n\
         entity: acme\n{due_line}updated: {updated}\n---\n\n# Title for {id}\n"
    )
}

async fn vault() -> Result<(tempfile::TempDir, tempfile::TempDir, Router), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    let root = vault_dir.path();
    write(root, "index.md", "# Root\n")?;
    write(
        root,
        "memory/tasks/t-1.md",
        &task("T-1", "action", "next", Some("2026-01-10"), "2026-01-01"),
    )?;
    write(
        root,
        "memory/tasks/t-2.md",
        &task("T-2", "action", "done", Some("2026-01-05"), "2026-01-01"),
    )?;
    write(
        root,
        "memory/tasks/t-3.md",
        &task("T-3", "action", "waiting", None, "2026-01-01"),
    )?;
    write(
        root,
        "memory/tasks/t-4.md",
        &task("T-4", "action", "next", Some("2999-01-01"), "2026-01-01"),
    )?;
    write(
        root,
        "memory/tasks/q-1.md",
        &task("Q-1", "question", "open", None, "2026-02-01"),
    )?;
    write(
        root,
        "memory/tasks/q-2.md",
        &task("Q-2", "question", "answered", None, "2026-02-02"),
    )?;
    write(
        root,
        "memory/decisions/vq-001.md",
        "---\nid: VQ-001\ntype: question\ntitle: Which region first\nstatus: open\nupdated: 2026-03-01\n---\n\n# Q\n",
    )?;
    write(
        root,
        "memory/decisions/vq-002.md",
        "---\nid: VQ-002\ntype: question\ntitle: Settled already\nstatus: resolved\nupdated: 2026-03-02\n---\n\n# Q\n",
    )?;
    EventSpec {
        kind: "job.failed",
        severity: "error",
        at: "2026-04-01T00:00:00Z",
        subject: Some("[[registry/workflows/ops/sync/jobs/nightly.toml|nightly]]"),
        fields: vec![("workflow", "sync"), ("job", "nightly")],
        ..EventSpec::default()
    }
    .write_to(root, "failed")?;
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
async fn alerts_list_overdue_tasks_open_questions_and_open_warnings_most_recent_first() -> Result<(), BoxError> {
    let (_vault, _config, router) = vault().await?;

    let (status, body) = fragment(&router, &format!("/{VAULT_NAME}/alerts")).await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    let order: Vec<usize> = [
        "nightly",
        "VQ-001 Which region first",
        "Q-1 Title for Q-1",
        "T-1 Title for T-1",
    ]
    .iter()
    .map(|needle| body.find(needle).unwrap_or(usize::MAX))
    .collect();
    assert!(
        order.iter().all(|at| *at != usize::MAX),
        "every alert is listed: {body}"
    );
    assert!(
        order.windows(2).all(|pair| pair[0] < pair[1]),
        "most recent first: {order:?} {body}"
    );
    for absent in ["T-2", "T-3", "T-4", "Q-2", "VQ-002"] {
        assert!(!body.contains(absent), "{absent} is not an alert: {body}");
    }
    assert!(body.contains(&format!("/{VAULT_NAME}/tasks/t-1")), "{body}");
    assert!(body.contains("4 alerts"), "{body}");
    Ok(())
}

#[tokio::test]
async fn the_bell_counts_the_alerts_and_opens_their_list() -> Result<(), BoxError> {
    let (_vault, _config, router) = vault().await?;

    let (_, page) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;
    assert!(page.contains(&format!("hx-get=\"/{VAULT_NAME}/alerts\"")), "{page}");
    assert!(
        page.contains(&format!("hx-get=\"/{VAULT_NAME}/alerts/summary\"")),
        "{page}"
    );
    assert!(
        !page.contains("title=\"Not yet available\"><span class=\"ic ic-bell\""),
        "{page}"
    );

    let (status, summary) = fragment(&router, &format!("/{VAULT_NAME}/alerts/summary")).await?;
    assert_eq!(status, StatusCode::OK, "{summary}");
    assert!(summary.contains(">4<"), "{summary}");
    Ok(())
}

#[tokio::test]
async fn a_vault_with_nothing_to_raise_says_so() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = fragment(&router, &format!("/{VAULT_NAME}/alerts")).await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Nothing needs you"), "{body}");
    Ok(())
}
