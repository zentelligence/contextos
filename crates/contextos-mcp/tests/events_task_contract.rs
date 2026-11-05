//! The server fires task events from the MCP calls that create or change a task, once across every process, and
//! records them as internal writes that Git follows.
mod support;

use serde_json::json;
use support::events_vault::{
    TASK_PATH, event_notes, events_harness, events_harness_with, events_of_kind, field, task_note,
};
use support::git_harness::{BoxError, Harness};

async fn update(harness: &Harness, patch: serde_json::Value) -> Result<serde_json::Value, BoxError> {
    harness
        .structured("frontmatter_update", json!({"path": TASK_PATH, "patch": patch}))
        .await
}

#[tokio::test]
async fn creating_a_task_fires_task_created_with_the_tasks_own_fields() -> Result<(), BoxError> {
    let harness = events_harness().await?;
    harness
        .structured(
            "fs_write_file",
            json!({"path": "memory/tasks/a-002.md", "content": task_note("A-002", "backlog", "")}),
        )
        .await?;

    let created = events_of_kind(harness.vault.path(), "task.created");
    assert_eq!(created.len(), 1, "{:?}", event_notes(harness.vault.path()));
    let (path, text) = &created[0];
    assert!(
        path.starts_with("memory/events/") && path.ends_with("-a-002.md"),
        "{path}"
    );
    assert!(
        text.starts_with("---\ntype: event\nkind: task.created\nseverity: info\nat: "),
        "{text}"
    );
    assert_eq!(field(text, "entity").as_deref(), Some("zenith"));
    // A task is referenced by its key; its path and base are kept beside the key.
    assert_eq!(field(text, "subject").as_deref(), Some("[[a-002|A-002]]"));
    assert_eq!(field(text, "subject_id").as_deref(), Some("A-002"));
    assert_eq!(field(text, "subject_path").as_deref(), Some("memory/tasks/a-002.md"));
    assert_eq!(field(text, "subject_base").as_deref(), Some("memory/tasks.base"));
    assert_eq!(field(text, "task_type").as_deref(), Some("action"));
    assert_eq!(field(text, "state").as_deref(), Some("backlog"));
    assert_eq!(field(text, "assignee").as_deref(), Some("ellis"));
    for common in ["\ntitle:", "\ncreated:", "\nupdated:", "\ntags:"] {
        assert!(!text.contains(common), "{common}: {text}");
    }
    Ok(())
}

#[tokio::test]
async fn changing_a_tasks_state_fires_one_state_changed_event_naming_both_states() -> Result<(), BoxError> {
    let harness = events_harness().await?;
    update(&harness, json!({"state": "doing", "claimed_by": "ellis/S-4"})).await?;

    let changed = events_of_kind(harness.vault.path(), "task.state-changed");
    assert_eq!(changed.len(), 1, "{:?}", event_notes(harness.vault.path()));
    let text = &changed[0].1;
    assert_eq!(field(text, "from").as_deref(), Some("next"));
    assert_eq!(field(text, "to").as_deref(), Some("doing"));
    assert_eq!(field(text, "actor").as_deref(), Some("ellis/S-4"));
    assert_eq!(field(text, "subject_id").as_deref(), Some("A-001"));
    assert_eq!(events_of_kind(harness.vault.path(), "task.created").len(), 0);
    Ok(())
}

#[tokio::test]
async fn one_write_changing_state_and_assignee_fires_both_events_with_the_same_time() -> Result<(), BoxError> {
    let harness = events_harness().await?;
    update(&harness, json!({"state": "review", "assignee": "piper"})).await?;

    let state = events_of_kind(harness.vault.path(), "task.state-changed");
    let reassigned = events_of_kind(harness.vault.path(), "task.reassigned");
    assert_eq!((state.len(), reassigned.len()), (1, 1));
    assert_eq!(field(&state[0].1, "at"), field(&reassigned[0].1, "at"));
    assert_eq!(field(&reassigned[0].1, "from").as_deref(), Some("ellis"));
    assert_eq!(field(&reassigned[0].1, "to").as_deref(), Some("piper"));
    Ok(())
}

