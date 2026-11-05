//! Contract tests for `frontmatter_link`: adding and removing wikilinks in
//! the link fields a base's sidecar declares, with the reciprocal written on
//! the linked note, in the shortest form Obsidian resolves.

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

fn note(id: &str) -> String {
    format!("---\nid: {id}\ntitle: {id} note\n---\nBody of {id}.\n")
}

/// A vault with a tasks base (declaring link fields) and three notes in it.
fn vault() -> Result<(tempfile::TempDir, ContextOsServer), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    for (relative, content) in [
        ("tasks.base", BASE.to_owned()),
        ("tasks.md", SIDECAR.to_owned()),
        ("tasks/alpha.md", note("Alpha")),
        ("tasks/beta.md", note("Beta")),
        ("tasks/gamma.md", note("Gamma")),
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

fn field(document: &Map<String, Value>, name: &str) -> Value {
    document.get(name).cloned().unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------
// Advertised schema
// ---------------------------------------------------------------------------

#[test]
fn the_advertised_schema_declares_the_operations() -> TestResult {
    let tools = ContextOsServer::catalogue().list_all();
    let tool = tools
        .iter()
        .find(|candidate| candidate.name == "frontmatter_link")
        .ok_or("frontmatter_link is not advertised")?;
    let properties = tool.input_schema.get("properties").ok_or("no properties")?;

    for required in ["path", "operations", "expected_hash"] {
        assert!(properties.get(required).is_some(), "{required} missing: {properties}");
    }
    assert_eq!(
        properties
            .get("operations")
            .and_then(|operations| operations.get("type")),
        Some(&json!("array"))
    );
    assert!(
        tool.description
            .as_ref()
            .is_some_and(|description| !description.is_empty())
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Adding
// ---------------------------------------------------------------------------

#[tokio::test]
async fn adding_a_same_scope_link_writes_the_short_wikilink_and_the_reciprocal() -> TestResult {
    let (vault, server) = vault()?;

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "Beta"}]),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let body = structured(&result)?;
    assert_eq!(body.get("path"), Some(&json!("tasks/alpha.md")));
    assert_eq!(body.get("warnings"), Some(&json!([])));
    let change = &body["changes"][0];
    assert_eq!(change["outcome"], json!("added"));
    assert_eq!(change["field"], json!("depends_on"));
    assert_eq!(change["target"], json!("tasks/beta.md"));
    assert_eq!(change["link"], json!("[[beta|Beta]]"));
    let reciprocal = &body["reciprocal"][0];
    assert_eq!(reciprocal["path"], json!("tasks/beta.md"));
    assert_eq!(reciprocal["field"], json!("blocks"));
    assert_eq!(reciprocal["outcome"], json!("added"));

    let alpha = frontmatter(&vault, "tasks/alpha.md")?;
    assert_eq!(field(&alpha, "depends_on"), json!(["[[beta|Beta]]"]));
    assert!(alpha.contains_key("updated"), "the note's updated stamp is refreshed");
    let beta = frontmatter(&vault, "tasks/beta.md")?;
    assert_eq!(field(&beta, "blocks"), json!(["[[alpha|Alpha]]"]));
    assert!(beta.contains_key("updated"));
    assert_eq!(
        std::fs::read_to_string(vault.path().join("tasks/beta.md"))?
            .lines()
            .last(),
        Some("Body of Beta."),
        "the body is untouched"
    );
    Ok(())
}

#[tokio::test]
async fn a_target_can_be_named_by_its_vault_path_as_well_as_its_id() -> TestResult {
    let (vault, server) = vault()?;

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "tasks/beta.md"}]),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(
        field(&frontmatter(&vault, "tasks/alpha.md")?, "depends_on"),
        json!(["[[beta|Beta]]"])
    );
    Ok(())
}

