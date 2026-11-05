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

fn minimal_definition() -> serde_json::Value {
    json!({
        "views": [{"type": "table", "name": "All"}]
    })
}

#[tokio::test]
async fn base_create_with_a_key_prefix_creates_a_sequential_sidecar()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server.clone(),
        "base_create",
        serde_json::from_value(json!({
            "path": "requirements.base",
            "definition": minimal_definition(),
            "key_prefix": "FR"
        }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let body = structured(&result)?;
    assert_eq!(body.get("sidecar_path"), Some(&json!("requirements.md")));
    assert_eq!(body.get("warnings"), Some(&json!([])));

    let sidecar = std::fs::read_to_string(vault.path().join("requirements.md"))?;
    assert!(sidecar.contains("key_prefix: FR"));
    assert!(
        sidecar.contains("key_format: '{key_prefix}-{key_id}'")
            || sidecar.contains("key_format: \"{key_prefix}-{key_id}\"")
    );
    assert!(sidecar.contains("key_prefixes:"), "{sidecar}");
    assert!(
        sidecar.contains("FR: 0"),
        "the default series starts at zero: {sidecar}"
    );
    assert!(
        !sidecar.contains("last_id"),
        "counters live only in key_prefixes: {sidecar}"
    );
    assert!(sidecar.contains("![[requirements.base|requirements]]"));
    Ok(())
}

#[tokio::test]
async fn base_create_without_a_key_prefix_creates_a_filepath_sidecar()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server.clone(),
        "base_create",
        serde_json::from_value(json!({
            "path": "notes.base",
            "definition": minimal_definition()
        }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");

    let sidecar = std::fs::read_to_string(vault.path().join("notes.md"))?;
    assert!(sidecar.contains("key_format: FILEPATH"));
    assert!(!sidecar.contains("key_prefix"));
    Ok(())
}

#[tokio::test]
async fn base_create_leaves_a_matching_existing_sidecar_untouched()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(
        vault.path().join("requirements.md"),
        "---\nkey_prefix: FR\nkey_format: \"{key_prefix}-{key_id}\"\nlast_id: 41\nlast_create: t0\nlast_update: t0\n---\n![[requirements.base|requirements]]\n",
    )?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server.clone(),
        "base_create",
        serde_json::from_value(json!({
            "path": "requirements.base",
            "definition": minimal_definition(),
            "key_prefix": "FR"
        }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let body = structured(&result)?;
    assert_eq!(body.get("warnings"), Some(&json!([])));

    let sidecar = std::fs::read_to_string(vault.path().join("requirements.md"))?;
    assert!(sidecar.contains("last_id: 41"));
    Ok(())
}

#[tokio::test]
async fn base_create_warns_without_overwriting_a_mismatched_existing_sidecar()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    // A stray sidecar with a different key_prefix already sits at this
    // path (e.g. hand-authored, or left over from a prior attempt), while
    // no .base file exists yet: base_create can still create the .base
    // fresh, but must not silently adopt or overwrite the mismatched
    // sidecar it finds alongside it.
    std::fs::write(
        vault.path().join("requirements.md"),
        "---\nkey_prefix: OLD\nkey_format: \"{key_prefix}-{key_id}\"\nlast_id: 5\nlast_create: t0\nlast_update: t0\n---\n",
    )?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server.clone(),
        "base_create",
        serde_json::from_value(json!({
            "path": "requirements.base",
            "definition": minimal_definition(),
            "key_prefix": "NEW"
        }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let body = structured(&result)?;
    let warnings = body
        .get("warnings")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("base_create omitted warnings"))?;
    assert!(!warnings.is_empty(), "expected a key-scheme conflict warning");

    let sidecar = std::fs::read_to_string(vault.path().join("requirements.md"))?;
    assert!(sidecar.contains("key_prefix: OLD"));
    assert!(sidecar.contains("last_id: 5"));
    Ok(())
}

#[tokio::test]
async fn base_apply_advances_last_update_but_never_a_counter_or_last_create()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    call_tool(
        server.clone(),
        "base_create",
        serde_json::from_value(json!({
            "path": "requirements.base",
            "definition": minimal_definition(),
            "key_prefix": "FR"
        }))?,
    )
    .await?;
    let before = std::fs::read_to_string(vault.path().join("requirements.md"))?;
    assert!(before.contains("FR: 0"), "{before}");

    let result = call_tool(
        server.clone(),
        "base_apply",
        serde_json::from_value(json!({
            "path": "requirements.base",
            "operations": [{"op": "add_formula", "name": "title_link", "expression": "file.asLink(note.title)"}]
        }))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");

    let after = std::fs::read_to_string(vault.path().join("requirements.md"))?;
    assert!(after.contains("FR: 0"), "{after}");
    assert!(!after.contains("last_id"), "{after}");
    assert!(after.contains("key_prefix: FR"));
    Ok(())
}

#[tokio::test]
async fn base_apply_against_a_base_with_no_sidecar_creates_none() -> Result<(), Box<dyn std::error::Error + Send + Sync>>
{
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(
        vault.path().join("hand-authored.base"),
        "views:\n  - type: table\n    name: All\n",
    )?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server.clone(),
        "base_apply",
        serde_json::from_value(json!({
            "path": "hand-authored.base",
            "operations": [{"op": "add_formula", "name": "title_link", "expression": "file.asLink(note.title)"}]
        }))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert!(!vault.path().join("hand-authored.md").exists());
    Ok(())
}
