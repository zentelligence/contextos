//! Tasks screen: the focus dashboard, stage strip, filters, and views.
//! Real `contextos-mcp` sessions against real temporary vault fixtures, no mocking, per `testing.md`.

mod support;
mod tasks_common;

use tasks_common::*;

#[tokio::test]
async fn a_vault_with_no_task_notes_renders_an_empty_focus_dashboard_not_an_error() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Nothing in flight."));
    assert!(body.contains("Nothing waiting on you."));
    assert!(body.contains("Nothing backlogged."));
    assert!(body.contains("Nothing completed yet."));
    assert!(body.contains("Backlog"));
    Ok(())
}

#[tokio::test]
async fn an_empty_table_view_still_shows_the_original_empty_state_copy() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/?view=table")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("No tasks match this view"));
    Ok(())
}

#[tokio::test]
async fn the_view_toggle_offers_the_three_named_modes() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(">Focus<"));
    assert!(body.contains(">All tasks<"));
    assert!(body.contains(">Hierarchy<"));
    Ok(())
}

#[tokio::test]
async fn every_default_stage_tile_renders_an_icon_not_a_bare_number() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    for icon in [
        "ic-inbox",
        "ic-circle",
        "ic-loader",
        "ic-clock",
        "ic-list-checks",
        "ic-scale",
        "ic-circle-help",
    ] {
        assert!(body.contains(icon), "expected the stage strip to render {icon}");
    }
    Ok(())
}

#[tokio::test]
async fn the_stage_strip_counts_tasks_per_configured_state() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note("T-001", "next", "zenith", None),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-002.md",
        &task_note("T-002", "next", "zenith", None),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-003.md",
        &task_note("T-003", "doing", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("T-001"));
    assert!(body.contains("T-002"));
    assert!(body.contains("T-003"));
    Ok(())
}

#[tokio::test]
async fn the_stage_query_parameter_drills_the_focus_view_into_one_stage() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note("T-001", "next", "zenith", None),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-002.md",
        &task_note("T-002", "doing", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/?stage=doing")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("T-002"));
    assert!(!body.contains("T-001"));
    assert!(body.contains("Clear filter"));
    Ok(())
}

#[tokio::test]
async fn the_stage_query_parameter_filters_the_table_view() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note("T-001", "next", "zenith", None),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-002.md",
        &task_note("T-002", "doing", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/?view=table&stage=doing")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("T-002"));
    assert!(!body.contains("T-001"));
    Ok(())
}

#[tokio::test]
async fn the_table_view_renders_the_id_in_its_own_column_separate_from_the_title() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note("T-001", "next", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/?view=table")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("<th>ID</th>"));
    let id_start = body
        .find("<td class=\"mono\">T-001</td>")
        .ok_or("the id in its own column")?;
    let link_start = body
        .find("<a href=\"/contract-fixture/tasks/t-001\">Title for T-001</a>")
        .ok_or("the title link with no id prefix")?;
    assert!(id_start < link_start, "the id column must render before the title cell");
    Ok(())
}

#[tokio::test]
async fn the_sort_parameter_orders_the_table_view_by_id() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-003.md",
        &task_note("T-003", "next", "zenith", None),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note("T-001", "next", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/?view=table&sort=id")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let first = body.find("T-001").ok_or("T-001 present")?;
    let second = body.find("T-003").ok_or("T-003 present")?;
    assert!(first < second, "ascending id order must put T-001 before T-003");
    Ok(())
}

#[tokio::test]
async fn the_entity_query_parameter_filters_both_the_strip_and_the_list() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note("T-001", "next", "zenith", None),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-002.md",
        &task_note("T-002", "next", "family", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/?entity=family")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("T-002"));
    assert!(!body.contains("T-001"));
    Ok(())
}

