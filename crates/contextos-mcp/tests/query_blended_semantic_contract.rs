//! End-to-end contract tests for `query_semantic` and `query_blended` with
//! semantic search live, through the real OpenAI-compatible provider code
//! against a loopback embeddings stub (no network access). Covers the
//! response shape, fusion, degradation when the provider fails, and the
//! stable error in semantic-only mode.

mod support;
use std::fs;

use contextos_mcp::{ContextOsServer, EmbeddingConfig, EmbeddingProvider};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, Value, json};
use support::embedding_stub::EmbeddingStub;

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

fn titles(content: &Map<String, Value>) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    Ok(hits(content)?
        .iter()
        .filter_map(|hit| hit.get("title").and_then(Value::as_str).map(str::to_owned))
        .collect())
}

fn write_topics(vault: &std::path::Path) -> TestResult {
    fs::create_dir_all(vault.join("topics"))?;
    for (name, note_type) in [("alpha", "task"), ("bravo", "note"), ("charlie", "task")] {
        let title = format!("{}{}", name[..1].to_uppercase(), &name[1..]);
        fs::write(
            vault.join(format!("topics/{name}.md")),
            format!(
                "---\ntitle: {title}\ntype: {note_type}\ndescription: About {name}\n---\n\n{title} content paragraph.\n"
            ),
        )?;
    }
    Ok(())
}

/// A server whose semantic search talks to `stub`. The stub must already be
/// running: construction probes the provider for its vector dimension. The
/// key is written in the configuration; the loopback stub ignores it.
fn semantic_server(
    vault: &std::path::Path,
    stub: &EmbeddingStub,
    text_enabled: bool,
) -> Result<ContextOsServer, Box<dyn std::error::Error + Send + Sync>> {
    let mut config = support::scoped_config(vec![vault.to_path_buf()])?;
    let settings = &mut config.vaults[0];
    settings.index_md.enabled = false;
    settings.search.semantic = true;
    settings.search.text = text_enabled;
    settings.search.embedding = EmbeddingConfig {
        provider: EmbeddingProvider::OpenaiCompatible,
        model: "stub".to_owned(),
        endpoint: stub.endpoint(),
        api_key: "sk-stub".to_owned(),
        api_key_env: String::new(),
        model_directory: None,
    };
    Ok(ContextOsServer::try_from(config)?)
}

/// A vault of three notes, a running embeddings stub, and a server wired to
/// it with the semantic index built.
///
/// The provider talks HTTP through a blocking client, which Tokio forbids
/// creating or dropping inside an async context, so the server is built and
/// dropped here in plain synchronous code and each call runs on the
/// fixture's own runtime. `server` is declared before `runtime` so it drops
/// first.
struct Fixture {
    server: ContextOsServer,
    stub: EmbeddingStub,
    runtime: tokio::runtime::Runtime,
    _vault: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::with_text(true)
    }

    fn with_text(text_enabled: bool) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
        let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
        write_topics(vault.path())?;
        let stub = EmbeddingStub::start()?;
        let server = semantic_server(vault.path(), &stub, text_enabled)?;
        let fixture = Self {
            server,
            stub,
            runtime,
            _vault: vault,
        };
        let rebuilt = fixture.call("query_index_rebuild", json!({"index": "semantic"}))?;
        assert_eq!(rebuilt.is_error, Some(false), "semantic rebuild failed: {rebuilt:?}");
        Ok(fixture)
    }

    fn call(
        &self,
        name: &'static str,
        input: Value,
    ) -> Result<CallToolResult, Box<dyn std::error::Error + Send + Sync>> {
        self.runtime
            .block_on(call_tool(self.server.clone(), name, arguments(input)?))
    }
}

#[test]
fn query_semantic_returns_pages_with_totals_metadata_and_relevance() -> TestResult {
    let fixture = Fixture::new()?;

    let result = fixture.call(
        "query_semantic",
        json!({"query": "content paragraph", "limit": 2, "offset": 1}),
    )?;
    let content = structured(&result)?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(content.get("total"), Some(&json!(3)));
    assert_eq!(content.get("offset"), Some(&json!(1)));
    assert_eq!(content.get("total_capped"), Some(&json!(false)));
    let page = hits(content)?;
    assert_eq!(page.len(), 2);
    for hit in page {
        let relevance = hit
            .get("relevance")
            .and_then(Value::as_f64)
            .ok_or("relevance missing")?;
        assert!((0.0..=1.0).contains(&relevance));
        assert!(hit.get("score").is_some());
        assert!(
            hit.get("chunk")
                .and_then(Value::as_str)
                .is_some_and(|chunk| !chunk.is_empty())
        );
        assert!(hit.get("heading_context").is_some_and(Value::is_array));
        assert!(hit.get("title").and_then(Value::as_str).is_some());
        assert!(hit.get("modified").and_then(Value::as_str).is_some());
        assert!(
            hit.get("metadata")
                .and_then(|metadata| metadata.get("description"))
                .and_then(Value::as_str)
                .is_some_and(|description| description.starts_with("About "))
        );
    }
    Ok(())
}

#[test]
fn query_semantic_sorts_by_title_and_filters_by_note_type() -> TestResult {
    let fixture = Fixture::new()?;

    let by_title = fixture.call(
        "query_semantic",
        json!({"query": "content paragraph", "limit": 10, "sort": "title", "order": "desc"}),
    )?;
    assert_eq!(titles(structured(&by_title)?)?, ["Charlie", "Bravo", "Alpha"]);

    let tasks = fixture.call(
        "query_semantic",
        json!({"query": "content paragraph", "limit": 10, "note_type": "task", "sort": "title"}),
    )?;
    let tasks = structured(&tasks)?;
    assert_eq!(tasks.get("total"), Some(&json!(2)));
    assert_eq!(titles(tasks)?, ["Alpha", "Charlie"]);
    Ok(())
}

