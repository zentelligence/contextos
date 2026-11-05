//! `frontmatter_read` with `expand_links` returns a note's frontmatter with every link value rewritten in place to a
//! vault-rooted path by the same rule `fs_attach_file`'s `expand_links` applies, so a client reads a `base` property
//! or any other link field resolved exactly as the server resolves it. Like expanded attachments, it reports that
//! plainly and carries no `content_hash`, so expanded values can never be written back over the note.

mod support;

/// The most distinct link targets one expansion resolves, as `fs_expand` caps it.
const MAX_DISTINCT_LINK_TARGETS: usize = 10_000;
use contextos_mcp::ContextOsServer;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, Value, json};
use std::fmt::Write as _;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

async fn call_tool(
    server: ContextOsServer,
    name: &'static str,
    arguments: Value,
) -> Result<CallToolResult, Box<dyn std::error::Error + Send + Sync>> {
    let arguments: Map<String, Value> = serde_json::from_value(arguments)?;
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

fn server_for(vault: &tempfile::TempDir) -> Result<ContextOsServer, Box<dyn std::error::Error + Send + Sync>> {
    Ok(ContextOsServer::try_from(support::scoped_config(vec![
        vault.path().to_path_buf(),
    ])?)?)
}

fn write(vault: &tempfile::TempDir, relative: &str, content: &str) -> TestResult {
    let path = vault.path().join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    Ok(())
}

fn structured(result: &CallToolResult) -> Result<&Value, Box<dyn std::error::Error + Send + Sync>> {
    result
        .structured_content
        .as_ref()
        .filter(|value| value.is_object())
        .ok_or_else(|| std::io::Error::other("tool result omitted structured content").into())
}

const DECISION: &str =
    "---\nid: D-001\nbase: '[[decisions.base|Decisions]]'\nparent: '[[missing]]'\ntitle: Plain text\n---\nBody.\n";

fn decisions_vault() -> Result<tempfile::TempDir, Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    for base in [
        "memory/decisions.base",
        "work/alpha/decisions.base",
        "work/beta/decisions.base",
    ] {
        write(&vault, base, "views:\n  - type: table\n    name: All\n")?;
    }
    write(&vault, "work/alpha/decisions/d-001.md", DECISION)?;
    Ok(vault)
}

#[tokio::test]
async fn expanded_frontmatter_resolves_a_bare_base_link_to_the_nearest_base() -> TestResult {
    let vault = decisions_vault()?;

    let result = call_tool(
        server_for(&vault)?,
        "frontmatter_read",
        json!({ "path": "work/alpha/decisions/d-001.md", "expand_links": true }),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let body = structured(&result)?;
    assert_eq!(
        body.pointer("/frontmatter/base"),
        Some(&json!("[[/work/alpha/decisions.base|Decisions]]"))
    );
    assert_eq!(
        body.pointer("/frontmatter/parent"),
        Some(&json!("[[/:!missing|missing]]"))
    );
    assert_eq!(body.pointer("/frontmatter/title"), Some(&json!("Plain text")));
    assert_eq!(body.pointer("/frontmatter/id"), Some(&json!("D-001")));
    assert_eq!(body.get("links_expanded"), Some(&json!(true)));
    assert_eq!(
        std::fs::read_to_string(vault.path().join("work/alpha/decisions/d-001.md"))?,
        DECISION
    );
    Ok(())
}

#[tokio::test]
async fn expanded_frontmatter_carries_no_content_hash() -> TestResult {
    let vault = decisions_vault()?;

    let result = call_tool(
        server_for(&vault)?,
        "frontmatter_read",
        json!({ "path": "work/alpha/decisions/d-001.md", "expand_links": true }),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert!(structured(&result)?.get("content_hash").is_none());
    assert!(
        result
            .content
            .iter()
            .filter_map(rmcp::model::ContentBlock::as_text)
            .all(|block| !block.text.contains("content_hash")),
        "expanded frontmatter must not offer a hash that could authorise an overwrite"
    );
    Ok(())
}

#[tokio::test]
async fn without_expand_links_frontmatter_is_returned_as_written_with_its_hash() -> TestResult {
    let vault = decisions_vault()?;
    let server = server_for(&vault)?;

    for arguments in [
        json!({ "path": "work/alpha/decisions/d-001.md" }),
        json!({ "path": "work/alpha/decisions/d-001.md", "expand_links": false }),
    ] {
        let result = call_tool(server.clone(), "frontmatter_read", arguments).await?;
        let body = structured(&result)?;
        assert_eq!(
            body.pointer("/frontmatter/base"),
            Some(&json!("[[decisions.base|Decisions]]"))
        );
        assert!(body.get("content_hash").is_some());
        assert!(body.get("links_expanded").is_none());
    }
    Ok(())
}

#[tokio::test]
async fn expand_links_on_a_file_that_is_not_markdown_is_rejected_not_ignored() -> TestResult {
    let vault = decisions_vault()?;
    write(&vault, "plain.txt", "---\nparent: '[[Foo]]'\n---\n")?;

    let refused = call_tool(
        server_for(&vault)?,
        "frontmatter_read",
        json!({ "path": "plain.txt", "expand_links": true }),
    )
    .await?;

    assert_eq!(refused.is_error, Some(true));
    assert_eq!(structured(&refused)?.get("code"), Some(&json!("format/not-markdown")));
    Ok(())
}

#[tokio::test]
async fn only_the_frontmatter_is_expanded_so_a_body_too_full_of_links_to_expand_still_reads() -> TestResult {
    let vault = decisions_vault()?;
    let mut note = String::from("---\nbase: '[[decisions.base]]'\n---\n");
    for index in 0..=MAX_DISTINCT_LINK_TARGETS {
        writeln!(note, "[[missing-{index}]]")?;
    }
    write(&vault, "work/alpha/decisions/busy.md", &note)?;

    let result = call_tool(
        server_for(&vault)?,
        "frontmatter_read",
        json!({ "path": "work/alpha/decisions/busy.md", "expand_links": true }),
    )
    .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(
        structured(&result)?.pointer("/frontmatter/base"),
        Some(&json!("[[/work/alpha/decisions.base|decisions.base]]"))
    );
    assert_eq!(structured(&result)?.get("body_start_line"), Some(&json!(4)));
    Ok(())
}
