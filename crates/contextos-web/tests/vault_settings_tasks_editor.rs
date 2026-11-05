//! Vault settings: the structured Tasks configuration editor over `.contextos/tasks.toml` (task types and their own
//! stages, the shared stages, hierarchy depth labels, and priorities). Real `contextos-mcp` sessions against real
//! temporary vault fixtures.

mod support;
mod vault_settings_common;

use serde_json::{Map, Value, json};
use vault_settings_common::*;

/// Every row of `rows` under `prefix`, as a browser submits it: an object's fields as `prefix.index.key`, a nested
/// list of rows (a task type's own stages) one level further in, and a plain value as `prefix.index`.
fn flatten_rows(fields: &mut Map<String, Value>, prefix: &str, rows: &Value) {
    for (index, row) in rows.as_array().into_iter().flatten().enumerate() {
        match row {
            Value::Object(row) => {
                for (key, value) in row {
                    if value.is_array() {
                        flatten_rows(fields, &format!("{prefix}.{index}.{key}"), value);
                    } else {
                        fields.insert(format!("{prefix}.{index}.{key}"), value.clone());
                    }
                }
            }
            other => {
                fields.insert(format!("{prefix}.{index}"), other.clone());
            }
        }
    }
}

/// The Tasks section's form as a browser submits it: every row's fields under indexed names.
fn form(config: &Value, expected_hash: &str, edit_action: Option<&str>) -> String {
    let mut fields = Map::new();
    fields.insert("target".to_owned(), json!("tasks"));
    fields.insert("expected_hash".to_owned(), json!(expected_hash));
    if let Some(action) = edit_action {
        fields.insert("edit_action".to_owned(), json!(action));
    }
    for (list, rows) in config.as_object().into_iter().flatten() {
        flatten_rows(&mut fields, list, rows);
    }
    Value::Object(fields).to_string()
}

fn two_stage_config() -> Value {
    json!({
        "stage": [
            {"state": "next", "label": "Next", "icon": "circle", "bucket": "backlog", "colour": "red"},
            {"state": "done", "label": "Done", "icon": "check", "bucket": "done", "colour": ""},
        ],
        "depth_label": ["Initiative", "Action"],
        "task_type": [
            {"value": "action", "label": "Action", "prefix": "A", "initial": ""},
        ],
        "priority": [
            {"value": "high", "label": "High"},
            {"value": "low", "label": "Low"},
        ],
    })
}

fn tasks_pane(body: &str) -> Result<&str, BoxError> {
    section_panel(body, "tasks")
}

#[tokio::test]
async fn an_absent_tasks_toml_shows_the_built_in_configuration_as_editable_rows() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/settings/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let pane = tasks_pane(&body)?;
    assert!(pane.contains("built-in defaults"), "{pane}");
    assert!(!pane.contains("<textarea"), "the raw text editor is gone: {pane}");
    for state in ["backlog", "next", "doing", "waiting", "review", "done", "dropped"] {
        assert!(pane.contains(&format!("value=\"{state}\"")), "{state}: {pane}");
    }
    assert!(!pane.contains("value=\"pending\""), "pending is not shipped: {pane}");
    assert!(pane.contains("name=\"task_type.1.initial\""), "{pane}");
    assert!(
        pane.contains("name=\"task_type.1.stage.0.state\" value=\"proposed\""),
        "{pane}"
    );
    assert!(
        pane.contains("name=\"task_type.2.stage.1.state\" value=\"answered\""),
        "{pane}"
    );
    assert!(
        pane.contains("Decision stages") && pane.contains("Question stages"),
        "{pane}"
    );
    assert!(pane.contains("The stages of Action, in lifecycle order"), "{pane}");
    assert!(pane.contains("name=\"priority.0.value\" value=\"highest\""), "{pane}");
    Ok(())
}

