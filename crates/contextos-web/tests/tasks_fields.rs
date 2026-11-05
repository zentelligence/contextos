//! Contract tests for the Tasks screen's field-type-aware widgets
//! (`routes::tasks_fields`): a `<select>` backed by real vault-configured
//! data for `entity`/`assignee`/`state`, a `<select>` backed by a fixed
//! value list for `priority`/`task_type`, and a `date` input for every
//! lifecycle field, on both the detail/edit form and the create form. Real
//! `contextos-mcp` sessions against real temporary vault fixtures
//! throughout, no mocking, per `testing.md`. Kept apart from `tests/tasks.rs`
//! (already close to this workspace's own file-size guidance) rather than
//! growing that file further, matching `tests/task_links.rs`'s identical
//! precedent.

mod support;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};

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

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

const ENTITIES_TOML: &str = "\
[[entities]]
slug = \"personal\"
name = \"Personal\"
accent = 1

[[entities]]
slug = \"family\"
name = \"Family\"
accent = 2
";

const AGENTS_TOML: &str = "\
[[member]]
slug = \"sam\"
name = \"Sam\"
domain = \"Ops\"
chief_of_staff = true
state = \"working\"
";

fn task_note(id: &str) -> String {
    format!(
        "---\n\
         id: {id}\n\
         type: task\n\
         title: Title for {id}\n\
         state: doing\n\
         task_type: action\n\
         entity: personal\n\
         assignee: sam\n\
         priority: high\n\
         due: 2026-09-20\n\
         ---\n\n# Title for {id}\n"
    )
}

