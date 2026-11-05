//! Tasks screen: the vault root strip, done-task hiding, the backlog, and wikilink redirects.
//! Real `contextos-mcp` sessions against real temporary vault fixtures, no mocking, per `testing.md`.

mod support;
mod tasks_common;

use tasks_common::*;
use tower::ServiceExt as _;

// ---------------------------------------------------------------------------
// Vault dashboard widgets: the vault root page's own Task lifecycle strip.
// It once also carried a "Needs attention" list derived directly from
// `state == "waiting"` and overdue `due` dates; that violated the vault's
// own governing requirement that a cross-cutting attention indicator must
// come from a proper events mechanism, never straight off `state`/`due`,
// and must not render until that mechanism exists. It has been removed;
// the regression test below proves it stays gone even when a task would
// have qualified under the old, non-compliant rule.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_vault_root_shows_the_task_lifecycle_strip_and_no_other_folder_is_a_page() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "docs/index.md", "# Docs\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        "---\nid: T-001\ntype: task\ntitle: Ship the thing\nstate: next\ntask_type: action\n\
         entity: zenith\nassignee: piper\n---\n\nBody.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, root_body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert_eq!(status, StatusCode::OK, "{root_body}");
    assert!(root_body.contains("Task lifecycle"));

    let (status, docs_body) = get(&router, &format!("/{VAULT_NAME}/docs/")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND, "{docs_body}");
    assert!(!docs_body.contains("Task lifecycle"));
    Ok(())
}

#[tokio::test]
async fn the_vault_root_never_raises_attention_from_a_waiting_overdue_tasks_state() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        "---\nid: T-001\ntype: task\ntitle: Blocked on the vendor\nstate: waiting\ntask_type: action\n\
         entity: zenith\nassignee: piper\ndue: 2020-01-01\n---\n\nBody.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Attention is events-sourced: the Task lifecycle widget's Attention tile stays at 0 for a task whose state alone
    // might look like trouble. (Its due date makes it an overdue alert in the Alert rail, which is a separate list.)
    let body = body
        .split("<div class=\"dashboard-task-widgets\"")
        .nth(1)
        .ok_or("the task widget fragment")?;
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

// ---------------------------------------------------------------------
// Done-state visibility: the Table/Hierarchy views' own `?done=` toggle
// ---------------------------------------------------------------------

#[tokio::test]
async fn done_tasks_are_hidden_from_the_table_view_by_default() -> Result<(), BoxError> {
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
        &task_note("T-002", "done", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/?view=table")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("T-001"));
    assert!(!body.contains("T-002"), "a done task must be hidden by default");
    assert!(body.contains("Show done"), "the toggle must offer to reveal it");

    let (status, shown_body) = get(&router, &format!("/{VAULT_NAME}/tasks/?view=table&done=show")).await?;
    assert_eq!(status, StatusCode::OK, "{shown_body}");
    assert!(shown_body.contains("T-002"), "?done=show must reveal it again");
    Ok(())
}

#[tokio::test]
async fn done_tasks_are_hidden_from_the_hierarchy_view_by_default_and_its_children_become_roots() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note("T-001", "done", "zenith", None),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-002.md",
        &task_note("T-002", "next", "zenith", Some("t-001")),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/?view=hierarchy")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("T-001"), "the done parent must be hidden");
    assert!(
        body.contains("T-002"),
        "its child must still render, promoted to a root"
    );
    Ok(())
}

#[tokio::test]
async fn clicking_the_done_stage_tile_shows_done_tasks_despite_the_hide_done_default() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note("T-001", "done", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/?view=table&stage=done")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("T-001"),
        "an explicit Done stage-tile selection must still show it"
    );
    Ok(())
}

// ---------------------------------------------------------------------
// "Browse all"/"View all"/overflow links land on the full Table view
// ---------------------------------------------------------------------