#[tokio::test]
async fn saving_the_form_writes_every_section_and_keeps_the_files_comments() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        ".contextos/tasks.toml",
        "# Kept by hand.\n[[stage]]\nstate = \"backlog\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;
    let (_, settings_page) = get(&router, &format!("/{VAULT_NAME}/settings/")).await?;
    let hash = {
        let pane = tasks_pane(&settings_page)?;
        let hidden_field = "name=\"expected_hash\" value=\"";
        let value_start = pane.find(hidden_field).ok_or("no hash")? + hidden_field.len();
        let value_end = value_start + pane[value_start..].find('"').ok_or("unterminated")?;
        pane[value_start..value_end].to_owned()
    };

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &form(&two_stage_config(), &hash, None),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!tasks_pane(&body)?.contains("role=\"alert\""), "{body}");
    let saved = std::fs::read_to_string(vault_dir.path().join(".contextos/tasks.toml"))?;
    assert!(saved.contains("# Kept by hand."), "{saved}");
    // A chosen colour is written with its stage; a stage left at its default carries none.
    assert_eq!(saved.matches("colour = ").count(), 1, "{saved}");
    assert!(saved.contains("colour = \"red\""), "{saved}");
    let parsed: contextos_web::TasksConfig = toml::from_str(&saved)?;
    assert_eq!(
        parsed
            .stage
            .iter()
            .map(|stage| stage.state.as_str())
            .collect::<Vec<_>>(),
        ["next", "done"]
    );
    assert_eq!(parsed.hierarchy.depth_labels, ["Initiative", "Action"]);
    assert_eq!(parsed.task_type.len(), 1);
    assert_eq!(
        parsed
            .priority
            .iter()
            .map(|entry| entry.value.as_str())
            .collect::<Vec<_>>(),
        ["high", "low"]
    );
    Ok(())
}

#[tokio::test]
async fn an_invalid_configuration_is_explained_and_never_written() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let mut config = two_stage_config();
    config["task_type"][0]["initial"] = json!("parked");
    let (status, body) = patch(&router, &format!("/{VAULT_NAME}/settings/"), &form(&config, "", None)).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let pane = tasks_pane(&body)?;
    assert!(
        pane.contains("starts new tasks in `parked`, which is not one of its own open states"),
        "{pane}"
    );
    assert!(pane.contains("value=\"next\""), "what was submitted is kept: {pane}");
    assert!(!vault_dir.path().join(".contextos/tasks.toml").exists());
    Ok(())
}

#[tokio::test]
async fn adding_removing_and_moving_rows_changes_the_draft_without_writing() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;
    let config = two_stage_config();

    let (_, added) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &form(&config, "", Some("add:stage")),
    )
    .await?;
    let pane = tasks_pane(&added)?;
    assert!(pane.contains("name=\"stage.2.state\" value=\"\""), "{pane}");
    assert!(pane.contains("Unsaved changes"), "{pane}");

    let (_, moved) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &form(&config, "", Some("down:stage:0")),
    )
    .await?;
    let pane = tasks_pane(&moved)?;
    assert!(pane.contains("name=\"stage.0.state\" value=\"done\""), "{pane}");
    assert!(pane.contains("name=\"stage.1.state\" value=\"next\""), "{pane}");

    let (_, removed) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &form(&config, "", Some("remove:priority:0")),
    )
    .await?;
    let pane = tasks_pane(&removed)?;
    assert!(pane.contains("name=\"priority.0.value\" value=\"low\""), "{pane}");
    assert!(!pane.contains("name=\"priority.1.value\""), "{pane}");

    assert!(
        !vault_dir.path().join(".contextos/tasks.toml").exists(),
        "no draft step writes the file"
    );
    Ok(())
}

