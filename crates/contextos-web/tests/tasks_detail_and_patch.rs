//! Tasks screen: the detail page, patching, and completing or cancelling a task.
//! Real `contextos-mcp` sessions against real temporary vault fixtures, no mocking, per `testing.md`.

mod support;
mod tasks_common;

use tasks_common::*;

// ---------------------------------------------------------------------------
// Task detail: GET, and both the frontmatter and body PATCH paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_detail_page_renders_every_known_field_and_the_body() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        "---\nid: T-001\ntype: task\ntitle: Ship the thing\nstate: next\ntask_type: action\n\
         entity: zenith\nassignee: piper\n---\n\nBody paragraph.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/t-001")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("T-001"));
    // The default `GET` renders the read view (no editable inputs): field values appear as plain text.
    assert!(body.contains("Ship the thing"));
    assert!(body.contains(">next<"));
    assert!(body.contains("Body paragraph."));
    Ok(())
}

#[tokio::test]
async fn an_unknown_task_id_is_not_found() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = get(&router, &format!("/{VAULT_NAME}/tasks/does-not-exist")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn patching_frontmatter_fields_through_the_real_note_path_updates_the_note() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        "---\nid: T-001\ntype: task\ntitle: Ship the thing\nstate: next\ntask_type: action\n\
         entity: zenith\nassignee: piper\n---\n\nBody paragraph.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/memory/tasks/t-001.md"),
        r#"{"patch": {"state": "doing"}}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, detail_body) = get(&router, &format!("/{VAULT_NAME}/tasks/t-001")).await?;
    assert_eq!(status, StatusCode::OK, "{detail_body}");
    // The default `GET` renders the read view: the saved state appears as plain text, not an editable input.
    assert!(detail_body.contains(">doing<"));
    Ok(())
}

#[tokio::test]
async fn patching_the_body_through_the_real_note_path_replaces_it() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        "---\nid: T-001\ntype: task\ntitle: Ship the thing\nstate: next\ntask_type: action\n\
         entity: zenith\nassignee: piper\n---\n\nOriginal body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/memory/tasks/t-001.md"),
        r#"{"body": {"old": "\nOriginal body.\n", "new": "\nReplaced body.\n"}}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, detail_body) = get(&router, &format!("/{VAULT_NAME}/tasks/t-001")).await?;
    assert_eq!(status, StatusCode::OK, "{detail_body}");
    assert!(detail_body.contains("Replaced body."));
    assert!(!detail_body.contains("Original body."));
    Ok(())
}

// Create: POST /{{vault_name}}/tasks/, and the /tasks/new screen, both
// live in tests/tasks_create.rs, kept apart from this file per its own
// module doc.

/// `POST /{{vault_name}}/tasks/{{id}}/complete` stamps `state: done` and
/// `completed` with the server's own today, never a client-supplied date -
/// the button that triggers it submits nothing but `expected_hash`.
#[tokio::test]
async fn completing_a_task_stamps_state_and_todays_date_server_side() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        "---\nid: T-001\ntype: task\ntitle: Ship the thing\nstate: next\ntask_type: action\n\
         entity: zenith\nassignee: piper\n---\n\nBody.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let today = time::OffsetDateTime::now_utc();
    let expected_prefix = format!("{:04}-{:02}-{:02}", today.year(), u8::from(today.month()), today.day());

    let (status, body) = patch(&router, &format!("/{VAULT_NAME}/tasks/t-001"), r#"{"state": "done"}"#).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The post-save re-render is the read view: the new state and completed date appear as plain text.
    assert!(body.contains(">done<"));
    assert!(body.contains(&expected_prefix), "expected {expected_prefix} in {body}");
    Ok(())
}

/// `POST /{{vault_name}}/tasks/{{id}}/cancel` is [`completing_a_task_stamps_state_and_todays_date_server_side`]'s
/// own `state: dropped`/`cancelled` counterpart.
#[tokio::test]
async fn cancelling_a_task_stamps_state_and_todays_date_server_side() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        "---\nid: T-001\ntype: task\ntitle: Ship the thing\nstate: next\ntask_type: action\n\
         entity: zenith\nassignee: piper\n---\n\nBody.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/tasks/t-001"),
        r#"{"state": "dropped"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The post-save re-render is the read view: the new state appears as plain text.
    assert!(body.contains(">dropped<"));
    Ok(())
}