#[tokio::test]
async fn the_hierarchy_view_orders_a_parent_immediately_before_its_children() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note("T-001", "next", "zenith", None),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-002.md",
        &task_note("T-002", "next", "zenith", Some("t-001")),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/?view=hierarchy")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parent_pos = body.find("T-001").ok_or("T-001 present")?;
    let child_pos = body.find("T-002").ok_or("T-002 present")?;
    assert!(parent_pos < child_pos, "parent must render before its child");
    Ok(())
}

#[tokio::test]
async fn the_focus_view_buckets_default_states_into_their_configured_sections() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note("T-001", "doing", "zenith", None),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-002.md",
        &task_note("T-002", "waiting", "zenith", None),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-003.md",
        &task_note("T-003", "backlog", "zenith", None),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-004.md",
        &task_note("T-004", "done", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("T-001"), "a doing task belongs in Now");
    assert!(body.contains("T-002"), "a waiting task belongs in Needs your call");
    assert!(body.contains("T-003"), "a backlog task belongs in Backlog");
    assert!(body.contains("T-004"), "a done task belongs in Done");
    assert!(body.contains("Now"));
    assert!(body.contains("Needs your call"));
    Ok(())
}

#[tokio::test]
async fn the_now_grid_caps_at_six_and_offers_an_overflow_link_into_the_drill_down() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    for n in 0..8 {
        write(
            vault_dir.path(),
            &format!("memory/tasks/t-{n:03}.md"),
            &task_note(&format!("T-{n:03}"), "doing", "zenith", None),
        )?;
    }
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("more in flight than the WIP limit shows"));

    let (status, drill_body) = get(&router, &format!("/{VAULT_NAME}/tasks/?stage=doing")).await?;
    assert_eq!(status, StatusCode::OK, "{drill_body}");
    for n in 0..8 {
        assert!(
            drill_body.contains(&format!("T-{n:03}")),
            "drill-down must show every doing task"
        );
    }
    Ok(())
}

#[tokio::test]
async fn the_search_filter_narrows_by_title_case_insensitively() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        "---\nid: T-001\ntype: task\ntitle: Ship the widget\nstate: doing\ntask_type: action\n\
         entity: zenith\n---\n\nBody.\n",
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-002.md",
        "---\nid: T-002\ntype: task\ntitle: Renew the licence\nstate: doing\ntask_type: action\n\
         entity: zenith\n---\n\nBody.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/?q=WIDGET")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Ship the widget"));
    assert!(!body.contains("Renew the licence"));
    Ok(())
}

#[tokio::test]
async fn the_assignee_filter_narrows_to_one_exact_assignee() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note_with_assignee_and_priority("T-001", "doing", "zenith", "piper", None),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-002.md",
        &task_note_with_assignee_and_priority("T-002", "doing", "zenith", "an-agent", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/?assignee=piper")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("T-001"));
    assert!(!body.contains("T-002"));
    Ok(())
}