#[tokio::test]
async fn a_type_given_its_own_stages_is_saved_with_them_and_can_return_to_the_shared_ones() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;
    let mut config = two_stage_config();
    config["task_type"].as_array_mut().ok_or("types")?.push(json!({
        "value": "question", "label": "Question", "prefix": "Q", "initial": "open",
    }));

    let (_, given) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &form(&config, "", Some("own:task_type:1")),
    )
    .await?;
    let pane = tasks_pane(&given)?;
    assert!(pane.contains("Question stages"), "{pane}");
    assert!(
        pane.contains("name=\"task_type.1.stage.0.state\" value=\"next\""),
        "its own stages start as a copy of the shared ones: {pane}"
    );
    assert!(
        !vault_dir.path().join(".contextos/tasks.toml").exists(),
        "a draft step writes nothing"
    );

    config["task_type"][1]["stage"] = json!([
        {"state": "open", "label": "Open", "icon": "", "bucket": "needs_call", "colour": "", "closes": ""},
        {"state": "answered", "label": "Answered", "icon": "", "bucket": "done", "colour": "", "closes": "completed"},
    ]);
    let (status, body) = patch(&router, &format!("/{VAULT_NAME}/settings/"), &form(&config, "", None)).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!tasks_pane(&body)?.contains("role=\"alert\""), "{body}");
    let saved = std::fs::read_to_string(vault_dir.path().join(".contextos/tasks.toml"))?;
    let parsed: contextos_web::TasksConfig = toml::from_str(&saved)?;
    let own: Vec<&str> = parsed
        .lifecycle("question")
        .iter()
        .map(|stage| stage.state.as_str())
        .collect();
    assert_eq!(own, ["open", "answered"], "{saved}");
    assert_eq!(parsed.task_type[1].initial.as_deref(), Some("open"));
    assert!(saved.contains("closes = \"completed\""), "{saved}");
    let shared: Vec<&str> = parsed
        .lifecycle("action")
        .iter()
        .map(|stage| stage.state.as_str())
        .collect();
    assert_eq!(shared, ["next", "done"]);

    let (_, returned) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &form(&config, "", Some("share:task_type:1")),
    )
    .await?;
    let pane = tasks_pane(&returned)?;
    assert!(!pane.contains("Question stages"), "{pane}");
    assert!(pane.contains("The stages of Action and Question"), "{pane}");
    Ok(())
}

#[tokio::test]
async fn a_malformed_submission_is_refused() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    for body in [
        r#"{"target": "tasks", "stage.0.shade": "red"}"#,
        r#"{"target": "tasks", "stage.x.state": "next"}"#,
        r#"{"target": "tasks", "edit_action": "explode:stage"}"#,
        r#"{"target": "tasks", "stage.0.state": ["next"]}"#,
        r#"{"target": "tasks", "task_type.0.holding_state": "pending"}"#,
    ] {
        let (status, page) = patch(&router, &format!("/{VAULT_NAME}/settings/"), body).await?;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}: {page}");
    }
    assert!(!vault_dir.path().join(".contextos/tasks.toml").exists());
    Ok(())
}

#[tokio::test]
async fn a_stale_save_is_refused_and_the_newer_file_survives() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        ".contextos/tasks.toml",
        "[[stage]]\nstate = \"backlog\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &form(&two_stage_config(), &"0".repeat(64), None),
    )
    .await?;
    assert!(tasks_pane(&body)?.contains("changed after you opened it"), "{body}");
    assert_eq!(
        std::fs::read_to_string(vault_dir.path().join(".contextos/tasks.toml"))?,
        "[[stage]]\nstate = \"backlog\"\n"
    );
    Ok(())
}

#[tokio::test]
async fn a_tasks_toml_that_cannot_be_read_is_reported_rather_than_silently_replaced() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/tasks.toml", "[[stage]\nbroken")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/settings/")).await?;
    let pane = tasks_pane(&body)?;
    assert!(pane.contains("could not be read"), "{pane}");
    assert!(pane.contains("Saving replaces it"), "{pane}");
    Ok(())
}

#[tokio::test]
async fn the_draft_only_buttons_skip_the_browsers_field_checks_but_save_does_not() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/settings/")).await?;
    let pane = tasks_pane(&body)?;
    let action_buttons = pane.matches("name=\"edit_action\"").count();
    assert!(action_buttons > 0);
    assert_eq!(
        pane.matches("formnovalidate name=\"edit_action\"").count(),
        action_buttons,
        "{pane}"
    );
    assert!(
        pane.contains("<button type=\"submit\" class=\"btn primary small\">Save</button>"),
        "{pane}"
    );
    Ok(())
}

#[tokio::test]
async fn an_unknown_key_in_tasks_toml_is_reported_rather_than_dropped_by_the_next_save() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        ".contextos/tasks.toml",
        "[[stage]]\nstate = \"next\"\nshade = \"red\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/settings/")).await?;
    let pane = tasks_pane(&body)?;
    assert!(pane.contains("could not be read"), "{pane}");
    assert!(pane.contains("shade"), "the notice names the unknown key: {pane}");
    Ok(())
}