#[tokio::test]
async fn the_backlog_browse_all_link_lands_on_the_table_view_offering_list_and_hierarchy_too() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note("T-001", "backlog", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let start = body
        .find("Browse all")
        .ok_or("the Backlog panel's own Browse all link")?;
    let href_start = body[..start].rfind("href=\"").ok_or("an href before the link text")? + "href=\"".len();
    let href_end = body[href_start..].find('"').ok_or("a closing quote")? + href_start;
    let href = &body[href_start..href_end];
    // Askama HTML-escapes the `&` between query parameters in an attribute value.
    assert_eq!(
        href,
        &format!("/{VAULT_NAME}/tasks/?view=table&#38;stage=action:backlog,action:next")
    );

    let (status, browse_body) = get(&router, &href.replace("&#38;", "&")).await?;
    assert_eq!(status, StatusCode::OK, "{browse_body}");
    assert!(browse_body.contains("T-001"));
    // The full Table view's own chrome is present - the view-mode tabs (so switching to Hierarchy is one click
    // away) and the card/list display toggle - none of which the previous card-only drill offered.
    assert!(browse_body.contains(">Hierarchy<"));
    assert!(browse_body.contains("tasks-display-toggle"));
    Ok(())
}

// ---------------------------------------------------------------------
// A task note's own dedicated `/tasks/{id}` route supersedes the generic
// vault content route, both for a direct visit and for a wikilink to it.
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_wikilink_to_a_task_note_resolves_to_its_dedicated_tasks_route() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    // The link lives in a wiki note, read on the wiki screen.
    write(
        vault_dir.path(),
        "memory/wiki/referencing-note.md",
        "# Referencing note\n\nSee [[T-001]] for details.\n",
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note("T-001", "next", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/referencing-note.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(&format!("href=\"/{VAULT_NAME}/tasks/t-001\"")));
    assert!(!body.contains("memory/tasks/t-001.md\""));
    Ok(())
}

#[tokio::test]
async fn visiting_a_task_notes_raw_path_redirects_to_its_dedicated_tasks_route() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note("T-001", "next", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/{VAULT_NAME}/memory/tasks/t-001.md"))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let Some(location) = response.headers().get("location") else {
        return Err("a redirect carries a Location header".into());
    };
    assert_eq!(location, &format!("/{VAULT_NAME}/tasks/t-001"));
    Ok(())
}

#[tokio::test]
async fn the_redirects_own_entity_query_parameter_is_carried_forward_percent_encoded() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note("T-001", "next", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    // `%26` decodes to a literal `&` in the incoming `?entity=`; the redirect target must re-encode it rather than
    // splicing it in raw, which would otherwise inject a spurious second query parameter into the `Location`.
    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/{VAULT_NAME}/memory/tasks/t-001.md?entity=team%26ops"))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let Some(location) = response.headers().get("location") else {
        return Err("a redirect carries a Location header".into());
    };
    assert_eq!(location, &format!("/{VAULT_NAME}/tasks/t-001?entity=team%26ops"));
    Ok(())
}

#[tokio::test]
async fn a_heading_qualified_wikilink_to_a_task_note_points_at_the_task_screen_where_the_anchor_exists()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/referencing-note.md",
        "# Referencing note\n\nSee [[T-001#Some Heading]] for details.\n",
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        "---\nid: T-001\ntype: task\ntitle: Title for T-001\nstate: next\ntask_type: action\n\
         entity: zenith\n---\n\n# Title for T-001\n\n## Some Heading\n\nBody.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/referencing-note.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The task screen renders the note's body through the same pipeline as every other screen, so the heading's
    // anchor exists there and the link keeps the task screen and its menus.
    assert!(
        body.contains(&format!("href=\"/{VAULT_NAME}/tasks/t-001#some-heading\"")),
        "{body}"
    );
    assert!(
        !body.contains(&format!("href=\"/{VAULT_NAME}/memory/tasks/t-001.md")),
        "{body}"
    );
    let (task_status, task_page) = get(&router, &format!("/{VAULT_NAME}/tasks/t-001")).await?;
    assert_eq!(task_status, StatusCode::OK, "{task_page}");
    assert!(
        task_page.contains("id=\"some-heading\""),
        "the anchor the link names must exist: {task_page}"
    );
    Ok(())
}

#[tokio::test]
async fn a_task_folders_own_index_note_is_never_shown() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/tasks/index.md", "# Tasks folder\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/memory/tasks/index.md")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(!body.contains("Tasks folder"));
    Ok(())
}
