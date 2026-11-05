//! Tasks screen: the folder a task note lives in beneath `memory/tasks/`, chosen when the task is created, shown on
//! its detail page, and changed by moving the note. Real `contextos-mcp` sessions against real temporary vault
//! fixtures, no mocking, per `testing.md`.

mod support;
mod tasks_common;

use tasks_common::*;

const TASKS_BASE_YAML: &str = "filters: file.inFolder(\"memory/tasks\") && file.basename != \"index\"\n\
    views:\n  - type: table\n    name: All\n    order:\n      - file.path\n";

fn seed_tasks_base(vault_dir: &Path) -> std::io::Result<()> {
    write(vault_dir, "index.md", "# Root\n")?;
    write(vault_dir, "memory/tasks.base", TASKS_BASE_YAML)?;
    write(
        vault_dir,
        "memory/tasks.md",
        "---\nkey_prefix: A\nkey_format: '{key_prefix}-{key_id}'\nlast_id: 0\n\
         last_create: 2026-01-01T00:00:00Z\nlast_update: 2026-01-01T00:00:00Z\n---\n",
    )
}

/// The detail page's read-only folder value, as the sidebar marks it up.
fn shown_folder(folder: &str) -> String {
    format!("<span class=\"task-field-value mono\">{folder}</span>")
}

async fn post(router: &Router, path: &str, json_body: &str) -> Result<(StatusCode, String), BoxError> {
    method_with_body(router, "POST", path, json_body).await
}

// ---------------------------------------------------------------------------
// Creating a task in a folder
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_create_form_offers_a_folder_with_the_existing_task_folders_as_suggestions() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    seed_tasks_base(vault_dir.path())?;
    write(
        vault_dir.path(),
        "memory/tasks/clients/acme/t-001.md",
        &task_note("T-001", "next", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/new")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("name=\"folder\""), "{body}");
    assert!(body.contains("<option value=\"clients/acme\">"), "{body}");
    // An ancestor of a folder holding tasks is a folder a task can be filed in too.
    assert!(body.contains("<option value=\"clients\">"), "{body}");
    assert!(
        !body.contains("memory/tasks/</span>"),
        "the folder box carries no fixed tasks folder prefix: {body}"
    );
    Ok(())
}

#[tokio::test]
async fn creating_a_task_with_a_folder_files_its_note_in_that_folder() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    seed_tasks_base(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = post(
        &router,
        &format!("/{VAULT_NAME}/tasks/"),
        r#"{"title": "Ship the thing", "folder": " clients/acme/ "}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        vault_dir.path().join("memory/tasks/clients/acme/a-001.md").is_file(),
        "the new note is filed in the chosen folder"
    );
    assert!(!vault_dir.path().join("memory/tasks/a-001.md").exists());

    let (status, detail) = get(&router, &format!("/{VAULT_NAME}/tasks/a-001")).await?;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert!(detail.contains(&shown_folder("clients/acme")), "{detail}");
    Ok(())
}

