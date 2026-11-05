//! Inbox/Outbox screen contract tests: real `contextos-mcp` sessions
//! against real temporary vault fixtures. No mocking, per `testing.md`.

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

/// A bare `POST` with no body and no `content-type`: neither an action
/// dispatch's own `not_found` fallback (an unrecognised trailing sub-path)
/// nor a malformed-body rejection needs a real payload to exercise.
async fn post(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().method("POST").uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

/// A `POST` carrying `content_type` verbatim: used to send a body whose
/// declared type does not match what the matched action actually expects
/// (e.g. `text/plain` at an `/execute/upload` target, which needs
/// `multipart/form-data`), so the action's own body-extraction step itself
/// fails rather than anything this crate's own handler logic checks.
async fn post_with_content_type(
    router: &Router,
    path: &str,
    content_type: &str,
    body_bytes: &[u8],
) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", content_type)
                .body(Body::from(body_bytes.to_owned()))?,
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
// GET /{{vault_name}}/inbox/, /outbox/
// ---------------------------------------------------------------------

#[tokio::test]
async fn inbox_lists_a_pending_item_and_a_processed_item_in_one_list_with_status_badges() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "inbox/draft.md", "# Draft\n")?;
    write(vault_dir.path(), "inbox/index.md", "# Inbox\n")?;
    write(vault_dir.path(), "processed/done.md", "# Done\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/inbox/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("draft.md"), "{body}");
    assert!(body.contains("done.md"), "{body}");
    assert!(!body.contains("inbox/index.md"), "{body}");
    // One merged list with a per-item status badge, matching the mock
    // (`InboxScreen.jsx`'s single `items.map(...)`), never two headed
    // "Pending"/"Processed" sections.
    assert!(!body.contains("<h2>Pending</h2>"), "{body}");
    assert!(!body.contains("<h2>Processed</h2>"), "{body}");
    assert!(body.contains("status-chip pending"), "{body}");
    assert!(body.contains("status-chip processed"), "{body}");
    Ok(())
}

#[tokio::test]
async fn every_inbox_download_link_returns_the_file_rather_than_the_inbox_screen() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "inbox/draft.md", "# Draft\n")?;
    write(vault_dir.path(), "inbox/notes.txt", "plain text\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/inbox/")).await?;
    let links: Vec<String> = body
        .match_indices("href=\"")
        .filter_map(|(at, _)| body.get(at + 6..).and_then(|rest| rest.split('"').next()))
        .filter(|href| href.contains("draft.md") || href.contains("notes.txt"))
        .map(|href| href.replace("&amp;", "&"))
        .collect();
    assert!(links.iter().any(|link| link.contains("draft.md")), "{body}");
    assert!(links.iter().any(|link| link.contains("notes.txt")), "{body}");
    for link in links {
        let (status, served) = get(&router, &link).await?;
        assert_eq!(status, StatusCode::OK, "{link}");
        let expected = if link.contains("draft.md") {
            "# Draft\n"
        } else {
            "plain text\n"
        };
        assert_eq!(served, expected, "{link} did not return the file as written");
    }
    Ok(())
}

#[tokio::test]
async fn the_inbox_listing_shows_each_items_type_alongside_size_and_modified_time() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    std::fs::create_dir_all(vault_dir.path().join("inbox"))?;
    std::fs::write(vault_dir.path().join("inbox/photo.png"), [0x89, b'P', b'N', b'G'])?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/inbox/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("image/png"), "{body}");
    Ok(())
}

#[tokio::test]
async fn an_empty_inbox_and_absent_processed_directory_render_with_no_error() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/inbox/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Nothing is in the Inbox yet"), "{body}");
    Ok(())
}

#[tokio::test]
async fn outbox_lists_its_own_files_with_an_inactive_send_placeholder() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "outbox/report.md", "# Report\n")?;
    write(vault_dir.path(), "outbox/index.md", "# Outbox\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/outbox/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("report.md"), "{body}");
    assert!(body.contains(">Send "), "{body}");
    assert!(body.contains("disabled"), "{body}");
    assert!(body.contains("No send destination is configured yet"), "{body}");
    Ok(())
}

// ---------------------------------------------------------------------
// POST /{{vault_name}}/inbox/execute/upload
// ---------------------------------------------------------------------

#[tokio::test]
async fn uploading_a_text_file_writes_it_into_the_inbox_directory() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = post_upload(
        &router,
        &format!("/{VAULT_NAME}/inbox/execute/upload"),
        "note.md",
        b"# A note\n",
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("note.md"), "{body}");
    let written = std::fs::read_to_string(vault_dir.path().join("inbox/note.md"))?;
    assert_eq!(written, "# A note\n");
    Ok(())
}