#[tokio::test]
async fn a_symmetric_field_writes_the_link_on_both_notes() -> TestResult {
    let (vault, server) = vault()?;

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "related", "target": "Gamma"}]),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(
        field(&frontmatter(&vault, "tasks/alpha.md")?, "related"),
        json!(["[[gamma|Gamma]]"])
    );
    assert_eq!(
        field(&frontmatter(&vault, "tasks/gamma.md")?, "related"),
        json!(["[[alpha|Alpha]]"])
    );
    Ok(())
}

#[tokio::test]
async fn a_field_with_no_inverse_writes_only_the_one_note() -> TestResult {
    let (vault, server) = vault()?;
    let beta_before = std::fs::read_to_string(vault.path().join("tasks/beta.md"))?;

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "blocked_by", "target": "Beta"}]),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(structured(&result)?["reciprocal"], json!([]));
    assert_eq!(
        std::fs::read_to_string(vault.path().join("tasks/beta.md"))?,
        beta_before
    );
    Ok(())
}

#[tokio::test]
async fn adding_twice_is_idempotent_and_the_second_call_repairs_a_missing_reciprocal() -> TestResult {
    let (vault, server) = vault()?;
    let add = json!([{"op": "add", "field": "depends_on", "target": "Beta"}]);
    link(&server, "tasks/alpha.md", add.clone()).await?;
    let alpha_after_first = std::fs::read_to_string(vault.path().join("tasks/alpha.md"))?;

    let again = link(&server, "tasks/alpha.md", add.clone()).await?;
    assert_eq!(structured(&again)?["changes"][0]["outcome"], json!("already-present"));
    assert_eq!(
        structured(&again)?["reciprocal"][0]["outcome"],
        json!("already-present")
    );
    assert_eq!(
        std::fs::read_to_string(vault.path().join("tasks/alpha.md"))?,
        alpha_after_first
    );

    // The other side is lost (a hand edit, or an interrupted earlier call).
    std::fs::write(vault.path().join("tasks/beta.md"), note("Beta"))?;
    let repaired = link(&server, "tasks/alpha.md", add).await?;

    assert_eq!(
        structured(&repaired)?["changes"][0]["outcome"],
        json!("already-present")
    );
    assert_eq!(structured(&repaired)?["reciprocal"][0]["outcome"], json!("added"));
    assert_eq!(
        std::fs::read_to_string(vault.path().join("tasks/alpha.md"))?,
        alpha_after_first
    );
    assert_eq!(
        field(&frontmatter(&vault, "tasks/beta.md")?, "blocks"),
        json!(["[[alpha|Alpha]]"])
    );
    Ok(())
}

#[tokio::test]
async fn existing_entries_are_kept_exactly_as_written() -> TestResult {
    let (vault, server) = vault()?;
    std::fs::write(
        vault.path().join("tasks/alpha.md"),
        "---\nid: Alpha\ndepends_on:\n- '[[tasks/gamma|the gamma one]]'\n---\nBody of Alpha.\n",
    )?;

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "Beta"}]),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(
        field(&frontmatter(&vault, "tasks/alpha.md")?, "depends_on"),
        json!(["[[tasks/gamma|the gamma one]]", "[[beta|Beta]]"])
    );
    Ok(())
}

#[tokio::test]
async fn an_existing_link_written_in_another_form_is_recognised_as_the_same_target() -> TestResult {
    let (vault, server) = vault()?;
    std::fs::write(
        vault.path().join("tasks/alpha.md"),
        "---\nid: Alpha\ndepends_on:\n- '[[tasks/beta|earlier]]'\n---\nBody of Alpha.\n",
    )?;

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "Beta"}]),
    )
    .await?;

    assert_eq!(structured(&result)?["changes"][0]["outcome"], json!("already-present"));
    assert_eq!(
        field(&frontmatter(&vault, "tasks/alpha.md")?, "depends_on"),
        json!(["[[tasks/beta|earlier]]"]),
        "no duplicate in the short form"
    );
    Ok(())
}

