//! `base_query`'s record lookup by id (`ids`), its validation of a filter's grammar before any note is scanned, and
//! Obsidian's `containsAny` filter function, exercised through the MCP adapter.
mod support;

use contextos_mcp::ContextOsServer;
use rmcp::model::CallToolResult;
use serde_json::{Map, Value, json};
use support::git_harness::call;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const TASKS_BASE: &str = "\
filters: file.inFolder(\"memory/tasks\") && file.basename != \"index\"
views:
  - type: table
    name: Open
    filters: '!status.containsAny(\"done\", \"dropped\", \"closed\")'
    order:
      - id
      - status
";

fn structured(result: &CallToolResult) -> Result<&Map<String, Value>, BoxError> {
    result
        .structured_content
        .as_ref()
        .and_then(Value::as_object)
        .ok_or_else(|| "tool result omitted structured content".into())
}

fn write(vault: &std::path::Path, relative: &str, content: &str) -> Result<(), BoxError> {
    let path = vault.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    Ok(())
}

/// A tasks base with three tasks in scope (one nested), and one note outside the base's scope that carries an id the
/// lookup must not find.
fn task_vault() -> Result<(tempfile::TempDir, ContextOsServer), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(vault.path(), "memory/tasks.base", TASKS_BASE)?;
    write(vault.path(), "memory/tasks/index.md", "# Tasks\n")?;
    write(
        vault.path(),
        "memory/tasks/a-001.md",
        "---\nid: A-001\nstatus: draft\ndepends_on:\n  - '[[a-002]]'\n---\n# First\n\nFirst body.\n",
    )?;
    write(
        vault.path(),
        "memory/tasks/nested/a-002.md",
        "---\nid: A-002\nstatus: done\n---\n# Second\n",
    )?;
    write(
        vault.path(),
        "memory/tasks/q-001.md",
        "---\nid: Q-001\nstatus: dropped\n---\n# Question\n",
    )?;
    write(
        vault.path(),
        "notes/a-003.md",
        "---\nid: A-003\nstatus: draft\n---\n# Outside the base\n",
    )?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    Ok((vault, server))
}

fn error_code(result: &CallToolResult) -> Result<String, BoxError> {
    Ok(structured(result)?
        .get("code")
        .and_then(Value::as_str)
        .ok_or("error result omitted its code")?
        .to_owned())
}

#[tokio::test]
async fn ids_return_each_record_in_the_order_asked_with_its_path_frontmatter_and_content_hash() -> Result<(), BoxError>
{
    let (_vault, server) = task_vault()?;
    let result = call(
        &server,
        "base_query",
        json!({"path": "memory/tasks.base", "ids": ["A-002", "A-001"]}),
    )
    .await?;
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let body = structured(&result)?;
    let records = body.get("records").and_then(Value::as_array).ok_or("records missing")?;
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["id"], "A-002");
    assert_eq!(records[0]["path"], "memory/tasks/nested/a-002.md");
    assert_eq!(records[0]["frontmatter"]["status"], "done");
    assert_eq!(records[1]["id"], "A-001");
    assert_eq!(records[1]["frontmatter"]["depends_on"], json!(["[[a-002]]"]));
    for record in records {
        let hash = record["content_hash"].as_str().ok_or("content_hash missing")?;
        assert_eq!(hash.len(), 64, "a SHA-256 hex digest");
        assert!(
            record.get("content").is_none(),
            "content is only returned when asked for"
        );
    }
    assert_eq!(body.get("not_found"), Some(&json!([])));
    assert_eq!(body.get("matched"), Some(&json!(2)));
    Ok(())
}

#[tokio::test]
async fn an_id_outside_the_bases_scope_or_absent_is_reported_not_found() -> Result<(), BoxError> {
    let (_vault, server) = task_vault()?;
    let result = call(
        &server,
        "base_query",
        json!({"path": "memory/tasks.base", "ids": ["A-003", "A-032", "Q-001"]}),
    )
    .await?;
    let body = structured(&result)?;
    let records = body.get("records").and_then(Value::as_array).ok_or("records missing")?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["id"], "Q-001");
    assert_eq!(body.get("not_found"), Some(&json!(["A-003", "A-032"])));
    Ok(())
}

#[tokio::test]
async fn include_content_returns_each_records_body_below_its_frontmatter() -> Result<(), BoxError> {
    let (_vault, server) = task_vault()?;
    let result = call(
        &server,
        "base_query",
        json!({"path": "memory/tasks.base", "ids": ["A-001"], "include_content": true}),
    )
    .await?;
    let body = structured(&result)?;
    assert_eq!(body["records"][0]["content"], "# First\n\nFirst body.\n");
    Ok(())
}