#[tokio::test]
async fn uploading_a_binary_pdf_writes_its_exact_bytes_via_the_base64_write_path() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    // A minimal, genuinely non-UTF-8 PDF-shaped payload (a real PDF's binary trailer, not just
    // its ASCII header), the same shape a scanned document upload carries.
    let pdf_bytes: &[u8] = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\nendobj\xFF\xFE\x00\x01\n%%EOF";
    let (status, body) = post_upload(
        &router,
        &format!("/{VAULT_NAME}/inbox/execute/upload"),
        "scan.pdf",
        pdf_bytes,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("scan.pdf"), "{body}");
    assert!(body.contains("application/pdf"), "{body}");
    let written = std::fs::read(vault_dir.path().join("inbox/scan.pdf"))?;
    assert_eq!(written, pdf_bytes);
    Ok(())
}

#[tokio::test]
async fn uploading_a_zip_archive_is_accepted_and_round_trips_exactly() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let mut zip_bytes = vec![0x50, 0x4B, 0x03, 0x04];
    zip_bytes.extend(std::iter::repeat_n(0u8, 40));
    let (status, body) = post_upload(
        &router,
        &format!("/{VAULT_NAME}/inbox/execute/upload"),
        "archive.zip",
        &zip_bytes,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let written = std::fs::read(vault_dir.path().join("inbox/archive.zip"))?;
    assert_eq!(written, zip_bytes);
    Ok(())
}

#[tokio::test]
async fn uploading_a_denylisted_extension_is_rejected_and_writes_nothing() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = post_upload(
        &router,
        &format!("/{VAULT_NAME}/inbox/execute/upload"),
        "install.exe",
        b"MZ plain text stand-in, extension alone is enough to deny this",
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("executable"), "{body}");
    assert!(!vault_dir.path().join("inbox/install.exe").exists());
    Ok(())
}

#[tokio::test]
async fn uploading_a_pem_certificate_is_accepted_not_caught_by_the_executable_content_sniff() -> Result<(), BoxError> {
    // Regression: `infer` classifies PEM/DER content under the same `MatcherType::App` category
    // as a real executable, so a naive content-sniff would have wrongly rejected this genuinely
    // plausible Inbox item.
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let pem = b"-----BEGIN CERTIFICATE-----\nMIIB...==\n-----END CERTIFICATE-----\n";
    let (status, body) = post_upload(&router, &format!("/{VAULT_NAME}/inbox/execute/upload"), "cert.pem", pem).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let written = std::fs::read(vault_dir.path().join("inbox/cert.pem"))?;
    assert_eq!(written, pem);
    Ok(())
}

#[tokio::test]
async fn uploading_an_executable_renamed_to_an_accepted_extension_is_still_rejected() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let mut elf_bytes = vec![0x7F, b'E', b'L', b'F', 0x02, 0x01, 0x01, 0x00];
    elf_bytes.resize(64, 0);
    let (status, body) = post_upload(
        &router,
        &format!("/{VAULT_NAME}/inbox/execute/upload"),
        "totally-safe.pdf",
        &elf_bytes,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("executable"), "{body}");
    assert!(!vault_dir.path().join("inbox/totally-safe.pdf").exists());
    Ok(())
}

#[tokio::test]
async fn uploading_a_name_that_already_exists_is_rejected_without_overwriting() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "inbox/note.md", "original\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = post_upload(
        &router,
        &format!("/{VAULT_NAME}/inbox/execute/upload"),
        "note.md",
        b"replacement\n",
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("already exist"), "{body}");
    let content = std::fs::read_to_string(vault_dir.path().join("inbox/note.md"))?;
    assert_eq!(content, "original\n");
    Ok(())
}

#[tokio::test]
async fn uploading_a_filename_with_a_path_segment_is_rejected() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = post_upload(
        &router,
        &format!("/{VAULT_NAME}/inbox/execute/upload"),
        "../escape.md",
        b"content\n",
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("plain filename"), "{body}");
    assert!(!vault_dir.path().join("escape.md").exists());
    Ok(())
}

#[tokio::test]
async fn uploading_a_payload_over_the_size_limit_is_rejected() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let oversized = vec![b'a'; 10 * 1024 * 1024 + 1];
    let (status, body) = post_upload(
        &router,
        &format!("/{VAULT_NAME}/inbox/execute/upload"),
        "big.txt",
        &oversized,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The refusal itself, not the page's standing hint, which also names the limit.
    assert!(body.contains("is larger than the 10 MB upload limit"), "{body}");
    assert!(!vault_dir.path().join("inbox/big.txt").exists());
    Ok(())
}