#[tokio::test]
async fn a_change_to_any_other_field_and_a_write_to_a_note_that_is_not_a_task_fire_nothing() -> Result<(), BoxError> {
    let harness = events_harness().await?;
    update(&harness, json!({"priority": "high"})).await?;
    harness
        .structured(
            "fs_write_file",
            json!({"path": "memory/tasks/guide.md", "content": "# Guide\n"}),
        )
        .await?;
    harness
        .structured(
            "fs_write_file",
            json!({"path": "alpha/other.md", "content": "---\ntype: task\nstate: next\n---\n"}),
        )
        .await?;
    harness
        .structured(
            "fs_write_file",
            json!({"path": "memory/tasks/notes.md", "content": "---\ntype: note\nstate: next\n---\n# Not a task\n"}),
        )
        .await?;

    assert_eq!(event_notes(harness.vault.path()), Vec::new());
    Ok(())
}

#[tokio::test]
async fn a_task_changed_outside_contextos_is_reported_at_the_next_write_to_it_as_one_combined_change()
-> Result<(), BoxError> {
    let harness = events_harness().await?;
    std::fs::write(
        harness.vault.path().join(TASK_PATH),
        task_note("A-001", "review", "claimed_by: ellis/S-9\n"),
    )?;
    assert_eq!(
        event_notes(harness.vault.path()),
        Vec::new(),
        "nothing fires for a change no call makes"
    );

    update(&harness, json!({"priority": "high"})).await?;

    let changed = events_of_kind(harness.vault.path(), "task.state-changed");
    assert_eq!(changed.len(), 1, "{:?}", event_notes(harness.vault.path()));
    assert_eq!(field(&changed[0].1, "from").as_deref(), Some("next"));
    assert_eq!(field(&changed[0].1, "to").as_deref(), Some("review"));
    Ok(())
}

#[tokio::test]
async fn a_state_the_tasks_type_does_not_declare_fires_unknown_state_once_and_an_unrelated_edit_fires_nothing()
-> Result<(), BoxError> {
    let harness = events_harness().await?;
    update(&harness, json!({"state": "doning"})).await?;
    let unknown = events_of_kind(harness.vault.path(), "task.unknown-state");
    assert_eq!(unknown.len(), 1, "{:?}", event_notes(harness.vault.path()));
    assert!(unknown[0].1.contains("\nseverity: warning\n"));
    assert_eq!(field(&unknown[0].1, "state").as_deref(), Some("doning"));

    update(&harness, json!({"priority": "low"})).await?;
    assert_eq!(events_of_kind(harness.vault.path(), "task.unknown-state").len(), 1);
    Ok(())
}