#[tokio::test]
async fn operations_apply_in_order_in_a_single_write() -> TestResult {
    let (vault, server) = vault()?;

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([
            {"op": "add", "field": "depends_on", "target": "Beta"},
            {"op": "add", "field": "depends_on", "target": "Gamma"},
            {"op": "remove", "field": "depends_on", "target": "Beta"}
        ]),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(
        field(&frontmatter(&vault, "tasks/alpha.md")?, "depends_on"),
        json!(["[[gamma|Gamma]]"])
    );
    assert_eq!(field(&frontmatter(&vault, "tasks/beta.md")?, "blocks"), json!([]));
    assert_eq!(
        field(&frontmatter(&vault, "tasks/gamma.md")?, "blocks"),
        json!(["[[alpha|Alpha]]"])
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Single-valued fields
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_single_valued_field_is_set_and_a_different_link_is_refused() -> TestResult {
    let (vault, server) = vault()?;
    link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "parent", "target": "Beta"}]),
    )
    .await?;
    assert_eq!(
        field(&frontmatter(&vault, "tasks/alpha.md")?, "parent"),
        json!("[[beta|Beta]]")
    );
    let before = std::fs::read_to_string(vault.path().join("tasks/alpha.md"))?;

    let refused = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "parent", "target": "Gamma"}]),
    )
    .await?;

    assert_rejected_with_code(&refused, "link/single-valued")?;
    assert_eq!(std::fs::read_to_string(vault.path().join("tasks/alpha.md"))?, before);
    Ok(())
}

// ---------------------------------------------------------------------------
// Removing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn removing_a_link_removes_the_reciprocal_and_removing_an_absent_one_is_not_an_error() -> TestResult {
    let (vault, server) = vault()?;
    link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "Beta"}]),
    )
    .await?;

    let removed = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "remove", "field": "depends_on", "target": "Beta"}]),
    )
    .await?;

    assert_eq!(removed.is_error, Some(false), "{removed:?}");
    assert_eq!(structured(&removed)?["changes"][0]["outcome"], json!("removed"));
    assert_eq!(structured(&removed)?["reciprocal"][0]["outcome"], json!("removed"));
    assert_eq!(field(&frontmatter(&vault, "tasks/alpha.md")?, "depends_on"), json!([]));
    assert_eq!(field(&frontmatter(&vault, "tasks/beta.md")?, "blocks"), json!([]));

    let again = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "remove", "field": "depends_on", "target": "Beta"}]),
    )
    .await?;

    assert_eq!(again.is_error, Some(false), "{again:?}");
    assert_eq!(structured(&again)?["changes"][0]["outcome"], json!("absent"));
    Ok(())
}

#[tokio::test]
async fn removing_a_single_valued_link_deletes_the_field() -> TestResult {
    let (vault, server) = vault()?;
    link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "parent", "target": "Beta"}]),
    )
    .await?;

    let removed = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "remove", "field": "parent", "target": "Beta"}]),
    )
    .await?;

    assert_eq!(removed.is_error, Some(false), "{removed:?}");
    assert!(!frontmatter(&vault, "tasks/alpha.md")?.contains_key("parent"));
    Ok(())
}