#[test]
fn blended_search_fuses_both_indexes_and_reports_which_matched_each_file() -> TestResult {
    let fixture = Fixture::new()?;

    let result = fixture.call("query_blended", json!({"query": "content", "limit": 10}))?;
    let content = structured(&result)?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(content.get("total"), Some(&json!(3)));
    assert_eq!(content.get("total_capped"), Some(&json!(false)));
    assert!(
        content
            .get("warnings")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    );
    assert!(content.get("index_freshness").is_some(), "the text index was consulted");
    let page = hits(content)?;
    assert_eq!(page.len(), 3);
    let mut previous = f64::INFINITY;
    for hit in page {
        assert_eq!(hit.get("matched_by"), Some(&json!(["text", "semantic"])));
        let relevance = hit
            .get("relevance")
            .and_then(Value::as_f64)
            .ok_or("relevance missing")?;
        assert!(relevance > 0.0 && relevance <= 1.0 + 1e-6);
        assert!(relevance <= previous, "hits must be ordered by relevance");
        previous = relevance;
    }
    Ok(())
}

#[test]
fn semantic_mode_returns_one_semantic_hit_per_file_and_skips_the_text_index() -> TestResult {
    let fixture = Fixture::new()?;

    let result = fixture.call(
        "query_blended",
        json!({"query": "unrelated words entirely", "mode": "semantic", "limit": 10}),
    )?;
    let content = structured(&result)?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(content.get("total"), Some(&json!(3)));
    assert!(
        content.get("index_freshness").is_none(),
        "the text index was not consulted"
    );
    for hit in hits(content)? {
        assert_eq!(hit.get("matched_by"), Some(&json!(["semantic"])));
    }
    Ok(())
}

#[test]
fn a_failing_embedding_provider_degrades_blended_search_but_fails_semantic_mode() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.stub.start_failing();

    let degraded = fixture.call("query_blended", json!({"query": "content", "limit": 10}))?;
    let content = structured(&degraded)?;
    assert_eq!(
        degraded.is_error,
        Some(false),
        "a provider failure must not fail blended search"
    );
    assert_eq!(content.get("total"), Some(&json!(3)));
    let warnings = content
        .get("warnings")
        .and_then(Value::as_array)
        .ok_or("warnings missing")?;
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0]
            .as_str()
            .is_some_and(|warning| warning.starts_with("embedding/provider-error: ")),
        "{warnings:?}"
    );
    for hit in hits(content)? {
        assert_eq!(hit.get("matched_by"), Some(&json!(["text"])));
    }

    let strict = fixture.call("query_blended", json!({"query": "content", "mode": "semantic"}))?;
    assert_eq!(strict.is_error, Some(true));
    assert_eq!(
        structured(&strict)?.get("code"),
        Some(&json!("embedding/provider-error"))
    );
    Ok(())
}

#[test]
fn count_only_pages_return_the_total_without_hits_for_semantic_and_blended_search() -> TestResult {
    let fixture = Fixture::new()?;

    for (tool, request) in [
        ("query_semantic", json!({"query": "content paragraph", "limit": 0})),
        ("query_blended", json!({"query": "content", "limit": 0})),
    ] {
        let result = fixture.call(tool, request)?;
        let content = structured(&result)?;
        assert_eq!(result.is_error, Some(false), "{tool}");
        assert!(hits(content)?.is_empty(), "{tool}");
        assert_eq!(content.get("total"), Some(&json!(3)), "{tool}");
    }
    Ok(())
}

#[test]
fn blended_search_with_the_text_index_off_returns_semantic_results_and_a_warning() -> TestResult {
    let fixture = Fixture::with_text(false)?;

    let result = fixture.call("query_blended", json!({"query": "content", "limit": 10}))?;
    let content = structured(&result)?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(content.get("total"), Some(&json!(3)));
    assert!(
        content.get("index_freshness").is_none(),
        "the text index was not consulted"
    );
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
    for hit in hits(content)? {
        assert_eq!(hit.get("matched_by"), Some(&json!(["semantic"])));
    }
    Ok(())
}

#[test]
fn an_unparsable_full_text_query_still_gets_semantic_results_and_a_warning() -> TestResult {
    let fixture = Fixture::new()?;

    let result = fixture.call(
        "query_blended",
        json!({"query": "why did we pick sqlite: notes (", "limit": 10}),
    )?;
    let content = structured(&result)?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(content.get("total"), Some(&json!(3)));
    let warnings = content
        .get("warnings")
        .and_then(Value::as_array)
        .ok_or("warnings missing")?;
    assert!(
        warnings
            .first()
            .and_then(Value::as_str)
            .is_some_and(|warning| warning.starts_with("index/invalid-query: ")),
        "{warnings:?}"
    );
    Ok(())
}

#[test]
fn with_text_off_and_the_provider_failing_search_reports_one_generic_unavailable_error() -> TestResult {
    let fixture = Fixture::with_text(false)?;
    fixture.stub.start_failing();

    let result = fixture.call("query_blended", json!({"query": "content"}))?;

    assert_eq!(result.is_error, Some(true));
    let content = structured(&result)?;
    assert_eq!(content.get("code"), Some(&json!("index/disabled")));
    assert!(
        content
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("embedding/provider-error")),
        "the underlying cause stays in the message"
    );
    assert!(
        content
            .get("remediation")
            .and_then(Value::as_str)
            .is_some_and(|hint| hint.contains("health")),
        "the remediation points at the health check"
    );
    Ok(())
}
