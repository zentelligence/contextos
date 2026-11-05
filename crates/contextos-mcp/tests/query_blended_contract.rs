//! Contract tests for the `query_blended` tool that do not need a working
//! embedding provider: its advertised schemas, input validation, stable
//! errors, and behaviour when semantic search is off. The fused behaviour
//! with a live provider is covered in `query_blended_semantic_contract.rs`.

mod support;
use std::fs;

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

fn server_for(
    vault: &std::path::Path,
    text_enabled: bool,
) -> Result<ContextOsServer, Box<dyn std::error::Error + Send + Sync>> {
    let mut config = support::scoped_config(vec![vault.to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;
    config.vaults[0].search.text = text_enabled;
    Ok(ContextOsServer::try_from(config)?)
}

fn write_notes(vault: &std::path::Path) -> TestResult {
    fs::create_dir_all(vault.join("notes"))?;
    for (name, body) in [
        ("one", "widget widget widget"),
        ("two", "widget widget filler"),
        ("three", "widget filler filler"),
    ] {
        fs::write(
            vault.join(format!("notes/{name}.md")),
            format!("---\ntitle: {name}\ntype: task\ndescription: About {name}\n---\n\n{body}\n"),
        )?;
    }
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
fn query_blended_is_advertised_with_a_flat_input_schema_and_a_flat_fallible_output_schema()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = ContextOsServer::catalogue();
    let tool = catalogue.get("query_blended").ok_or("missing query_blended")?;
    assert!(
        tool.description
            .as_deref()
            .is_some_and(|text| text.contains("semantic"))
    );

    let input = Value::Object((*tool.input_schema).clone());
    let properties = input
        .get("properties")
        .and_then(Value::as_object)
        .ok_or("input omitted properties")?;
    let mut names: Vec<&str> = properties.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "exclude_paths",
            "limit",
            "min_relevance",
            "mode",
            "note_type",
            "offset",
            "order",
            "path_prefix",
            "query",
            "sort",
            "vault"
        ],
        "tags and fields are text-index filters and deliberately absent"
    );
    assert_eq!(
        properties.get("mode").and_then(|property| property.get("enum")),
        Some(&json!(["blended", "text", "semantic"]))
    );
    assert_eq!(input.get("required"), Some(&json!(["query"])));
    assert_eq!(input.get("additionalProperties"), Some(&json!(false)));
    assert_flat(&input, "query_blended input");

    let output = Value::Object(
        (**tool
            .output_schema
            .as_ref()
            .ok_or("query_blended omitted its output schema")?)
        .clone(),
    );
    let output_properties = output
        .get("properties")
        .and_then(Value::as_object)
        .ok_or("output omitted properties")?;
    let mut output_names: Vec<&str> = output_properties.keys().map(String::as_str).collect();
    output_names.sort_unstable();
    assert_eq!(
        output_names,
        [
            "code",
            "hits",
            "index_freshness",
            "message",
            "offset",
            "path",
            "remediation",
            "total",
            "total_capped",
            "warnings"
        ]
    );
    assert!(
        output.get("required").is_none(),
        "a fallible output schema has no required array"
    );
    Ok(())
}

#[tokio::test]
async fn blended_search_with_semantic_off_returns_text_results_and_a_code_prefixed_warning() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write_notes(vault.path())?;
    let server = server_for(vault.path(), true)?;

    let result = call_tool(
        server,
        "query_blended",
        arguments(json!({"query": "widget", "limit": 10}))?,
    )
    .await?;
    let content = structured(&result)?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(content.get("total"), Some(&json!(3)));
    assert_eq!(content.get("offset"), Some(&json!(0)));
    assert_eq!(content.get("total_capped"), Some(&json!(false)));
    let warnings = content
        .get("warnings")
        .and_then(Value::as_array)
        .ok_or("warnings missing")?;
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0]
            .as_str()
            .is_some_and(|warning| warning.starts_with("index/disabled: ")),
        "{warnings:?}"
    );
    let best = hits(content)?.first().ok_or("expected hits")?;
    assert_eq!(best.get("path"), Some(&json!("notes/one.md")));
    assert_eq!(best.get("title"), Some(&json!("one")));
    assert_eq!(best.get("matched_by"), Some(&json!(["text"])));
    assert_eq!(best.get("relevance"), Some(&json!(1.0)));
    assert!(
        best.get("snippet")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty())
    );
    assert_eq!(
        best.get("metadata").and_then(|metadata| metadata.get("description")),
        Some(&json!("About one"))
    );
    assert!(content.get("index_freshness").is_some());
    Ok(())
}

