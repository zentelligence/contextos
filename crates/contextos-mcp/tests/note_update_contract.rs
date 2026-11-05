//! Contract tests for `note_update`: one conflict-checked write that merge-patches a note's frontmatter and replaces
//! its body, keeping `created`, stamping `updated`, and holding the patch and body to the rules a new note follows.
//! Line endings are covered with every other text write in `line_endings_contract.rs`.

mod support;
use contextos_mcp::ContextOsServer;
use contextos_obsidian::FrontmatterDocument;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, Value, json};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

async fn call_tool(
    server: ContextOsServer,
    name: &'static str,
    arguments: Map<String, Value>,
) -> Result<CallToolResult, Box<dyn std::error::Error + Send + Sync>> {
    let (server_transport, client_transport) = tokio::io::duplex(8192);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });
    let mut client = ().serve(client_transport).await?;
    let result = client
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments))
        .await;
    client.close().await?;
    server_handle.await??;
    Ok(result?)
}

fn structured(result: &CallToolResult) -> Result<&Map<String, Value>, Box<dyn std::error::Error + Send + Sync>> {
    result
        .structured_content
        .as_ref()
        .and_then(Value::as_object)
        .ok_or_else(|| std::io::Error::other("tool result omitted structured content").into())
}

fn arguments(value: Value) -> Result<Map<String, Value>, serde_json::Error> {
    serde_json::from_value(value)
}

const NOTE: &str = "---\nid: VD-001\ntitle: One file per record\nstatus: adopted\ncreated: 2026-08-07T09:00+10:00\nupdated: 2026-08-07T09:00+10:00\n---\n\n# One file per record\n\nOld body.\n";

fn vault_with(
    relative: &str,
    content: &str,
) -> Result<(tempfile::TempDir, ContextOsServer), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let path = vault.path().join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    Ok((vault, server))
}

async fn hash_of(server: &ContextOsServer, path: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let read = call_tool(server.clone(), "frontmatter_read", arguments(json!({"path": path}))?).await?;
    structured(&read)?
        .get("content_hash")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| std::io::Error::other("frontmatter_read omitted content_hash").into())
}

fn text(result: &CallToolResult) -> &str {
    result
        .content
        .first()
        .and_then(rmcp::model::ContentBlock::as_text)
        .map_or("", |content| content.text.as_str())
}

fn code(result: &CallToolResult) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    assert_eq!(result.is_error, Some(true), "{result:?}");
    structured(result)?
        .get("code")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| std::io::Error::other("an error result omitted its code").into())
}