#[tokio::test]
async fn creating_a_task_with_no_folder_files_it_at_the_top_of_the_tasks_folder() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    seed_tasks_base(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = post(
        &router,
        &format!("/{VAULT_NAME}/tasks/"),
        r#"{"title": "Ship the thing", "folder": ""}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(vault_dir.path().join("memory/tasks/a-001.md").is_file());
    Ok(())
}

#[tokio::test]
async fn creating_a_task_in_a_folder_that_leaves_the_tasks_folder_is_refused_and_writes_nothing() -> Result<(), BoxError>
{
    let refused = [
        "../decisions",
        "/memory/tasks/clients",
        "clients\\acme",
        "clients//acme",
        "C:/clients",
        ".hidden",
        "clients/.archive",
    ];
    for folder in refused {
        let vault_dir = tempfile::tempdir()?;
        let config_dir = tempfile::tempdir()?;
        seed_tasks_base(vault_dir.path())?;
        let sidecar_before = std::fs::read_to_string(vault_dir.path().join("memory/tasks.md"))?;
        let router = router_over(vault_dir.path(), config_dir.path()).await?;

        let request = serde_json::json!({ "title": "Ship the thing", "folder": folder }).to_string();
        let (status, body) = post(&router, &format!("/{VAULT_NAME}/tasks/"), &request).await?;
        assert_eq!(status, StatusCode::OK, "{folder:?}: {body}");
        assert!(body.contains("Not created: the folder"), "{folder:?}: {body}");
        assert_eq!(
            std::fs::read_to_string(vault_dir.path().join("memory/tasks.md"))?,
            sidecar_before,
            "{folder:?}: no id is spent on a refused folder"
        );
        assert!(!vault_dir.path().join("memory/decisions").exists(), "{folder:?}");
    }
    Ok(())
}

#[tokio::test]
async fn creating_a_task_with_a_folder_that_is_not_text_is_refused() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    seed_tasks_base(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = post(
        &router,
        &format!("/{VAULT_NAME}/tasks/"),
        r#"{"title": "Ship the thing", "folder": ["clients", "acme"]}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Not created: the folder"), "{body}");
    assert!(!vault_dir.path().join("memory/tasks/a-001.md").exists());
    Ok(())
}

// ---------------------------------------------------------------------------
// Viewing and changing an existing task's folder
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_detail_page_shows_the_folder_and_the_edit_form_offers_to_move_it() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    seed_tasks_base(vault_dir.path())?;
    write(
        vault_dir.path(),
        "memory/tasks/clients/acme/t-001.md",
        &task_note("T-001", "next", "zenith", None),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/internal/t-002.md",
        &task_note("T-002", "next", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, read_view) = get(&router, &format!("/{VAULT_NAME}/tasks/t-001")).await?;
    assert_eq!(status, StatusCode::OK, "{read_view}");
    assert!(
        read_view.contains(&shown_folder("clients/acme")),
        "the folder is shown relative to the tasks folder: {read_view}"
    );
    assert!(
        !read_view.contains(&shown_folder("memory/tasks/clients/acme")),
        "the folder carries no fixed tasks folder prefix: {read_view}"
    );

    let (status, edit_view) = get(&router, &format!("/{VAULT_NAME}/tasks/t-001/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{edit_view}");
    assert!(
        edit_view.contains("name=\"folder\" value=\"clients/acme\""),
        "{edit_view}"
    );
    assert!(edit_view.contains("<option value=\"internal\">"), "{edit_view}");
    assert!(
        !edit_view.contains("memory/tasks/</span>"),
        "the folder box carries no fixed tasks folder prefix: {edit_view}"
    );
    Ok(())
}

#[tokio::test]
async fn the_detail_page_names_a_task_at_the_top_of_the_tasks_folder_as_the_top_level() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    seed_tasks_base(vault_dir.path())?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note("T-001", "next", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, read_view) = get(&router, &format!("/{VAULT_NAME}/tasks/t-001")).await?;
    assert_eq!(status, StatusCode::OK, "{read_view}");
    assert!(
        read_view.contains("<span class=\"task-field-value mono\"><span class=\"hint\">Top level</span></span>"),
        "{read_view}"
    );
    assert!(!read_view.contains(&shown_folder("memory/tasks")), "{read_view}");
    Ok(())
}

#[tokio::test]
async fn the_detail_page_shows_a_folder_the_move_form_cannot_name_relative_to_the_tasks_folder() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    seed_tasks_base(vault_dir.path())?;
    // `#` breaks a link naming the task by its path, so a folder typed here may not hold one, but a note filed
    // there outside this screen is still a task and still says where it lives.
    write(
        vault_dir.path(),
        "memory/tasks/q1#review/t-001.md",
        &task_note("T-001", "next", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    for page in ["t-001", "t-001/edit"] {
        let (status, view) = get(&router, &format!("/{VAULT_NAME}/tasks/{page}")).await?;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert!(view.contains(&shown_folder("q1#review")), "{page}: {view}");
        assert!(
            !view.contains(&shown_folder("memory/tasks/q1#review")),
            "{page}: {view}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn moving_a_task_to_another_folder_moves_its_note_and_keeps_its_page() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    seed_tasks_base(vault_dir.path())?;
    let original = task_note("T-001", "next", "zenith", None);
    write(vault_dir.path(), "memory/tasks/inbox/t-001.md", &original)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/tasks/t-001"),
        r#"{"folder": "clients/acme"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(&shown_folder("clients/acme")), "{body}");
    assert!(!vault_dir.path().join("memory/tasks/inbox/t-001.md").exists());
    assert_eq!(
        std::fs::read_to_string(vault_dir.path().join("memory/tasks/clients/acme/t-001.md"))?,
        original,
        "a move changes where the note lives, never what it says"
    );

    let (status, detail) = get(&router, &format!("/{VAULT_NAME}/tasks/t-001")).await?;
    assert_eq!(status, StatusCode::OK, "{detail}");
    Ok(())
}

#[tokio::test]
async fn moving_a_task_to_an_empty_folder_files_it_at_the_top_of_the_tasks_folder() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    seed_tasks_base(vault_dir.path())?;
    write(
        vault_dir.path(),
        "memory/tasks/inbox/t-001.md",
        &task_note("T-001", "next", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(&router, &format!("/{VAULT_NAME}/tasks/t-001"), r#"{"folder": ""}"#).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(vault_dir.path().join("memory/tasks/t-001.md").is_file());
    assert!(!vault_dir.path().join("memory/tasks/inbox/t-001.md").exists());
    Ok(())
}

#[tokio::test]
async fn moving_a_task_to_the_folder_it_is_already_in_changes_nothing() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    seed_tasks_base(vault_dir.path())?;
    write(
        vault_dir.path(),
        "memory/tasks/inbox/t-001.md",
        &task_note("T-001", "next", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/tasks/t-001"),
        r#"{"folder": "inbox/"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("Not moved"), "{body}");
    assert!(vault_dir.path().join("memory/tasks/inbox/t-001.md").is_file());
    Ok(())
}

#[tokio::test]
async fn moving_a_task_onto_a_note_of_the_same_name_is_refused_and_replaces_nothing() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    seed_tasks_base(vault_dir.path())?;
    write(
        vault_dir.path(),
        "memory/tasks/inbox/t-001.md",
        &task_note("T-001", "next", "zenith", None),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/archive/t-001.md",
        "# Someone else's note\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/tasks/t-001"),
        r#"{"folder": "archive"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Not moved"), "{body}");
    assert!(body.contains("already filed in archive"), "{body}");
    assert!(!body.contains("memory/tasks/archive"), "{body}");
    assert!(vault_dir.path().join("memory/tasks/inbox/t-001.md").is_file());
    assert_eq!(
        std::fs::read_to_string(vault_dir.path().join("memory/tasks/archive/t-001.md"))?,
        "# Someone else's note\n"
    );
    Ok(())
}

#[tokio::test]
async fn moving_a_task_out_of_the_tasks_folder_is_refused_and_moves_nothing() -> Result<(), BoxError> {
    for folder in [
        "../decisions",
        "/memory/notes",
        "inbox\\old",
        ".trash",
        "inbox/../../notes",
        "a#b",
        "x\n- fake line",
    ] {
        let vault_dir = tempfile::tempdir()?;
        let config_dir = tempfile::tempdir()?;
        seed_tasks_base(vault_dir.path())?;
        write(
            vault_dir.path(),
            "memory/tasks/inbox/t-001.md",
            &task_note("T-001", "next", "zenith", None),
        )?;
        let router = router_over(vault_dir.path(), config_dir.path()).await?;

        let request = serde_json::json!({ "folder": folder }).to_string();
        let (status, body) = patch(&router, &format!("/{VAULT_NAME}/tasks/t-001"), &request).await?;
        assert_eq!(status, StatusCode::OK, "{folder:?}: {body}");
        assert!(body.contains("Not moved: the folder"), "{folder:?}: {body}");
        assert!(
            vault_dir.path().join("memory/tasks/inbox/t-001.md").is_file(),
            "{folder:?}"
        );
        assert!(!vault_dir.path().join("memory/decisions").exists(), "{folder:?}");
    }
    Ok(())
}

#[tokio::test]
async fn a_folder_submitted_with_other_fields_is_not_mistaken_for_a_move() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    seed_tasks_base(vault_dir.path())?;
    write(
        vault_dir.path(),
        "memory/tasks/inbox/t-001.md",
        &task_note("T-001", "next", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/tasks/t-001"),
        r#"{"folder": "archive", "title": "Renamed"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("Malformed") && !body.contains("Not moved"), "{body}");
    let saved = std::fs::read_to_string(vault_dir.path().join("memory/tasks/inbox/t-001.md"))?;
    assert!(
        saved.contains("title: Renamed"),
        "the submission was saved as fields: {saved}"
    );
    assert!(!vault_dir.path().join("memory/tasks/archive").exists());

    // A close action's shape (one field and the hash) beside a folder is not a move either.
    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/tasks/t-001"),
        r#"{"folder": "archive", "expected_hash": "0000"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("Not moved"), "{body}");
    assert!(vault_dir.path().join("memory/tasks/inbox/t-001.md").is_file());
    assert!(!vault_dir.path().join("memory/tasks/archive").exists());
    Ok(())
}

#[tokio::test]
async fn a_tasks_base_that_takes_no_sub_folders_refuses_a_move_into_one() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    seed_tasks_base(vault_dir.path())?;
    write(
        vault_dir.path(),
        "memory/tasks.base",
        "filters: file.folder == \"memory/tasks\" && file.basename != \"index\"\n\
         views:\n  - type: table\n    name: All\n",
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note("T-001", "next", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/tasks/t-001"),
        r#"{"folder": "clients"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Not moved:"), "{body}");
    assert!(body.contains("sub-folder"), "{body}");
    assert!(vault_dir.path().join("memory/tasks/t-001.md").is_file());
    assert!(!vault_dir.path().join("memory/tasks/clients").exists());
    Ok(())
}

#[tokio::test]
async fn a_move_the_server_refuses_for_another_reason_says_why() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    seed_tasks_base(vault_dir.path())?;
    write(
        vault_dir.path(),
        "memory/tasks/inbox/t-001.md",
        &task_note("T-001", "next", "zenith", None),
    )?;
    // A file where the destination folder would be: the server cannot create the folder.
    write(vault_dir.path(), "memory/tasks/clients", "not a folder\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/tasks/t-001"),
        r#"{"folder": "clients"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Not moved:"), "{body}");
    assert!(
        !body.contains("the note could not be moved"),
        "the server's own reason is shown: {body}"
    );
    assert!(vault_dir.path().join("memory/tasks/inbox/t-001.md").is_file());
    Ok(())
}

#[tokio::test]
async fn a_tasks_base_that_takes_no_sub_folders_refuses_one_with_its_own_reason() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    seed_tasks_base(vault_dir.path())?;
    write(
        vault_dir.path(),
        "memory/tasks.base",
        "filters: file.folder == \"memory/tasks\" && file.basename != \"index\"\n\
         views:\n  - type: table\n    name: All\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = post(
        &router,
        &format!("/{VAULT_NAME}/tasks/"),
        r#"{"title": "Ship the thing", "folder": "clients"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Not created:"), "{body}");
    assert!(body.contains("sub-folder"), "{body}");
    assert!(!vault_dir.path().join("memory/tasks/clients").exists());
    Ok(())
}
