//! Contract tests for line endings: every text write stores `\n` line endings, converting any `\r\n`
//! or lone `\r` it is given or finds, and says so in a warning, the way paths are always given with `/`. Binary writes
//! are stored as given.

mod support;
use contextos_mcp::ContextOsServer;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, Value, json};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

const CONVERTED: &str = "write/line-endings-converted";

async fn call_tool(
    server: ContextOsServer,
    name: &'static str,
    arguments: Value,
) -> Result<CallToolResult, Box<dyn std::error::Error + Send + Sync>> {
    let arguments: Map<String, Value> = serde_json::from_value(arguments)?;
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

fn field<'a>(result: &'a CallToolResult, name: &str) -> Option<&'a Value> {
    result.structured_content.as_ref().and_then(|content| content.get(name))
}

fn warned(result: &CallToolResult) -> bool {
    field(result, "warnings")
        .and_then(Value::as_array)
        .is_some_and(|warnings| {
            warnings
                .iter()
                .any(|warning| warning.as_str().is_some_and(|text| text.contains(CONVERTED)))
        })
}

fn vault() -> Result<(tempfile::TempDir, ContextOsServer), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    Ok((vault, server))
}

async fn hash_of(server: &ContextOsServer, path: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let info = call_tool(server.clone(), "fs_get_file_info", json!({"path": path})).await?;
    field(&info, "content_hash")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| std::io::Error::other("fs_get_file_info omitted content_hash").into())
}

#[tokio::test]
async fn a_text_write_stores_unix_line_endings_and_returns_their_hash() -> TestResult {
    let (vault, server) = vault()?;

    let written = call_tool(
        server.clone(),
        "fs_write_file",
        json!({"path": "note.md", "content": "# Note\r\n\r\nBody.\r\n"}),
    )
    .await?;

    assert_eq!(written.is_error, Some(false), "{written:?}");
    assert_eq!(
        std::fs::read_to_string(vault.path().join("note.md"))?,
        "# Note\n\nBody.\n"
    );
    assert_eq!(
        field(&written, "content_hash").and_then(Value::as_str),
        Some(hash_of(&server, "note.md").await?.as_str())
    );
    assert!(warned(&written), "the conversion is reported: {written:?}");

    let classic = call_tool(
        server.clone(),
        "fs_write_file",
        json!({"path": "classic.md", "content": "# Classic\r\rBody.\r"}),
    )
    .await?;
    assert_eq!(
        std::fs::read_to_string(vault.path().join("classic.md"))?,
        "# Classic\n\nBody.\n"
    );
    assert!(
        warned(&classic),
        "a lone carriage return is a line ending too: {classic:?}"
    );

    let unix = call_tool(
        server,
        "fs_write_file",
        json!({"path": "plain.md", "content": "Plain.\n"}),
    )
    .await?;
    assert!(!warned(&unix), "nothing to convert, nothing reported: {unix:?}");
    Ok(())
}

#[tokio::test]
async fn a_binary_write_is_stored_exactly_as_given() -> TestResult {
    let (vault, server) = vault()?;

    let written = call_tool(
        server,
        "fs_write_file",
        json!({"path": "data.bin", "content_base64": "b25lDQp0d28NCg=="}),
    )
    .await?;

    assert_eq!(written.is_error, Some(false), "{written:?}");
    assert_eq!(std::fs::read(vault.path().join("data.bin"))?, b"one\r\ntwo\r\n");
    assert!(!warned(&written));
    Ok(())
}

#[tokio::test]
async fn an_edit_matches_unix_line_ending_text_in_a_windows_line_ending_file_and_converts_it() -> TestResult {
    let (vault, server) = vault()?;
    std::fs::write(vault.path().join("note.md"), "# Note\r\nfirst line\r\nsecond line\r\n")?;

    let edited = call_tool(
        server,
        "fs_edit_file",
        json!({"path": "note.md", "edits": [{"old_text": "first line\nsecond line", "new_text": "only line"}]}),
    )
    .await?;

    assert_eq!(edited.is_error, Some(false), "{edited:?}");
    assert_eq!(
        std::fs::read_to_string(vault.path().join("note.md"))?,
        "# Note\nonly line\n"
    );
    assert!(warned(&edited), "{edited:?}");
    Ok(())
}

#[tokio::test]
async fn note_tools_write_unix_line_endings_throughout() -> TestResult {
    let (vault, server) = vault()?;

    let created = call_tool(
        server.clone(),
        "note_create",
        json!({"path": "created.md", "content": "# Created\r\n\r\nBody.\r\n"}),
    )
    .await?;
    assert_eq!(created.is_error, Some(false), "{created:?}");
    let text = std::fs::read_to_string(vault.path().join("created.md"))?;
    assert!(
        !text.contains('\r') && text.ends_with("# Created\n\nBody.\n"),
        "{text:?}"
    );

    std::fs::write(
        vault.path().join("windows.md"),
        "---\r\ntitle: Windows\r\nstatus: open\r\n---\r\n\r\nOld.\r\n",
    )?;
    let patched = call_tool(
        server.clone(),
        "frontmatter_update",
        json!({"path": "windows.md", "patch": {"status": "done"}}),
    )
    .await?;
    assert_eq!(patched.is_error, Some(false), "{patched:?}");
    let text = std::fs::read_to_string(vault.path().join("windows.md"))?;
    assert!(!text.contains('\r') && text.ends_with("---\n\nOld.\n"), "{text:?}");

    std::fs::write(
        vault.path().join("update.md"),
        "---\r\ntitle: Update\r\n---\r\n\r\nOld.\r\n",
    )?;
    let hash = hash_of(&server, "update.md").await?;
    let updated = call_tool(
        server,
        "note_update",
        json!({"path": "update.md", "expected_hash": hash, "patch": {"status": "done"}, "body": "\r\nNew.\r\n"}),
    )
    .await?;
    assert_eq!(
        updated.is_error,
        Some(false),
        "a Windows line ending body is converted, not refused: {updated:?}"
    );
    let text = std::fs::read_to_string(vault.path().join("update.md"))?;
    assert!(!text.contains('\r') && text.ends_with("---\n\nNew.\n"), "{text:?}");
    assert!(warned(&updated), "{updated:?}");
    Ok(())
}

#[tokio::test]
async fn a_windows_line_ending_body_for_a_unix_line_ending_note_is_converted_not_refused() -> TestResult {
    let (vault, server) = vault()?;
    std::fs::write(vault.path().join("note.md"), "---\ntitle: Unix\n---\n\nOld.\n")?;
    let hash = hash_of(&server, "note.md").await?;

    let updated = call_tool(
        server,
        "note_update",
        json!({"path": "note.md", "expected_hash": hash, "body": "\r\nNew.\r\n"}),
    )
    .await?;

    assert_eq!(updated.is_error, Some(false), "{updated:?}");
    let text = std::fs::read_to_string(vault.path().join("note.md"))?;
    assert!(!text.contains('\r') && text.ends_with("---\n\nNew.\n"), "{text:?}");
    assert!(warned(&updated), "{updated:?}");
    Ok(())
}
