//! Command Centre Tasks screen contract tests for the creation screen:
//! `GET /{{vault_name}}/tasks/new` and `POST /{{vault_name}}/tasks/` against
//! a real `contextos-mcp` session and a real temporary vault fixture, no
//! mocking, per `testing.md`. Kept apart from `tests/tasks.rs` (already
//! close to this workspace's own file-size guidance) rather than growing
//! that file further, matching `tests/tasks_fields.rs`'s identical
//! precedent.

mod support;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode};

use contextos_web::McpClientSet;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";

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

async fn post(router: &Router, path: &str, json_body: &str) -> Result<(StatusCode, String, HeaderMap), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
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

const TASKS_BASE_YAML: &str = "filters: file.inFolder(\"memory/tasks\") && file.basename != \"index\"\n\
    views:\n  - type: table\n    name: All\n    order:\n      - file.path\n";

fn seed_sequential_sidecar(vault_dir: &Path) -> std::io::Result<()> {
    write(vault_dir, "memory/tasks.base", TASKS_BASE_YAML)?;
    write(
        vault_dir,
        "memory/tasks.md",
        "---\nkey_prefix: A\nkey_format: '{key_prefix}-{key_id}'\nlast_id: 0\n\
         last_create: 2026-01-01T00:00:00Z\nlast_update: 2026-01-01T00:00:00Z\n---\n",
    )
}

#[tokio::test]
async fn creating_a_task_allocates_the_next_id_and_redirects_to_its_detail_page() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_sequential_sidecar(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/tasks/"),
        r#"{"title": "Ship the thing", "entity": "zenith"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let redirect = headers
        .get("HX-Redirect")
        .and_then(|value| value.to_str().ok())
        .ok_or("HX-Redirect header present")?;
    assert_eq!(redirect, format!("/{VAULT_NAME}/tasks/a-001"));

    let (status, detail_body) = get(&router, redirect).await?;
    assert_eq!(status, StatusCode::OK, "{detail_body}");
    // The redirect target's default `GET` renders the read view: field values appear as plain text.
    assert!(detail_body.contains("Ship the thing"));
    assert!(detail_body.contains(">action<"));
    assert!(detail_body.contains(">next<"));
    Ok(())
}

/// Where today's Journal entry is, in the default journal folder and date format.
fn journal_path_for_today(vault_dir: &Path) -> std::path::PathBuf {
    let date = contextos_web::journal::format_date(time::OffsetDateTime::now_utc().date());
    vault_dir.join(format!("journal/{}/{}/{date}.md", &date[0..4], &date[5..7]))
}

/// Creating a task lists it in today's Journal entry under the section
/// `.contextos/journal.toml` names for new tasks, whether or not a Journal
/// template is configured: with none, the entry gains just that section.
#[tokio::test]
async fn creating_a_task_lists_it_under_the_configured_section_of_todays_journal_entry() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        ".contextos/journal.toml",
        "task_opened_section = \"New actions\"\n",
    )?;
    seed_sequential_sidecar(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _headers) = post(
        &router,
        &format!("/{VAULT_NAME}/tasks/"),
        r#"{"title": "Ship the thing", "entity": "zenith"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let journal_content = std::fs::read_to_string(journal_path_for_today(vault_dir.path()))?;
    // A task is referenced by its key, never its path, so the entry survives the task being moved.
    assert_eq!(
        journal_content,
        "## New actions\n\n- Ship the thing ([[a-001|A-001]])\n"
    );
    Ok(())
}

/// With no section configured for new tasks (the default), creating a task
/// leaves the Journal alone.
#[tokio::test]
async fn creating_a_task_writes_nothing_to_the_journal_unless_a_section_is_configured() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_sequential_sidecar(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _headers) = post(
        &router,
        &format!("/{VAULT_NAME}/tasks/"),
        r#"{"title": "Ship the thing", "entity": "zenith"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!journal_path_for_today(vault_dir.path()).exists());
    Ok(())
}

/// A blank title is never rejected: the create screen deliberately performs
/// no client-side validation at all (nothing but plain named fields and
/// `hx-ext="json-enc"`, no `hx-vals`), so a blank submission still reaches
/// the server, which defaults it to `Untitled task` and still creates and
/// redirects - the reader edits the real title on the real detail page
/// afterwards, the same as every other field.
#[tokio::test]
async fn creating_a_task_with_no_title_defaults_it_and_still_redirects() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_sequential_sidecar(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(&router, &format!("/{VAULT_NAME}/tasks/"), r#"{"title": ""}"#).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let redirect = headers
        .get("HX-Redirect")
        .and_then(|value| value.to_str().ok())
        .ok_or("HX-Redirect header present")?;
    assert_eq!(redirect, format!("/{VAULT_NAME}/tasks/a-001"));

    let (status, detail_body) = get(&router, redirect).await?;
    assert_eq!(status, StatusCode::OK, "{detail_body}");
    assert!(detail_body.contains("Untitled task"));
    Ok(())
}

/// `GET /{{vault_name}}/tasks/new` renders the identical field-by-field
/// two-column view as an existing task's own detail page, minus
/// `task_type` (`routes::tasks_create::new_task_form`'s own doc comment
/// covers why), with sensible defaults for a screen no note backs yet.
#[tokio::test]
async fn the_new_task_screen_renders_the_same_field_widgets_as_the_detail_page() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/new")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("New task"));
    assert!(body.contains(r#"name="title" data-field="title""#));
    assert!(body.contains(r#"name="state" data-field="state""#));
    assert!(body.contains("value=\"next\""));
    // No `.contextos/team.toml` is configured in this fixture, so `assignee`
    // falls back to a plain text input rather than a select with no options
    // - and, either way, no hardcoded identity value is ever offered.
    assert!(body.contains(r#"name="assignee""#));
    assert!(!body.contains("value=\"piper\""));
    assert!(!body.contains(r#"data-field="task_type""#));
    Ok(())
}

/// The create POST reads exactly the same flatly-named fields
/// (`raw_fields_to_patch`) the frontmatter-save PATCH does: an array field
/// splits its comma-joined value, and the note's initial body content
/// writes atomically in the same `base_record_create` call.
#[tokio::test]
async fn creating_a_task_splits_tags_and_writes_the_initial_body_atomically() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_sequential_sidecar(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/tasks/"),
        r#"{"title": "Ship the thing", "tags": "ops, urgent", "body": "\nInitial content.\n"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let redirect = headers
        .get("HX-Redirect")
        .and_then(|value| value.to_str().ok())
        .ok_or("HX-Redirect header present")?;

    let (status, detail_body) = get(&router, redirect).await?;
    assert_eq!(status, StatusCode::OK, "{detail_body}");
    assert!(detail_body.contains("ops, urgent"));
    assert!(detail_body.contains("Initial content."));
    Ok(())
}
