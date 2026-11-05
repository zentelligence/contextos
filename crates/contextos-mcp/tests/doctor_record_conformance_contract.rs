//! The `record_conformance` doctor check over the MCP adapter: it replaces `base_property`, reports damaged records
//! and base properties without ever repairing them, and reports identically over stdio and streamable HTTP.

mod support;
use contextos_mcp::{ContextOsServer, HttpConfig};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Map, Value, json};
use support::templated_vault::{BoxError, STANDARD_DECLARATION, call_tool, standard_files, structured, vault_with};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

type TestResult = Result<(), BoxError>;

/// The standard task vault plus a record typed `note`, another matching its template, a note whose `base` names a base
/// that does not select it, and a note with no base at all.
fn damaged_files() -> Vec<(String, String)> {
    let mut files = standard_files(STANDARD_DECLARATION);
    files.push((
        "tasks/a-001.md".to_owned(),
        "---\nid: A-001\nbase: '[[tasks.base]]'\ntype: note\n---\nBody\n".to_owned(),
    ));
    files.push((
        "tasks/a-002.md".to_owned(),
        "---\nid: A-002\nbase: '[[tasks.base]]'\ntype: task\ntask_type: action\nlevel: 1\n---\nBody\n".to_owned(),
    ));
    files.push((
        "meridian/registry/d-1.md".to_owned(),
        "---\nid: D-1\nbase: '[[meridian/decisions.base]]'\n---\nBody\n".to_owned(),
    ));
    files.push((
        "meridian/decisions.base".to_owned(),
        "filters: file.inFolder(\"meridian/decisions\")\n".to_owned(),
    ));
    files.push((
        "meridian/registry.base".to_owned(),
        "filters: file.inFolder(\"meridian/registry\")\n".to_owned(),
    ));
    files
}

fn check_of<'a>(report: &'a Map<String, Value>, kind: &str) -> Option<&'a Map<String, Value>> {
    report
        .get("checks")?
        .as_array()?
        .iter()
        .filter_map(Value::as_object)
        .find(|check| check.get("kind") == Some(&json!(kind)))
}

fn kinds(report: &Map<String, Value>) -> Vec<String> {
    report
        .get("checks")
        .and_then(Value::as_array)
        .map(|checks| {
            checks
                .iter()
                .filter_map(|check| check.get("kind").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn record_conformance_reports_the_damage_and_is_never_auto_fixable() -> TestResult {
    let (_vault, server) = vault_with(&damaged_files(), None)?;

    let result = call_tool(&server, "doctor", json!({})).await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let report = structured(&result)?;
    let check = check_of(report, "record_conformance").ok_or("no record_conformance check")?;
    assert_eq!(check.get("status"), Some(&json!("fail")));
    assert_eq!(check.get("auto_fixable"), Some(&json!(false)));
    assert_eq!(check.get("remediation_tool"), None);
    assert_eq!(check.get("subject"), Some(&json!("Record conformance")));
    let message = check.get("message").and_then(Value::as_str).unwrap_or_default();
    assert!(message.contains("tasks/a-001.md"), "{message}");
    assert!(!message.contains("tasks/a-002.md"), "{message}");
    assert!(
        message.contains("meridian/registry.base is the one base that selects it"),
        "{message}"
    );
    Ok(())
}

#[tokio::test]
async fn record_conformance_is_never_auto_fixable_and_doctor_resolve_changes_no_note() -> TestResult {
    let (vault, server) = vault_with(&damaged_files(), None)?;
    let watched = [
        "tasks/a-001.md",
        "tasks/a-002.md",
        "meridian/registry/d-1.md",
        "tasks.md",
    ];
    let read = || -> Result<Vec<String>, std::io::Error> {
        watched
            .iter()
            .map(|path| std::fs::read_to_string(vault.path().join(path)))
            .collect()
    };
    let before = read()?;

    let result = call_tool(&server, "doctor_resolve", json!({})).await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let body = structured(&result)?;
    let outcomes = body
        .get("outcomes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        outcomes
            .iter()
            .all(|outcome| outcome.get("subject") != Some(&json!("Record conformance"))),
        "doctor_resolve must not act on the check: {outcomes:?}"
    );
    let report = body
        .get("report")
        .and_then(Value::as_object)
        .ok_or("doctor_resolve omitted its report")?;
    let check = check_of(report, "record_conformance").ok_or("the report omitted record_conformance")?;
    assert_eq!(check.get("status"), Some(&json!("fail")));
    assert_eq!(read()?, before, "no note or sidecar changed");
    Ok(())
}

#[tokio::test]
async fn the_doctor_schema_advertises_record_conformance_and_no_longer_base_property() -> TestResult {
    let (_vault, server) = vault_with(&standard_files(STANDARD_DECLARATION), None)?;

    let result = call_tool(&server, "doctor", json!({})).await?;
    let report = structured(&result)?;
    let reported = kinds(report);
    assert!(reported.iter().any(|kind| kind == "record_conformance"), "{reported:?}");
    assert!(
        !reported.iter().any(|kind| kind == "base_property"),
        "base_property must no longer appear: {reported:?}"
    );

    let doctor = ContextOsServer::catalogue()
        .get("doctor")
        .ok_or("doctor is not advertised")?
        .clone();
    let advertised = serde_json::to_string(&doctor)?;
    assert!(!advertised.contains("base_property"), "{advertised}");
    Ok(())
}

async fn call_over_http(
    url: &str,
    token: &str,
    name: &'static str,
    arguments: Value,
) -> Result<CallToolResult, BoxError> {
    let arguments: Map<String, Value> = serde_json::from_value(arguments)?;
    let config = StreamableHttpClientTransportConfig::with_uri(url.to_owned()).auth_header(token.to_owned());
    let client = ().serve(StreamableHttpClientTransport::from_config(config)).await?;
    let result = client
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments))
        .await;
    client.cancel().await?;
    Ok(result?)
}

#[tokio::test]
async fn record_conformance_is_identical_over_stdio_and_http() -> TestResult {
    let (_vault, server) = vault_with(&damaged_files(), None)?;
    let token = "record-conformance-parity-token";
    let http = HttpConfig {
        bind: "127.0.0.1:0".to_owned(),
        token: token.to_owned(),
        token_env: String::new(),
        max_body_kb: 2048,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}{}", listener.local_addr()?, contextos_mcp::HTTP_MOUNT_PATH);
    let router = contextos_mcp::build_router(server.clone(), &http)?;
    let shutdown = CancellationToken::new();
    let stop = shutdown.clone();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { stop.cancelled().await })
            .await;
    });

    let over_stdio = call_tool(&server, "doctor", json!({})).await?;
    let over_http = call_over_http(&url, token, "doctor", json!({})).await?;
    shutdown.cancel();
    handle.await?;

    let stdio_check = check_of(structured(&over_stdio)?, "record_conformance")
        .ok_or("stdio omitted record_conformance")?
        .clone();
    let http_check = check_of(structured(&over_http)?, "record_conformance")
        .ok_or("http omitted record_conformance")?
        .clone();
    assert_eq!(stdio_check, http_check);
    assert_eq!(stdio_check.get("status"), Some(&json!("fail")));
    Ok(())
}
