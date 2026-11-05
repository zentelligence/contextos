//! The Tasks screen's attention indicator: a task shows amber or red when the most recent event about it is a warning
//! or an error, and any later event about the task clears it. Real `contextos` sessions over real temporary vaults.

mod support;
mod tasks_common;

use support::events::EventSpec;
use tasks_common::*;

const LINK_1: &str = "[[a-1|A-1]]";
const LINK_2: &str = "[[a-2|A-2]]";
const TASKS_BASE: &str = "memory/tasks.base";

fn event_about(
    link: &'static str,
    id: &'static str,
    kind: &'static str,
    severity: &'static str,
    at: &'static str,
) -> EventSpec<'static> {
    EventSpec {
        kind,
        severity,
        at,
        subject: Some(link),
        subject_id: Some(id),
        subject_path: Some(if id == "A-1" {
            "memory/tasks/a-1.md"
        } else {
            "memory/tasks/a-2.md"
        }),
        subject_base: Some(TASKS_BASE),
        ..EventSpec::default()
    }
}

async fn vault_with_two_tasks() -> Result<(tempfile::TempDir, tempfile::TempDir, Router), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/a-1.md",
        &task_note("A-1", "doing", "acme", None),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/a-2.md",
        &task_note("A-2", "doing", "acme", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;
    Ok((vault_dir, config_dir, router))
}

/// The markup of the card or row for `id`: from the id to the end of its element, enough to hold its indicator.
fn around<'a>(body: &'a str, id: &str) -> &'a str {
    let Some(start) = body
        .find(&format!("{id}</span>"))
        .or_else(|| body.find(&format!(">{id}<")))
    else {
        return "";
    };
    let rest = &body[start..];
    let end = rest
        .find("</article>")
        .or_else(|| rest.find("</tr>"))
        .unwrap_or(rest.len());
    &rest[..end]
}