#[tokio::test]
async fn one_call_patches_frontmatter_and_replaces_the_body_in_one_write() -> TestResult {
    let (vault, server) = vault_with("decisions/vd-001.md", NOTE)?;
    let hash = hash_of(&server, "decisions/vd-001.md").await?;

    let result = call_tool(
        server.clone(),
        "note_update",
        arguments(json!({
            "path": "decisions/vd-001.md", "expected_hash": hash,
            "patch": {"status": "superseded", "superseded_by": "VD-002"},
            "body": "\n# One file per record\n\nNew body.\n"
        }))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let fields = structured(&result)?;
    assert_eq!(fields.get("path"), Some(&json!("decisions/vd-001.md")));
    let written = std::fs::read_to_string(vault.path().join("decisions/vd-001.md"))?;
    assert_eq!(
        fields.get("content_hash").and_then(Value::as_str),
        Some(hash_of(&server, "decisions/vd-001.md").await?.as_str()),
        "the returned hash is the written note's"
    );
    let document = FrontmatterDocument::try_from(written.as_str())?;
    assert_eq!(document.frontmatter().get("status"), Some(&json!("superseded")));
    assert_eq!(document.frontmatter().get("superseded_by"), Some(&json!("VD-002")));
    assert_eq!(
        document.frontmatter().get("created"),
        Some(&json!("2026-08-07T09:00+10:00"))
    );
    assert_ne!(
        document.frontmatter().get("updated"),
        Some(&json!("2026-08-07T09:00+10:00")),
        "updated is stamped"
    );
    assert_eq!(document.body(), "\n# One file per record\n\nNew body.\n");
    Ok(())
}

#[tokio::test]
async fn a_body_only_update_needs_no_patch_and_a_patch_only_update_keeps_the_body() -> TestResult {
    let (vault, server) = vault_with("note.md", NOTE)?;

    let hash = hash_of(&server, "note.md").await?;
    let body_only = call_tool(
        server.clone(),
        "note_update",
        arguments(json!({"path": "note.md", "expected_hash": hash, "body": "\nReplaced.\n"}))?,
    )
    .await?;
    assert_eq!(body_only.is_error, Some(false), "{body_only:?}");

    let hash = hash_of(&server, "note.md").await?;
    let patch_only = call_tool(
        server.clone(),
        "note_update",
        arguments(json!({"path": "note.md", "expected_hash": hash, "patch": {"status": "superseded"}}))?,
    )
    .await?;
    assert_eq!(patch_only.is_error, Some(false), "{patch_only:?}");
    let written = std::fs::read_to_string(vault.path().join("note.md"))?;
    assert!(
        written.contains("status: superseded") && written.ends_with("---\n\nReplaced.\n"),
        "{written}"
    );
    Ok(())
}

#[tokio::test]
async fn a_stale_missing_or_malformed_hash_writes_nothing() -> TestResult {
    let (vault, server) = vault_with("note.md", NOTE)?;

    for (hash, expected) in [("0".repeat(64), "io/conflict"), ("stale".to_owned(), "io/invalid-hash")] {
        let input = json!({"path": "note.md", "expected_hash": hash, "patch": {"status": "superseded"}});
        let result = call_tool(server.clone(), "note_update", arguments(input)?).await?;
        assert_eq!(code(&result)?, expected);
    }
    let missing = call_tool(
        server.clone(),
        "note_update",
        arguments(json!({"path": "note.md", "patch": {"status": "superseded"}}))?,
    )
    .await?;
    assert_eq!(missing.is_error, Some(true));
    assert!(text(&missing).contains("missing field `expected_hash`"), "{missing:?}");
    assert_eq!(std::fs::read_to_string(vault.path().join("note.md"))?, NOTE);
    Ok(())
}

#[tokio::test]
async fn the_server_maintained_timestamps_cannot_be_patched() -> TestResult {
    let (vault, server) = vault_with("note.md", NOTE)?;
    let hash = hash_of(&server, "note.md").await?;

    for patch in [json!({"created": "2020-01-01"}), json!({"updated": null})] {
        let result = call_tool(
            server.clone(),
            "note_update",
            arguments(json!({"path": "note.md", "expected_hash": hash, "patch": patch}))?,
        )
        .await?;
        assert_eq!(code(&result)?, "format/note-managed-field");
        let remediation = structured(&result)?
            .get("remediation")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(remediation.contains("created is kept"), "{remediation}");
    }
    assert_eq!(std::fs::read_to_string(vault.path().join("note.md"))?, NOTE);
    Ok(())
}

#[tokio::test]
async fn an_invalid_title_body_or_line_ending_is_refused_and_writes_nothing() -> TestResult {
    let (vault, server) = vault_with("note.md", NOTE)?;
    let hash = hash_of(&server, "note.md").await?;

    for (input, expected) in [
        (json!({"patch": {"title": ""}}), "format/note"),
        (json!({"patch": {"description": "One file per record"}}), "format/note"),
        (json!({"body": "See [[unclosed.\n"}), "format/markdown"),
    ] {
        let mut arguments_value = input.clone();
        arguments_value["path"] = json!("note.md");
        arguments_value["expected_hash"] = json!(hash);
        let result = call_tool(server.clone(), "note_update", arguments(arguments_value)?).await?;
        assert_eq!(code(&result)?, expected, "{input}");
    }
    assert_eq!(std::fs::read_to_string(vault.path().join("note.md"))?, NOTE);
    Ok(())
}

#[tokio::test]
async fn nothing_to_change_unknown_fields_a_missing_note_and_a_non_markdown_path_are_refused() -> TestResult {
    let (vault, server) = vault_with("note.md", NOTE)?;
    std::fs::write(vault.path().join("data.txt"), "text\n")?;
    let hash = hash_of(&server, "note.md").await?;

    let empty = call_tool(
        server.clone(),
        "note_update",
        arguments(json!({"path": "note.md", "expected_hash": hash, "patch": {}}))?,
    )
    .await?;
    assert_eq!(code(&empty)?, "io/invalid-argument");

    let unknown = call_tool(
        server.clone(),
        "note_update",
        arguments(json!({"path": "note.md", "expected_hash": hash, "content": "x"}))?,
    )
    .await?;
    assert_eq!(unknown.is_error, Some(true));
    assert!(text(&unknown).contains("unknown field `content`"), "{unknown:?}");

    let missing = call_tool(
        server.clone(),
        "note_update",
        arguments(json!({"path": "absent.md", "expected_hash": hash, "body": "x\n"}))?,
    )
    .await?;
    assert_eq!(code(&missing)?, "path/not-found");

    let text = call_tool(
        server.clone(),
        "note_update",
        arguments(json!({"path": "data.txt", "expected_hash": hash, "body": "x\n"}))?,
    )
    .await?;
    assert_eq!(code(&text)?, "io/invalid-argument");

    let escape = call_tool(
        server,
        "note_update",
        arguments(json!({"path": "../outside.md", "expected_hash": hash, "body": "x\n"}))?,
    )
    .await?;
    assert_eq!(code(&escape)?, "path/outside-root");
    Ok(())
}

#[tokio::test]
async fn a_note_starting_with_a_byte_order_mark_keeps_its_frontmatter() -> TestResult {
    let source = "\u{feff}---\ntitle: Marked\ncreated: 2026-08-07T09:00+10:00\n---\nOld\n";
    let (vault, server) = vault_with("note.md", source)?;
    let read = call_tool(
        server.clone(),
        "frontmatter_read",
        arguments(json!({"path": "note.md"}))?,
    )
    .await?;
    assert_eq!(
        structured(&read)?
            .get("frontmatter")
            .and_then(|fields| fields.get("title")),
        Some(&json!("Marked"))
    );
    let hash = hash_of(&server, "note.md").await?;

    let result = call_tool(
        server,
        "note_update",
        arguments(json!({"path": "note.md", "expected_hash": hash, "patch": {"status": "x"}, "body": "New\n"}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let written = std::fs::read_to_string(vault.path().join("note.md"))?;
    assert!(
        written.starts_with("\u{feff}---\ntitle: Marked\ncreated: 2026-08-07T09:00+10:00\nstatus: x\n"),
        "{written:?}"
    );
    assert!(written.ends_with("---\nNew\n"), "{written:?}");
    Ok(())
}

#[tokio::test]
async fn a_stale_hash_is_reported_as_a_conflict_before_the_patch_is_judged() -> TestResult {
    let (vault, server) = vault_with("note.md", NOTE)?;
    let hash = hash_of(&server, "note.md").await?;
    let changed = NOTE.replace("status: adopted", "status: adopted\ndescription: Renamed");
    std::fs::write(vault.path().join("note.md"), &changed)?;

    let result = call_tool(
        server,
        "note_update",
        arguments(json!({"path": "note.md", "expected_hash": hash, "patch": {"title": "Renamed"}}))?,
    )
    .await?;

    assert_eq!(code(&result)?, "io/conflict");
    assert_eq!(std::fs::read_to_string(vault.path().join("note.md"))?, changed);
    Ok(())
}
