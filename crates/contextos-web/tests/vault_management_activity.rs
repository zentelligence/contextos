//! Vault settings: the read-only Activity log over the operation log and session summaries. Real `contextos-mcp`
//! sessions against real temporary vault fixtures.

mod support;
mod vault_settings_common;

use vault_settings_common::*;

fn activity_fixture(vault: &Path) -> std::io::Result<()> {
    write(vault, "index.md", "# Root\n")?;
    write(
        vault,
        "memory/log/2026/09/2026-09-30.md",
        "# 2026-09-30: Operation Log\n\n08:00:00 | fs_write_file | modify | Tidied | files: notes/a.md\n",
    )?;
    write(
        vault,
        "memory/log/2026/10/2026-10-07.md",
        "# 2026-10-07: Operation Log\n\n11:52:05 | base_record_create | create | Created a task \\| with a bar | files: memory/tasks/a-031.md, memory/tasks.md\n12:00:14 | fs_write_file | modify | Overwrote a task | files: memory/tasks/a-026.md\nan unstructured line\n",
    )?;
    write(vault, "memory/log/index.md", "# Log\n")?;
    write(
        vault,
        "memory/sessions/2026/05/2026-05-01-0054-claude-md-split.md",
        "# Session\n\nSplit **CLAUDE.md**.\n",
    )
}

#[tokio::test]
async fn the_activity_log_opens_on_the_latest_day_as_a_structured_table() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    activity_fixture(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, page) = get(&router, &format!("/{VAULT_NAME}/settings/activity/")).await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("<h3>2026-10-07</h3>"), "the latest day opens: {page}");
    assert!(page.contains("<td class=\"mono\">11:52:05</td>"), "{page}");
    assert!(page.contains("<span class=\"badge\">create</span>"), "{page}");
    assert!(
        page.contains("Created a task | with a bar"),
        "an escaped bar is shown as a bar: {page}"
    );
    assert!(
        page.contains(&format!("href=\"/{VAULT_NAME}/memory/tasks/a-031.md\"")),
        "{page}"
    );
    assert!(
        page.contains("an unstructured line"),
        "nothing in the log is hidden: {page}"
    );
    assert!(page.contains("2 operations"), "{page}");
    assert!(
        page.contains("rel=\"prev\""),
        "the previous logged day is offered: {page}"
    );
    assert!(
        page.contains("October") && page.contains("September"),
        "months are named: {page}"
    );
    Ok(())
}

#[tokio::test]
async fn the_activity_log_offers_no_way_to_change_what_it_shows() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    activity_fixture(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    for path in [
        "",
        "log/2026/10/2026-10-07.md",
        "sessions/2026/05/2026-05-01-0054-claude-md-split.md",
    ] {
        let (status, page) = get(&router, &format!("/{VAULT_NAME}/settings/activity/{path}")).await?;
        assert_eq!(status, StatusCode::OK, "{path}: {page}");
        assert!(page.contains("Read only."), "{path}");
        let panel = &page[page.find("vm-panel").ok_or("no panel")?..];
        for affordance in ["hx-put", "hx-post", "hx-patch", "<textarea", ">Edit<", ">Save<"] {
            assert!(!panel.contains(affordance), "{path} offers {affordance}");
        }
    }
    for method in ["POST", "PUT", "PATCH"] {
        let response = tower::ServiceExt::oneshot(
            router.clone(),
            Request::builder()
                .method(method)
                .uri(format!("/{VAULT_NAME}/settings/activity/log/2026/10/2026-10-07.md"))
                .body(Body::from("{}"))?,
        )
        .await?;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED, "{method}");
    }
    Ok(())
}

#[tokio::test]
async fn a_session_summary_reads_rendered_with_its_date_and_title() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    activity_fixture(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, page) = get(&router, &format!("/{VAULT_NAME}/settings/activity/sessions")).await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("<h3>Claude md split</h3>"), "{page}");
    assert!(page.contains("2026-05-01 00:54"), "{page}");
    assert!(page.contains("<strong>CLAUDE.md</strong>"), "{page}");
    assert!(
        page.contains(">Operation log<") && page.contains(">Session summaries<"),
        "{page}"
    );
    Ok(())
}

#[tokio::test]
async fn a_vault_with_only_session_summaries_opens_on_them() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/sessions/2026-05-08-lint.md", "# Lint\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, page) = get(&router, &format!("/{VAULT_NAME}/settings/activity/")).await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("<h3>Lint</h3>"), "{page}");
    let (status, _) = get(&router, &format!("/{VAULT_NAME}/settings/activity/log")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}
