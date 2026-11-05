//! The task creation screen and route choose the id series from the task
//! type, offering only the types whose prefix `memory/tasks.md` declares:
//! a real `contextos-mcp` session over a real temporary vault, no mocking.

mod support;

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode};

use contextos_web::McpClientSet;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";

const TASKS_BASE_YAML: &str = "filters: file.inFolder(\"memory/tasks\") && file.basename != \"index\"\n\
    views:\n  - type: table\n    name: All\n    order:\n      - file.path\n";

async fn router_over(vault_dir: &Path, config_dir: &Path) -> Result<Router, BoxError> {
    let config_path = support::write_vault_config(config_dir, vault_dir)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(contextos_web::build_router(
        clients,
        Some(config_dir),
        &config_dir.join("web.toml"),
    ))
}

async fn get(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

async fn post(router: &Router, json_body: &str) -> Result<(StatusCode, String, HeaderMap), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/{VAULT_NAME}/tasks/"))
                .header("content-type", "application/json")
                .body(Body::from(json_body.to_owned()))?,
        )
        .await?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned(), headers))
}

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

/// A vault whose tasks sidecar declares each `(prefix, last id)` series, with
/// `A` as the default.
fn vault_declaring(series: &[(&str, u64)]) -> Result<tempfile::TempDir, BoxError> {
    let vault = tempfile::tempdir()?;
    write(vault.path(), "index.md", "# Root\n")?;
    write(vault.path(), "memory/tasks.base", TASKS_BASE_YAML)?;
    let mut counters = String::new();
    for (prefix, last_id) in series {
        writeln!(counters, "  {prefix}: {last_id}")?;
    }
    write(
        vault.path(),
        "memory/tasks.md",
        &format!(
            "---\nkey_prefix: A\nkey_format: '{{key_prefix}}-{{key_id}}'\nkey_prefixes:\n{counters}\
             last_create: 2026-01-01T00:00:00Z\nlast_update: 2026-01-01T00:00:00Z\n---\n"
        ),
    )?;
    Ok(vault)
}

fn redirect_of(headers: &HeaderMap) -> Result<&str, BoxError> {
    headers
        .get("HX-Redirect")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| "HX-Redirect header present".into())
}

fn created_notes(vault: &Path) -> Result<Vec<String>, BoxError> {
    let folder = vault.join("memory/tasks");
    if !folder.exists() {
        return Ok(Vec::new());
    }
    let mut names: Vec<String> = std::fs::read_dir(folder)?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    Ok(names)
}