// ---------------------------------------------------------------------------
// Link form
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_same_scope_link_uses_the_scope_form_even_when_another_note_elsewhere_shares_its_name() -> TestResult {
    let (vault, server) = vault()?;
    std::fs::create_dir_all(vault.path().join("elsewhere"))?;
    std::fs::write(vault.path().join("elsewhere/alpha.md"), "---\nid: Other\n---\n")?;

    let result = link(
        &server,
        "tasks/gamma.md",
        json!([{"op": "add", "field": "depends_on", "target": "Alpha"}]),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(
        field(&frontmatter(&vault, "tasks/gamma.md")?, "depends_on"),
        json!(["[[alpha|Alpha]]"]),
        "inside a base the link is the note's path within that base, which is unique there by construction"
    );
    assert_eq!(
        field(&frontmatter(&vault, "tasks/alpha.md")?, "blocks"),
        json!(["[[gamma|Gamma]]"])
    );
    Ok(())
}

#[tokio::test]
async fn a_target_outside_the_bases_scope_is_linked_by_its_full_path_and_gets_no_reciprocal() -> TestResult {
    let (vault, server) = vault()?;
    std::fs::create_dir_all(vault.path().join("notes"))?;
    std::fs::write(vault.path().join("notes/loose.md"), "---\ntitle: Loose\n---\nBody.\n")?;
    let loose_before = std::fs::read_to_string(vault.path().join("notes/loose.md"))?;

    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "related", "target": "notes/loose.md"}]),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(
        field(&frontmatter(&vault, "tasks/alpha.md")?, "related"),
        json!(["[[notes/loose|Loose]]"])
    );
    assert_eq!(
        std::fs::read_to_string(vault.path().join("notes/loose.md"))?,
        loose_before
    );
    assert_eq!(
        structured(&result)?["warnings"],
        json!([]),
        "a link to a note in no declaring base is normal, so its skipped reciprocal is reported by outcome only"
    );
    assert_eq!(structured(&result)?["reciprocal"][0]["outcome"], json!("skipped"));
    assert_eq!(structured(&result)?["reciprocal"][0]["written"], json!(false));
    Ok(())
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

async fn assert_refused_and_nothing_changed(
    vault: &tempfile::TempDir,
    server: &ContextOsServer,
    path: &str,
    operations: Value,
    code: &str,
) -> TestResult {
    let files = ["tasks/alpha.md", "tasks/beta.md", "tasks/gamma.md", "tasks.md"];
    let before: Vec<String> = files
        .iter()
        .map(|file| std::fs::read_to_string(vault.path().join(file)))
        .collect::<Result<_, _>>()?;

    let result = link(server, path, operations.clone()).await?;

    assert_rejected_with_code(&result, code).map_err(|error| format!("{operations}: {error}"))?;
    let after: Vec<String> = files
        .iter()
        .map(|file| std::fs::read_to_string(vault.path().join(file)))
        .collect::<Result<_, _>>()?;
    assert_eq!(before, after, "{operations}: a refused call changed a file");
    Ok(())
}

#[tokio::test]
async fn a_target_that_does_not_exist_is_refused_whether_named_by_id_or_path() -> TestResult {
    let (vault, server) = vault()?;

    for target in ["Nobody", "tasks/missing.md", "tasks/missing"] {
        assert_refused_and_nothing_changed(
            &vault,
            &server,
            "tasks/alpha.md",
            json!([{"op": "add", "field": "depends_on", "target": target}]),
            "link/target-not-found",
        )
        .await?;
    }
    Ok(())
}

#[tokio::test]
async fn one_bad_operation_refuses_the_whole_call() -> TestResult {
    let (vault, server) = vault()?;

    assert_refused_and_nothing_changed(
        &vault,
        &server,
        "tasks/alpha.md",
        json!([
            {"op": "add", "field": "depends_on", "target": "Beta"},
            {"op": "add", "field": "depends_on", "target": "Nobody"}
        ]),
        "link/target-not-found",
    )
    .await
}

#[tokio::test]
async fn an_id_shared_by_two_notes_is_ambiguous() -> TestResult {
    let (vault, server) = vault()?;
    std::fs::write(vault.path().join("tasks/twin.md"), note("Beta"))?;

    assert_refused_and_nothing_changed(
        &vault,
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "Beta"}]),
        "link/target-ambiguous",
    )
    .await
}

#[tokio::test]
async fn a_note_cannot_link_to_itself() -> TestResult {
    let (vault, server) = vault()?;

    for target in ["Alpha", "tasks/alpha.md"] {
        assert_refused_and_nothing_changed(
            &vault,
            &server,
            "tasks/alpha.md",
            json!([{"op": "add", "field": "related", "target": target}]),
            "link/self-reference",
        )
        .await?;
    }
    Ok(())
}