#[tokio::test]
async fn text_mode_returns_no_warning_and_honours_paging_sorting_and_note_type() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write_notes(vault.path())?;
    let server = server_for(vault.path(), true)?;

    let result = call_tool(
        server,
        "query_blended",
        arguments(json!({
            "query": "widget", "mode": "text", "limit": 2, "offset": 1,
            "sort": "title", "note_type": "task"
        }))?,
    )
    .await?;
    let content = structured(&result)?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(content.get("total"), Some(&json!(3)));
    assert_eq!(content.get("offset"), Some(&json!(1)));
    assert!(
        content
            .get("warnings")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    );
    let titles: Vec<&str> = hits(content)?
        .iter()
        .filter_map(|hit| hit.get("title").and_then(Value::as_str))
        .collect();
    assert_eq!(
        titles,
        ["three", "two"],
        "title order one, three, two; offset 1 skips one"
    );
    Ok(())
}

#[tokio::test]
async fn semantic_mode_without_semantic_search_fails_with_the_stable_disabled_error() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write_notes(vault.path())?;
    let server = server_for(vault.path(), true)?;

    let result = call_tool(
        server,
        "query_blended",
        arguments(json!({"query": "widget", "mode": "semantic"}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(true));
    let content = structured(&result)?;
    assert_eq!(content.get("code"), Some(&json!("index/disabled")));
    assert!(
        content
            .get("remediation")
            .and_then(Value::as_str)
            .is_some_and(|hint| !hint.is_empty())
    );
    Ok(())
}

#[tokio::test]
async fn with_neither_index_enabled_the_call_fails_with_the_stable_disabled_error() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = server_for(vault.path(), false)?;

    let result = call_tool(server, "query_blended", arguments(json!({"query": "widget"}))?).await?;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(structured(&result)?.get("code"), Some(&json!("index/disabled")));
    Ok(())
}

#[tokio::test]
async fn out_of_bounds_pages_and_relevance_are_rejected_with_the_stable_invalid_query_code() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write_notes(vault.path())?;
    let server = server_for(vault.path(), true)?;

    for rejected in [
        json!({"query": "widget", "limit": 101}),
        json!({"query": "widget", "limit": 10, "offset": 191}),
        json!({"query": "widget", "min_relevance": 2.0}),
    ] {
        let result = call_tool(server.clone(), "query_blended", arguments(rejected)?).await?;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(structured(&result)?.get("code"), Some(&json!("index/invalid-query")));
    }

    let bad_syntax = call_tool(server, "query_blended", arguments(json!({"query": "title:("}))?).await?;
    assert_eq!(bad_syntax.is_error, Some(true));
    assert_eq!(
        structured(&bad_syntax)?.get("code"),
        Some(&json!("index/invalid-query"))
    );
    Ok(())
}

#[tokio::test]
async fn unknown_fields_including_the_text_only_filters_and_unknown_modes_are_rejected() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = server_for(vault.path(), true)?;

    for (rejected, expected_fragment) in [
        (json!({"query": "widget", "tags": ["a"]}), "unknown field `tags`"),
        (
            json!({"query": "widget", "fields": {"a": "b"}}),
            "unknown field `fields`",
        ),
        (json!({"query": "widget", "mode": "hybrid"}), "hybrid"),
        (json!({}), "missing field `query`"),
    ] {
        let result = call_tool(server.clone(), "query_blended", arguments(rejected)?).await?;
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

#[tokio::test]
async fn path_prefix_and_exclude_paths_scope_blended_results() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    for folder in ["keep", "drop"] {
        fs::create_dir_all(vault.path().join(folder))?;
        fs::write(
            vault.path().join(format!("{folder}/note.md")),
            "shared words appear here\n",
        )?;
    }
    let server = server_for(vault.path(), true)?;

    for scope in [json!({"path_prefix": "keep"}), json!({"exclude_paths": ["drop"]})] {
        let mut request = json!({"query": "shared", "mode": "text"});
        request
            .as_object_mut()
            .ok_or("request must be an object")?
            .extend(scope.as_object().cloned().unwrap_or_default());
        let result = call_tool(server.clone(), "query_blended", arguments(request)?).await?;
        let content = structured(&result)?;
        let found: Vec<&str> = hits(content)?
            .iter()
            .filter_map(|hit| hit.get("path").and_then(Value::as_str))
            .collect();
        assert_eq!(found, ["keep/note.md"], "{scope}");
    }
    Ok(())
}