#[tokio::test]
async fn the_states_a_vault_declares_in_tasks_toml_decide_what_is_unknown() -> Result<(), BoxError> {
    let harness = support::events_vault::events_harness_with(
        |_| {},
        |root| {
            std::fs::create_dir_all(root.join(".contextos"))?;
            std::fs::write(
                root.join(".contextos/tasks.toml"),
                "[[stage]]\nstate = \"next\"\n[[stage]]\nstate = \"shipping\"\n",
            )
        },
    )
    .await?;
    update(&harness, json!({"state": "shipping"})).await?;
    assert_eq!(events_of_kind(harness.vault.path(), "task.unknown-state").len(), 0);
    update(&harness, json!({"state": "doing"})).await?;
    assert_eq!(events_of_kind(harness.vault.path(), "task.unknown-state").len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_lapsed_claim_taken_afresh_fires_claim_lapsed_as_a_warning() -> Result<(), BoxError> {
    let harness = support::events_vault::events_harness_with(
        |_| {},
        |root| {
            std::fs::write(
                root.join(TASK_PATH),
                task_note(
                    "A-001",
                    "doing",
                    "claimed_by: ellis/S-1\nclaimed_until: 2020-01-01T00:00:00Z\n",
                ),
            )
        },
    )
    .await?;
    update(
        &harness,
        json!({"claimed_by": "ellis/S-2", "claimed_until": "2099-01-01T00:00:00Z"}),
    )
    .await?;

    let lapsed = events_of_kind(harness.vault.path(), "task.claim-lapsed");
    assert_eq!(lapsed.len(), 1, "{:?}", event_notes(harness.vault.path()));
    assert!(lapsed[0].1.contains("\nseverity: warning\n"));
    assert_eq!(field(&lapsed[0].1, "claimed_by").as_deref(), Some("ellis/S-1"));
    Ok(())
}

#[tokio::test]
async fn moving_a_task_fires_task_moved_naming_both_paths_and_the_moved_task_is_still_tracked() -> Result<(), BoxError>
{
    let harness = events_harness().await?;
    harness
        .structured(
            "fs_move_file",
            json!({"source": TASK_PATH, "destination": "memory/tasks/zenith/a-001.md"}),
        )
        .await?;
    let moved = events_of_kind(harness.vault.path(), "task.moved");
    assert_eq!(moved.len(), 1, "{:?}", event_notes(harness.vault.path()));
    let text = &moved[0].1;
    assert_eq!(field(text, "severity").as_deref(), Some("info"));
    assert_eq!(field(text, "from").as_deref(), Some(TASK_PATH));
    assert_eq!(field(text, "to").as_deref(), Some("memory/tasks/zenith/a-001.md"));
    assert_eq!(field(text, "subject").as_deref(), Some("[[a-001|A-001]]"));
    assert_eq!(
        field(text, "subject_path").as_deref(),
        Some("memory/tasks/zenith/a-001.md")
    );
    assert_eq!(field(text, "subject_base").as_deref(), Some("memory/tasks.base"));
    assert_eq!(event_notes(harness.vault.path()).len(), 1, "a move fires nothing else");
    harness
        .structured(
            "frontmatter_update",
            json!({"path": "memory/tasks/zenith/a-001.md", "patch": {"state": "doing"}}),
        )
        .await?;
    assert_eq!(events_of_kind(harness.vault.path(), "task.state-changed").len(), 1);

    harness
        .structured("fs_delete_file", json!({"path": "memory/tasks/zenith/a-001.md"}))
        .await?;
    assert_eq!(event_notes(harness.vault.path()).len(), 2, "a delete fires nothing");
    Ok(())
}

#[tokio::test]
async fn moving_a_folder_of_tasks_within_the_tasks_fires_task_moved_for_each_task_in_it() -> Result<(), BoxError> {
    let harness = support::events_vault::events_harness_with(
        |_| {},
        |root| {
            std::fs::create_dir_all(root.join("memory/tasks/inbox/deep"))?;
            std::fs::write(root.join("memory/tasks/inbox/a-008.md"), task_note("A-008", "next", ""))?;
            std::fs::write(
                root.join("memory/tasks/inbox/deep/a-009.md"),
                task_note("A-009", "next", ""),
            )
        },
    )
    .await?;
    harness
        .structured(
            "fs_move_file",
            json!({"source": "memory/tasks/inbox", "destination": "memory/tasks/clients"}),
        )
        .await?;

    let mut moves: Vec<(Option<String>, Option<String>)> = events_of_kind(harness.vault.path(), "task.moved")
        .iter()
        .map(|(_, text)| (field(text, "from"), field(text, "to")))
        .collect();
    moves.sort();
    assert_eq!(
        moves,
        [
            (
                Some("memory/tasks/inbox/a-008.md".to_owned()),
                Some("memory/tasks/clients/a-008.md".to_owned())
            ),
            (
                Some("memory/tasks/inbox/deep/a-009.md".to_owned()),
                Some("memory/tasks/clients/deep/a-009.md".to_owned())
            ),
        ],
        "{:?}",
        event_notes(harness.vault.path())
    );

    harness
        .structured(
            "frontmatter_update",
            json!({"path": "memory/tasks/clients/a-008.md", "patch": {"state": "doing"}}),
        )
        .await?;
    assert_eq!(
        events_of_kind(harness.vault.path(), "task.state-changed").len(),
        1,
        "the moved task is still tracked, so its change is reported and it is not created afresh"
    );
    assert_eq!(events_of_kind(harness.vault.path(), "task.created").len(), 0);
    Ok(())
}

#[tokio::test]
async fn a_missing_table_is_rebuilt_silently_and_the_next_change_is_then_reported() -> Result<(), BoxError> {
    let harness = events_harness().await?;
    for state_file in std::fs::read_dir(harness.vault.path().join(".contextos"))?.flatten() {
        let name = state_file.file_name().to_string_lossy().into_owned();
        if name.starts_with("events-") {
            std::fs::remove_file(state_file.path())?;
        }
    }
    update(&harness, json!({"state": "doing"})).await?;
    assert_eq!(
        event_notes(harness.vault.path()),
        Vec::new(),
        "a rebuilt table invents no event"
    );

    update(&harness, json!({"state": "review"})).await?;
    assert_eq!(events_of_kind(harness.vault.path(), "task.state-changed").len(), 1);
    Ok(())
}

#[tokio::test]
async fn event_notes_are_staged_in_git_with_the_write_that_raised_them_and_raise_no_events_of_their_own()
-> Result<(), BoxError> {
    let harness = events_harness().await?;
    let before = harness.commit_count().await?;
    update(&harness, json!({"state": "doing"})).await?;

    let events = event_notes(harness.vault.path());
    assert_eq!(events.len(), 1, "an event note raises no event of its own");
    let pending = harness.pending_paths().await?;
    assert!(pending.contains(&events[0].0), "{pending:?}");
    assert!(pending.contains(&TASK_PATH.to_owned()), "{pending:?}");
    harness.settle()?;
    assert_eq!(
        harness.commit_count().await?,
        before + 1,
        "one commit holds the write and its event"
    );
    assert_eq!(harness.dirty_paths().await?, Vec::<String>::new());
    Ok(())
}

#[tokio::test]
async fn the_operation_log_does_not_record_event_notes() -> Result<(), BoxError> {
    let harness = events_harness().await?;
    update(&harness, json!({"state": "doing"})).await?;
    let events = event_notes(harness.vault.path());
    harness.server.flush_operation_logs()?;
    let mut logged = String::new();
    let mut pending = vec![harness.vault.path().join("memory/log")];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).into_iter().flatten().flatten() {
            if entry.path().is_dir() {
                pending.push(entry.path());
            } else {
                logged.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
            }
        }
    }
    assert!(
        logged.contains(TASK_PATH),
        "the operation log records the tool write: {logged:?}"
    );
    assert!(!logged.contains(&events[0].0), "{logged}");
    Ok(())
}

