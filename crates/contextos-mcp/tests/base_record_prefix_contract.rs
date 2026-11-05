//! Contract tests for choosing an id series: `base_record_create`'s
//! `prefix`, `base_create`'s `key_prefixes`, independent per-prefix
//! counters, the migration of a single-series sidecar to the `key_prefixes`
//! map form, and the preservation of every other sidecar frontmatter key.

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
    prefix: Option<&str>,
) -> Result<CallToolResult, Box<dyn std::error::Error + Send + Sync>> {
    let mut request = json!({"path": "tasks.base", "frontmatter": {}, "content": "Body.\n"});
    if let (Some(request), Some(prefix)) = (request.as_object_mut(), prefix) {
        request.insert("prefix".to_owned(), json!(prefix));
    }
    call_tool(server.clone(), "base_record_create", arguments(request)?).await
}

fn sidecar_frontmatter(
    vault: &tempfile::TempDir,
) -> Result<Map<String, Value>, Box<dyn std::error::Error + Send + Sync>> {
    let content = std::fs::read_to_string(vault.path().join("tasks.md"))?;
    Ok(FrontmatterDocument::try_from(content.as_str())?.frontmatter().clone())
}

fn assert_rejected_with_code(result: &CallToolResult, code: &str) -> TestResult {
    assert_eq!(result.is_error, Some(true), "{result:?}");
    assert_eq!(structured(result)?.get("code"), Some(&json!(code)), "{result:?}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Advertised schemas
// ---------------------------------------------------------------------------

#[test]
fn the_advertised_schemas_declare_the_prefix_parameters() -> TestResult {
    let tools = ContextOsServer::catalogue().list_all();
    let property = |tool: &str, name: &str| -> Option<Value> {
        let tool = tools.iter().find(|candidate| candidate.name == tool)?;
        tool.input_schema.get("properties")?.get(name).cloned()
    };

    let prefix = property("base_record_create", "prefix").ok_or("base_record_create advertises no prefix")?;
    assert_eq!(prefix.get("type"), Some(&json!("string")), "{prefix}");
    assert!(prefix.get("description").is_some_and(Value::is_string), "{prefix}");

    let key_prefixes = property("base_create", "key_prefixes").ok_or("base_create advertises no key_prefixes")?;
    assert_eq!(key_prefixes.get("type"), Some(&json!("array")), "{key_prefixes}");
    assert_eq!(
        key_prefixes.get("items").and_then(|items| items.get("type")),
        Some(&json!("string")),
        "{key_prefixes}"
    );
    assert!(key_prefixes.get("description").is_some_and(Value::is_string));
    Ok(())
}

// ---------------------------------------------------------------------------
// Choosing a series
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_named_prefix_mints_from_its_own_independent_counter() -> TestResult {
    let (vault, server) = vault_with_server()?;
    create_base(&server, json!({"key_prefix": "TASK", "key_prefixes": ["DEC", "QUE"]})).await?;

    let mut keys = Vec::new();
    for prefix in [None, Some("DEC"), None, Some("DEC"), Some("QUE")] {
        let result = create_record(&server, prefix).await?;
        assert_eq!(result.is_error, Some(false), "{result:?}");
        keys.push(structured(&result)?.get("key").cloned().ok_or("no key")?);
    }

    assert_eq!(
        keys,
        vec![
            json!("TASK-001"),
            json!("DEC-001"),
            json!("TASK-002"),
            json!("DEC-002"),
            json!("QUE-001")
        ]
    );
    for (file, id) in [("task-001", "TASK-001"), ("dec-001", "DEC-001"), ("dec-002", "DEC-002")] {
        let note = std::fs::read_to_string(vault.path().join(format!("tasks/{file}.md")))?;
        assert!(note.contains(&format!("id: {id}")), "{file}: {note}");
    }
    let sidecar = sidecar_frontmatter(&vault)?;
    assert_eq!(
        sidecar.get("key_prefixes"),
        Some(&json!({"TASK": 2, "DEC": 2, "QUE": 1}))
    );
    assert_eq!(sidecar.get("last_id"), None);
    Ok(())
}

#[tokio::test]
async fn naming_the_default_prefix_is_equivalent_to_omitting_it() -> TestResult {
    let (_vault, server) = vault_with_server()?;
    create_base(&server, json!({"key_prefix": "TASK", "key_prefixes": ["DEC"]})).await?;

    let first = create_record(&server, None).await?;
    let second = create_record(&server, Some("TASK")).await?;

    assert_eq!(structured(&first)?.get("key"), Some(&json!("TASK-001")));
    assert_eq!(structured(&second)?.get("key"), Some(&json!("TASK-002")));
    Ok(())
}

#[tokio::test]
async fn an_undeclared_prefix_is_rejected_with_a_stable_code_and_changes_nothing() -> TestResult {
    let (vault, server) = vault_with_server()?;
    create_base(&server, json!({"key_prefix": "TASK", "key_prefixes": ["DEC"]})).await?;
    let sidecar_before = std::fs::read_to_string(vault.path().join("tasks.md"))?;

    for undeclared in ["QUE", "dec", "TASKS", "../TASK"] {
        let result = create_record(&server, Some(undeclared)).await?;

        assert_rejected_with_code(&result, "io/invalid-key-prefix")?;
        let message = structured(&result)?
            .get("message")
            .and_then(Value::as_str)
            .ok_or("no message")?;
        assert!(
            message.contains("TASK, DEC"),
            "the message names the declared prefixes: {message}"
        );
    }

    let notes: Vec<_> = std::fs::read_dir(vault.path().join("tasks"))?.collect();
    assert!(notes.is_empty(), "no note was written: {notes:?}");
    assert_eq!(std::fs::read_to_string(vault.path().join("tasks.md"))?, sidecar_before);
    Ok(())
}

#[tokio::test]
async fn a_base_declaring_several_prefixes_and_no_default_requires_one() -> TestResult {
    let (vault, server) = vault_with_server()?;
    create_base(&server, json!({"key_prefixes": ["SPEC", "NEED"]})).await?;
    assert_eq!(sidecar_frontmatter(&vault)?.get("key_prefix"), None);

    let omitted = create_record(&server, None).await?;
    assert_rejected_with_code(&omitted, "io/invalid-key-prefix")?;

    let named = create_record(&server, Some("NEED")).await?;
    assert_eq!(named.is_error, Some(false), "{named:?}");
    assert_eq!(structured(&named)?.get("key"), Some(&json!("NEED-001")));
    Ok(())
}

#[tokio::test]
async fn a_filepath_keyed_base_rejects_a_prefix() -> TestResult {
    let (_vault, server) = vault_with_server()?;
    create_base(&server, json!({})).await?;

    let result = call_tool(
        server.clone(),
        "base_record_create",
        arguments(json!({"path": "tasks.base", "filename": "note.md", "prefix": "TASK"}))?,
    )
    .await?;

    assert_rejected_with_code(&result, "io/invalid-key-prefix")
}

#[tokio::test]
async fn a_declared_prefix_whose_next_id_is_already_held_by_a_note_is_caught_up_without_touching_other_series()
-> TestResult {
    let (vault, server) = vault_with_server()?;
    std::fs::write(
        vault.path().join("tasks.md"),
        "---\nkey_prefix: TASK\nkey_format: \"{key_prefix}-{key_id}\"\nkey_prefixes:\n  TASK: 0\n  DEC: 0\nlast_create: t0\nlast_update: t0\n---\n",
    )?;
    create_base(&server, json!({"key_prefix": "TASK"})).await?;
    std::fs::write(
        vault.path().join("tasks/old-decision.md"),
        "---\nid: DEC-001\n---\nBody.\n",
    )?;

    let result = create_record(&server, Some("DEC")).await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(structured(&result)?.get("key"), Some(&json!("DEC-002")));
    let sidecar = sidecar_frontmatter(&vault)?;
    assert_eq!(sidecar.get("key_prefixes"), Some(&json!({"TASK": 0, "DEC": 2})));
    Ok(())
}

// ---------------------------------------------------------------------------
// Migrating a single-series sidecar, and preserving everything else in it
// ---------------------------------------------------------------------------

/// The template link the legacy sidecar declares; [`write_legacy_template`] writes the note it names.
const LEGACY_TEMPLATE_LINK: &str = "[[task-template|Tasks]]";

const LEGACY_SIDECAR: &str = "---\ntemplate: '[[task-template|Tasks]]'\nkey_prefix: SPEC\nkey_format: \"{key_prefix}-{key_id}\"\nlast_id: 41\nlast_create: t0\nlast_update: t0\naliases:\n- counter\n---\n![[tasks.base|tasks]]\n\nHand-written note about this base.\n";

/// The template note [`LEGACY_TEMPLATE_LINK`] names.
fn write_legacy_template(vault: &tempfile::TempDir) -> TestResult {
    std::fs::write(
        vault.path().join("task-template.md"),
        "---\ntype: task\n---\nTemplate body.\n",
    )?;
    Ok(())
}

#[tokio::test]
async fn the_first_write_migrates_a_legacy_sidecar_and_preserves_everything_else_in_it() -> TestResult {
    let (vault, server) = vault_with_server()?;
    std::fs::write(vault.path().join("tasks.md"), LEGACY_SIDECAR)?;
    write_legacy_template(&vault)?;
    std::fs::write(
        vault.path().join("tasks.base"),
        "filters: file.inFolder(\"tasks\")\nviews:\n- type: table\n  name: All\n",
    )?;

    let result = create_record(&server, None).await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(structured(&result)?.get("key"), Some(&json!("SPEC-042")));
    let sidecar = sidecar_frontmatter(&vault)?;
    assert_eq!(sidecar.get("key_prefixes"), Some(&json!({"SPEC": 42})));
    assert_eq!(
        sidecar.get("last_id"),
        None,
        "the legacy counter is removed once migrated"
    );
    assert_eq!(sidecar.get("key_prefix"), Some(&json!("SPEC")));
    assert_eq!(sidecar.get("template"), Some(&json!(LEGACY_TEMPLATE_LINK)));
    assert_eq!(sidecar.get("aliases"), Some(&json!(["counter"])));
    let content = std::fs::read_to_string(vault.path().join("tasks.md"))?;
    assert!(content.contains("Hand-written note about this base."), "{content}");
    Ok(())
}

#[tokio::test]
async fn a_legacy_sidecar_still_rejects_an_undeclared_prefix() -> TestResult {
    let (vault, server) = vault_with_server()?;
    std::fs::write(vault.path().join("tasks.md"), LEGACY_SIDECAR)?;
    std::fs::write(
        vault.path().join("tasks.base"),
        "filters: file.inFolder(\"tasks\")\nviews:\n- type: table\n  name: All\n",
    )?;

    let result = create_record(&server, Some("DEC")).await?;

    assert_rejected_with_code(&result, "io/invalid-key-prefix")?;
    assert_eq!(std::fs::read_to_string(vault.path().join("tasks.md"))?, LEGACY_SIDECAR);
    Ok(())
}

#[tokio::test]
async fn a_filepath_sidecar_keeps_its_template_link_when_a_record_is_created() -> TestResult {
    let (vault, server) = vault_with_server()?;
    std::fs::write(
        vault.path().join("tasks.md"),
        "---\nkey_format: FILEPATH\nlast_id: 0\nlast_create: t0\nlast_update: t0\ntemplate: '[[requirement|Requirement]]'\n---\n![[tasks.base|tasks]]\n",
    )?;
    std::fs::create_dir_all(vault.path().join("templates"))?;
    std::fs::write(
        vault.path().join("templates/requirement.md"),
        "---\ntype: requirement\n---\nTemplate body.\n",
    )?;
    std::fs::write(
        vault.path().join("tasks.base"),
        "filters: file.inFolder(\"tasks\")\nviews:\n- type: table\n  name: All\n",
    )?;

    let result = call_tool(
        server.clone(),
        "base_record_create",
        arguments(json!({"path": "tasks.base", "filename": "note.md", "content": "Body.\n"}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let sidecar = sidecar_frontmatter(&vault)?;
    assert_eq!(sidecar.get("template"), Some(&json!("[[requirement|Requirement]]")));
    assert_eq!(sidecar.get("key_format"), Some(&json!("FILEPATH")));
    Ok(())
}

#[tokio::test]
async fn base_apply_migrates_a_legacy_sidecar_keeping_its_counter_and_only_refreshing_last_update() -> TestResult {
    let (vault, server) = vault_with_server()?;
    std::fs::write(vault.path().join("tasks.md"), LEGACY_SIDECAR)?;
    std::fs::write(
        vault.path().join("tasks.base"),
        "filters: file.inFolder(\"tasks\")\nviews:\n- type: table\n  name: All\n",
    )?;

    let result = call_tool(
        server.clone(),
        "base_apply",
        arguments(json!({
            "path": "tasks.base",
            "operations": [{"op": "add_view", "name": "Other", "view": {"type": "table"}}]
        }))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let sidecar = sidecar_frontmatter(&vault)?;
    assert_eq!(sidecar.get("key_prefixes"), Some(&json!({"SPEC": 41})));
    assert_eq!(sidecar.get("last_id"), None);
    assert_eq!(
        sidecar.get("last_create"),
        Some(&json!("t0")),
        "a schema edit is not record activity"
    );
    assert_ne!(sidecar.get("last_update"), Some(&json!("t0")));
    assert_eq!(sidecar.get("template"), Some(&json!(LEGACY_TEMPLATE_LINK)));
    Ok(())
}

// ---------------------------------------------------------------------------
// base_create declaring several prefixes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn base_create_declares_every_prefix_with_a_zero_counter_and_the_first_as_default() -> TestResult {
    let (vault, server) = vault_with_server()?;

    create_base(
        &server,
        json!({"key_prefix": "TASK", "key_prefixes": ["DEC", "TASK", "QUE"]}),
    )
    .await?;

    let sidecar = sidecar_frontmatter(&vault)?;
    assert_eq!(sidecar.get("key_prefix"), Some(&json!("TASK")));
    assert_eq!(sidecar.get("key_format"), Some(&json!("{key_prefix}-{key_id}")));
    assert_eq!(
        sidecar.get("key_prefixes"),
        Some(&json!({"TASK": 0, "DEC": 0, "QUE": 0}))
    );
    assert_eq!(sidecar.get("last_id"), None);
    Ok(())
}

#[tokio::test]
async fn base_create_rejects_an_unsafe_prefix_before_writing_anything() -> TestResult {
    for request in [
        json!({"key_prefix": "../x"}),
        json!({"key_prefixes": ["BAD-1"]}),
        json!({"key_prefix": "TASK", "key_prefixes": ["task"]}),
        json!({"key_prefixes": [""]}),
    ] {
        let (vault, server) = vault_with_server()?;
        let mut arguments_value = json!({"path": "tasks.base", "definition": definition_scoped_to("tasks")});
        if let (Some(arguments_value), Some(request)) = (arguments_value.as_object_mut(), request.as_object()) {
            arguments_value.extend(request.clone());
        }

        let result = call_tool(server.clone(), "base_create", arguments(arguments_value)?).await?;

        assert_rejected_with_code(&result, "format/base-sidecar")?;
        assert!(!vault.path().join("tasks.base").exists(), "{request}");
        assert!(!vault.path().join("tasks.md").exists(), "{request}");
    }
    Ok(())
}

#[tokio::test]
async fn base_create_leaves_a_sidecar_with_a_different_scheme_untouched_and_warns() -> TestResult {
    let (vault, server) = vault_with_server()?;
    std::fs::write(vault.path().join("tasks.md"), LEGACY_SIDECAR)?;

    let result = call_tool(
        server.clone(),
        "base_create",
        arguments(json!({
            "path": "tasks.base",
            "definition": definition_scoped_to("tasks"),
            "key_prefix": "SPEC",
            "key_prefixes": ["DEC"]
        }))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let warnings = structured(&result)?
        .get("warnings")
        .and_then(Value::as_array)
        .ok_or("no warnings")?;
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(std::fs::read_to_string(vault.path().join("tasks.md"))?, LEGACY_SIDECAR);
    Ok(())
}

#[tokio::test]
async fn base_create_with_the_same_prefixes_as_an_existing_sidecar_is_not_a_conflict() -> TestResult {
    let (vault, server) = vault_with_server()?;
    create_base(&server, json!({"key_prefix": "TASK", "key_prefixes": ["DEC"]})).await?;
    let sidecar_before = std::fs::read_to_string(vault.path().join("tasks.md"))?;
    // A sidecar that outlives its `.base` (deleted by hand, say): creating the
    // base again must recognise the same scheme rather than warn.
    std::fs::remove_file(vault.path().join("tasks.base"))?;

    let result = call_tool(
        server.clone(),
        "base_create",
        arguments(json!({
            "path": "tasks.base",
            "definition": definition_scoped_to("tasks"),
            "key_prefix": "TASK",
            "key_prefixes": ["DEC"]
        }))?,
    )
    .await?;

    assert_eq!(structured(&result)?.get("warnings"), Some(&json!([])));
    assert_eq!(std::fs::read_to_string(vault.path().join("tasks.md"))?, sidecar_before);
    Ok(())
}

// ---------------------------------------------------------------------------
// Hostile or malformed stored sidecars
// ---------------------------------------------------------------------------

const TASKS_BASE: &str = "filters: file.inFolder(\"tasks\")\nviews:\n- type: table\n  name: All\n";

/// Sidecar frontmatter no honest tool would write: each must be refused as a
/// malformed sidecar, before anything is written.
fn malformed_sidecars() -> Vec<(&'static str, String)> {
    let header = "---\nkey_format: \"{key_prefix}-{key_id}\"\nlast_create: t0\nlast_update: t0\n";
    let over_long = "P".repeat(33);
    vec![
        (
            "traversal in a legacy prefix",
            format!("{header}key_prefix: ../evil\nlast_id: 0\n---\n"),
        ),
        (
            "separator in a declared prefix",
            format!("{header}key_prefixes:\n  a/b: 0\n---\n"),
        ),
        (
            "prefixes differing only by case",
            format!("{header}key_prefixes:\n  TASK: 0\n  task: 0\n---\n"),
        ),
        (
            "an over-long prefix",
            format!("{header}key_prefixes:\n  {over_long}: 0\n---\n"),
        ),
        (
            "a counter alongside the map",
            format!("{header}key_prefix: TASK\nlast_id: 4\nkey_prefixes:\n  TASK: 4\n---\n"),
        ),
        (
            "a default that is not declared",
            format!("{header}key_prefix: ZED\nkey_prefixes:\n  TASK: 0\n---\n"),
        ),
        ("an empty map", format!("{header}key_prefixes: {{}}\n---\n")),
        (
            "a non-numeric counter",
            format!("{header}key_prefixes:\n  TASK: four\n---\n"),
        ),
        (
            "no key_format at all",
            "---\nkey_prefix: TASK\nlast_id: 0\nlast_create: t0\nlast_update: t0\n---\n".to_owned(),
        ),
    ]
}

fn markdown_files_outside(vault: &std::path::Path, allowed: &[&str]) -> Result<Vec<String>, std::io::Error> {
    let mut found = Vec::new();
    let mut pending = vec![vault.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "md") {
                let relative = path.strip_prefix(vault).unwrap_or(&path).to_string_lossy().into_owned();
                if !allowed.contains(&relative.as_str()) {
                    found.push(relative);
                }
            }
        }
    }
    found.sort();
    Ok(found)
}

#[tokio::test]
async fn a_malformed_stored_sidecar_is_refused_before_anything_is_written() -> TestResult {
    for (description, sidecar) in malformed_sidecars() {
        let (vault, server) = vault_with_server()?;
        std::fs::write(vault.path().join("tasks.md"), &sidecar)?;
        std::fs::write(vault.path().join("tasks.base"), TASKS_BASE)?;

        let result = create_record(&server, None).await?;

        assert_rejected_with_code(&result, "format/base-sidecar").map_err(|error| format!("{description}: {error}"))?;
        assert_eq!(
            std::fs::read_to_string(vault.path().join("tasks.md"))?,
            sidecar,
            "{description}: the sidecar was rewritten"
        );
        assert_eq!(
            markdown_files_outside(vault.path(), &["tasks.md"])?,
            Vec::<String>::new(),
            "{description}: a note was written, possibly outside the base's folder"
        );
    }
    Ok(())
}

#[tokio::test]
async fn base_apply_leaves_a_malformed_stored_sidecar_untouched_and_warns() -> TestResult {
    let (vault, server) = vault_with_server()?;
    let (_, sidecar) = malformed_sidecars().remove(0);
    std::fs::write(vault.path().join("tasks.md"), &sidecar)?;
    std::fs::write(vault.path().join("tasks.base"), TASKS_BASE)?;

    let result = call_tool(
        server.clone(),
        "base_apply",
        arguments(json!({
            "path": "tasks.base",
            "operations": [{"op": "add_view", "name": "Other", "view": {"type": "table"}}]
        }))?,
    )
    .await?;

    assert_eq!(
        result.is_error,
        Some(false),
        "the schema edit itself succeeds: {result:?}"
    );
    let warnings = structured(&result)?
        .get("warnings")
        .and_then(Value::as_array)
        .ok_or("no warnings")?;
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(std::fs::read_to_string(vault.path().join("tasks.md"))?, sidecar);
    Ok(())
}
