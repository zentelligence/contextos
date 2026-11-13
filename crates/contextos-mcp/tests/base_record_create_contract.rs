mod support;
use contextos_mcp::ContextOsServer;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, json};

async fn call_tool(
    server: ContextOsServer,
    name: &'static str,
    arguments: Map<String, serde_json::Value>,
) -> Result<CallToolResult, Box<dyn std::error::Error + Send + Sync>> {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
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

fn structured(
    result: &CallToolResult,
) -> Result<&serde_json::Map<String, serde_json::Value>, Box<dyn std::error::Error + Send + Sync>> {
    result
        .structured_content
        .as_ref()
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| std::io::Error::other("tool result omitted structured content").into())
}

fn minimal_definition_scoped_to(folder: &str) -> serde_json::Value {
    json!({
        "filters": format!("file.inFolder(\"{folder}\") && file.basename != \"index\""),
        "views": [{"type": "table", "name": "All"}]
    })
}

#[tokio::test]
async fn creates_a_keyed_record_within_the_bases_own_scope_and_advances_the_sidecar()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("requirements"))?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    call_tool(
        server.clone(),
        "base_create",
        serde_json::from_value(json!({
            "path": "requirements.base",
            "definition": minimal_definition_scoped_to("requirements"),
            "key_prefix": "REQ"
        }))?,
    )
    .await?;

    let result = call_tool(
        server.clone(),
        "base_record_create",
        serde_json::from_value(json!({
            "path": "requirements.base",
            "frontmatter": {"description": "A new requirement."},
            "content": "Body.\n"
        }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let body = structured(&result)?;
    assert_eq!(body.get("key"), Some(&json!("REQ-001")));
    assert_eq!(body.get("path"), Some(&json!("requirements/req-001.md")));
    assert_eq!(body.get("sidecar_path"), Some(&json!("requirements.md")));
    assert_eq!(body.get("warnings"), Some(&json!([])));

    let note = std::fs::read_to_string(vault.path().join("requirements/req-001.md"))?;
    assert!(note.contains("id: REQ-001"));
    assert!(note.contains("description: A new requirement."));
    assert!(note.contains("Body."));

    let sidecar = std::fs::read_to_string(vault.path().join("requirements.md"))?;
    assert!(sidecar.contains("REQ: 1"), "{sidecar}");
    assert!(!sidecar.contains("last_id"), "{sidecar}");

    let second = call_tool(
        server.clone(),
        "base_record_create",
        serde_json::from_value(json!({
            "path": "requirements.base",
            "frontmatter": {},
            "content": "Second body.\n"
        }))?,
    )
    .await?;
    assert_eq!(second.is_error, Some(false), "{second:?}");
    let second_body = structured(&second)?;
    assert_eq!(second_body.get("key"), Some(&json!("REQ-002")));
    Ok(())
}

#[tokio::test]
async fn creates_a_record_with_no_id_field_for_a_filepath_keyed_base()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("notes"))?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    call_tool(
        server.clone(),
        "base_create",
        serde_json::from_value(json!({
            "path": "notes.base",
            "definition": minimal_definition_scoped_to("notes")
        }))?,
    )
    .await?;

    let result = call_tool(
        server.clone(),
        "base_record_create",
        serde_json::from_value(json!({
            "path": "notes.base",
            "frontmatter": {"title": "My Note"},
            "content": "Body.\n",
            "filename": "my-note.md"
        }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let body = structured(&result)?;
    assert_eq!(
        body.get("key"),
        None,
        "a FILEPATH-keyed base assigns no key, so `key` is left out"
    );
    assert_eq!(body.get("path"), Some(&json!("notes/my-note.md")));

    let note = std::fs::read_to_string(vault.path().join("notes/my-note.md"))?;
    assert!(!note.contains("id:"));
    Ok(())
}

#[tokio::test]
async fn a_filepath_keyed_base_without_an_explicit_filename_is_rejected()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("notes"))?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    call_tool(
        server.clone(),
        "base_create",
        serde_json::from_value(json!({
            "path": "notes.base",
            "definition": minimal_definition_scoped_to("notes")
        }))?,
    )
    .await?;

    let result = call_tool(
        server.clone(),
        "base_record_create",
        serde_json::from_value(json!({
            "path": "notes.base",
            "frontmatter": {"title": "My Note"},
            "content": "Body.\n"
        }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(true));
    Ok(())
}

#[tokio::test]
async fn a_relative_folder_is_resolved_against_the_bases_own_in_folder_scope()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("requirements/functional"))?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    call_tool(
        server.clone(),
        "base_create",
        serde_json::from_value(json!({
            "path": "requirements.base",
            "definition": minimal_definition_scoped_to("requirements"),
            "key_prefix": "REQ"
        }))?,
    )
    .await?;

    let result = call_tool(
        server.clone(),
        "base_record_create",
        serde_json::from_value(json!({
            "path": "requirements.base",
            "frontmatter": {},
            "content": "Body.\n",
            "folder": "functional"
        }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let body = structured(&result)?;
    assert_eq!(body.get("path"), Some(&json!("requirements/functional/req-001.md")));
    Ok(())
}

#[tokio::test]
async fn a_leading_slash_folder_is_vault_root_relative_and_ignores_in_folder()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("requirements"))?;
    std::fs::create_dir_all(vault.path().join("elsewhere"))?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    call_tool(
        server.clone(),
        "base_create",
        serde_json::from_value(json!({
            "path": "requirements.base",
            "definition": minimal_definition_scoped_to("requirements"),
            "key_prefix": "REQ"
        }))?,
    )
    .await?;

    let result = call_tool(
        server.clone(),
        "base_record_create",
        serde_json::from_value(json!({
            "path": "requirements.base",
            "frontmatter": {},
            "content": "Body.\n",
            "folder": "/elsewhere"
        }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let body = structured(&result)?;
    assert_eq!(body.get("path"), Some(&json!("elsewhere/req-001.md")));
    Ok(())
}

#[tokio::test]
async fn a_base_with_no_in_folder_scope_and_no_folder_argument_is_rejected()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    call_tool(
        server.clone(),
        "base_create",
        serde_json::from_value(json!({
            "path": "unscoped.base",
            "definition": {"views": [{"type": "table", "name": "All"}]},
            "key_prefix": "UN"
        }))?,
    )
    .await?;

    let result = call_tool(
        server.clone(),
        "base_record_create",
        serde_json::from_value(json!({
            "path": "unscoped.base",
            "frontmatter": {},
            "content": "Body.\n"
        }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(true));
    Ok(())
}

#[tokio::test]
async fn caller_supplied_id_in_frontmatter_is_rejected() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("requirements"))?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    call_tool(
        server.clone(),
        "base_create",
        serde_json::from_value(json!({
            "path": "requirements.base",
            "definition": minimal_definition_scoped_to("requirements"),
            "key_prefix": "REQ"
        }))?,
    )
    .await?;

    let result = call_tool(
        server.clone(),
        "base_record_create",
        serde_json::from_value(json!({
            "path": "requirements.base",
            "frontmatter": {"id": "REQ-999"},
            "content": "Body.\n"
        }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(true));
    Ok(())
}

/// A `FILEPATH`-keyed base (`key_prefix` omitted, `next_key` returns `None`,
/// identity set by hand) has no id of its own to assign, so a
/// caller-supplied one is accepted rather than unconditionally rejected: the
/// same "no id to name one from" gap `resolve_filename`'s own `filename`
/// requirement already covers, just for `id` instead.
#[tokio::test]
async fn a_filepath_keyed_base_accepts_a_caller_supplied_id() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("notes"))?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    call_tool(
        server.clone(),
        "base_create",
        serde_json::from_value(json!({
            "path": "notes.base",
            "definition": minimal_definition_scoped_to("notes")
        }))?,
    )
    .await?;

    let result = call_tool(
        server.clone(),
        "base_record_create",
        serde_json::from_value(json!({
            "path": "notes.base",
            "frontmatter": {"title": "My Note", "id": "CUSTOM-1"},
            "content": "Body.\n",
            "filename": "my-note.md"
        }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let body = structured(&result)?;
    // `key` still reflects the sidecar's own scheme (none, for `FILEPATH`),
    // distinct from whatever id the caller chose to put in frontmatter.
    assert_eq!(
        body.get("key"),
        None,
        "a FILEPATH-keyed base assigns no key, so `key` is left out"
    );

    let note = std::fs::read_to_string(vault.path().join("notes/my-note.md"))?;
    assert!(note.contains("id: CUSTOM-1"));
    Ok(())
}

/// `base_record_create` refuses to write a note whose id already exists
/// elsewhere within the owning base's own scope, whether the id came from
/// the caller (`FILEPATH` scheme) or was about to be server-assigned
/// (`Sequential` scheme, protecting against a hand-authored note that
/// already claims the sidecar's own next id).
#[tokio::test]
async fn a_duplicate_id_within_the_bases_scope_is_rejected() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("notes"))?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    call_tool(
        server.clone(),
        "base_create",
        serde_json::from_value(json!({
            "path": "notes.base",
            "definition": minimal_definition_scoped_to("notes")
        }))?,
    )
    .await?;
    let first = call_tool(
        server.clone(),
        "base_record_create",
        serde_json::from_value(json!({
            "path": "notes.base",
            "frontmatter": {"title": "First", "id": "CUSTOM-1"},
            "content": "Body.\n",
            "filename": "first.md"
        }))?,
    )
    .await?;
    assert_eq!(first.is_error, Some(false), "{first:?}");

    let second = call_tool(
        server.clone(),
        "base_record_create",
        serde_json::from_value(json!({
            "path": "notes.base",
            "frontmatter": {"title": "Second", "id": "CUSTOM-1"},
            "content": "Body.\n",
            "filename": "second.md"
        }))?,
    )
    .await?;
    assert_eq!(second.is_error, Some(true), "{second:?}");
    assert!(!vault.path().join("notes/second.md").exists());
    Ok(())
}

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// A vault with a `requirements` folder and a `REQ`-keyed base scoped to it, plus a second folder holding a base
/// of its own, so a test can place out-of-band notes inside and outside the scope.
async fn requirements_vault() -> Result<(tempfile::TempDir, ContextOsServer), Box<dyn std::error::Error + Send + Sync>>
{
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("requirements"))?;
    std::fs::create_dir_all(vault.path().join("elsewhere"))?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    call_tool(
        server.clone(),
        "base_create",
        serde_json::from_value(json!({
            "path": "requirements.base",
            "definition": minimal_definition_scoped_to("requirements"),
            "key_prefix": "REQ"
        }))?,
    )
    .await?;
    Ok((vault, server))
}

fn write_out_of_band(vault: &std::path::Path, relative: &str, id: &str) -> std::io::Result<()> {
    std::fs::write(
        vault.join(relative),
        format!("---\nid: {id}\ntitle: Written outside the tool\n---\nBody.\n"),
    )
}

async fn create_record(server: &ContextOsServer) -> Result<CallToolResult, Box<dyn std::error::Error + Send + Sync>> {
    call_tool(
        server.clone(),
        "base_record_create",
        serde_json::from_value(json!({
            "path": "requirements.base",
            "frontmatter": {},
            "content": "Body.\n"
        }))?,
    )
    .await
}

/// A note written outside the tool that already holds the sidecar's next key no longer blocks creation: the
/// counter is caught up to the highest key the series has in scope and the next free key is assigned.
#[tokio::test]
async fn a_hand_authored_note_holding_the_next_sequential_key_is_stepped_past() -> TestResult {
    let (vault, server) = requirements_vault().await?;
    write_out_of_band(vault.path(), "requirements/hand-authored.md", "REQ-001")?;

    let result = create_record(&server).await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let body = structured(&result)?;
    assert_eq!(body.get("key"), Some(&json!("REQ-002")));
    assert_eq!(body.get("path"), Some(&json!("requirements/req-002.md")));
    let note = std::fs::read_to_string(vault.path().join("requirements/req-002.md"))?;
    assert!(note.contains("id: REQ-002"), "{note}");
    let sidecar = std::fs::read_to_string(vault.path().join("requirements.md"))?;
    assert!(sidecar.contains("REQ: 2"), "{sidecar}");
    Ok(())
}

/// The caller is told the counter was behind, through the sidecar warnings the result already carries, and the
/// warning names the key that was taken and the number the counter was caught up to.
#[tokio::test]
async fn stepping_past_out_of_band_keys_is_reported_as_a_warning() -> TestResult {
    let (vault, server) = requirements_vault().await?;
    write_out_of_band(vault.path(), "requirements/a.md", "REQ-001")?;
    write_out_of_band(vault.path(), "requirements/b.md", "REQ-004")?;

    let result = create_record(&server).await?;

    let warnings = structured(&result)?
        .get("warnings")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let text = warnings[0].as_str().unwrap_or_default();
    assert!(text.contains("REQ-001 already existed"), "{text}");
    assert!(text.contains("caught up to 4"), "{text}");
    Ok(())
}

/// The next key follows the highest key in scope, not the first free one: a gap below it is never back-filled.
#[tokio::test]
async fn the_next_key_follows_the_highest_key_in_scope_not_the_first_gap() -> TestResult {
    let (vault, server) = requirements_vault().await?;
    write_out_of_band(vault.path(), "requirements/a.md", "REQ-001")?;
    write_out_of_band(vault.path(), "requirements/b.md", "REQ-007")?;

    let result = create_record(&server).await?;

    assert_eq!(structured(&result)?.get("key"), Some(&json!("REQ-008")));
    Ok(())
}

/// A series' counter is trusted when it is ahead of the notes in scope: keys of records that were deleted are never
/// reused just because a lower key is all that remains.
#[tokio::test]
async fn a_counter_ahead_of_every_key_in_scope_decides_the_key_so_deleted_keys_are_not_reused() -> TestResult {
    let (vault, server) = requirements_vault().await?;
    let sidecar_path = vault.path().join("requirements.md");
    let sidecar = std::fs::read_to_string(&sidecar_path)?;
    assert!(sidecar.contains("REQ: 0"), "{sidecar}");
    std::fs::write(&sidecar_path, sidecar.replace("REQ: 0", "REQ: 10"))?;
    write_out_of_band(vault.path(), "requirements/old.md", "REQ-003")?;

    let result = create_record(&server).await?;

    // The counter says 10, so the next key is REQ-011 and nothing collides.
    assert_eq!(structured(&result)?.get("key"), Some(&json!("REQ-011")));
    assert_eq!(structured(&result)?.get("warnings"), Some(&json!([])));
    Ok(())
}

/// Only the requested series, in this base's scope, counts: another prefix, a key that merely resembles the
/// series, and a note in another folder are all ignored when catching the counter up.
#[tokio::test]
async fn only_this_series_in_this_bases_scope_moves_the_counter() -> TestResult {
    let (vault, server) = requirements_vault().await?;
    write_out_of_band(vault.path(), "requirements/held.md", "REQ-001")?;
    write_out_of_band(vault.path(), "requirements/other-series.md", "DEC-050")?;
    write_out_of_band(vault.path(), "requirements/lookalike.md", "XREQ-060")?;
    write_out_of_band(vault.path(), "requirements/not-a-number.md", "REQ-abc")?;
    write_out_of_band(vault.path(), "elsewhere/outside-scope.md", "REQ-070")?;

    let result = create_record(&server).await?;

    assert_eq!(structured(&result)?.get("key"), Some(&json!("REQ-002")));
    Ok(())
}

/// Keys wider than the zero-padded width are read as numbers, so REQ-1000 outranks REQ-999.
#[tokio::test]
async fn keys_wider_than_the_padded_width_are_compared_as_numbers() -> TestResult {
    let (vault, server) = requirements_vault().await?;
    write_out_of_band(vault.path(), "requirements/a.md", "REQ-001")?;
    write_out_of_band(vault.path(), "requirements/b.md", "REQ-999")?;
    write_out_of_band(vault.path(), "requirements/c.md", "REQ-1000")?;

    let result = create_record(&server).await?;

    assert_eq!(structured(&result)?.get("key"), Some(&json!("REQ-1001")));
    Ok(())
}

/// When nothing collides, the sidecar alone decides: no catch-up, no warning, and the counter moves by one.
#[tokio::test]
async fn when_the_next_key_is_free_the_counter_alone_decides_and_nothing_is_reported() -> TestResult {
    let (vault, server) = requirements_vault().await?;
    write_out_of_band(vault.path(), "requirements/ahead.md", "REQ-005")?;
    let sidecar_path = vault.path().join("requirements.md");
    let sidecar = std::fs::read_to_string(&sidecar_path)?;
    std::fs::write(&sidecar_path, sidecar.replace("REQ: 0", "REQ: 5"))?;

    let result = create_record(&server).await?;

    assert_eq!(structured(&result)?.get("key"), Some(&json!("REQ-006")));
    assert_eq!(structured(&result)?.get("warnings"), Some(&json!([])));
    let sidecar = std::fs::read_to_string(&sidecar_path)?;
    assert!(sidecar.contains("REQ: 6"), "{sidecar}");
    Ok(())
}

/// If the scope holds more notes than one query returns, the highest key could be among those left out, so the
/// call refuses instead of guessing, and writes neither a note nor the sidecar.
#[tokio::test]
async fn a_scope_too_large_to_read_back_refuses_to_guess_and_writes_nothing() -> TestResult {
    let (vault, server) = requirements_vault().await?;
    write_out_of_band(vault.path(), "requirements/held.md", "REQ-001")?;
    for number in 0..5_001 {
        write_out_of_band(
            vault.path(),
            &format!("requirements/filler-{number}.md"),
            &format!("OTHER-{number}"),
        )?;
    }
    let sidecar_before = std::fs::read_to_string(vault.path().join("requirements.md"))?;

    let result = create_record(&server).await?;

    assert_eq!(result.is_error, Some(true), "{result:?}");
    let body = structured(&result)?;
    assert_eq!(body.get("code"), Some(&json!("io/invalid-argument")));
    let message = body
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert!(message.contains("read back every id"), "{message}");
    assert!(!vault.path().join("requirements/req-001.md").exists());
    assert!(!vault.path().join("requirements/req-002.md").exists());
    assert_eq!(
        std::fs::read_to_string(vault.path().join("requirements.md"))?,
        sidecar_before
    );
    Ok(())
}

/// A key whose number is the largest a counter can hold leaves no free key above it, so the call refuses instead of
/// wrapping onto an id that is already taken.
#[tokio::test]
async fn a_series_whose_highest_key_is_the_largest_number_refuses_instead_of_reusing_it() -> TestResult {
    let (vault, server) = requirements_vault().await?;
    write_out_of_band(vault.path(), "requirements/a.md", "REQ-001")?;
    write_out_of_band(vault.path(), "requirements/max.md", &format!("REQ-{}", u64::MAX))?;
    let sidecar_before = std::fs::read_to_string(vault.path().join("requirements.md"))?;

    let result = create_record(&server).await?;

    assert_eq!(result.is_error, Some(true), "{result:?}");
    assert_eq!(structured(&result)?.get("code"), Some(&json!("io/invalid-argument")));
    assert_eq!(
        std::fs::read_to_string(vault.path().join("requirements.md"))?,
        sidecar_before
    );
    let notes = std::fs::read_dir(vault.path().join("requirements"))?.count();
    assert_eq!(notes, 2, "no record may be written");
    Ok(())
}

/// A base that declares no `file.inFolder` scope covers the whole vault, so a record created with an absolute
/// folder still catches the counter up against every note in the vault.
#[tokio::test]
async fn a_base_with_no_folder_scope_catches_up_against_the_whole_vault() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("anywhere"))?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    call_tool(
        server.clone(),
        "base_create",
        serde_json::from_value(json!({
            "path": "loose.base",
            "definition": {"views": [{"type": "table", "name": "All"}]},
            "key_prefix": "REQ"
        }))?,
    )
    .await?;
    write_out_of_band(vault.path(), "anywhere/held.md", "REQ-001")?;

    let result = call_tool(
        server.clone(),
        "base_record_create",
        serde_json::from_value(json!({
            "path": "loose.base",
            "folder": "/anywhere",
            "frontmatter": {},
            "content": "Body.\n"
        }))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(structured(&result)?.get("key"), Some(&json!("REQ-002")));
    Ok(())
}