/// Completing a task lists it in today's Journal entry under the section
/// `.contextos/journal.toml` names for closed tasks.
#[tokio::test]
async fn completing_a_task_lists_it_under_the_configured_section_of_todays_journal_entry() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        "---\nid: T-001\ntype: task\ntitle: Ship the thing\nstate: next\ntask_type: action\n\
         entity: zenith\nassignee: piper\n---\n\nBody.\n",
    )?;
    write(
        vault_dir.path(),
        ".contextos/journal.toml",
        "task_closed_section = \"Completed actions\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(&router, &format!("/{VAULT_NAME}/tasks/t-001"), r#"{"state": "done"}"#).await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let journal_content = std::fs::read_to_string(journal_path_for_today(vault_dir.path()))?;
    assert!(journal_content.contains("## Completed actions"), "{journal_content}");
    assert!(journal_content.contains("Ship the thing"), "{journal_content}");
    // A task is referenced by its key, never its path, so the entry survives the task being moved.
    assert!(journal_content.contains("[[t-001|T-001]]"), "{journal_content}");
    assert!(!journal_content.contains("memory/tasks/"), "{journal_content}");
    Ok(())
}

/// Cancelling a task is [`completing_a_task_lists_it_under_the_configured_section_of_todays_journal_entry`]'s
/// counterpart: a consciously parked task is closed too.
#[tokio::test]
async fn cancelling_a_task_lists_it_under_the_configured_section_of_todays_journal_entry() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        "---\nid: T-001\ntype: task\ntitle: Ship the thing\nstate: next\ntask_type: action\n\
         entity: zenith\nassignee: piper\n---\n\nBody.\n",
    )?;
    write(
        vault_dir.path(),
        ".contextos/journal.toml",
        "task_closed_section = \"Completed actions\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/tasks/t-001"),
        r#"{"state": "dropped"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let journal_content = std::fs::read_to_string(journal_path_for_today(vault_dir.path()))?;
    assert!(journal_content.contains("## Completed actions"), "{journal_content}");
    assert!(journal_content.contains("Ship the thing"), "{journal_content}");
    Ok(())
}

/// With no section configured for closed tasks (the default), closing a task leaves the Journal alone.
#[tokio::test]
async fn closing_a_task_writes_nothing_to_the_journal_unless_a_section_is_configured() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        "---\nid: T-001\ntype: task\ntitle: Ship the thing\nstate: next\ntask_type: action\n\
         entity: zenith\nassignee: piper\n---\n\nBody.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(&router, &format!("/{VAULT_NAME}/tasks/t-001"), r#"{"state": "done"}"#).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!vault_dir.path().join("journal").exists());
    Ok(())
}

/// A closed task is listed in place of the section's placeholder in a day's entry written from the template, and
/// nothing else in the entry changes.
#[tokio::test]
async fn closing_a_task_lists_it_in_the_templates_section_in_place_of_its_placeholder() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "templates/daily-journal.md",
        "# <% tp.date.now(\"YYYY-MM-DD\") %>\n\n## Done today\n\n> What got finished.\n\n- [ ] \n\n---\n\n## Notes\n",
    )?;
    write(
        vault_dir.path(),
        ".contextos/journal.toml",
        "task_closed_section = \"done TODAY\"\n",
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        "---\nid: T-001\ntype: task\ntitle: Ship the thing\nstate: next\ntask_type: action\n\
         entity: zenith\nassignee: piper\n---\n\nBody.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(&router, &format!("/{VAULT_NAME}/tasks/t-001"), r#"{"state": "done"}"#).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let path = journal_path_for_today(vault_dir.path());
    let date = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or("the entry is named for its date")?;
    assert_eq!(
        std::fs::read_to_string(&path)?,
        format!(
            "# {date}\n\n## Done today\n\n> What got finished.\n\n- Ship the thing ([[t-001|T-001]])\n\n---\n\n## Notes\n"
        )
    );
    Ok(())
}

/// `PATCH /{{vault_name}}/tasks/{{id}}` (the frontmatter-save form's own
/// dedicated route, distinct from the generic `.md` mutation route) reads
/// its submission the identical way [`create`]'s own POST does.
#[tokio::test]
async fn patching_frontmatter_through_the_dedicated_tasks_route_updates_the_note() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        "---\nid: T-001\ntype: task\ntitle: Ship the thing\nstate: next\ntask_type: action\n\
         entity: zenith\nassignee: piper\n---\n\nBody.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(&router, &format!("/{VAULT_NAME}/tasks/t-001"), r#"{"state": "doing"}"#).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The post-save re-render is the read view: the new state appears as plain text.
    assert!(body.contains(">doing<"));
    Ok(())
}

