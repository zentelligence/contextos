//! Inbox/Outbox folder navigation contract tests: browsing nested folders in both trees, uploading and ingesting
//! from a browsed folder, and confinement to each tree, through real `contextos-mcp` sessions against real temporary
//! vaults.

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

async fn post_json(router: &Router, path: &str, json_body: &str) -> Result<(StatusCode, String), BoxError> {
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
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

/// Builds one `multipart/form-data` request over a single `file` field, the
/// same shape the Inbox upload form's own `hx-encoding="multipart/form-data"`
/// submission produces, and posts it to `path`.
async fn post_upload(
    router: &Router,
    path: &str,
    filename: &str,
    bytes: &[u8],
) -> Result<(StatusCode, String), BoxError> {
    const BOUNDARY: &str = "contract-test-boundary";
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n").as_bytes(),
    );
    body.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", format!("multipart/form-data; boundary={BOUNDARY}"))
                .body(Body::from(body))?,
        )
        .await?;
    let status = response.status();
    let response_body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&response_body).into_owned()))
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

/// Seeds `memory/tasks.base` and its sidecar so `base_record_create`
/// (Ingest's own task-creation call) has a real, sequential-key base to
/// write against, matching `tests/tasks.rs`'s identical fixture.
fn seed_tasks_base(vault_dir: &Path) -> std::io::Result<()> {
    write(vault_dir, "memory/tasks.base", TASKS_BASE_YAML)?;
    write(
        vault_dir,
        "memory/tasks.md",
        "---\nkey_prefix: A\nkey_format: '{key_prefix}-{key_id}'\nlast_id: 0\n\
         last_create: 2026-01-01T00:00:00Z\nlast_update: 2026-01-01T00:00:00Z\n---\n",
    )
}

fn seed_karla(vault_dir: &Path) -> std::io::Result<()> {
    write(
        vault_dir,
        ".contextos/team.toml",
        "[[member]]\nslug = \"karla\"\nname = \"Karla\"\n",
    )
}

// ---------------------------------------------------------------------
// Nested folder navigation
// ---------------------------------------------------------------------

#[tokio::test]
async fn the_merged_root_lists_top_level_folders_from_both_trees() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "inbox/2026-09/receipt.md", "# Receipt\n")?;
    write(vault_dir.path(), "processed/2026-08/old.md", "# Old\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/inbox/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("2026-09"), "{body}");
    assert!(body.contains("2026-08"), "{body}");
    assert!(
        body.contains(&format!("href=\"/{VAULT_NAME}/inbox/2026-09\"")),
        "{body}"
    );
    assert!(
        body.contains(&format!("href=\"/{VAULT_NAME}/inbox/processed/2026-08\"")),
        "{body}"
    );
    assert!(
        !body.contains("/inbox/inbox"),
        "the inbox name must not be repeated: {body}"
    );
    // Files inside those subfolders are not flattened into the root view.
    assert!(!body.contains("receipt.md"), "{body}");
    assert!(!body.contains("old.md"), "{body}");
    Ok(())
}

#[tokio::test]
async fn browsing_an_inbox_subfolder_shows_its_own_contents_and_a_named_ancestor_trail() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "inbox/2026-09/receipt.md", "# Receipt\n")?;
    write(vault_dir.path(), "inbox/2026-09/receipts/scan.md", "# Scan\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/inbox/2026-09")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("receipt.md"), "{body}");
    assert!(body.contains("receipts"), "{body}");
    assert!(!body.contains("scan.md"), "{body}");
    // A named row for every ancestor (never a ".." idiom), the current
    // folder itself carrying no link.
    assert!(body.contains("</span> Inbox</a>"), "{body}");
    assert!(body.contains("href=\"/contract-fixture/inbox/\""), "{body}");
    assert!(
        !body.contains("</span> inbox</a>"),
        "the inbox directory is the Inbox row itself: {body}"
    );
    assert!(
        body.contains(&format!("href=\"/{VAULT_NAME}/inbox/2026-09/receipts\"")),
        "{body}"
    );
    // The current folder is a non-linked `<div>`, not an `<a>`.
    assert!(body.contains("ancestor-row current"), "{body}");
    assert!(body.contains("</span> 2026-09</div>"), "{body}");
    assert!(!body.contains(">..<"), "{body}");
    Ok(())
}

