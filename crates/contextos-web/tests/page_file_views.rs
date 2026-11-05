//! Page file view contract tests: a Mermaid diagram, a Canvas, and a Base shown on the screen that owns their folder,
//! the Base's view tabs, and its row edits, through real `contextos-mcp` sessions against real temporary vaults.

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

async fn request(router: &Router, method: &str, path: &str, body: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))?,
        )
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

// ---------------------------------------------------------------------
// Mermaid rendering
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_valid_standalone_mermaid_file_renders_to_svg() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/diagram.mermaid", "graph TD\n  A --> B\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/diagram.mermaid")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<svg"));
    assert!(!body.contains("diagnostic-panel"));
    Ok(())
}

#[tokio::test]
async fn a_malformed_mermaid_diagram_renders_the_diagnostic_panel_not_a_server_error() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/bad.mermaid",
        "not valid mermaid syntax {{{\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/bad.mermaid")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("diagnostic-panel"));
    assert!(!body.contains("<svg"));
    Ok(())
}

// ---------------------------------------------------------------------
// Canvas rendering
// ---------------------------------------------------------------------

const VALID_CANVAS: &str = r#"{
  "nodes": [
    {"id": "n1", "type": "text", "x": 0, "y": 0, "width": 200, "height": 60, "text": "Node One"},
    {"id": "n2", "type": "text", "x": 300, "y": 0, "width": 200, "height": 60, "text": "Node Two"}
  ],
  "edges": [
    {"id": "e1", "fromNode": "n1", "toNode": "n2"}
  ]
}"#;

#[tokio::test]
async fn a_valid_canvas_file_renders_to_svg_using_its_own_positions() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/diagram.canvas", VALID_CANVAS)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/diagram.canvas")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<svg"));
    assert!(body.contains("data-id=\"n1\""));
    assert!(body.contains("x=\"0\""));
    assert!(body.contains("x=\"300\""));
    Ok(())
}

#[tokio::test]
async fn a_malformed_canvas_file_renders_the_diagnostic_panel_not_a_server_error() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/bad.canvas", "{ this is not valid json")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/bad.canvas")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("diagnostic-panel"));
    Ok(())
}

#[tokio::test]
async fn canvas_mutation_methods_all_return_405_read_only_in_v1() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "diagram.canvas", VALID_CANVAS)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    for method in ["POST", "PATCH", "PUT", "DELETE"] {
        let (status, _) = request(&router, method, &format!("/{VAULT_NAME}/diagram.canvas"), "{}").await?;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "method {method}");
    }
    // The file was never touched by the read-only route.
    assert_eq!(
        std::fs::read_to_string(vault_dir.path().join("diagram.canvas"))?,
        VALID_CANVAS
    );
    Ok(())
}

// ---------------------------------------------------------------------
// `.base` rendering and CRUD round trip
// ---------------------------------------------------------------------

const TASKS_BASE: &str =
    "views:\n  - type: table\n    name: \"Active\"\n    order:\n      - file.path\n      - status\n";

#[tokio::test]
async fn a_base_file_renders_its_matched_rows_as_a_card_grid() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/active.base", TASKS_BASE)?;
    write(
        vault_dir.path(),
        "task-one.md",
        "---\nstatus: active\n---\n# Task one\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/active.base")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("task-one.md"));
    // No screen owns a note at the vault root: the card names it without linking anywhere.
    assert!(body.contains("No screen shows this note yet."), "{body}");
    assert!(!body.contains(&format!("href=\"/{VAULT_NAME}/task-one.md\"")));
    assert!(body.contains("active"));
    Ok(())
}

/// The view switcher must be `base_query`-backed (real `?view=`
/// resolution), never a client-side re-filter of already-fetched rows: a
/// per-view `filters` expression (`base_query.rs`'s
/// `QueryDefinition::from_document`, `merge_and`) narrows which rows each
/// named view actually returns, and switching the `?view=` query parameter
/// on the real route must return the differently-filtered row set.
const MULTI_VIEW_BASE: &str = "views:\n  - type: table\n    name: \"Active\"\n    filters: 'status == \"active\"'\n    order:\n      - file.path\n      - status\n  - type: table\n    name: \"Done\"\n    filters: 'status == \"done\"'\n    order:\n      - file.path\n      - status\n";

