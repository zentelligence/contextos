//! Zero-vault bootstrap contract tests: a `Config` with no `[[vault]]` entries at all must still let `contextos-mcp`
//! complete a real MCP handshake, advertise its full tool catalogue, and report an empty vault collection, while a
//! vault-dependent tool call fails with a typed, actionable error rather than panicking or falling back to an
//! implicit current-working-directory vault.

use contextos_mcp::{Config, ContextOsServer};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, json};

fn zero_vault_config() -> Result<Config, Box<dyn std::error::Error + Send + Sync>> {
    Ok(Config::try_from("[server]\ntransports = [\"stdio\"]\n")?)
}

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

#[tokio::test]
async fn mcp_completes_its_handshake_and_lists_every_tool_with_zero_configured_vaults()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = ContextOsServer::try_from(zero_vault_config()?)?;
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });

    // `().serve(client_transport)` performs the MCP `initialize` handshake itself; reaching `list_tools` at all
    // proves it completed.
    let mut client = ().serve(client_transport).await?;
    let tools = client.list_tools(None).await?;
    let resources = client.list_resources(None).await?;
    client.close().await?;
    server_handle.await??;

    assert!(
        tools.tools.iter().any(|tool| tool.name == "vault_info"),
        "zero-vault server should still advertise its normal tool catalogue"
    );
    assert!(
        tools
            .tools
            .iter()
            .any(|tool| tool.name == "fs_list_allowed_directories"),
        "zero-vault server should still advertise its normal tool catalogue"
    );
    // Nothing to enumerate with zero configured roots, but the call itself must succeed rather than error:
    // `resources/list` walks `VaultSet`'s own roots, which is empty here, not a failure condition.
    assert!(
        resources.resources.is_empty(),
        "`resources.resources` should be empty, got {:?}",
        resources.resources
    );
    Ok(())
}

#[tokio::test]
async fn vault_info_reports_an_empty_vault_collection_with_none_configured()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = ContextOsServer::try_from(zero_vault_config()?)?;

    let result = call_tool(server, "vault_info", Map::new()).await?;

    assert_eq!(result.is_error, Some(false));
    let content = result
        .structured_content
        .as_ref()
        .ok_or_else(|| std::io::Error::other("vault_info omitted structured content"))?;
    assert_eq!(content.get("vaults"), Some(&json!([])));
    Ok(())
}

#[tokio::test]
async fn fs_list_allowed_directories_reports_an_empty_collection_with_none_configured()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = ContextOsServer::try_from(zero_vault_config()?)?;

    let result = call_tool(server, "fs_list_allowed_directories", Map::new()).await?;

    assert_eq!(result.is_error, Some(false));
    let content = result
        .structured_content
        .as_ref()
        .ok_or_else(|| std::io::Error::other("fs_list_allowed_directories omitted structured content"))?;
    assert_eq!(content.get("directories"), Some(&json!([])));
    Ok(())
}

/// `vault_index_rebuild` defaults its `path` parameter to `"."`, the existing "act on the sole configured vault"
/// shorthand: with zero vaults configured there is no implicit vault for `"."` to mean, so this must fail with the
/// typed `path/invalid-root` error, never an implicit current-working-directory fallback and never a panic.
#[tokio::test]
async fn a_vault_dependent_tool_call_fails_with_a_typed_actionable_error_with_none_configured()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = ContextOsServer::try_from(zero_vault_config()?)?;

    let result = call_tool(server, "vault_index_rebuild", Map::new()).await?;

    assert_eq!(result.is_error, Some(true));
    let content = result
        .structured_content
        .as_ref()
        .ok_or_else(|| std::io::Error::other("vault_index_rebuild omitted structured content"))?;
    assert_eq!(content.get("code"), Some(&json!("path/invalid-root")));
    assert_eq!(
        content.get("remediation"),
        Some(&json!(
            "Configure at least one allowed vault directory and restart the server."
        ))
    );
    Ok(())
}