#[tokio::test]
async fn an_undeclared_field_is_refused_naming_the_declared_ones() -> TestResult {
    let (vault, server) = vault()?;

    assert_refused_and_nothing_changed(
        &vault,
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "title", "target": "Beta"}]),
        "link/undeclared-field",
    )
    .await?;
    let result = link(
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "title", "target": "Beta"}]),
    )
    .await?;
    let message = structured(&result)?["message"].as_str().ok_or("no message")?.to_owned();
    assert!(
        message.contains("depends_on, blocks, blocked_by, related, parent"),
        "{message}"
    );
    Ok(())
}

#[tokio::test]
async fn a_note_no_declaring_base_governs_is_refused() -> TestResult {
    let (vault, server) = vault()?;
    std::fs::create_dir_all(vault.path().join("notes"))?;
    std::fs::write(vault.path().join("notes/loose.md"), "---\ntitle: Loose\n---\n")?;

    assert_refused_and_nothing_changed(
        &vault,
        &server,
        "notes/loose.md",
        json!([{"op": "add", "field": "related", "target": "tasks/alpha.md"}]),
        "link/no-link-fields",
    )
    .await
}

#[tokio::test]
async fn a_malformed_link_field_value_is_refused() -> TestResult {
    let (vault, server) = vault()?;
    std::fs::write(
        vault.path().join("tasks/alpha.md"),
        "---\nid: Alpha\ndepends_on: 5\n---\n",
    )?;

    assert_refused_and_nothing_changed(
        &vault,
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "Beta"}]),
        "link/malformed-field",
    )
    .await
}

#[tokio::test]
async fn a_target_whose_name_cannot_be_written_as_a_wikilink_is_refused() -> TestResult {
    let (vault, server) = vault()?;
    std::fs::write(vault.path().join("tasks/odd#name.md"), note("Odd"))?;

    assert_refused_and_nothing_changed(
        &vault,
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "Odd"}]),
        "link/unlinkable-text",
    )
    .await
}

#[tokio::test]
async fn a_stale_expected_hash_is_rejected_and_writes_nothing() -> TestResult {
    let (vault, server) = vault()?;
    let stale = "0".repeat(64);
    let alpha_before = std::fs::read_to_string(vault.path().join("tasks/alpha.md"))?;
    let beta_before = std::fs::read_to_string(vault.path().join("tasks/beta.md"))?;

    let result = call_tool(
        server.clone(),
        "frontmatter_link",
        arguments(json!({
            "path": "tasks/alpha.md",
            "expected_hash": stale,
            "operations": [{"op": "add", "field": "depends_on", "target": "Beta"}]
        }))?,
    )
    .await?;

    assert_rejected_with_code(&result, "io/conflict")?;
    assert_eq!(
        std::fs::read_to_string(vault.path().join("tasks/alpha.md"))?,
        alpha_before
    );
    assert_eq!(
        std::fs::read_to_string(vault.path().join("tasks/beta.md"))?,
        beta_before
    );
    Ok(())
}

