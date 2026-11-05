//! Contract tests for the paging, sorting, relevance, and note type
//! controls on `query_text`, and the per-hit metadata it now returns.
//! Complements `query_contract.rs`, which covers the tool's original
//! behaviour and stays unchanged to prove that behaviour is preserved.

mod support;
use std::fs;
use std::time::{Duration, SystemTime};

use contextos_mcp::ContextOsServer;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, Value, json};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

async fn call_tool(
    server: ContextOsServer,
    name: &'static str,
    arguments: Map<String, Value>,
) -> Result<CallToolResult, Box<dyn std::error::Error + Send + Sync>> {
    let (server_transport, client_transport) = tokio::io::duplex(65536);
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

fn arguments(value: Value) -> Result<Map<String, Value>, Box<dyn std::error::Error + Send + Sync>> {
    Ok(serde_json::from_value(value)?)
}

fn hits(content: &Map<String, Value>) -> Result<&Vec<Value>, Box<dyn std::error::Error + Send + Sync>> {
    content
        .get("hits")
        .and_then(Value::as_array)
        .ok_or_else(|| std::io::Error::other("result omitted hits").into())
}

fn hit_paths(content: &Map<String, Value>) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    Ok(hits(content)?
        .iter()
        .filter_map(|hit| hit.get("path").and_then(Value::as_str).map(str::to_owned))
        .collect())
}

fn write_note(vault: &std::path::Path, relative: &str, content: &str, age_seconds: u64) -> TestResult {
    let absolute = vault.join(relative);
    if let Some(parent) = absolute.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&absolute, content)?;
    let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_770_000_000_u64.saturating_sub(age_seconds));
    fs::File::options()
        .write(true)
        .open(&absolute)?
        .set_modified(modified)?;
    Ok(())
}

/// A server over `vault` with managed `index.md` generation off, so only
/// the notes a test writes are searchable.
fn server_for(vault: &std::path::Path) -> Result<ContextOsServer, Box<dyn std::error::Error + Send + Sync>> {
    let mut config = support::scoped_config(vec![vault.to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;
    Ok(ContextOsServer::try_from(config)?)
}

/// Five notes with clearly different keyword density so the relevance
/// ranking has no ties: one (most relevant) to five (least), each carrying
/// a title, a type, and a description.
fn write_ranked_notes(vault: &std::path::Path) -> TestResult {
    let notes = [
        ("one", "task", "widget widget widget widget widget", 500),
        ("two", "note", "widget widget widget widget filler", 400),
        ("three", "task", "widget widget widget filler filler", 300),
        ("four", "note", "widget widget filler filler filler", 200),
        ("five", "task", "widget filler filler filler filler", 100),
    ];
    for (name, note_type, body, age) in notes {
        write_note(
            vault,
            &format!("notes/{name}.md"),
            &format!(
                "---\ntitle: {name}\ntype: {note_type}\nstatus: active\ndescription: About {name}\n\
                 updated: 2026-09-24\n---\n\n{body}\n"
            ),
            age,
        )?;
    }
    Ok(())
}

fn assert_stable_error(result: &CallToolResult, code: &str) -> TestResult {
    assert_eq!(result.is_error, Some(true));
    let content = structured(result)?;
    assert_eq!(content.get("code"), Some(&json!(code)));
    assert!(
        content
            .get("remediation")
            .and_then(Value::as_str)
            .is_some_and(|remediation| !remediation.is_empty()),
        "an error must carry a remediation hint"
    );
    Ok(())
}

fn assert_flat(schema: &Value, label: &str) {
    match schema {
        Value::Object(object) => {
            for forbidden in ["oneOf", "anyOf", "allOf", "$ref", "$defs"] {
                assert!(!object.contains_key(forbidden), "{label} must not use {forbidden}");
            }
            if let Some(kind) = object.get("type") {
                assert!(kind.is_string(), "{label} has a non-string type: {kind}");
                assert_ne!(kind, &json!("null"), "{label} must not use a null type");
            }
            for (key, child) in object {
                assert_flat(child, &format!("{label}.{key}"));
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                assert_flat(child, &format!("{label}[{index}]"));
            }
        }
        _ => {}
    }
}

#[test]
fn query_text_advertises_the_paging_sort_relevance_and_note_type_inputs_as_flat_schemas()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = ContextOsServer::catalogue();
    let tool = catalogue.get("query_text").ok_or("missing query_text")?;
    let schema = Value::Object((*tool.input_schema).clone());
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or("query_text omitted input properties")?;

    for (field, kind) in [
        ("offset", "integer"),
        ("sort", "string"),
        ("order", "string"),
        ("min_relevance", "number"),
        ("note_type", "string"),
    ] {
        assert_eq!(
            properties.get(field).and_then(|property| property.get("type")),
            Some(&json!(kind)),
            "{field} must advertise a plain {kind} type"
        );
    }
    assert_eq!(
        properties.get("sort").and_then(|property| property.get("enum")),
        Some(&json!(["relevance", "title", "modified"]))
    );
    assert_eq!(
        properties.get("order").and_then(|property| property.get("enum")),
        Some(&json!(["asc", "desc"]))
    );
    assert_eq!(schema.get("required"), Some(&json!(["query"])));
    assert_eq!(schema.get("additionalProperties"), Some(&json!(false)));
    assert_flat(&schema, "query_text input");
    Ok(())
}

#[tokio::test]
async fn query_text_serves_pages_of_one_ranking_and_reports_the_total_and_offset() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write_ranked_notes(vault.path())?;
    let server = server_for(vault.path())?;

    let everything = call_tool(
        server.clone(),
        "query_text",
        arguments(json!({"query": "widget", "limit": 10}))?,
    )
    .await?;
    let everything = structured(&everything)?;
    assert_eq!(everything.get("total"), Some(&json!(5)));
    assert_eq!(everything.get("offset"), Some(&json!(0)));
    let ranking = hit_paths(everything)?;
    assert_eq!(ranking.len(), 5);

    let page = call_tool(
        server,
        "query_text",
        arguments(json!({"query": "widget", "limit": 2, "offset": 2}))?,
    )
    .await?;
    let page = structured(&page)?;
    assert_eq!(page.get("total"), Some(&json!(5)), "total counts every match");
    assert_eq!(page.get("offset"), Some(&json!(2)));
    assert_eq!(hit_paths(page)?, ranking[2..4]);
    Ok(())
}

