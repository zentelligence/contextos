//! Contract tests for `frontmatter_link` and the sidecars of bases that do or do not govern the note: an unreadable,
//! malformed, or unparsable sidecar is refused only when its base may govern the note being edited.

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

const SIDECAR: &str = "---\nkey_format: FILEPATH\nlast_id: 0\nlast_create: t0\nlast_update: t0\nlink_fields:\n  depends_on:\n    inverse: blocks\n  blocks:\n    inverse: depends_on\n  related:\n    inverse: related\n  parent:\n    single: true\n---\n![[tasks.base|tasks]]\n";
const BASE: &str = "filters: file.inFolder(\"tasks\")\nviews:\n- type: table\n  name: All\n";
const UNSCOPED_BASE: &str = "filters: file.ext == \"md\"\nviews:\n- type: table\n  name: All\n";

fn note(id: &str) -> String {
    format!("---\nid: {id}\ntitle: {id} note\n---\nBody of {id}.\n")
}

fn write(vault: &tempfile::TempDir, relative: &str, content: &str) -> Result<(), std::io::Error> {
    let path = vault.path().join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

fn vault() -> Result<(tempfile::TempDir, ContextOsServer), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    for (relative, content) in [
        ("tasks.base", BASE.to_owned()),
        ("tasks.md", SIDECAR.to_owned()),
        ("tasks/alpha.md", note("Alpha")),
        ("tasks/beta.md", note("Beta")),
        ("tasks/gamma.md", note("Gamma")),
    ] {
        write(&vault, relative, &content)?;
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

fn field(document: &Map<String, Value>, name: &str) -> Value {
    document.get(name).cloned().unwrap_or(Value::Null)
}

async fn assert_refused_and_nothing_changed(
    vault: &tempfile::TempDir,
    server: &ContextOsServer,
    path: &str,
    operations: Value,
    code: &str,
) -> TestResult {
    let files = ["tasks/alpha.md", "tasks/beta.md", "tasks/gamma.md", "tasks.md"];
    let read = || -> Result<Vec<String>, std::io::Error> {
        files
            .iter()
            .map(|file| std::fs::read_to_string(vault.path().join(file)))
            .collect()
    };
    let before = read()?;

    let result = link(server, path, operations.clone()).await?;

    assert_rejected_with_code(&result, code).map_err(|error| format!("{operations}: {error}"))?;
    assert_eq!(before, read()?, "{operations}: a refused call changed a file");
    Ok(())
}

// ---------------------------------------------------------------------------
// Sidecars that do not concern the note
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unrelated_base_with_an_unreadable_sidecar_does_not_break_the_call() -> TestResult {
    let (vault, server) = vault()?;
    write(
        &vault,
        "elsewhere.base",
        "filters: file.inFolder(\"elsewhere\")\nviews:\n- type: table\n  name: All\n",
    )?;
    write(&vault, "elsewhere.md", "---\nkey_format: [unclosed\n---\n")?;

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "Beta"}]),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    Ok(())
}

/// A stray `.base` with no folder filter reaches every note, and a note beside it that is not a sidecar at all (a
/// daily note, say) is read as its malformed sidecar.
fn a_vault_wide_base_beside_a_note_that_is_not_its_sidecar()
-> Result<(tempfile::TempDir, ContextOsServer), Box<dyn std::error::Error + Send + Sync>> {
    let (vault, server) = vault()?;
    write(&vault, "stray.base", "views:\n- type: table\n  name: Table\n")?;
    write(
        &vault,
        "stray.md",
        "---\ndate: 2026-08-04\ntags:\n- note\n---\n# Tuesday\n",
    )?;
    Ok((vault, server))
}

#[tokio::test]
async fn a_note_naming_its_base_is_governed_by_it_despite_another_base_with_a_malformed_sidecar() -> TestResult {
    let (vault, server) = a_vault_wide_base_beside_a_note_that_is_not_its_sidecar()?;
    for (file, id) in [("tasks/alpha.md", "Alpha"), ("tasks/beta.md", "Beta")] {
        write(
            &vault,
            file,
            &format!("---\nid: {id}\ntitle: {id} note\nbase: '[[tasks.base]]'\n---\nBody of {id}.\n"),
        )?;
    }

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "Beta"}]),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(
        field(&frontmatter(&vault, "tasks/beta.md")?, "blocks"),
        json!(["[[alpha|Alpha]]"])
    );
    Ok(())
}

#[tokio::test]
async fn a_malformed_sidecar_that_may_govern_the_note_is_refused_naming_its_base() -> TestResult {
    let (vault, server) = a_vault_wide_base_beside_a_note_that_is_not_its_sidecar()?;

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "Beta"}]),
    )
    .await?;

    assert_rejected_with_code(&result, "format/base-sidecar")?;
    let message = structured(&result)?
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        message.contains("stray.base"),
        "the error must name the base: {message}"
    );
    assert!(
        !frontmatter(&vault, "tasks/beta.md")?.contains_key("blocks"),
        "a refused call changed a note"
    );
    Ok(())
}

