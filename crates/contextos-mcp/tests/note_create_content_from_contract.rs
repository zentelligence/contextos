//! Contract tests for `note_create` with `content_from`: the server copies a vault file's body into the new note
//! verbatim, so content that only needs to be copied never passes through the caller. The new note's frontmatter
//! comes from the call's own `frontmatter` and defaults, never from the source.

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

fn structured(result: &CallToolResult) -> Result<&Map<String, Value>, Box<dyn std::error::Error + Send + Sync>> {
    result
        .structured_content
        .as_ref()
        .and_then(Value::as_object)
        .ok_or_else(|| std::io::Error::other("tool result omitted structured content").into())
}

fn code(result: &CallToolResult) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    assert_eq!(result.is_error, Some(true), "{result:?}");
    structured(result)?
        .get("code")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| std::io::Error::other("an error result omitted its code").into())
}

/// A vault holding each `(relative path, bytes)` file.
fn vault_with(
    files: &[(&str, &[u8])],
) -> Result<(tempfile::TempDir, ContextOsServer), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    for (relative, content) in files {
        let path = vault.path().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)?;
    }
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    Ok((vault, server))
}

async fn hash_of(server: &ContextOsServer, path: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let read = call_tool(server.clone(), "fs_read_text_file", json!({"path": path, "head": 1})).await?;
    structured(&read)?
        .get("content_hash")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| std::io::Error::other("fs_read_text_file omitted content_hash").into())
}

const CLIPPED: &str = "---\nsource: https://example.com/article\nauthor: Someone\n---\n# Café notes\n\nNaïve résumé, 日本語, and 🦀 [[a link]].\n";
const CLIPPED_BODY: &str = "# Café notes\n\nNaïve résumé, 日本語, and 🦀 [[a link]].\n";