#[tokio::test]
async fn query_text_hits_carry_relevance_and_the_notes_frontmatter_metadata() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write_ranked_notes(vault.path())?;
    let server = server_for(vault.path())?;

    let result = call_tool(server, "query_text", arguments(json!({"query": "widget", "limit": 1}))?).await?;
    let content = structured(&result)?;

    assert_eq!(result.is_error, Some(false));
    let best = hits(content)?.first().ok_or("expected a hit")?;
    assert_eq!(best.get("path"), Some(&json!("notes/one.md")));
    assert_eq!(best.get("relevance"), Some(&json!(1.0)));
    assert!(best.get("score").is_some(), "the raw score is still reported");
    assert!(best.get("modified").is_some());
    let metadata = best
        .get("metadata")
        .and_then(Value::as_object)
        .ok_or("metadata missing")?;
    assert_eq!(metadata.get("description"), Some(&json!("About one")));
    assert_eq!(metadata.get("status"), Some(&json!("active")));
    assert_eq!(metadata.get("note_type"), Some(&json!("task")));
    assert_eq!(metadata.get("updated"), Some(&json!("2026-09-24")));
    assert!(
        !metadata.contains_key("summary"),
        "absent fields are omitted, never null"
    );
    assert!(!metadata.contains_key("excerpt"));
    Ok(())
}

#[tokio::test]
async fn query_text_sorts_by_title_and_order_reverses_it() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write_ranked_notes(vault.path())?;
    let server = server_for(vault.path())?;

    let ascending = call_tool(
        server.clone(),
        "query_text",
        arguments(json!({"query": "widget", "limit": 10, "sort": "title"}))?,
    )
    .await?;
    let titles: Vec<&str> = hits(structured(&ascending)?)?
        .iter()
        .filter_map(|hit| hit.get("title").and_then(Value::as_str))
        .collect();
    assert_eq!(titles, ["five", "four", "one", "three", "two"]);

    let descending = call_tool(
        server.clone(),
        "query_text",
        arguments(json!({"query": "widget", "limit": 10, "sort": "title", "order": "desc"}))?,
    )
    .await?;
    let titles: Vec<&str> = hits(structured(&descending)?)?
        .iter()
        .filter_map(|hit| hit.get("title").and_then(Value::as_str))
        .collect();
    assert_eq!(titles, ["two", "three", "one", "four", "five"]);

    let recent = call_tool(
        server,
        "query_text",
        arguments(json!({"query": "widget", "limit": 10, "sort": "modified"}))?,
    )
    .await?;
    assert_eq!(
        hit_paths(structured(&recent)?)?,
        [
            "notes/five.md",
            "notes/four.md",
            "notes/three.md",
            "notes/two.md",
            "notes/one.md"
        ],
        "most recently modified first"
    );
    Ok(())
}

#[tokio::test]
async fn query_text_note_type_and_minimum_relevance_narrow_the_results_and_the_total() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write_ranked_notes(vault.path())?;
    let server = server_for(vault.path())?;

    let tasks = call_tool(
        server.clone(),
        "query_text",
        arguments(json!({"query": "widget", "limit": 10, "note_type": "task"}))?,
    )
    .await?;
    let tasks = structured(&tasks)?;
    assert_eq!(tasks.get("total"), Some(&json!(3)));
    assert_eq!(hit_paths(tasks)?, ["notes/one.md", "notes/three.md", "notes/five.md"]);

    let everything = call_tool(
        server.clone(),
        "query_text",
        arguments(json!({"query": "widget", "limit": 10}))?,
    )
    .await?;
    let second = hits(structured(&everything)?)?
        .get(1)
        .and_then(|hit| hit.get("relevance"))
        .and_then(Value::as_f64)
        .ok_or("expected a second hit")?;
    let strong = call_tool(
        server,
        "query_text",
        arguments(json!({"query": "widget", "limit": 10, "min_relevance": second}))?,
    )
    .await?;
    let strong = structured(&strong)?;
    assert_eq!(strong.get("total"), Some(&json!(2)));
    assert_eq!(hit_paths(strong)?, ["notes/one.md", "notes/two.md"]);
    Ok(())
}