const DECLARES_BASE: &str = "---\nid: Alpha\ntitle: Alpha note\nbase: '[[tasks.base]]'\n---\nBody of Alpha.\n";
const NOT_A_SIDECAR: &str = "---\ndate: 2026-08-04\n---\n# Tuesday\n";
const UNPARSABLE: &str = "---\nkey_format: [unclosed\n---\n";

fn message_of(result: &CallToolResult) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    Ok(structured(result)?
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}

#[tokio::test]
async fn a_note_naming_its_base_is_governed_by_it_despite_another_base_with_an_unparsable_sidecar() -> TestResult {
    let (vault, server) = vault()?;
    write(&vault, "stray.base", "views:\n- type: table\n  name: Table\n")?;
    write(&vault, "stray.md", UNPARSABLE)?;
    write(&vault, "tasks/alpha.md", DECLARES_BASE)?;

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "Beta"}]),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    Ok(())
}

#[tokio::test]
async fn an_unparsable_sidecar_that_may_govern_the_note_is_refused_naming_its_base() -> TestResult {
    let (vault, server) = vault()?;
    write(&vault, "stray.base", "views:\n- type: table\n  name: Table\n")?;
    write(&vault, "stray.md", UNPARSABLE)?;

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "Beta"}]),
    )
    .await?;

    assert_rejected_with_code(&result, "format/frontmatter")?;
    let message = message_of(&result)?;
    assert!(
        message.contains("stray.base"),
        "the error must name the base: {message}"
    );
    Ok(())
}

#[tokio::test]
async fn the_failure_of_the_notes_own_base_is_reported_ahead_of_another_base_failing() -> TestResult {
    let (vault, server) = vault()?;
    // Two vault-wide bases with unusable sidecars, one on either side of `tasks.base` in any search order, and a
    // third that is valid and would govern the note if its own base were skipped.
    for stray in ["aa", "zz"] {
        write(
            &vault,
            &format!("{stray}.base"),
            "views:\n- type: table\n  name: Table\n",
        )?;
        write(&vault, &format!("{stray}.md"), NOT_A_SIDECAR)?;
    }
    write(&vault, "mm.base", "views:\n- type: table\n  name: Table\n")?;
    write(
        &vault,
        "mm.md",
        "---\nkey_format: FILEPATH\nlast_id: 0\nlast_create: t0\nlast_update: t0\nlink_fields:\n  depends_on:\n    inverse: blocks\n  blocks:\n    inverse: depends_on\n---\n",
    )?;
    write(&vault, "tasks.md", NOT_A_SIDECAR)?;
    write(&vault, "tasks/alpha.md", DECLARES_BASE)?;

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "Beta"}]),
    )
    .await?;

    assert_rejected_with_code(&result, "format/base-sidecar")?;
    let message = message_of(&result)?;
    assert!(
        message.contains("tasks.base"),
        "the error must name the note's own base: {message}"
    );
    Ok(())
}

#[tokio::test]
async fn a_base_named_by_a_note_outside_its_scope_does_not_excuse_another_bases_failure() -> TestResult {
    let (vault, server) = a_vault_wide_base_beside_a_note_that_is_not_its_sidecar()?;
    write(&vault, "other/alpha.md", DECLARES_BASE)?;

    let result = link(
        &server,
        "other/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "tasks/beta.md"}]),
    )
    .await?;

    assert_rejected_with_code(&result, "format/base-sidecar")?;
    assert!(message_of(&result)?.contains("stray.base"));
    Ok(())
}

#[tokio::test]
async fn a_governing_base_with_an_unreadable_sidecar_is_refused() -> TestResult {
    let (vault, server) = vault()?;
    write(&vault, "tasks.md", "---\nkey_format: [unclosed\n---\n")?;

    assert_refused_and_nothing_changed(
        &vault,
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "Beta"}]),
        "format/frontmatter",
    )
    .await
}

#[tokio::test]
async fn a_base_declaring_no_folder_scope_governs_the_whole_vault_unless_a_narrower_one_does() -> TestResult {
    let (vault, server) = vault()?;
    write(&vault, "misc/one.md", &note("One"))?;
    write(&vault, "all.base", UNSCOPED_BASE)?;
    write(
        &vault,
        "all.md",
        "---\nkey_format: FILEPATH\nlast_id: 0\nlast_create: t0\nlast_update: t0\nlink_fields:\n  see_also: {}\n---\n",
    )?;

    let wide = link(
        &server,
        "misc/one.md",
        json!([{"op": "add", "field": "see_also", "target": "tasks/alpha.md"}]),
    )
    .await?;
    assert_eq!(wide.is_error, Some(false), "{wide:?}");
    assert_eq!(
        field(&frontmatter(&vault, "misc/one.md")?, "see_also"),
        json!(["[[tasks/alpha|Alpha]]"]),
        "a base with no folder scope covers the vault, so a note's path in it is its vault path"
    );

    let narrower = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "see_also", "target": "Beta"}]),
    )
    .await?;
    assert_rejected_with_code(&narrower, "link/undeclared-field")
}