#[tokio::test]
async fn the_backlog_panel_sorts_by_priority_and_offers_a_pull_next_task_link() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note_with_assignee_and_priority("T-001", "backlog", "zenith", "piper", Some("low")),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-002.md",
        &task_note_with_assignee_and_priority("T-002", "backlog", "zenith", "piper", Some("high")),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Pull next task"));
    assert!(
        body.contains(&format!("href=\"/{VAULT_NAME}/tasks/t-002\">")),
        "the pull-next button must link to the backlog row ranked higher in the declared priority vocabulary"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Compliance: a "Blocked" tile is shown (matching the mock's own wording),
// but only as a disabled, static placeholder - never a real, data-derived
// aggregate. `StageStrip.jsx`'s own red "N BLOCKED" pill and a per-card red
// "blocked" badge both require a proper events mechanism this codebase does
// not have yet, so the placeholder's own count must stay fixed at 0
// regardless of how many real `waiting`/`blocked`-like tasks a vault
// actually has, and it must be visibly inert (disabled, "Not yet
// available"), not a clickable filter into real data.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_attention_tile_never_counts_a_task_by_its_state_alone() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note("T-001", "waiting", "zenith", None),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-002.md",
        &task_note("T-002", "waiting", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let tile = body
        .split("task-stage-tile-attention")
        .nth(1)
        .ok_or("the Attention tile")?;
    let tile = tile.split("</a>").next().unwrap_or_default();
    assert!(
        tile.contains("task-stage-count\">0<") && tile.contains(">Attention<"),
        "a task's state never raises attention; only a warning or error event about it does: {tile}"
    );
    Ok(())
}

#[tokio::test]
async fn the_tasks_screen_shows_a_live_attention_tile_opening_signals() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note("T-001", "waiting", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("task-stage-tile-disabled"), "{body}");
    assert!(
        body.contains(&format!("task-stage-tile-attention\" href=\"/{VAULT_NAME}/signals/\"")),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn the_vault_root_task_widget_also_shows_the_attention_tile() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let widget = body
        .split("<div class=\"dashboard-task-widgets\"")
        .nth(1)
        .ok_or("the task widget")?;
    assert!(widget.contains("task-stage-tile-attention"), "{widget}");
    assert!(!body.contains("task-stage-tile-disabled"), "{body}");
    Ok(())
}

#[tokio::test]
async fn the_tasks_sidebar_heading_is_marked_active_only_on_the_tasks_screen() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(&format!("class=\"nav-link active\" href=\"/{VAULT_NAME}/tasks/\"")));

    let (root_status, root_body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert_eq!(root_status, StatusCode::OK);
    assert!(root_body.contains(&format!("class=\"nav-link \" href=\"/{VAULT_NAME}/tasks/\"")));
    Ok(())
}

/// 21:45 UTC on 13 October 2026: 08:45 on the 14th in Sydney, under daylight saving time.
const SYDNEY_MORNING: time::OffsetDateTime = time::macros::datetime!(2026-10-13 21:45 UTC);

#[tokio::test]
async fn the_done_today_tile_lists_the_tasks_closed_as_completed_on_the_operators_today() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note("T-001", "doing", "zenith", None),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-002.md",
        "---\nid: T-002\ntype: task\ntitle: Finished on UTC's today\nstate: done\ntask_type: action\n\
         entity: zenith\ncompleted: 2026-10-13\n---\n\nBody.\n",
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-003.md",
        "---\nid: T-003\ntype: task\ntitle: Child still open\nstate: next\ntask_type: action\n\
         entity: zenith\nparent: '[[t-001]]'\n---\n\nBody.\n",
    )?;
    let router = router_over_at(vault_dir.path(), config_dir.path(), "Australia/Sydney", SYDNEY_MORNING).await?;
    // Closing through the web stamps `completed` with the operator's today in the server's zone: the 14th.
    let (status, body) = patch(&router, &format!("/{VAULT_NAME}/tasks/t-001"), r#"{"state": "done"}"#).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("2026-10-14"), "{body}");

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/?done=hide")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains(r#"href="/contract-fixture/tasks/?view=table&#38;completed=today""#),
        "the strip offers a Done today tile, never under the hide: {body}"
    );

    for view in ["table", "focus", "hierarchy"] {
        let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/?view={view}&completed=today")).await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body.contains("Title for T-001"),
            "{view}: closed today, shown although done: {body}"
        );
        assert!(
            !body.contains("Finished on UTC"),
            "{view}: closed on another local day: {body}"
        );
        assert!(!body.contains("Child still open"), "{view}: still open: {body}");
    }
    let (_, body) = get(&router, &format!("/{VAULT_NAME}/tasks/?view=table&completed=today")).await?;
    assert!(
        body.contains(r#"<input type="hidden" name="completed" value="today">"#),
        "the filter form keeps the selection: {body}"
    );

    for refused in ["yesterday", ""] {
        let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/?view=table&completed={refused}")).await?;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{refused:?} is refused, not ignored: {body}"
        );
    }

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("completed=today"),
        "the Overview's strip has the tile too: {body}"
    );
    Ok(())
}