#[tokio::test]
async fn query_text_with_a_zero_limit_returns_only_the_exact_total() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write_ranked_notes(vault.path())?;
    let server = server_for(vault.path())?;

    let result = call_tool(server, "query_text", arguments(json!({"query": "widget", "limit": 0}))?).await?;
    let content = structured(&result)?;

    assert_eq!(result.is_error, Some(false));
    assert!(
        hits(content)?.is_empty(),
        "`hits(content)?` should be empty, got {:?}",
        hits(content)?
    );
    assert_eq!(content.get("total"), Some(&json!(5)));
    Ok(())
}

#[tokio::test]
async fn query_text_rejects_out_of_bounds_pages_and_relevance_with_the_stable_code() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write_ranked_notes(vault.path())?;
    let server = server_for(vault.path())?;

    for rejected in [
        json!({"query": "widget", "limit": 101}),
        json!({"query": "widget", "limit": 10, "offset": 191}),
        json!({"query": "widget", "limit": 0, "offset": 201}),
        json!({"query": "widget", "limit": 10, "offset": u64::MAX}),
        json!({"query": "widget", "min_relevance": 1.5}),
        json!({"query": "widget", "min_relevance": -0.1}),
    ] {
        let result = call_tool(server.clone(), "query_text", arguments(rejected.clone())?).await?;
        assert_stable_error(&result, "index/invalid-query")?;
    }
    Ok(())
}

#[tokio::test]
async fn query_text_accepts_the_largest_page_that_ends_exactly_at_the_window() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write_ranked_notes(vault.path())?;
    let server = server_for(vault.path())?;

    let result = call_tool(
        server,
        "query_text",
        arguments(json!({"query": "widget", "limit": 100, "offset": 100}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    assert!(
        hits(structured(&result)?)?.is_empty(),
        "`hits(structured(&result)?)?` should be empty, got {:?}",
        hits(structured(&result)?)?
    );
    assert_eq!(structured(&result)?.get("total"), Some(&json!(5)));
    Ok(())
}

#[tokio::test]
async fn query_text_rejects_an_unknown_sort_key_order_or_field() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write_ranked_notes(vault.path())?;
    let server = server_for(vault.path())?;

    for (rejected, expected_fragment) in [
        (json!({"query": "widget", "sort": "bogus"}), "bogus"),
        (json!({"query": "widget", "order": "sideways"}), "sideways"),
        (json!({"query": "widget", "page": 2}), "unknown field `page`"),
        (json!({"query": "widget", "offset": "two"}), "expected u64"),
    ] {
        let result = call_tool(server.clone(), "query_text", arguments(rejected)?).await?;
        assert_eq!(result.is_error, Some(true));
        let message = result
            .content
            .first()
            .and_then(rmcp::model::ContentBlock::as_text)
            .map(|content| content.text.as_str())
            .ok_or("error omitted text")?;
        assert!(message.contains(expected_fragment), "{message}");
    }
    Ok(())
}

#[test]
fn query_semantic_advertises_the_paging_controls_as_optional_flat_inputs() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = ContextOsServer::catalogue();
    let tool = catalogue.get("query_semantic").ok_or("missing query_semantic")?;
    assert!(
        tool.description
            .as_deref()
            .is_some_and(|text| text.contains("offset") && text.contains("200")),
        "the description must state the paging rule"
    );
    let schema = Value::Object((*tool.input_schema).clone());
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or("query_semantic omitted input properties")?;

    for (field, kind) in [
        ("offset", "integer"),
        ("sort", "string"),
        ("order", "string"),
        ("min_relevance", "number"),
        ("note_type", "string"),
    ] {
        assert_eq!(
            properties.get(field).and_then(|property| property.get("type")),
            Some(&json!(kind)),
            "{field} must advertise a plain {kind} type"
        );
    }
    assert_eq!(schema.get("required"), Some(&json!(["query"])));
    assert_eq!(schema.get("additionalProperties"), Some(&json!(false)));
    assert_flat(&schema, "query_semantic input");
    Ok(())
}

#[tokio::test]
async fn query_semantic_validates_the_page_and_relevance_before_it_checks_the_index_is_enabled() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    // Semantic search is off by default, so a valid request reports that.
    let server = server_for(vault.path())?;

    for rejected in [
        json!({"query": "widget", "limit": 101}),
        json!({"query": "widget", "limit": 10, "offset": 191}),
        json!({"query": "widget", "min_relevance": 1.5}),
    ] {
        let result = call_tool(server.clone(), "query_semantic", arguments(rejected)?).await?;
        assert_stable_error(&result, "index/invalid-query")?;
    }

    let valid = call_tool(server, "query_semantic", arguments(json!({"query": "widget"}))?).await?;
    assert_stable_error(&valid, "index/disabled")?;
    Ok(())
}