#[tokio::test]
async fn switching_the_view_tab_re_queries_base_query_with_the_named_views_own_filters() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/work.base", MULTI_VIEW_BASE)?;
    write(
        vault_dir.path(),
        "task-active.md",
        "---\nstatus: active\n---\n# Task active\n",
    )?;
    write(
        vault_dir.path(),
        "task-done.md",
        "---\nstatus: done\n---\n# Task done\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    // No `?view=` at all: defaults to the first view ("Active"), matching
    // `base_query`'s own default and the tab strip marking it active.
    //
    // Checked via each row's own `data-note-path` (the row-edit form's own
    // marker), not a bare filename substring match.
    let (status, default_body) = get(&router, &format!("/{VAULT_NAME}/wiki/work.base")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(default_body.contains("data-note-path=\"task-active.md\""));
    assert!(!default_body.contains("data-note-path=\"task-done.md\""));
    assert!(tab_is_active(&default_body, "Active"), "{default_body}");
    assert!(!tab_is_active(&default_body, "Done"), "{default_body}");

    // Switching to the "Done" tab: the exact htmx request the tab's own
    // `hx-get` issues returns the "Done" view's own differently-filtered rows.
    let tab = default_body
        .split("hx-get=\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .find(|href| href.contains("view=Done"))
        .ok_or("no Done tab")?
        .replace("&amp;", "&");
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(&tab)
                .header("hx-request", "true")
                .body(Body::empty())?,
        )
        .await?;
    let status = response.status();
    let done_body = String::from_utf8_lossy(&to_bytes(response.into_body(), usize::MAX).await?).into_owned();
    assert_eq!(status, StatusCode::OK);
    assert!(done_body.contains("data-note-path=\"task-done.md\""));
    assert!(!done_body.contains("data-note-path=\"task-active.md\""));
    assert!(tab_is_active(&done_body, "Done"), "{done_body}");
    assert!(!tab_is_active(&done_body, "Active"), "{done_body}");
    Ok(())
}

/// Whether the view-tab button labelled `name` carries the `active` class,
/// tolerant of `askama`'s own exact attribute whitespace/ordering: finds
/// the `<button ...>{name}</button>` block and checks its own `class`
/// attribute value, rather than matching a brittle multi-line literal.
fn tab_is_active(body: &str, name: &str) -> bool {
    let label = format!(">{name}</button>");
    let Some(label_at) = body.find(&label) else {
        return false;
    };
    let Some(button_at) = body[..label_at].rfind("<button") else {
        return false;
    };
    body[button_at..label_at].contains("class=\"active\"")
}

#[tokio::test]
async fn editing_a_row_dispatches_frontmatter_update_never_touching_the_base_file() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "tasks.base", TASKS_BASE)?;
    write(
        vault_dir.path(),
        "task-one.md",
        "---\nstatus: active\n---\n# Task one\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _) = request(
        &router,
        "PATCH",
        &format!("/{VAULT_NAME}/tasks.base"),
        r#"{"target": "row", "note_path": "task-one.md", "patch": {"status": "done"}}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);

    // The row's own note was updated...
    let note = std::fs::read_to_string(vault_dir.path().join("task-one.md"))?;
    assert!(note.contains("status: done"));
    // ...and the `.base` file's own definition was never written (the two
    // mutation targets are never conflated).
    let base_source = std::fs::read_to_string(vault_dir.path().join("tasks.base"))?;
    assert_eq!(base_source, TASKS_BASE);
    Ok(())
}

#[tokio::test]
async fn editing_the_view_definition_dispatches_base_apply_never_touching_a_matched_note() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "tasks.base", TASKS_BASE)?;
    write(
        vault_dir.path(),
        "task-one.md",
        "---\nstatus: active\n---\n# Task one\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _) = request(
        &router,
        "PATCH",
        &format!("/{VAULT_NAME}/tasks.base"),
        r#"{"target": "definition", "operations": [{"op": "add_formula", "name": "count_it", "expression": "1"}]}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);

    // The `.base` file's own definition changed...
    let base_source = std::fs::read_to_string(vault_dir.path().join("tasks.base"))?;
    assert_ne!(base_source, TASKS_BASE);
    assert!(base_source.contains("count_it"));
    // ...and the matched note was never written.
    let note = std::fs::read_to_string(vault_dir.path().join("task-one.md"))?;
    assert!(note.contains("status: active"));
    Ok(())
}

#[tokio::test]
async fn deleting_from_a_base_route_removes_a_definition_entry_never_a_note() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let base_with_formula =
        "formulas:\n  count_it: '1'\nviews:\n  - type: table\n    name: \"Active\"\n    order:\n      - file.path\n";
    write(vault_dir.path(), "tasks.base", base_with_formula)?;
    write(
        vault_dir.path(),
        "task-one.md",
        "---\nstatus: active\n---\n# Task one\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _) = request(
        &router,
        "DELETE",
        &format!("/{VAULT_NAME}/tasks.base"),
        r#"{"operations": [{"op": "remove_formula", "name": "count_it"}]}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);

    let base_source = std::fs::read_to_string(vault_dir.path().join("tasks.base"))?;
    assert!(!base_source.contains("count_it"));
    // The note file still exists, untouched.
    assert!(vault_dir.path().join("task-one.md").exists());
    Ok(())
}