/// `PATCH /{{vault_name}}/tasks/{{id}}/body` (the body-save form's own
/// dedicated route) mirrors the generic route's whole-text-replace
/// contract with flat, plain-form-native field names.
#[tokio::test]
async fn patching_the_body_through_the_dedicated_tasks_route_replaces_it() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        "---\nid: T-001\ntype: task\ntitle: Ship the thing\nstate: next\ntask_type: action\n\
         entity: zenith\nassignee: piper\n---\n\nOriginal body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/tasks/t-001"),
        r#"{"old": "\nOriginal body.\n", "new": "\nReplaced body.\n"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Replaced body."));
    assert!(!body.contains("Original body."));
    Ok(())
}

#[tokio::test]
async fn marking_a_task_done_sets_state_and_completed() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        "---\nid: T-001\ntype: task\ntitle: Ship the thing\nstate: next\ntask_type: action\n\
         entity: zenith\nassignee: piper\n---\n\nBody.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/memory/tasks/t-001.md"),
        r#"{"patch": {"state": "done", "completed": "2026-09-13"}}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, detail_body) = get(&router, &format!("/{VAULT_NAME}/tasks/t-001")).await?;
    assert_eq!(status, StatusCode::OK, "{detail_body}");
    // The default `GET` renders the read view: state and completed date appear as plain text.
    assert!(detail_body.contains(">done<"));
    assert!(detail_body.contains(">2026-09-13<"));
    Ok(())
}

// ---------------------------------------------------------------------------
// Each task type closes through its own states
// ---------------------------------------------------------------------------

fn typed_task(id: &str, task_type: &str, state: &str) -> String {
    format!(
        "---\nid: {id}\ntype: task\ntitle: Settle the thing\nstate: {state}\ntask_type: {task_type}\n\
         entity: zenith\nassignee: piper\n---\n\nBody.\n"
    )
}

#[tokio::test]
async fn an_open_decision_offers_its_own_closing_states_and_adopting_it_stamps_completed() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/d-001.md",
        &typed_task("D-001", "decision", "proposed"),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, page) = get(&router, &format!("/{VAULT_NAME}/tasks/d-001")).await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(
        page.contains("Mark as Adopted") && page.contains("Mark as Dropped"),
        "{page}"
    );
    assert!(
        !page.contains("Mark as Done"),
        "a decision offers no action's close: {page}"
    );

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/tasks/d-001"),
        r#"{"state": "adopted"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let note = std::fs::read_to_string(vault_dir.path().join("memory/tasks/d-001.md"))?;
    assert!(note.contains("state: adopted"), "{note}");
    assert!(note.contains("completed: "), "adopting stamps completed: {note}");
    assert!(
        !body.contains("Mark as Adopted"),
        "a closed decision offers no close: {body}"
    );
    Ok(())
}

#[tokio::test]
async fn answering_a_question_stamps_completed_and_dropping_one_stamps_cancelled() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/q-001.md",
        &typed_task("Q-001", "question", "open"),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/q-002.md",
        &typed_task("Q-002", "question", "open"),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/tasks/q-001"),
        r#"{"state": "answered"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/tasks/q-002"),
        r#"{"state": "dropped"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let answered = std::fs::read_to_string(vault_dir.path().join("memory/tasks/q-001.md"))?;
    assert!(
        answered.contains("completed: ") && !answered.contains("cancelled: "),
        "{answered}"
    );
    let dropped = std::fs::read_to_string(vault_dir.path().join("memory/tasks/q-002.md"))?;
    assert!(
        dropped.contains("cancelled: ") && !dropped.contains("completed: "),
        "{dropped}"
    );
    Ok(())
}

#[tokio::test]
async fn a_state_the_tasks_type_does_not_declare_is_refused_and_nothing_is_written() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let note = typed_task("D-001", "decision", "proposed");
    write(vault_dir.path(), "memory/tasks/d-001.md", &note)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(&router, &format!("/{VAULT_NAME}/tasks/d-001"), r#"{"state": "doing"}"#).await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("not a state of a Decision") && body.contains("Proposed, Adopted, or Dropped"),
        "{body}"
    );
    assert_eq!(
        std::fs::read_to_string(vault_dir.path().join("memory/tasks/d-001.md"))?,
        note
    );
    Ok(())
}