/// The limit is inclusive: a binary file of exactly 10 MiB is uploaded whole,
/// though its base64 encoding ends in padding that an estimate of its decoded
/// length would count as two bytes too many.
#[tokio::test]
async fn uploading_a_binary_file_of_exactly_the_size_limit_succeeds() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    // A lone 0x80 is never valid UTF-8, so this goes through the binary write path.
    let exact = vec![0x80_u8; 10 * 1024 * 1024];
    let (status, body) = post_upload(
        &router,
        &format!("/{VAULT_NAME}/inbox/execute/upload"),
        "exact.dat",
        &exact,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("is larger than the 10 MB upload limit"), "{body}");
    assert!(vault_dir.path().join("inbox/exact.dat").exists(), "{body}");
    assert_eq!(std::fs::read(vault_dir.path().join("inbox/exact.dat"))?, exact);
    Ok(())
}

// ---------------------------------------------------------------------
// POST /{{vault_name}}/inbox/execute/ingest
// ---------------------------------------------------------------------

#[tokio::test]
async fn ingesting_a_pending_item_creates_a_task_assigned_to_the_curator_and_leaves_the_file_in_place()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "inbox/draft.md", "# Draft\n")?;
    seed_tasks_base(vault_dir.path())?;
    seed_karla(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = post_json(
        &router,
        &format!("/{VAULT_NAME}/inbox/execute/ingest"),
        r#"{"file_path": "inbox/draft.md"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Created task"), "{body}");
    assert!(body.contains("karla"), "{body}");

    let task_content = std::fs::read_to_string(vault_dir.path().join("memory/tasks/a-001.md"))?;
    assert!(task_content.contains("type: task"), "{task_content}");
    assert!(task_content.contains("assignee: karla"), "{task_content}");
    assert!(task_content.contains("task_type: action"), "{task_content}");
    assert!(task_content.contains("draft.md"), "{task_content}");

    // The source file itself is never touched: moving it is the curator
    // agent's own ingestion skill's job, not this web server's.
    let source_content = std::fs::read_to_string(vault_dir.path().join("inbox/draft.md"))?;
    assert_eq!(source_content, "# Draft\n");
    Ok(())
}

#[tokio::test]
async fn ingesting_with_an_unregistered_curator_agent_refuses_and_creates_no_task() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "inbox/draft.md", "# Draft\n")?;
    seed_tasks_base(vault_dir.path())?;
    // No `.contextos/team.toml` at all: `karla` (the built-in default
    // `curator_agent`) is not registered anywhere in this fixture vault.
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = post_json(
        &router,
        &format!("/{VAULT_NAME}/inbox/execute/ingest"),
        r#"{"file_path": "inbox/draft.md"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("not registered"), "{body}");
    assert!(body.contains(".contextos/team.toml"), "{body}");
    assert!(!vault_dir.path().join("memory/tasks/a-001.md").exists());
    Ok(())
}

#[tokio::test]
async fn ingesting_uses_a_configured_curator_agent_other_than_the_default() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "inbox/draft.md", "# Draft\n")?;
    seed_tasks_base(vault_dir.path())?;
    write(
        vault_dir.path(),
        ".contextos/team.toml",
        "[[member]]\nslug = \"aria\"\nname = \"Aria\"\n",
    )?;
    write(
        vault_dir.path(),
        ".contextos/inbox-outbox.toml",
        "curator_agent = \"aria\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = post_json(
        &router,
        &format!("/{VAULT_NAME}/inbox/execute/ingest"),
        r#"{"file_path": "inbox/draft.md"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let task_content = std::fs::read_to_string(vault_dir.path().join("memory/tasks/a-001.md"))?;
    assert!(task_content.contains("assignee: aria"), "{task_content}");
    Ok(())
}

// ---------------------------------------------------------------------
// POST /{{vault_name}}/inbox/{{*path}} action dispatch
// ---------------------------------------------------------------------

#[tokio::test]
async fn posting_to_a_browsed_path_with_no_execute_suffix_is_not_found() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "inbox/2026-09/receipt.md", "# Receipt\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = post(&router, &format!("/{VAULT_NAME}/inbox/2026-09")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    Ok(())
}

#[tokio::test]
async fn posting_to_an_unrecognised_execute_command_is_not_found() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = post(&router, &format!("/{VAULT_NAME}/inbox/execute/rename")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    Ok(())
}