#[tokio::test]
async fn browsing_a_processed_folder_shows_its_contents_beneath_the_processed_name() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "processed/2026-08/old.md", "# Old\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/inbox/processed/2026-08")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("old.md"), "{body}");
    assert!(
        body.contains(&format!("href=\"/{VAULT_NAME}/inbox/processed\"")),
        "{body}"
    );
    assert!(body.contains("</span> 2026-08</div>"), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_deeper_inbox_trail_links_every_ancestor_at_its_own_short_path() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "inbox/2026-09/receipts/scan.md", "# Scan\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/inbox/2026-09/receipts")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("scan.md"), "{body}");
    assert!(body.contains(&format!("href=\"/{VAULT_NAME}/inbox/\"")), "{body}");
    assert!(
        body.contains(&format!("href=\"/{VAULT_NAME}/inbox/2026-09\"")),
        "{body}"
    );
    assert!(!body.contains("/inbox/inbox"), "{body}");
    Ok(())
}

#[tokio::test]
async fn an_inbox_path_naming_another_part_of_the_vault_stays_confined_to_the_inbox() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/tasks/secret.md", "# Secret\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/inbox/memory/tasks")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(!body.contains("secret.md"), "{body}");
    Ok(())
}

#[tokio::test]
async fn uploading_into_a_browsed_subfolder_writes_it_there() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "inbox/2026-09/receipt.md", "# Receipt\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = post_upload(
        &router,
        &format!("/{VAULT_NAME}/inbox/2026-09/execute/upload"),
        "note.md",
        b"# A note\n",
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let written = std::fs::read_to_string(vault_dir.path().join("inbox/2026-09/note.md"))?;
    assert_eq!(written, "# A note\n");
    Ok(())
}

#[tokio::test]
async fn upload_is_refused_while_browsing_the_processed_tree() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "processed/2026-08/old.md", "# Old\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = post_upload(
        &router,
        &format!("/{VAULT_NAME}/inbox/processed/2026-08/execute/upload"),
        "note.md",
        b"# A note\n",
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("processed"), "{body}");
    assert!(!vault_dir.path().join("processed/2026-08/note.md").exists());
    Ok(())
}

#[tokio::test]
async fn a_traversal_attempt_in_the_browse_path_falls_back_to_the_merged_root_with_an_error() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "inbox/draft.md", "# Draft\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/inbox/2026-09/../escape")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("hint error"), "{body}");
    // Falls back to the merged root's own real content rather than a blank
    // or broken page.
    assert!(body.contains("draft.md"), "{body}");
    Ok(())
}

#[tokio::test]
async fn ingesting_a_whole_folder_creates_one_task_naming_the_folder_and_touches_nothing_inside_it()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "inbox/2026-09/receipt.md", "# Receipt\n")?;
    write(vault_dir.path(), "inbox/2026-09/scan.md", "# Scan\n")?;
    seed_tasks_base(vault_dir.path())?;
    seed_karla(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = post_json(
        &router,
        &format!("/{VAULT_NAME}/inbox/execute/ingest"),
        r#"{"folder_path": "inbox/2026-09"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Created task"), "{body}");

    // Exactly one task, naming the folder as a whole.
    assert!(!vault_dir.path().join("memory/tasks/a-002.md").exists());
    let task_content = std::fs::read_to_string(vault_dir.path().join("memory/tasks/a-001.md"))?;
    assert!(task_content.contains("Ingest 2026-09/"), "{task_content}");
    assert!(task_content.contains("inbox/2026-09"), "{task_content}");

    // Neither file inside the folder is touched.
    assert_eq!(
        std::fs::read_to_string(vault_dir.path().join("inbox/2026-09/receipt.md"))?,
        "# Receipt\n"
    );
    assert_eq!(
        std::fs::read_to_string(vault_dir.path().join("inbox/2026-09/scan.md"))?,
        "# Scan\n"
    );
    Ok(())
}

#[tokio::test]
async fn a_processed_folder_offers_no_ingest_action() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "processed/2026-08/old.md", "# Old\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/inbox/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("Ingest folder"), "{body}");
    Ok(())
}

#[tokio::test]
async fn outbox_supports_the_same_folder_navigation_with_no_ingest_concept() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "outbox/2026-09/report.md", "# Report\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, root_body) = get(&router, &format!("/{VAULT_NAME}/outbox/")).await?;
    assert_eq!(status, StatusCode::OK, "{root_body}");
    assert!(root_body.contains("2026-09"), "{root_body}");
    assert!(
        root_body.contains(&format!("href=\"/{VAULT_NAME}/outbox/2026-09\"")),
        "{root_body}"
    );
    assert!(!root_body.contains("/outbox/outbox"), "{root_body}");

    let (status, sub_body) = get(&router, &format!("/{VAULT_NAME}/outbox/2026-09")).await?;
    assert_eq!(status, StatusCode::OK, "{sub_body}");
    assert!(sub_body.contains("report.md"), "{sub_body}");
    assert!(sub_body.contains("</span> Outbox</a>"), "{sub_body}");
    assert!(!sub_body.contains("Ingest"), "{sub_body}");
    Ok(())
}