#[tokio::test]
async fn a_task_with_no_events_shows_no_indicator() -> Result<(), BoxError> {
    let (_vault, _config, router) = vault_with_two_tasks().await?;

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;

    assert!(!body.contains("task-attention"), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_warning_shows_amber_an_error_shows_red_and_other_tasks_show_nothing() -> Result<(), BoxError> {
    let (vault, _config, router) = vault_with_two_tasks().await?;
    event_about(LINK_1, "A-1", "task.claim-lapsed", "warning", "2026-10-28T03:00:00Z").write_to(vault.path(), "w")?;

    let (_, amber) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;
    event_about(LINK_2, "A-2", "task.unknown-state", "error", "2026-10-28T03:01:00Z").write_to(vault.path(), "e")?;
    let (_, both) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;

    assert!(
        around(&amber, "A-1").contains("status-dot warning task-attention"),
        "{amber}"
    );
    assert!(!around(&amber, "A-2").contains("task-attention"), "{amber}");
    assert!(around(&both, "A-1").contains("status-dot warning"), "{both}");
    assert!(
        around(&both, "A-2").contains("status-dot error task-attention"),
        "{both}"
    );
    assert!(
        both.contains("Needs attention: error"),
        "an accessible name, not colour alone: {both}"
    );
    Ok(())
}

#[tokio::test]
async fn any_later_event_about_the_task_clears_its_indicator() -> Result<(), BoxError> {
    let (vault, _config, router) = vault_with_two_tasks().await?;
    event_about(LINK_1, "A-1", "task.claim-lapsed", "warning", "2026-10-28T03:00:00Z").write_to(vault.path(), "w")?;
    event_about(LINK_1, "A-1", "task.state-changed", "info", "2026-10-28T04:00:00Z").write_to(vault.path(), "later")?;

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;

    assert!(!body.contains("task-attention"), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_warning_after_an_ordinary_event_shows_again() -> Result<(), BoxError> {
    let (vault, _config, router) = vault_with_two_tasks().await?;
    event_about(LINK_1, "A-1", "task.state-changed", "info", "2026-10-28T03:00:00Z").write_to(vault.path(), "a")?;
    event_about(LINK_1, "A-1", "task.claim-lapsed", "warning", "2026-10-28T04:00:00Z").write_to(vault.path(), "b")?;

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;

    assert!(around(&body, "A-1").contains("task-attention"), "{body}");
    Ok(())
}

#[tokio::test]
async fn the_indicator_shows_in_the_table_the_cards_and_the_hierarchy_alike() -> Result<(), BoxError> {
    let (vault, _config, router) = vault_with_two_tasks().await?;
    event_about(LINK_1, "A-1", "task.claim-lapsed", "warning", "2026-10-28T03:00:00Z").write_to(vault.path(), "w")?;

    for view in ["table", "table&display=cards", "hierarchy", "focus"] {
        let (_, body) = get(&router, &format!("/{VAULT_NAME}/tasks/?view={view}")).await?;
        assert!(body.contains("status-dot warning task-attention"), "{view}: {body}");
    }
    Ok(())
}

#[tokio::test]
async fn the_task_list_reloads_itself_when_the_stream_says_attention_may_have_changed() -> Result<(), BoxError> {
    let (_vault, _config, router) = vault_with_two_tasks().await?;

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/tasks/?view=table&entity=acme")).await?;

    assert!(body.contains("hx-trigger=\"sse:attention, sse:reload\""), "{body}");
    assert!(
        body.contains("hx-get=\"/contract-fixture/tasks/?view=table&#38;entity=acme\""),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn a_vault_without_events_still_lists_its_tasks() -> Result<(), BoxError> {
    let (_vault, _config, router) = vault_with_two_tasks().await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/?view=table")).await?;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("A-1") && body.contains("A-2"));
    Ok(())
}

#[tokio::test]
async fn moving_a_task_keeps_its_indicator_because_a_move_clears_nothing() -> Result<(), BoxError> {
    let (vault, _config, router) = vault_with_two_tasks().await?;
    event_about(LINK_1, "A-1", "task.claim-lapsed", "warning", "2026-10-28T03:00:00Z").write_to(vault.path(), "w")?;
    event_about(LINK_1, "A-1", "task.moved", "info", "2026-10-28T04:00:00Z").write_to(vault.path(), "moved")?;

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;

    assert!(around(&body, "A-1").contains("task-attention"), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_warning_raised_before_a_task_was_moved_still_shows_on_the_task_where_it_now_is() -> Result<(), BoxError> {
    let (vault, _config, router) = vault_with_two_tasks().await?;
    std::fs::create_dir_all(vault.path().join("memory/tasks/clients"))?;
    std::fs::rename(
        vault.path().join("memory/tasks/a-1.md"),
        vault.path().join("memory/tasks/clients/a-1.md"),
    )?;
    // Recorded while the task was still at the top of the tasks folder.
    event_about(LINK_1, "A-1", "task.claim-lapsed", "warning", "2026-10-28T03:00:00Z").write_to(vault.path(), "w")?;

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;

    assert!(around(&body, "A-1").contains("task-attention"), "{body}");
    assert!(!around(&body, "A-2").contains("task-attention"), "{body}");
    Ok(())
}

#[tokio::test]
async fn an_event_about_a_record_of_another_base_with_the_same_key_leaves_the_task_alone() -> Result<(), BoxError> {
    let (vault, _config, router) = vault_with_two_tasks().await?;
    EventSpec {
        kind: "review.flagged",
        severity: "warning",
        at: "2026-10-28T03:00:00Z",
        subject: Some("[[a-1|A-1]]"),
        subject_id: Some("A-1"),
        subject_path: Some("registry/actions/a-1.md"),
        subject_base: Some("registry/actions.base"),
        ..EventSpec::default()
    }
    .write_to(vault.path(), "other")?;

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;

    assert!(!body.contains("task-attention"), "{body}");
    Ok(())
}