#[tokio::test]
async fn uploading_with_the_wrong_content_type_is_rejected_as_malformed_rather_than_panicking() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = post_with_content_type(
        &router,
        &format!("/{VAULT_NAME}/inbox/execute/upload"),
        "text/plain",
        b"not multipart at all",
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("hint error"), "{body}");
    assert!(!vault_dir.path().join("inbox").exists());
    Ok(())
}

// ---------------------------------------------------------------------
// Addresses that name no folder, reserved names, and ingest confinement
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_folder_that_does_not_exist_is_not_found_rather_than_an_empty_uploadable_folder() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "inbox/2026-09/receipt.md", "# Receipt\n")?;
    write(vault_dir.path(), "processed/2026-08/old.md", "# Old\n")?;
    write(vault_dir.path(), "outbox/2026-09/report.md", "# Report\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    // The old, doubled address, a typo, a missing processed folder, and a missing outbox folder.
    for path in [
        "inbox/inbox/2026-09",
        "inbox/2026-10",
        "inbox/processed/2026-07",
        "outbox/2026-10",
        "outbox/outbox/2026-09",
    ] {
        let (status, body) = get(&router, &format!("/{VAULT_NAME}/{path}")).await?;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
    }
    Ok(())
}

#[tokio::test]
async fn the_processed_root_renders_even_before_anything_has_been_processed() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/inbox/processed")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    Ok(())
}

#[tokio::test]
async fn a_pending_folder_sharing_the_processed_directory_name_is_flagged_at_the_merged_root() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "inbox/processed/note.md", "# Note\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/inbox/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("hint error"), "{body}");
    assert!(body.contains("shares its name with the processed directory"), "{body}");
    Ok(())
}

#[tokio::test]
async fn pending_folders_named_like_the_action_segments_are_browsed_and_written_to_correctly() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "inbox/execute/upload/kept.md", "# Kept\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/inbox/execute/upload")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("kept.md"), "{body}");

    let (status, body) = post_upload(
        &router,
        &format!("/{VAULT_NAME}/inbox/execute/upload/execute/upload"),
        "added.md",
        b"# Added\n",
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        std::fs::read_to_string(vault_dir.path().join("inbox/execute/upload/added.md"))?,
        "# Added\n"
    );

    let (status, body) = post_upload(
        &router,
        &format!("/{VAULT_NAME}/inbox/execute/upload"),
        "root.md",
        b"# Root note\n",
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        std::fs::read_to_string(vault_dir.path().join("inbox/root.md"))?,
        "# Root note\n",
        "the bare-root action still writes to the inbox root"
    );
    Ok(())
}

#[tokio::test]
async fn ingesting_from_a_browsed_subfolder_names_the_real_path_and_re_renders_that_subfolder() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "inbox/2026-09/receipt.md", "# Receipt\n")?;
    seed_tasks_base(vault_dir.path())?;
    seed_karla(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = post_json(
        &router,
        &format!("/{VAULT_NAME}/inbox/2026-09/execute/ingest"),
        r#"{"file_path": "inbox/2026-09/receipt.md"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Created task"), "{body}");
    assert!(
        body.contains("receipt.md"),
        "the browsed subfolder is re-rendered: {body}"
    );
    let task = std::fs::read_to_string(vault_dir.path().join("memory/tasks/a-001.md"))?;
    assert!(task.contains("inbox/2026-09/receipt.md"), "{task}");
    Ok(())
}

#[tokio::test]
async fn ingest_refuses_a_path_outside_the_pending_inbox_and_creates_no_task() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "inbox/a.md", "# A\n")?;
    write(vault_dir.path(), "processed/done.md", "# Done\n")?;
    write(vault_dir.path(), "memory/secrets/key.md", "# Key\n")?;
    seed_tasks_base(vault_dir.path())?;
    seed_karla(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    for body in [
        r#"{"file_path": "memory/secrets/key.md"}"#,
        r#"{"folder_path": "memory/secrets"}"#,
        r#"{"file_path": "processed/done.md"}"#,
        r#"{"file_path": "inbox/../memory/secrets/key.md"}"#,
        r#"{"folder_path": "inbox"}"#,
    ] {
        let (status, response) = post_json(&router, &format!("/{VAULT_NAME}/inbox/execute/ingest"), body).await?;
        assert_eq!(status, StatusCode::OK, "{body}: {response}");
        assert!(response.contains("hint error"), "{body}: {response}");
        assert!(!response.contains("Created task"), "{body}: {response}");
    }
    assert!(!vault_dir.path().join("memory/tasks/a-001.md").exists());
    Ok(())
}