#[tokio::test]
async fn the_detail_page_renders_entity_assignee_and_state_as_selects_over_real_vault_data() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/entities.toml", ENTITIES_TOML)?;
    write(vault_dir.path(), ".contextos/team.toml", AGENTS_TOML)?;
    write(vault_dir.path(), "memory/tasks/t-001.md", &task_note("T-001"))?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/t-001/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert!(
        body.contains(r#"<select name="entity" data-field="entity""#),
        "entity should render as a select: {body}"
    );
    assert!(
        body.contains(r#"value="personal""#),
        "the configured personal entity should be offered: {body}"
    );
    assert!(
        body.contains("Personal"),
        "the entity's own configured name should be the option label: {body}"
    );
    assert!(
        body.contains(r#"value="family""#),
        "every configured entity should be offered, not only the current one: {body}"
    );

    assert!(
        body.contains(r#"<select name="assignee" data-field="assignee""#),
        "assignee should render as a select: {body}"
    );
    assert!(
        body.contains(r#"value="sam""#),
        "the configured sam agent should be offered: {body}"
    );

    assert!(
        body.contains(r#"<select name="state" data-field="state""#),
        "state should render as a select: {body}"
    );
    assert!(
        body.contains(r#"value="doing""#) && body.contains("Doing"),
        "the configured stage list should back the state select: {body}"
    );
    Ok(())
}

#[tokio::test]
async fn the_detail_page_renders_a_two_column_layout_with_metadata_in_the_sidebar() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/tasks/t-001.md", &task_note("T-001"))?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/t-001/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let main_pos = body.find("task-detail-main").ok_or("main column container present")?;
    let sidebar_pos = body
        .find("task-detail-sidebar")
        .ok_or("sidebar column container present")?;
    assert!(
        main_pos < sidebar_pos,
        "the main column should render before the sidebar: {body}"
    );

    // The body editor is this screen's own genuine primary content (the
    // free-form notes an operator actually reads), so it belongs in the
    // wide main column; every structured frontmatter field, including
    // `title`, is metadata and belongs in the sidebar alongside it. HTML
    // forms cannot nest, so the body editor and the frontmatter fields stay
    // two independent `<form>`s (separate PATCH submissions, unchanged);
    // only their column placement changed.
    let body_editor_pos = body.find("task-body-form").ok_or("the body editor form is present")?;
    assert!(
        main_pos < body_editor_pos && body_editor_pos < sidebar_pos,
        "the body editor should render inside the main column, not the sidebar: {body}"
    );

    let title_pos = body.find(r#"data-field="title""#).ok_or("title field present")?;
    let entity_pos = body.find(r#"data-field="entity""#).ok_or("entity field present")?;
    assert!(
        sidebar_pos < title_pos && title_pos < entity_pos,
        "title should render inside the sidebar, before entity: {body}"
    );

    let link_field_pos = body
        .find("task-link-field")
        .ok_or("the relocated link-fields widget is present")?;
    assert!(
        sidebar_pos < link_field_pos,
        "the link-fields widget should render inside the sidebar, not the main column: {body}"
    );
    Ok(())
}

#[tokio::test]
async fn the_detail_page_renders_a_date_input_for_every_lifecycle_field() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/tasks/t-001.md", &task_note("T-001"))?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/t-001/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains(r#"<input type="date" name="due" data-field="due""#),
        "due should render as a date input: {body}"
    );
    assert!(
        body.contains(r#"value="2026-09-20""#),
        "the date's own value should round-trip: {body}"
    );
    Ok(())
}

#[tokio::test]
async fn the_detail_page_renders_priority_and_task_type_as_fixed_selects() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/tasks/t-001.md", &task_note("T-001"))?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/t-001/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains(r#"<select name="priority" data-field="priority""#),
        "priority should render as a select: {body}"
    );
    assert!(
        body.contains("(none)"),
        "priority must offer a blank/(none) option: {body}"
    );
    assert!(
        body.contains(r#"<select name="task_type" data-field="task_type""#),
        "task_type should render as a select: {body}"
    );
    assert!(body.contains(">Action<") && body.contains(">Decision<"));
    Ok(())
}

#[tokio::test]
async fn entity_falls_back_to_a_text_input_when_the_vault_has_no_configured_entities() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/tasks/t-001.md", &task_note("T-001"))?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/t-001/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !body.contains(r#"<select name="entity" data-field="entity""#),
        "entity must not render as an empty, useless select: {body}"
    );
    assert!(
        body.contains(r#"<input type="text" name="entity" data-field="entity""#),
        "entity should fall back to a plain text input: {body}"
    );
    Ok(())
}

#[tokio::test]
async fn assignee_falls_back_to_a_text_input_when_the_vault_has_no_configured_team() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/tasks/t-001.md", &task_note("T-001"))?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/t-001/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !body.contains(r#"<select name="assignee" data-field="assignee""#),
        "assignee must not render as an empty, useless select: {body}"
    );
    assert!(
        body.contains(r#"<input type="text" name="assignee" data-field="assignee""#),
        "assignee should fall back to a plain text input: {body}"
    );
    assert!(
        !body.contains("value=\"piper\""),
        "no hardcoded identity should ever be offered: {body}"
    );
    Ok(())
}

#[tokio::test]
async fn the_new_task_screen_offers_entity_priority_and_assignee_as_real_selects() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/entities.toml", ENTITIES_TOML)?;
    write(vault_dir.path(), ".contextos/team.toml", AGENTS_TOML)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/new")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert!(body.contains(r#"<select name="entity" data-field="entity">"#));
    assert!(body.contains(r#"value="personal""#) && body.contains(r#"value="family""#));
    assert!(body.contains(r#"<select name="priority" data-field="priority">"#));
    assert!(body.contains("(none)"));
    assert!(body.contains(r#"<select name="assignee" data-field="assignee">"#));
    assert!(body.contains(r#"value="sam""#));
    Ok(())
}

/// Regression test for the create screen's own defining property: nothing
/// but plain, natively-named form fields carries a value into the create
/// POST (`hx-ext="json-enc"`, no `hx-vals` anywhere on this screen), so the
/// server - never client-side JavaScript - owns what a submitted value
/// means. Every field this screen renders needs a `name` matching its
/// `data-field` key, or the value never reaches the request body at all.
#[tokio::test]
async fn every_new_task_field_carries_a_name_matching_its_data_field_key() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/entities.toml", ENTITIES_TOML)?;
    write(vault_dir.path(), ".contextos/team.toml", AGENTS_TOML)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/new")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    for key in ["title", "entity", "priority", "assignee", "state"] {
        assert!(
            body.contains(&format!(r#"name="{key}" data-field="{key}""#)),
            "the new task screen's {key} field must carry name=\"{key}\" alongside data-field=\"{key}\", \
             or its value never reaches the request body at all: {body}"
        );
    }
    assert!(
        !body.contains(r#"data-field="task_type""#),
        "task_type must never be offered on the create screen (see routes::tasks_create::new_task_form): \
         it decides which id prefix base_record_create allocates, and memory/tasks.base only has one scheme: {body}"
    );
    Ok(())
}