#[tokio::test]
async fn two_servers_sharing_a_vault_record_each_change_exactly_once() -> Result<(), BoxError> {
    let harness = events_harness().await?;
    let second =
        contextos_mcp::ContextOsServer::try_from(support::scoped_config(vec![harness.vault.path().to_path_buf()])?)?;

    update(&harness, json!({"state": "doing"})).await?;
    let result = support::git_harness::call(
        &second,
        "frontmatter_update",
        json!({"path": TASK_PATH, "patch": {"priority": "high"}}),
    )
    .await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");

    assert_eq!(
        events_of_kind(harness.vault.path(), "task.state-changed").len(),
        1,
        "the second process must see the first one's record, not fire the change again"
    );
    Ok(())
}

#[tokio::test]
async fn two_servers_changing_the_same_task_at_once_fire_the_change_exactly_once() -> Result<(), BoxError> {
    let harness = events_harness().await?;
    let second =
        contextos_mcp::ContextOsServer::try_from(support::scoped_config(vec![harness.vault.path().to_path_buf()])?)?;
    let patch = json!({"path": TASK_PATH, "patch": {"state": "doing"}});

    let (first_result, second_result) = tokio::join!(
        support::git_harness::call(&harness.server, "frontmatter_update", patch.clone()),
        support::git_harness::call(&second, "frontmatter_update", patch),
    );

    assert_eq!(first_result?.is_error, Some(false));
    assert_eq!(second_result?.is_error, Some(false));
    assert_eq!(events_of_kind(harness.vault.path(), "task.state-changed").len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_member_state_event_is_staged_in_git_with_the_team_file_and_raises_nothing_further() -> Result<(), BoxError> {
    let harness = support::events_vault::events_harness_with(
        |_| {},
        |root| {
            std::fs::create_dir_all(root.join(".contextos"))?;
            std::fs::write(
                root.join(".contextos/team.toml"),
                "[[member]]\nslug = \"ellis\"\nname = \"Ellis\"\n",
            )
        },
    )
    .await?;
    let before = harness.commit_count().await?;
    let result = harness
        .structured("agent_state_change", json!({"agent": "ellis", "state": "blocked"}))
        .await?;

    let event_path = result
        .get("event_path")
        .and_then(serde_json::Value::as_str)
        .ok_or("no event_path")?;
    let pending = harness.pending_paths().await?;
    assert!(pending.contains(&event_path.to_owned()), "{pending:?}");
    assert_eq!(event_notes(harness.vault.path()).len(), 1);
    harness.settle()?;
    assert_eq!(harness.commit_count().await?, before + 1);
    Ok(())
}

#[tokio::test]
async fn a_folder_of_tasks_taken_away_forgets_them_whatever_the_folder_is_called() -> Result<(), BoxError> {
    let harness = support::events_vault::events_harness_with(
        |_| {},
        |root| {
            std::fs::create_dir_all(root.join("memory/tasks/2026.Q1"))?;
            std::fs::write(
                root.join("memory/tasks/2026.Q1/a-009.md"),
                task_note("A-009", "next", ""),
            )
        },
    )
    .await?;
    harness
        .structured(
            "fs_move_file",
            json!({"source": "memory/tasks/2026.Q1", "destination": "elsewhere"}),
        )
        .await?;
    assert_eq!(event_notes(harness.vault.path()), Vec::new(), "a move fires nothing");

    harness
        .structured(
            "fs_write_file",
            json!({"path": "memory/tasks/2026.Q1/a-009.md", "content": task_note("A-009", "next", "")}),
        )
        .await?;

    assert_eq!(
        events_of_kind(harness.vault.path(), "task.created").len(),
        1,
        "the task that left was forgotten, so one made afresh at its path is new: {:?}",
        event_notes(harness.vault.path())
    );
    Ok(())
}

#[tokio::test]
async fn creating_a_task_with_note_create_fires_task_created() -> Result<(), BoxError> {
    let harness = events_harness().await?;

    harness
        .structured(
            "note_create",
            json!({
                "path": "memory/tasks/b-001.md",
                "frontmatter": {
                    "type": "task", "task_type": "action", "id": "B-001", "entity": "zenith",
                    "state": "backlog", "assignee": "ellis"
                },
                "content": "# Task B-001\n"
            }),
        )
        .await?;

    let created = events_of_kind(harness.vault.path(), "task.created");
    assert_eq!(created.len(), 1, "{:?}", event_notes(harness.vault.path()));
    assert_eq!(field(&created[0].1, "subject_id").as_deref(), Some("B-001"));
    Ok(())
}

#[tokio::test]
async fn recording_a_task_through_its_base_fires_task_created() -> Result<(), BoxError> {
    let harness = events_harness_with(
        |_| {},
        |root| {
            std::fs::write(
                root.join("memory/tasks.md"),
                "---\nbase: '[[memory/tasks.base|Tasks]]'\nlast_create: 2026-10-28T00:00:00Z\n\
                 last_update: 2026-10-28T00:00:00Z\nkey_prefixes:\n  A: 1\nkey_format: '{key_prefix}-{key_id}'\n\
                 fixed_fields: [type]\n---\n",
            )
        },
    )
    .await?;

    let created = harness
        .call(
            "base_record_create",
            json!({
                "path": "memory/tasks.base",
                "prefix": "A",
                "frontmatter": {
                    "type": "task", "task_type": "action", "entity": "zenith",
                    "state": "backlog", "assignee": "ellis"
                }
            }),
        )
        .await?;
    assert_eq!(created.is_error, Some(false), "{created:?}");

    let fired = events_of_kind(harness.vault.path(), "task.created");
    assert_eq!(fired.len(), 1, "{:?}", event_notes(harness.vault.path()));
    assert_eq!(
        field(&fired[0].1, "subject_id").as_deref(),
        Some("A-002"),
        "{}",
        fired[0].1
    );
    Ok(())
}

#[tokio::test]
async fn a_task_moved_to_another_vault_is_created_there_and_dropped_silently_from_the_source() -> Result<(), BoxError> {
    let first = tempfile::Builder::new().prefix("first").tempdir()?;
    let second = tempfile::Builder::new().prefix("second").tempdir()?;
    for vault in [first.path(), second.path()] {
        std::fs::create_dir_all(vault.join("memory/tasks"))?;
        std::fs::write(vault.join("memory/tasks.base"), support::events_vault::TASKS_BASE)?;
    }
    std::fs::write(first.path().join(TASK_PATH), task_note("A-001", "next", ""))?;
    let server = contextos_mcp::ContextOsServer::try_from(support::scoped_config(vec![
        first.path().to_path_buf(),
        second.path().to_path_buf(),
    ])?)?;
    for vault in [first.path(), second.path()] {
        support::git_harness::call(&server, "git_init", json!({"vault": vault})).await?;
    }
    // The first write to each vault builds its table from the notes as they are, silently.
    support::git_harness::call(
        &server,
        "fs_write_file",
        json!({"path": second.path().join("memory/tasks/seed.md"), "content": "# Seed\n"}),
    )
    .await?;
    support::git_harness::call(
        &server,
        "frontmatter_update",
        json!({"path": first.path().join(TASK_PATH), "patch": {"tags": ["x"]}}),
    )
    .await?;
    assert_eq!(event_notes(first.path()), Vec::new());

    let moved = support::git_harness::call(
        &server,
        "fs_move_file",
        json!({"source": first.path().join(TASK_PATH), "destination": second.path().join(TASK_PATH)}),
    )
    .await?;
    assert_eq!(moved.is_error, Some(false), "{moved:?}");

    let created = events_of_kind(second.path(), "task.created");
    assert_eq!(created.len(), 1, "{:?}", event_notes(second.path()));
    assert_eq!(field(&created[0].1, "subject_id").as_deref(), Some("A-001"));
    assert_eq!(
        event_notes(first.path()),
        Vec::new(),
        "the source vault drops its entry silently"
    );
    Ok(())
}

#[tokio::test]
async fn a_refused_write_to_a_task_fires_no_event_and_changes_nothing() -> Result<(), BoxError> {
    let harness = events_harness().await?;
    let before = std::fs::read_to_string(harness.vault.path().join(TASK_PATH))?;

    let stale = harness
        .call(
            "fs_write_file",
            json!({
                "path": TASK_PATH,
                "content": task_note("A-001", "doing", ""),
                "expected_hash": "0000000000000000000000000000000000000000000000000000000000000000"
            }),
        )
        .await?;
    let invalid = harness
        .call(
            "frontmatter_update",
            json!({"path": "memory/tasks/missing.md", "patch": {"state": "doing"}}),
        )
        .await?;
    let created_nothing = harness
        .call(
            "fs_write_file",
            json!({"path": "../outside.md", "content": task_note("A-009", "backlog", "")}),
        )
        .await?;

    for refused in [&stale, &invalid, &created_nothing] {
        assert_eq!(refused.is_error, Some(true), "{refused:?}");
    }
    assert_eq!(event_notes(harness.vault.path()), Vec::new());
    assert_eq!(std::fs::read_to_string(harness.vault.path().join(TASK_PATH))?, before);
    Ok(())
}

/// The frontmatter that makes a note a task of the scaffold's `action` type.
fn task_patch(id: &str) -> serde_json::Value {
    json!({
        "type": "task", "task_type": "action", "id": id, "entity": "zenith",
        "state": "backlog", "assignee": "ellis"
    })
}

/// Creates a note in the tasks folder that is not a task, returning its content hash.
async fn plain_note(harness: &Harness, path: &str) -> Result<String, BoxError> {
    let created = harness
        .structured(
            "note_create",
            json!({"path": path, "frontmatter": {"type": "note"}, "content": "# Not yet a task\n"}),
        )
        .await?;
    assert_eq!(
        event_notes(harness.vault.path()),
        Vec::new(),
        "a plain note fires nothing"
    );
    created["content_hash"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "note_create returned no content_hash".into())
}

#[tokio::test]
async fn turning_a_note_into_a_task_with_note_update_fires_task_created() -> Result<(), BoxError> {
    let harness = events_harness().await?;
    let hash = plain_note(&harness, "memory/tasks/c-001.md").await?;

    harness
        .structured(
            "note_update",
            json!({"path": "memory/tasks/c-001.md", "expected_hash": hash, "patch": task_patch("C-001")}),
        )
        .await?;

    let created = events_of_kind(harness.vault.path(), "task.created");
    assert_eq!(created.len(), 1, "{:?}", event_notes(harness.vault.path()));
    assert_eq!(field(&created[0].1, "subject_id").as_deref(), Some("C-001"));
    assert_eq!(field(&created[0].1, "state").as_deref(), Some("backlog"));
    Ok(())
}

#[tokio::test]
async fn turning_a_note_into_a_task_with_frontmatter_update_fires_task_created() -> Result<(), BoxError> {
    let harness = events_harness().await?;
    plain_note(&harness, "memory/tasks/c-002.md").await?;

    harness
        .structured(
            "frontmatter_update",
            json!({"path": "memory/tasks/c-002.md", "patch": task_patch("C-002")}),
        )
        .await?;

    let created = events_of_kind(harness.vault.path(), "task.created");
    assert_eq!(created.len(), 1, "{:?}", event_notes(harness.vault.path()));
    assert_eq!(field(&created[0].1, "subject_id").as_deref(), Some("C-002"));
    Ok(())
}

#[tokio::test]
async fn updating_something_that_was_already_a_task_does_not_fire_task_created_again() -> Result<(), BoxError> {
    let harness = events_harness().await?;
    let created = harness
        .structured(
            "note_create",
            json!({
                "path": "memory/tasks/c-003.md",
                "frontmatter": task_patch("C-003"),
                "content": "# Task C-003\n"
            }),
        )
        .await?;
    let hash = created["content_hash"].as_str().ok_or("no content_hash")?.to_owned();
    assert_eq!(events_of_kind(harness.vault.path(), "task.created").len(), 1);

    harness
        .structured(
            "note_update",
            json!({"path": "memory/tasks/c-003.md", "expected_hash": hash, "patch": {"priority": "high"}}),
        )
        .await?;
    update(&harness, json!({"priority": "low"})).await?;

    assert_eq!(
        events_of_kind(harness.vault.path(), "task.created").len(),
        1,
        "{:?}",
        event_notes(harness.vault.path())
    );
    Ok(())
}

#[tokio::test]
async fn updating_a_task_the_server_found_in_place_does_not_announce_it_as_created() -> Result<(), BoxError> {
    let harness = events_harness().await?;

    update(&harness, json!({"priority": "high"})).await?;

    assert_eq!(events_of_kind(harness.vault.path(), "task.created").len(), 0);
    Ok(())
}

#[tokio::test]
async fn a_task_moved_out_of_the_tasks_fires_nothing_whether_moved_alone_or_with_its_folder() -> Result<(), BoxError> {
    let harness = support::events_vault::events_harness_with(
        |_| {},
        |root| {
            std::fs::create_dir_all(root.join("memory/tasks/inbox"))?;
            std::fs::write(root.join("memory/tasks/inbox/a-008.md"), task_note("A-008", "next", ""))
        },
    )
    .await?;
    harness
        .structured(
            "fs_move_file",
            json!({"source": TASK_PATH, "destination": "archive/a-001.md"}),
        )
        .await?;
    harness
        .structured(
            "fs_move_file",
            json!({"source": "memory/tasks/inbox", "destination": "archive/inbox"}),
        )
        .await?;

    assert_eq!(event_notes(harness.vault.path()), Vec::new());
    Ok(())
}