#[tokio::test]
async fn the_new_task_screen_offers_only_the_types_whose_prefix_the_sidecar_declares() -> Result<(), BoxError> {
    let vault = vault_declaring(&[("A", 0), ("D", 0)])?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/new")).await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(r#"data-field="task_type""#), "{body}");
    assert!(body.contains(r#"value="action""#));
    assert!(body.contains(r#"value="decision""#));
    assert!(!body.contains(r#"value="question""#), "Q is not declared");
    Ok(())
}

#[tokio::test]
async fn the_new_task_screen_offers_the_question_type_when_its_prefix_is_declared() -> Result<(), BoxError> {
    let vault = vault_declaring(&[("A", 0), ("D", 0), ("Q", 0)])?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault.path(), config_dir.path()).await?;

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/tasks/new")).await?;

    assert!(body.contains(r#"value="question""#), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_single_series_sidecar_offers_no_type_choice() -> Result<(), BoxError> {
    let vault = vault_declaring(&[("A", 0)])?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/new")).await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains(r#"data-field="task_type""#));
    Ok(())
}

#[tokio::test]
async fn a_decision_task_mints_its_id_from_the_decision_series() -> Result<(), BoxError> {
    let vault = vault_declaring(&[("A", 3), ("D", 2)])?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault.path(), config_dir.path()).await?;

    let (status, body, headers) = post(&router, r#"{"title": "Pick a store", "task_type": "decision"}"#).await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(redirect_of(&headers)?, format!("/{VAULT_NAME}/tasks/d-003"));
    let note = std::fs::read_to_string(vault.path().join("memory/tasks/d-003.md"))?;
    assert!(note.to_ascii_lowercase().contains("id: d-003"), "{note}");
    assert!(note.contains("task_type: decision"), "{note}");
    let sidecar = std::fs::read_to_string(vault.path().join("memory/tasks.md"))?;
    assert!(sidecar.contains("D: 3"), "{sidecar}");
    assert!(sidecar.contains("A: 3"), "the action counter is untouched: {sidecar}");
    Ok(())
}

#[tokio::test]
async fn a_question_task_mints_its_id_from_the_question_series() -> Result<(), BoxError> {
    let vault = vault_declaring(&[("A", 1), ("D", 1), ("Q", 0)])?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault.path(), config_dir.path()).await?;

    let (status, body, headers) = post(&router, r#"{"title": "Which store?", "task_type": "question"}"#).await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(redirect_of(&headers)?, format!("/{VAULT_NAME}/tasks/q-001"));
    let note = std::fs::read_to_string(vault.path().join("memory/tasks/q-001.md"))?;
    assert!(note.contains("task_type: question"), "{note}");
    Ok(())
}

#[tokio::test]
async fn omitting_the_type_creates_an_action_from_the_action_series() -> Result<(), BoxError> {
    let vault = vault_declaring(&[("A", 3), ("D", 2)])?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault.path(), config_dir.path()).await?;

    let (status, body, headers) = post(&router, r#"{"title": "Ship it"}"#).await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(redirect_of(&headers)?, format!("/{VAULT_NAME}/tasks/a-004"));
    let note = std::fs::read_to_string(vault.path().join("memory/tasks/a-004.md"))?;
    assert!(note.contains("task_type: action"), "{note}");
    Ok(())
}

#[tokio::test]
async fn a_type_the_sidecar_does_not_declare_is_rejected_and_creates_nothing() -> Result<(), BoxError> {
    for (series, task_type) in [
        (vec![("A", 0), ("D", 0)], "question"),
        (vec![("A", 0)], "decision"),
        (vec![("A", 0), ("D", 0)], "project"),
    ] {
        let vault = vault_declaring(&series)?;
        let config_dir = tempfile::tempdir()?;
        let router = router_over(vault.path(), config_dir.path()).await?;
        let sidecar_before = std::fs::read_to_string(vault.path().join("memory/tasks.md"))?;

        let (status, body, headers) =
            post(&router, &format!(r#"{{"title": "Nope", "task_type": "{task_type}"}}"#)).await?;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(headers.get("HX-Redirect").is_none(), "{task_type} must not redirect");
        assert!(body.contains("not available"), "{body}");
        assert!(created_notes(vault.path())?.is_empty(), "{task_type} wrote a note");
        assert_eq!(
            std::fs::read_to_string(vault.path().join("memory/tasks.md"))?,
            sidecar_before
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_task_type_that_is_not_a_string_is_rejected_rather_than_defaulted() -> Result<(), BoxError> {
    for task_type in ["5", "true", "[\"decision\"]", "{\"a\": 1}"] {
        let vault = vault_declaring(&[("A", 0), ("D", 0)])?;
        let config_dir = tempfile::tempdir()?;
        let router = router_over(vault.path(), config_dir.path()).await?;

        let (status, body, headers) =
            post(&router, &format!(r#"{{"title": "Nope", "task_type": {task_type}}}"#)).await?;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(headers.get("HX-Redirect").is_none(), "{task_type} must not redirect");
        assert!(created_notes(vault.path())?.is_empty(), "{task_type} wrote a note");
    }
    Ok(())
}

#[tokio::test]
async fn a_new_task_left_to_its_type_starts_in_its_types_initial_state_with_no_review_date() -> Result<(), BoxError> {
    let vault = vault_declaring(&[("A", 0), ("D", 0), ("Q", 0)])?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault.path(), config_dir.path()).await?;

    for task_type in ["action", "decision", "question"] {
        let (status, body, _) = post(
            &router,
            &format!(r#"{{"title": "Settle it", "task_type": "{task_type}", "state": "", "entity": "zenith"}}"#),
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    for (file, state) in [("a-001.md", "next"), ("d-001.md", "proposed"), ("q-001.md", "open")] {
        let note = std::fs::read_to_string(vault.path().join("memory/tasks").join(file))?;
        assert!(note.contains(&format!("state: {state}")), "{note}");
        assert!(
            !note.contains("scheduled:"),
            "nothing schedules a review on its own: {note}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_chosen_state_of_the_type_and_a_chosen_review_date_are_kept_as_given() -> Result<(), BoxError> {
    let vault = vault_declaring(&[("A", 0), ("D", 0)])?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault.path(), config_dir.path()).await?;

    let (status, body, _) = post(
        &router,
        r#"{"title": "Choose a host", "task_type": "decision", "state": "proposed", "scheduled": "2026-11-01", "entity": "zenith"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let note = std::fs::read_to_string(vault.path().join("memory/tasks/d-001.md"))?;
    assert!(note.contains("state: proposed"), "{note}");
    assert!(note.contains("scheduled: 2026-11-01"), "{note}");
    Ok(())
}

#[tokio::test]
async fn a_chosen_state_the_type_does_not_declare_is_refused_and_creates_nothing() -> Result<(), BoxError> {
    let vault = vault_declaring(&[("A", 0), ("D", 0)])?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault.path(), config_dir.path()).await?;

    let (status, body, headers) = post(
        &router,
        r#"{"title": "Choose a host", "task_type": "decision", "state": "doing", "entity": "zenith"}"#,
    )
    .await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("is not a state of a Decision") && body.contains("Proposed, Adopted, Dropped"),
        "{body}"
    );
    assert!(headers.get("HX-Redirect").is_none());
    assert!(
        created_notes(vault.path())?.is_empty(),
        "a refused state writes no note"
    );
    Ok(())
}

#[tokio::test]
async fn a_task_created_in_a_closing_state_is_stamped_with_its_close_date() -> Result<(), BoxError> {
    let vault = vault_declaring(&[("A", 0), ("D", 0)])?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault.path(), config_dir.path()).await?;

    let (status, body, _) = post(
        &router,
        r#"{"title": "Record a choice", "task_type": "decision", "state": "adopted", "entity": "zenith"}"#,
    )
    .await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    let note = std::fs::read_to_string(vault.path().join("memory/tasks/d-001.md"))?;
    assert!(
        note.contains("state: adopted") && note.contains("completed: 20"),
        "{note}"
    );
    Ok(())
}

#[tokio::test]
async fn while_tasks_toml_cannot_be_read_no_task_is_created_and_the_reason_is_given() -> Result<(), BoxError> {
    let vault = vault_declaring(&[("A", 0)])?;
    write(
        vault.path(),
        ".contextos/tasks.toml",
        "[[stage]]\nstate = \"triage\"\nshade = \"red\"\n",
    )?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault.path(), config_dir.path()).await?;

    let (_, body, headers) = post(
        &router,
        r#"{"title": "Triage it", "state": "triage", "entity": "zenith"}"#,
    )
    .await?;

    assert!(
        body.contains("tasks.toml could not be read") && body.contains("shade"),
        "{body}"
    );
    assert!(headers.get("HX-Redirect").is_none());
    assert!(
        created_notes(vault.path())?.is_empty(),
        "nothing is created against a configuration that cannot be read"
    );
    Ok(())
}

#[tokio::test]
async fn the_new_task_form_leaves_the_state_to_the_type_and_says_where_each_type_starts() -> Result<(), BoxError> {
    let vault = vault_declaring(&[("A", 0), ("D", 0)])?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault.path(), config_dir.path()).await?;

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/tasks/new")).await?;
    assert!(
        body.contains(r#"<option value="" selected>Default for its type</option>"#),
        "{body}"
    );
    assert!(
        body.contains("A new action starts Next and a new decision starts Proposed, unless you choose its state."),
        "{body}"
    );
    assert!(
        body.contains("Proposed (Decision)"),
        "every type's states are offered: {body}"
    );
    Ok(())
}

#[tokio::test]
async fn a_vault_declared_task_type_and_its_prefix_are_offered_and_used() -> Result<(), BoxError> {
    let vault = vault_declaring(&[("A", 0), ("C", 0)])?;
    write(
        vault.path(),
        ".contextos/tasks.toml",
        "[[task_type]]\nvalue = \"action\"\nprefix = \"A\"\n\n[[task_type]]\nvalue = \"chore\"\nlabel = \"Chore\"\nprefix = \"C\"\n",
    )?;
    let config_dir = tempfile::tempdir()?;
    let router = router_over(vault.path(), config_dir.path()).await?;

    let (_, form) = get(&router, &format!("/{VAULT_NAME}/tasks/new")).await?;
    assert!(form.contains(r#"value="chore""#), "{form}");
    assert!(!form.contains(r#"value="decision""#), "{form}");
    let (status, body, _) = post(
        &router,
        r#"{"title": "Water plants", "task_type": "chore", "state": "", "entity": "zenith"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        vault.path().join("memory/tasks/c-001.md").exists(),
        "{:?}",
        created_notes(vault.path())?
    );
    Ok(())
}
