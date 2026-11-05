//! A temporary vault whose task base declares series templates, and the MCP calls the template and doctor contract
//! tests share.
//!
//! A binary that includes this file without using every item would otherwise warn, which is expected rather than
//! genuine dead code.
#![allow(dead_code)]

use contextos_mcp::ContextOsServer;
use contextos_obsidian::FrontmatterDocument;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, Value, json};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A task template: `type` and `task_type` are literal, `level` is a number, and the rest are the template's own
/// defaults for people using it in Obsidian.
pub const TASK_TEMPLATE: &str = "---\ntype: task\ntask_type: action\nlevel: 1\nstate: next\npriority: normal\nentity: \"{{entity}}\"\n---\nTemplate body.\n";
pub const DECISION_TEMPLATE: &str =
    "---\ntype: task\ntask_type: decision\nlevel: 1\nstate: open\n---\nTemplate body.\n";
pub const TASKS_BASE: &str = "filters: file.inFolder(\"tasks\")\nviews:\n- type: table\n  name: All\n";

/// A keyed sidecar for `tasks.base` with series `A`, `D`, and `Q`, followed by `declaration` (hand-authored keys).
pub fn sidecar(declaration: &str) -> String {
    format!(
        "---\nkey_prefix: A\nkey_format: \"{{key_prefix}}-{{key_id}}\"\nkey_prefixes:\n  A: 0\n  D: 0\n  Q: 0\nlast_create: t0\nlast_update: t0\n{declaration}---\n![[tasks.base|tasks]]\n"
    )
}

/// The declaration the contract tests use unless a test names its own: one template for every series, a template of
/// its own for `D`, and `task_type` and `level` fixed beyond `type`.
pub const STANDARD_DECLARATION: &str = "template: '[[templates/task|Task]]'\nseries_templates:\n  D: '[[templates/task-decision|Decision task]]'\nfixed_fields: [task_type, level]\n";

/// The files of a vault with the task base, its sidecar declaring `declaration`, and both templates.
pub fn standard_files(declaration: &str) -> Vec<(String, String)> {
    vec![
        ("tasks.base".to_owned(), TASKS_BASE.to_owned()),
        ("tasks.md".to_owned(), sidecar(declaration)),
        ("templates/task.md".to_owned(), TASK_TEMPLATE.to_owned()),
        ("templates/task-decision.md".to_owned(), DECISION_TEMPLATE.to_owned()),
        ("tasks/.keep".to_owned(), String::new()),
    ]
}

/// A vault holding `files` (vault-relative path and content), with Git and managed indexes off so nothing but the
/// files under test exist, and a server over it. `max_read_mb` replaces the default read limit when given.
pub fn vault_with(
    files: &[(String, String)],
    max_read_mb: Option<u64>,
) -> Result<(tempfile::TempDir, ContextOsServer), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    for (relative, content) in files {
        let path = vault.path().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)?;
    }
    let mut config = super::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.enabled = false;
    config.vaults[0].index_md.enabled = false;
    if let Some(limit) = max_read_mb {
        config.vaults[0].limits.max_read_mb = limit;
    }
    let server = ContextOsServer::try_from(config)?;
    Ok((vault, server))
}

pub async fn call_tool(
    server: &ContextOsServer,
    name: &'static str,
    arguments: Value,
) -> Result<CallToolResult, BoxError> {
    let arguments: Map<String, Value> = serde_json::from_value(arguments)?;
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server = server.clone();
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), BoxError>(())
    });
    let mut client = ().serve(client_transport).await?;
    let result = client
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments))
        .await;
    client.close().await?;
    server_handle.await??;
    Ok(result?)
}

pub fn structured(result: &CallToolResult) -> Result<&Map<String, Value>, BoxError> {
    result
        .structured_content
        .as_ref()
        .and_then(Value::as_object)
        .ok_or_else(|| std::io::Error::other("tool result omitted structured content").into())
}

/// `base_record_create` on `tasks.base` in series `prefix` (the default series when `None`) with `frontmatter`.
pub async fn create_record(
    server: &ContextOsServer,
    prefix: Option<&str>,
    frontmatter: Value,
) -> Result<CallToolResult, BoxError> {
    let mut request = json!({"path": "tasks.base", "frontmatter": frontmatter, "content": "Body.\n"});
    if let (Some(request), Some(prefix)) = (request.as_object_mut(), prefix) {
        request.insert("prefix".to_owned(), json!(prefix));
    }
    call_tool(server, "base_record_create", request).await
}

/// The vault-relative path of the record a successful `base_record_create` wrote.
pub fn created_path(result: &CallToolResult) -> Result<String, BoxError> {
    assert_eq!(result.is_error, Some(false), "{result:?}");
    Ok(structured(result)?
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| std::io::Error::other("base_record_create omitted its path"))?
        .to_owned())
}

pub fn frontmatter_of(vault: &tempfile::TempDir, relative: &str) -> Result<Map<String, Value>, BoxError> {
    let content = std::fs::read_to_string(vault.path().join(relative))?;
    Ok(FrontmatterDocument::try_from(content.as_str())?.frontmatter().clone())
}

/// Every vault-relative path of a Markdown file under `folder`, sorted.
pub fn notes_under(vault: &tempfile::TempDir, folder: &str) -> Result<Vec<String>, BoxError> {
    let mut found = Vec::new();
    let directory = vault.path().join(folder);
    if directory.is_dir() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            if entry.path().extension().is_some_and(|extension| extension == "md") {
                found.push(format!("{folder}/{}", entry.file_name().to_string_lossy()));
            }
        }
    }
    found.sort();
    Ok(found)
}

pub fn failure_text(result: &CallToolResult) -> Result<String, BoxError> {
    Ok(serde_json::to_string(structured(result)?)?)
}
