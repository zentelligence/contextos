//! Contract tests for the `base` property `base_record_create` stamps on a new record note: a wikilink to the base
//! file the record was created through, so a note names its governing base.

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

fn definition_scoped_to(folder: &str) -> Value {
    json!({
        "filters": format!("file.inFolder(\"{folder}\") && file.basename != \"index\""),
        "views": [{"type": "table", "name": "All"}]
    })
}

/// A vault with a `tasks/` folder and a server over it.
fn vault_with_server() -> Result<(tempfile::TempDir, ContextOsServer), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("tasks"))?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    Ok((vault, server))
}

async fn create_base(server: &ContextOsServer, extra: Value) -> TestResult {
    let mut request = json!({"path": "tasks.base", "definition": definition_scoped_to("tasks")});
    if let (Some(request), Some(extra)) = (request.as_object_mut(), extra.as_object()) {
        request.extend(extra.clone());
    }
    let result = call_tool(server.clone(), "base_create", arguments(request)?).await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    Ok(())
}

async fn create_record(
    server: &ContextOsServer,
    frontmatter: Value,
) -> Result<CallToolResult, Box<dyn std::error::Error + Send + Sync>> {
    let request = json!({"path": "tasks.base", "frontmatter": frontmatter, "content": "Body.\n"});
    call_tool(server.clone(), "base_record_create", arguments(request)?).await
}

/// The record notes in the `tasks/` folder, leaving out the folder's managed `index.md`.
fn record_notes(
    vault: &tempfile::TempDir,
) -> Result<Vec<std::path::PathBuf>, Box<dyn std::error::Error + Send + Sync>> {
    let mut notes = Vec::new();
    for entry in std::fs::read_dir(vault.path().join("tasks"))? {
        let path = entry?.path();
        if path.file_name().is_some_and(|name| name != "index.md") {
            notes.push(path);
        }
    }
    Ok(notes)
}

/// The frontmatter of the only record note in the `tasks/` folder.
fn only_record_frontmatter(
    vault: &tempfile::TempDir,
) -> Result<Map<String, Value>, Box<dyn std::error::Error + Send + Sync>> {
    let notes = record_notes(vault)?;
    let [note] = notes.as_slice() else {
        return Err(format!("expected one record note, found {notes:?}").into());
    };
    let content = std::fs::read_to_string(note)?;
    Ok(FrontmatterDocument::try_from(content.as_str())?.frontmatter().clone())
}

fn assert_rejected_with_code(result: &CallToolResult, code: &str) -> TestResult {
    assert_eq!(result.is_error, Some(true), "{result:?}");
    assert_eq!(structured(result)?.get("code"), Some(&json!(code)), "{result:?}");
    Ok(())
}

#[tokio::test]
async fn a_new_record_is_stamped_with_a_link_to_its_base() -> TestResult {
    let (vault, server) = vault_with_server()?;
    create_base(&server, json!({"key_prefix": "T"})).await?;

    let result = create_record(&server, json!({})).await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(
        only_record_frontmatter(&vault)?.get("base"),
        Some(&json!("[[tasks.base]]"))
    );
    Ok(())
}

#[tokio::test]
async fn a_supplied_link_to_the_same_base_is_kept_as_written() -> TestResult {
    let (vault, server) = vault_with_server()?;
    create_base(&server, json!({"key_prefix": "T"})).await?;

    let result = create_record(&server, json!({"base": "[[tasks.base|Tasks]]"})).await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(
        only_record_frontmatter(&vault)?.get("base"),
        Some(&json!("[[tasks.base|Tasks]]"))
    );
    Ok(())
}

#[tokio::test]
async fn a_supplied_link_to_another_base_is_refused_and_nothing_is_written() -> TestResult {
    let (vault, server) = vault_with_server()?;
    create_base(&server, json!({"key_prefix": "T"})).await?;
    let sidecar_before = std::fs::read_to_string(vault.path().join("tasks.md"))?;

    for supplied in [
        json!("[[other.base]]"),
        json!("tasks.base"),
        json!("[[tasks]]"),
        json!(3),
        json!(["[[tasks.base]]"]),
    ] {
        let result = create_record(&server, json!({"base": supplied})).await?;

        assert_rejected_with_code(&result, "io/invalid-argument")?;
    }
    assert!(
        record_notes(&vault)?.is_empty(),
        "`record_notes(&vault)?` should be empty, got {:?}",
        record_notes(&vault)?
    );
    assert_eq!(std::fs::read_to_string(vault.path().join("tasks.md"))?, sidecar_before);
    Ok(())
}
