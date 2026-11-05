//! Contract tests for the `base` property as a hint to which base governs a note when `frontmatter_link` edits
//! its links: it settles a note that two equally scoped declaring bases both contain, and is ignored when it names
//! no base that contains the note.

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

const SIDECAR: &str = "---\nkey_format: FILEPATH\nlast_id: 0\nlast_create: t0\nlast_update: t0\nlink_fields:\n  depends_on:\n    inverse: blocks\n  blocks:\n    inverse: depends_on\n  blocked_by: {}\n  related:\n    inverse: related\n  parent:\n    single: true\n---\n![[tasks.base|tasks]]\n";
const BASE: &str = "filters: file.inFolder(\"tasks\")\nviews:\n- type: table\n  name: All\n";

fn note_in(base: Option<&str>, id: &str) -> String {
    let base_line = base.map_or_else(String::new, |base| format!("base: '{base}'\n"));
    format!("---\nid: {id}\n{base_line}title: {id} note\n---\nBody of {id}.\n")
}

/// A vault where `tasks.base` and `twin.base` both scope `tasks/` and both declare the link fields, and `alpha`
/// carries the given `base` property.
fn vault_with_twin_bases(
    alpha_base: Option<&str>,
) -> Result<(tempfile::TempDir, ContextOsServer), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    for (relative, content) in [
        ("tasks.base", BASE.to_owned()),
        ("tasks.md", SIDECAR.to_owned()),
        ("twin.base", BASE.to_owned()),
        ("twin.md", SIDECAR.to_owned()),
        ("tasks/alpha.md", note_in(alpha_base, "Alpha")),
        ("tasks/beta.md", note_in(None, "Beta")),
    ] {
        let path = vault.path().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)?;
    }
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    Ok((vault, server))
}

fn frontmatter(
    vault: &tempfile::TempDir,
    relative: &str,
) -> Result<Map<String, Value>, Box<dyn std::error::Error + Send + Sync>> {
    let content = std::fs::read_to_string(vault.path().join(relative))?;
    Ok(FrontmatterDocument::try_from(content.as_str())?.frontmatter().clone())
}

async fn link(
    server: &ContextOsServer,
    path: &str,
    operations: Value,
) -> Result<CallToolResult, Box<dyn std::error::Error + Send + Sync>> {
    call_tool(
        server.clone(),
        "frontmatter_link",
        arguments(json!({"path": path, "operations": operations}))?,
    )
    .await
}

fn assert_rejected_with_code(result: &CallToolResult, code: &str) -> TestResult {
    assert_eq!(result.is_error, Some(true), "{result:?}");
    assert_eq!(structured(result)?.get("code"), Some(&json!(code)), "{result:?}");
    Ok(())
}

fn add_dependency() -> Value {
    json!([{"op": "add", "field": "depends_on", "target": "Beta"}])
}

#[tokio::test]
async fn a_note_naming_its_base_settles_two_bases_of_equal_scope() -> TestResult {
    let (vault, server) = vault_with_twin_bases(Some("[[tasks.base]]"))?;

    let result = link(&server, "tasks/alpha.md", add_dependency()).await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(
        frontmatter(&vault, "tasks/alpha.md")?.get("depends_on"),
        Some(&json!(["[[beta|Beta]]"]))
    );
    Ok(())
}

#[tokio::test]
async fn a_note_without_the_property_still_meets_the_ambiguity() -> TestResult {
    let (_vault, server) = vault_with_twin_bases(None)?;

    let result = link(&server, "tasks/alpha.md", add_dependency()).await?;

    assert_rejected_with_code(&result, "link/ambiguous-base")
}

#[tokio::test]
async fn a_property_naming_no_base_that_contains_the_note_is_ignored() -> TestResult {
    for hint in ["[[elsewhere.base]]", "[[not a base]]", "tasks.base"] {
        let (_vault, server) = vault_with_twin_bases(Some(hint))?;

        let result = link(&server, "tasks/alpha.md", add_dependency()).await?;

        assert_rejected_with_code(&result, "link/ambiguous-base")?;
    }
    Ok(())
}