#[tokio::test]
async fn the_source_body_is_copied_verbatim_under_the_calls_own_frontmatter() -> TestResult {
    let (vault, server) = vault_with(&[("inbox/clipped.md", CLIPPED.as_bytes())])?;

    let result = call_tool(
        server.clone(),
        "note_create",
        json!({
            "path": "memory/raw/2026/10/2026-10-19-cafe-notes.md",
            "frontmatter": {"title": "Café notes", "origin": "inbox/clipped.md"},
            "content_from": {"path": "inbox/clipped.md"}
        }),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let written = std::fs::read_to_string(vault.path().join("memory/raw/2026/10/2026-10-19-cafe-notes.md"))?;
    let document = FrontmatterDocument::try_from(written.as_str())?;
    assert_eq!(document.body(), CLIPPED_BODY, "the body is the source's, byte for byte");
    let frontmatter = document.frontmatter();
    assert_eq!(frontmatter.get("title"), Some(&json!("Café notes")));
    assert_eq!(frontmatter.get("origin"), Some(&json!("inbox/clipped.md")));
    assert_eq!(frontmatter.get("type"), Some(&json!("note")), "defaults still apply");
    assert_eq!(
        frontmatter.get("source"),
        None,
        "the source's frontmatter is not carried over"
    );
    assert_eq!(
        frontmatter.get("author"),
        None,
        "the source's frontmatter is not carried over"
    );
    assert_eq!(
        structured(&result)?.get("content_hash").and_then(Value::as_str),
        Some(
            hash_of(&server, "memory/raw/2026/10/2026-10-19-cafe-notes.md")
                .await?
                .as_str()
        ),
        "the returned hash is the written note's"
    );
    assert_eq!(
        std::fs::read_to_string(vault.path().join("inbox/clipped.md"))?,
        CLIPPED,
        "the source is left untouched"
    );
    Ok(())
}

#[tokio::test]
async fn a_multi_megabyte_single_line_source_is_copied_whole() -> TestResult {
    let line = "é🦀 word ".repeat(300_000);
    let source = format!("{line}\n");
    let (vault, server) = vault_with(&[("inbox/transcript.md", source.as_bytes())])?;

    let result = call_tool(
        server,
        "note_create",
        json!({"path": "raw/transcript.md", "content_from": {"path": "inbox/transcript.md"}}),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let written = std::fs::read_to_string(vault.path().join("raw/transcript.md"))?;
    assert_eq!(FrontmatterDocument::try_from(written.as_str())?.body(), source);
    Ok(())
}

#[tokio::test]
async fn a_source_without_frontmatter_is_copied_whole() -> TestResult {
    let (vault, server) = vault_with(&[("inbox/plain.md", b"# Plain\n\nNo properties here.\n")])?;

    let result = call_tool(
        server,
        "note_create",
        json!({"path": "raw/plain.md", "content_from": {"path": "inbox/plain.md"}}),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let written = std::fs::read_to_string(vault.path().join("raw/plain.md"))?;
    assert_eq!(
        FrontmatterDocument::try_from(written.as_str())?.body(),
        "# Plain\n\nNo properties here.\n"
    );
    Ok(())
}

#[tokio::test]
async fn a_text_source_that_is_not_markdown_is_copied_whole_even_when_it_opens_with_a_rule() -> TestResult {
    let transcript = "---\nSpeaker 1: hello\n---\nSpeaker 2: goodbye\n";
    let (vault, server) = vault_with(&[("inbox/call.txt", transcript.as_bytes())])?;

    let result = call_tool(
        server,
        "note_create",
        json!({"path": "raw/call.md", "frontmatter": {"title": "Call"}, "content_from": {"path": "inbox/call.txt"}}),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let written = std::fs::read_to_string(vault.path().join("raw/call.md"))?;
    let document = FrontmatterDocument::try_from(written.as_str())?;
    assert_eq!(document.body(), transcript, "frontmatter is a Markdown notion");
    assert_eq!(document.frontmatter().get("Speaker 1"), None);
    Ok(())
}

#[tokio::test]
async fn an_empty_source_makes_a_note_with_an_empty_body() -> TestResult {
    let (vault, server) = vault_with(&[("inbox/empty.md", b"")])?;

    let result = call_tool(
        server,
        "note_create",
        json!({"path": "raw/empty.md", "content_from": {"path": "inbox/empty.md"}}),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let written = std::fs::read_to_string(vault.path().join("raw/empty.md"))?;
    assert_eq!(FrontmatterDocument::try_from(written.as_str())?.body(), "");
    Ok(())
}

#[tokio::test]
async fn references_follow_the_copied_body() -> TestResult {
    let (vault, server) = vault_with(&[("inbox/clipped.md", CLIPPED.as_bytes())])?;

    let result = call_tool(
        server,
        "note_create",
        json!({
            "path": "raw/clipped.md",
            "content_from": {"path": "inbox/clipped.md"},
            "references": [{"target": "inbox/clipped", "summary": "The clipped original."}]
        }),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let written = std::fs::read_to_string(vault.path().join("raw/clipped.md"))?;
    assert_eq!(
        FrontmatterDocument::try_from(written.as_str())?.body(),
        format!("{CLIPPED_BODY}\n# References\n\n- [[inbox/clipped]]: The clipped original.\n")
    );
    Ok(())
}

#[tokio::test]
async fn a_matching_expected_hash_copies_the_source() -> TestResult {
    let (vault, server) = vault_with(&[("inbox/clipped.md", CLIPPED.as_bytes())])?;
    let hash = hash_of(&server, "inbox/clipped.md").await?;

    let result = call_tool(
        server,
        "note_create",
        json!({"path": "raw/clipped.md", "content_from": {"path": "inbox/clipped.md", "expected_hash": hash}}),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert!(vault.path().join("raw/clipped.md").is_file());
    Ok(())
}

#[tokio::test]
async fn a_source_changed_since_it_was_read_is_refused_and_nothing_is_written() -> TestResult {
    let (vault, server) = vault_with(&[("inbox/clipped.md", CLIPPED.as_bytes())])?;
    let stale = hash_of(&server, "inbox/clipped.md").await?;
    std::fs::write(vault.path().join("inbox/clipped.md"), "# Changed\n")?;

    let result = call_tool(
        server,
        "note_create",
        json!({"path": "raw/clipped.md", "content_from": {"path": "inbox/clipped.md", "expected_hash": stale}}),
    )
    .await?;

    assert_eq!(code(&result)?, "io/conflict");
    let path = structured(&result)?
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        std::path::Path::new(path).ends_with("inbox/clipped.md"),
        "the conflict names the source, not the note being created: {path}"
    );
    assert!(!vault.path().join("raw/clipped.md").exists());
    Ok(())
}

#[tokio::test]
async fn a_malformed_expected_hash_is_refused() -> TestResult {
    let (vault, server) = vault_with(&[("inbox/clipped.md", CLIPPED.as_bytes())])?;

    let result = call_tool(
        server,
        "note_create",
        json!({"path": "raw/clipped.md", "content_from": {"path": "inbox/clipped.md", "expected_hash": "abc"}}),
    )
    .await?;

    assert_eq!(code(&result)?, "io/invalid-hash");
    assert!(!vault.path().join("raw/clipped.md").exists());
    Ok(())
}

#[tokio::test]
async fn content_and_content_from_together_are_refused() -> TestResult {
    let (vault, server) = vault_with(&[("inbox/clipped.md", CLIPPED.as_bytes())])?;

    let result = call_tool(
        server,
        "note_create",
        json!({"path": "raw/clipped.md", "content": "Body.\n", "content_from": {"path": "inbox/clipped.md"}}),
    )
    .await?;

    assert_eq!(code(&result)?, "io/invalid-argument");
    assert!(!vault.path().join("raw/clipped.md").exists());
    Ok(())
}

#[tokio::test]
async fn neither_content_nor_content_from_is_refused() -> TestResult {
    let (vault, server) = vault_with(&[])?;

    let result = call_tool(server, "note_create", json!({"path": "raw/nothing.md"})).await?;

    assert_eq!(code(&result)?, "io/invalid-argument");
    assert!(!vault.path().join("raw/nothing.md").exists());
    Ok(())
}

#[tokio::test]
async fn an_unknown_field_in_content_from_is_refused() -> TestResult {
    let (vault, server) = vault_with(&[("inbox/clipped.md", CLIPPED.as_bytes())])?;

    let result = call_tool(
        server,
        "note_create",
        json!({"path": "raw/clipped.md", "content_from": {"path": "inbox/clipped.md", "keep_frontmatter": true}}),
    )
    .await;

    // Unknown fields fail deserialisation, reported either as a protocol error or as an error result.
    if let Ok(result) = result {
        assert_eq!(result.is_error, Some(true), "{result:?}");
    }
    assert!(!vault.path().join("raw/clipped.md").exists());
    Ok(())
}

#[tokio::test]
async fn a_missing_source_is_refused_as_not_found() -> TestResult {
    let (vault, server) = vault_with(&[])?;

    let result = call_tool(
        server,
        "note_create",
        json!({"path": "raw/missing.md", "content_from": {"path": "inbox/missing.md"}}),
    )
    .await?;

    assert_eq!(code(&result)?, "path/not-found");
    assert!(!vault.path().join("raw/missing.md").exists());
    Ok(())
}

#[tokio::test]
async fn a_source_outside_the_vault_is_refused() -> TestResult {
    let outside = tempfile::Builder::new().prefix("outside").tempdir()?;
    std::fs::write(outside.path().join("secret.md"), "secret\n")?;
    let (vault, server) = vault_with(&[])?;

    let traversal = call_tool(
        server.clone(),
        "note_create",
        json!({"path": "raw/traversal.md", "content_from": {"path": "../secret.md"}}),
    )
    .await?;
    let absolute = call_tool(
        server,
        "note_create",
        json!({
            "path": "raw/absolute.md",
            "content_from": {"path": outside.path().join("secret.md").to_string_lossy()}
        }),
    )
    .await?;

    assert_eq!(traversal.is_error, Some(true), "{traversal:?}");
    assert_eq!(absolute.is_error, Some(true), "{absolute:?}");
    assert!(code(&traversal)?.starts_with("path/"), "{traversal:?}");
    assert!(code(&absolute)?.starts_with("path/"), "{absolute:?}");
    assert!(!vault.path().join("raw/traversal.md").exists());
    assert!(!vault.path().join("raw/absolute.md").exists());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_source_reached_through_a_symlink_out_of_the_vault_is_refused() -> TestResult {
    let outside = tempfile::Builder::new().prefix("outside").tempdir()?;
    std::fs::write(outside.path().join("secret.md"), "secret\n")?;
    let (vault, server) = vault_with(&[])?;
    std::os::unix::fs::symlink(outside.path(), vault.path().join("escape"))?;

    let result = call_tool(
        server,
        "note_create",
        json!({"path": "raw/escape.md", "content_from": {"path": "escape/secret.md"}}),
    )
    .await?;

    assert!(code(&result)?.starts_with("path/"), "{result:?}");
    assert!(!vault.path().join("raw/escape.md").exists());
    Ok(())
}

#[tokio::test]
async fn a_binary_source_is_refused() -> TestResult {
    let (vault, server) = vault_with(&[("inbox/image.md", &[0x89, b'P', b'N', b'G', 0x00, 0xff, 0xfe])])?;

    let result = call_tool(
        server,
        "note_create",
        json!({"path": "raw/image.md", "content_from": {"path": "inbox/image.md"}}),
    )
    .await?;

    assert_eq!(code(&result)?, "io/binary");
    assert!(!vault.path().join("raw/image.md").exists());
    Ok(())
}

#[tokio::test]
async fn a_source_with_malformed_frontmatter_is_refused_rather_than_repaired() -> TestResult {
    let (vault, server) = vault_with(&[("inbox/broken.md", b"---\ntitle: [unclosed\n---\nBody.\n")])?;

    let result = call_tool(
        server,
        "note_create",
        json!({"path": "raw/broken.md", "content_from": {"path": "inbox/broken.md"}}),
    )
    .await?;

    assert_eq!(code(&result)?, "format/frontmatter");
    assert!(!vault.path().join("raw/broken.md").exists());
    Ok(())
}

#[tokio::test]
async fn copying_onto_an_existing_note_is_refused_and_leaves_it_untouched() -> TestResult {
    let (vault, server) = vault_with(&[
        ("inbox/clipped.md", CLIPPED.as_bytes()),
        ("raw/clipped.md", b"Existing.\n"),
    ])?;

    let result = call_tool(
        server,
        "note_create",
        json!({"path": "raw/clipped.md", "content_from": {"path": "inbox/clipped.md"}}),
    )
    .await?;

    assert_eq!(code(&result)?, "io/expected-hash-required");
    assert_eq!(
        std::fs::read_to_string(vault.path().join("raw/clipped.md"))?,
        "Existing.\n"
    );
    Ok(())
}

/// A vault whose read limit is 1 MiB, holding one source of `size` bytes.
fn vault_with_one_mib_limit_and_source_of(
    size: usize,
) -> Result<(tempfile::TempDir, ContextOsServer), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("inbox"))?;
    std::fs::write(vault.path().join("inbox/big.md"), "a".repeat(size))?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    for configured in &mut config.vaults {
        configured.limits.max_read_mb = 1;
    }
    let server = ContextOsServer::try_from(config)?;
    Ok((vault, server))
}

#[tokio::test]
async fn a_source_exactly_at_the_read_limit_is_copied_whole() -> TestResult {
    let (vault, server) = vault_with_one_mib_limit_and_source_of(1024 * 1024)?;

    let result = call_tool(
        server,
        "note_create",
        json!({"path": "raw/big.md", "content_from": {"path": "inbox/big.md"}}),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let written = std::fs::read_to_string(vault.path().join("raw/big.md"))?;
    assert_eq!(
        FrontmatterDocument::try_from(written.as_str())?.body().len(),
        1024 * 1024
    );
    Ok(())
}

#[tokio::test]
async fn a_source_over_the_read_limit_is_refused_rather_than_cut_short() -> TestResult {
    let (vault, server) = vault_with_one_mib_limit_and_source_of(1024 * 1024 + 1)?;

    let result = call_tool(
        server,
        "note_create",
        json!({"path": "raw/big.md", "content_from": {"path": "inbox/big.md"}}),
    )
    .await?;

    assert_eq!(code(&result)?, "io/too-large");
    assert!(!vault.path().join("raw/big.md").exists());
    Ok(())
}

#[tokio::test]
async fn a_markdown_source_with_an_upper_case_extension_still_leaves_its_frontmatter_behind() -> TestResult {
    let (vault, server) = vault_with(&[("inbox/Clip.MD", CLIPPED.as_bytes())])?;

    let result = call_tool(
        server,
        "note_create",
        json!({"path": "raw/clip.md", "content_from": {"path": "inbox/Clip.MD"}}),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let written = std::fs::read_to_string(vault.path().join("raw/clip.md"))?;
    let document = FrontmatterDocument::try_from(written.as_str())?;
    assert_eq!(document.body(), CLIPPED_BODY);
    assert_eq!(document.frontmatter().get("source"), None);
    Ok(())
}

#[tokio::test]
async fn a_source_with_windows_line_endings_is_stored_with_unix_ones_and_says_so() -> TestResult {
    let source = "---\r\nsource: clipper\r\n---\r\n# Heading\r\n\r\nLine.\r\n";
    let (vault, server) = vault_with(&[("inbox/windows.md", source.as_bytes())])?;

    let result = call_tool(
        server,
        "note_create",
        json!({"path": "raw/windows.md", "content_from": {"path": "inbox/windows.md"}}),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let written = std::fs::read_to_string(vault.path().join("raw/windows.md"))?;
    let document = FrontmatterDocument::try_from(written.as_str())?;
    assert_eq!(document.body(), "# Heading\n\nLine.\n");
    assert_eq!(document.frontmatter().get("source"), None);
    assert!(
        result_text_mentions(&result, "write/line-endings-converted")?,
        "the conversion is reported: {result:?}"
    );
    Ok(())
}

fn result_text_mentions(result: &CallToolResult, needle: &str) -> Result<bool, serde_json::Error> {
    Ok(serde_json::to_string(&result.structured_content)?.contains(needle))
}

#[tokio::test]
async fn a_directory_source_is_refused() -> TestResult {
    let (vault, server) = vault_with(&[("inbox/folder/note.md", b"Body.\n")])?;

    let result = call_tool(
        server,
        "note_create",
        json!({"path": "raw/folder.md", "content_from": {"path": "inbox/folder"}}),
    )
    .await?;

    assert_eq!(code(&result)?, "io/filesystem");
    assert!(!vault.path().join("raw/folder.md").exists());
    Ok(())
}

#[tokio::test]
async fn a_malformed_reference_is_refused_before_the_source_is_read() -> TestResult {
    let (vault, server) = vault_with(&[])?;

    let result = call_tool(
        server,
        "note_create",
        json!({
            "path": "raw/missing.md",
            "content_from": {"path": "inbox/missing.md"},
            "references": [{"target": "[[broken]]", "summary": "Bad."}]
        }),
    )
    .await?;

    assert_eq!(
        code(&result)?,
        "io/invalid-argument",
        "the request is judged whole before the source is read, so the missing source is never reached"
    );
    assert!(!vault.path().join("raw/missing.md").exists());
    Ok(())
}

#[tokio::test]
async fn a_source_in_another_vault_is_copied_by_name() -> TestResult {
    let notes = tempfile::Builder::new().prefix("notes").tempdir()?;
    let archive = tempfile::Builder::new().prefix("archive").tempdir()?;
    std::fs::create_dir_all(archive.path().join("inbox"))?;
    std::fs::write(archive.path().join("inbox/clipped.md"), CLIPPED)?;
    let mut config = support::scoped_config(vec![notes.path().to_path_buf(), archive.path().to_path_buf()])?;
    for (vault, name) in config.vaults.iter_mut().zip(["notes", "archive"]) {
        vault.name = Some(name.to_owned());
    }
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(
        server,
        "note_create",
        json!({
            "path": "notes://raw/clipped.md",
            "content_from": {"path": "archive://inbox/clipped.md"}
        }),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let written = std::fs::read_to_string(notes.path().join("raw/clipped.md"))?;
    assert_eq!(FrontmatterDocument::try_from(written.as_str())?.body(), CLIPPED_BODY);
    Ok(())
}