#[tokio::test]
async fn the_current_expected_hash_is_accepted() -> TestResult {
    let (vault, server) = vault()?;
    let read = call_tool(
        server.clone(),
        "frontmatter_read",
        arguments(json!({"path": "tasks/alpha.md"}))?,
    )
    .await?;
    let hash = structured(&read)?["content_hash"].clone();

    let result = call_tool(
        server.clone(),
        "frontmatter_link",
        arguments(json!({
            "path": "tasks/alpha.md",
            "expected_hash": hash,
            "operations": [{"op": "add", "field": "depends_on", "target": "Beta"}]
        }))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(
        field(&frontmatter(&vault, "tasks/alpha.md")?, "depends_on"),
        json!(["[[beta|Beta]]"])
    );
    Ok(())
}

#[tokio::test]
async fn an_unknown_incomplete_or_over_specified_operation_is_rejected() -> TestResult {
    let (_vault, server) = vault()?;

    for operations in [
        json!([{"op": "toggle", "field": "depends_on", "target": "Beta"}]),
        json!([{"op": "add", "field": "depends_on"}]),
        json!([{"op": "add", "field": "depends_on", "target": "Beta", "extra": 1}]),
    ] {
        let rejected = match link(&server, "tasks/alpha.md", operations.clone()).await {
            Ok(result) => result.is_error == Some(true),
            Err(_) => true,
        };
        assert!(rejected, "{operations} must be rejected");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Which base governs a note
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_narrowest_declaring_base_governs_a_note() -> TestResult {
    let (vault, server) = vault()?;
    std::fs::create_dir_all(vault.path().join("tasks/inner"))?;
    std::fs::write(vault.path().join("tasks/inner/deep.md"), note("Deep"))?;
    std::fs::write(
        vault.path().join("inner.base"),
        "filters: file.inFolder(\"tasks/inner\")\nviews:\n- type: table\n  name: All\n",
    )?;
    std::fs::write(
        vault.path().join("inner.md"),
        "---\nkey_format: FILEPATH\nlast_id: 0\nlast_create: t0\nlast_update: t0\nlink_fields:\n  see_also: {}\n---\n",
    )?;

    let outer_field = link(
        &server,
        "tasks/inner/deep.md",
        json!([{"op": "add", "field": "related", "target": "tasks/alpha.md"}]),
    )
    .await?;
    assert_rejected_with_code(&outer_field, "link/undeclared-field")?;

    let inner_field = link(
        &server,
        "tasks/inner/deep.md",
        json!([{"op": "add", "field": "see_also", "target": "tasks/alpha.md"}]),
    )
    .await?;
    assert_eq!(inner_field.is_error, Some(false), "{inner_field:?}");
    assert_eq!(
        field(&frontmatter(&vault, "tasks/inner/deep.md")?, "see_also"),
        json!(["[[tasks/alpha|Alpha]]"])
    );
    Ok(())
}

#[tokio::test]
async fn two_declaring_bases_of_equal_scope_are_ambiguous() -> TestResult {
    let (vault, server) = vault()?;
    std::fs::write(vault.path().join("twin.base"), BASE)?;
    std::fs::write(vault.path().join("twin.md"), SIDECAR)?;

    assert_refused_and_nothing_changed(
        &vault,
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "depends_on", "target": "Beta"}]),
        "link/ambiguous-base",
    )
    .await
}

#[tokio::test]
async fn a_malformed_link_declaration_in_the_governing_sidecar_is_refused() -> TestResult {
    let (vault, server) = vault()?;
    std::fs::write(
        vault.path().join("tasks.md"),
        "---\nkey_format: FILEPATH\nlast_id: 0\nlast_create: t0\nlast_update: t0\nlink_fields:\n  blocks:\n    inverse: nowhere\n---\n",
    )?;

    assert_refused_and_nothing_changed(
        &vault,
        &server,
        "tasks/alpha.md",
        json!([{"op": "add", "field": "blocks", "target": "Beta"}]),
        "format/base-sidecar",
    )
    .await
}

#[tokio::test]
async fn a_base_that_declares_no_link_fields_does_not_shadow_a_wider_one() -> TestResult {
    let (vault, server) = vault()?;
    std::fs::create_dir_all(vault.path().join("tasks/inner"))?;
    std::fs::write(vault.path().join("tasks/inner/deep.md"), note("Deep"))?;
    std::fs::write(
        vault.path().join("inner.base"),
        "filters: file.inFolder(\"tasks/inner\")\nviews:\n- type: table\n  name: All\n",
    )?;
    std::fs::write(
        vault.path().join("inner.md"),
        "---\nkey_format: FILEPATH\nlast_id: 0\nlast_create: t0\nlast_update: t0\n---\n",
    )?;

    let result = link(
        &server,
        "tasks/inner/deep.md",
        json!([{"op": "add", "field": "related", "target": "Alpha"}]),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    Ok(())
}