#[tokio::test]
async fn changing_a_tasks_type_needs_a_state_of_its_new_type() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let note = typed_task("A-001", "action", "next");
    write(vault_dir.path(), "memory/tasks/a-001.md", &note)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;
    let path = vault_dir.path().join("memory/tasks/a-001.md");

    let (_, refused) = patch(
        &router,
        &format!("/{VAULT_NAME}/tasks/a-001"),
        r#"{"task_type": "question", "state": "next"}"#,
    )
    .await?;
    assert!(refused.contains("not a state of a Question"), "{refused}");
    assert_eq!(std::fs::read_to_string(&path)?, note, "a refused change writes nothing");

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/tasks/a-001"),
        r#"{"task_type": "question", "state": "open"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let saved = std::fs::read_to_string(&path)?;
    assert!(
        saved.contains("task_type: question") && saved.contains("state: open"),
        "{saved}"
    );
    Ok(())
}

#[tokio::test]
async fn editing_a_task_whose_state_its_type_does_not_declare_keeps_that_state_without_refusal() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/d-001.md",
        &typed_task("D-001", "decision", "pending"),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/tasks/d-001"),
        r#"{"task_type": "decision", "state": "pending", "title": "Settle it later"}"#,
    )
    .await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    let saved = std::fs::read_to_string(vault_dir.path().join("memory/tasks/d-001.md"))?;
    assert!(
        saved.contains("title: Settle it later") && saved.contains("state: pending"),
        "{saved}"
    );
    Ok(())
}

#[tokio::test]
async fn closing_a_task_through_the_full_edit_form_stamps_the_date_its_blank_field_carried() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/d-001.md",
        &typed_task("D-001", "decision", "proposed"),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/a-001.md",
        &typed_task("A-001", "action", "doing"),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/tasks/d-001"),
        r#"{"title": "Settle the thing", "task_type": "decision", "state": "adopted", "completed": "", "cancelled": ""}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let adopted = std::fs::read_to_string(vault_dir.path().join("memory/tasks/d-001.md"))?;
    assert!(
        adopted.contains("completed: 20"),
        "the blank field is stamped: {adopted}"
    );

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/tasks/a-001"),
        r#"{"title": "Settle the thing", "task_type": "decision", "state": "dropped", "completed": "", "cancelled": ""}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let retyped = std::fs::read_to_string(vault_dir.path().join("memory/tasks/a-001.md"))?;
    assert!(
        retyped.contains("cancelled: 20"),
        "a type change and close in one save stamps too: {retyped}"
    );
    Ok(())
}

#[tokio::test]
async fn closing_a_task_already_closed_under_an_older_state_keeps_its_date_and_its_journal_entry()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/d-001.md",
        "---\nid: D-001\ntype: task\ntitle: Settle the thing\nstate: done\ntask_type: decision\n\
         entity: zenith\nassignee: piper\ncompleted: 2026-01-01\n---\n\nBody.\n",
    )?;
    write(
        vault_dir.path(),
        ".contextos/journal.toml",
        "task_closed_section = \"Completed actions\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/tasks/d-001"),
        r#"{"state": "adopted"}"#,
    )
    .await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    let note = std::fs::read_to_string(vault_dir.path().join("memory/tasks/d-001.md"))?;
    assert!(
        note.contains("state: adopted") && note.contains("completed: 2026-01-01"),
        "{note}"
    );
    assert!(
        !journal_path_for_today(vault_dir.path()).exists(),
        "moving an already-closed task to its type's closing state is not a second close"
    );
    Ok(())
}

#[tokio::test]
async fn while_tasks_toml_cannot_be_read_a_state_change_is_refused_with_the_reason_and_other_edits_save()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        ".contextos/tasks.toml",
        "[[stage]]\nstate = \"triage\"\nshade = \"red\"\n",
    )?;
    let note = typed_task("A-001", "action", "triage");
    write(vault_dir.path(), "memory/tasks/a-001.md", &note)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;
    let path = vault_dir.path().join("memory/tasks/a-001.md");

    let (_, refused) = patch(
        &router,
        &format!("/{VAULT_NAME}/tasks/a-001"),
        r#"{"state": "in_progress"}"#,
    )
    .await?;
    assert!(
        refused.contains("tasks.toml could not be read") && refused.contains("shade"),
        "{refused}"
    );
    assert_eq!(std::fs::read_to_string(&path)?, note, "a refused change writes nothing");

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/tasks/a-001"),
        r#"{"state": "triage", "title": "Triage it"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(std::fs::read_to_string(&path)?.contains("title: Triage it"));
    Ok(())
}