#[tokio::test]
async fn a_looked_up_content_hash_is_accepted_as_the_expected_hash_of_a_conditional_update() -> Result<(), BoxError> {
    let (_vault, server) = task_vault()?;
    let lookup = call(
        &server,
        "base_query",
        json!({"path": "memory/tasks.base", "ids": ["A-001"]}),
    )
    .await?;
    let record = &structured(&lookup)?["records"][0];
    let update = call(
        &server,
        "frontmatter_update",
        json!({
            "path": record["path"],
            "patch": {"status": "doing", "claimed_by": "kai/run-1"},
            "expected_hash": record["content_hash"],
        }),
    )
    .await?;
    assert_ne!(update.is_error, Some(true), "{update:?}");
    let stale = call(
        &server,
        "frontmatter_update",
        json!({
            "path": record["path"],
            "patch": {"claimed_by": "sol/run-2"},
            "expected_hash": record["content_hash"],
        }),
    )
    .await?;
    assert_eq!(
        stale.is_error,
        Some(true),
        "a second claim against the same version must be refused"
    );
    assert_eq!(error_code(&stale)?, "io/conflict");
    Ok(())
}

#[tokio::test]
async fn an_id_held_by_two_records_returns_both_and_a_diagnostic() -> Result<(), BoxError> {
    let (vault, server) = task_vault()?;
    write(
        vault.path(),
        "memory/tasks/copy-of-a-001.md",
        "---\nid: A-001\nstatus: draft\n---\n# Copy\n",
    )?;
    let result = call(
        &server,
        "base_query",
        json!({"path": "memory/tasks.base", "ids": ["A-001"]}),
    )
    .await?;
    let body = structured(&result)?;
    let records = body.get("records").and_then(Value::as_array).ok_or("records missing")?;
    assert_eq!(records.len(), 2);
    let diagnostics = body
        .get("diagnostics")
        .and_then(Value::as_array)
        .ok_or("diagnostics missing")?;
    assert!(
        diagnostics
            .iter()
            .any(|entry| entry["code"] == "query/base-query-duplicate-id"),
        "{diagnostics:?}"
    );
    Ok(())
}

#[tokio::test]
async fn ids_are_refused_alongside_a_view_definition_limit_or_format_and_when_empty_repeated_or_too_many()
-> Result<(), BoxError> {
    let (_vault, server) = task_vault()?;
    let too_many: Vec<String> = (0..=100).map(|number| format!("A-{number:03}")).collect();
    for arguments in [
        json!({"path": "memory/tasks.base", "ids": ["A-001"], "view": "Open"}),
        json!({"path": "memory/tasks.base", "ids": ["A-001"], "limit": 1}),
        json!({"path": "memory/tasks.base", "ids": ["A-001"], "format": "json"}),
        json!({"definition": {"filters": "id == \"A-001\"", "order": ["id"]}, "ids": ["A-001"]}),
        json!({"path": "memory/tasks.base", "ids": []}),
        json!({"path": "memory/tasks.base", "ids": ["A-001", "A-001"]}),
        json!({"path": "memory/tasks.base", "ids": [""]}),
        json!({"path": "memory/tasks.base", "ids": too_many}),
        json!({"path": "memory/tasks.base", "include_content": true}),
    ] {
        let result = call(&server, "base_query", arguments.clone()).await?;
        assert_eq!(result.is_error, Some(true), "{arguments}");
        assert_eq!(error_code(&result)?, "io/invalid-argument", "{arguments}");
    }
    Ok(())
}

#[tokio::test]
async fn a_view_filtering_with_contains_any_runs_and_selects_what_holds_none_of_the_values() -> Result<(), BoxError> {
    let (_vault, server) = task_vault()?;
    let result = call(
        &server,
        "base_query",
        json!({"path": "memory/tasks.base", "view": "Open", "format": "json"}),
    )
    .await?;
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let body = structured(&result)?;
    let content = body.get("content").and_then(Value::as_str).ok_or("content missing")?;
    let rows: Vec<Value> = serde_json::from_str(content)?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "A-001");
    Ok(())
}

#[tokio::test]
async fn an_unsupported_filter_is_reported_even_when_no_note_would_reach_it() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(
        vault.path(),
        "empty.base",
        "filters: file.inFolder(\"nothing-here\")\nviews:\n  - type: table\n    name: Late\n    filters: due > \"2026-01-01\"\n    order:\n      - due\n",
    )?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    let result = call(&server, "base_query", json!({"path": "empty.base"})).await?;
    assert_eq!(result.is_error, Some(true), "{result:?}");
    assert_eq!(error_code(&result)?, "query/base-query-unsupported-filter");
    Ok(())
}
