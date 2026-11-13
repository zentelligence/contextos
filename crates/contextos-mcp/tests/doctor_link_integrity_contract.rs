//! `doctor`'s `Link integrity` check through the tool: passing with the number of links checked, failing with counts
//! and a capped listing, never auto-fixable, and left alone by `doctor_resolve`.

mod support;

use contextos_mcp::ContextOsServer;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, Value, json};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

async fn call_tool(server: ContextOsServer, name: &'static str, arguments: Value) -> Result<CallToolResult, TestError> {
    let arguments: Map<String, Value> = serde_json::from_value(arguments)?;
    let (server_transport, client_transport) = tokio::io::duplex(65536);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), TestError>(())
    });
    let mut client = ().serve(client_transport).await?;
    let result = client
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments))
        .await;
    client.close().await?;
    server_handle.await??;
    Ok(result?)
}

fn server_over(vault: &tempfile::TempDir) -> Result<ContextOsServer, TestError> {
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.enabled = false;
    config.vaults[0].index_md.enabled = false;
    Ok(ContextOsServer::try_from(config)?)
}

fn link_check(result: &CallToolResult) -> Result<Map<String, Value>, TestError> {
    result
        .structured_content
        .as_ref()
        .and_then(|content| content.get("report").or(Some(content)))
        .and_then(|content| content.get("checks"))
        .and_then(Value::as_array)
        .and_then(|checks| {
            checks
                .iter()
                .find(|check| check.get("subject") == Some(&json!("Link integrity")))
        })
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| std::io::Error::other("no Link integrity check reported").into())
}

fn message(check: &Map<String, Value>) -> &str {
    check.get("message").and_then(Value::as_str).unwrap_or_default()
}

#[tokio::test]
async fn a_vault_whose_links_resolve_passes_with_the_count_of_links_checked() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("a.md"), "[[b]] and [b](b.md)\n")?;
    std::fs::write(vault.path().join("b.md"), "x\n")?;

    let result = call_tool(server_over(&vault)?, "doctor", json!({})).await?;

    let check = link_check(&result)?;
    assert_eq!(check.get("status"), Some(&json!("pass")));
    assert_eq!(message(&check), "2 links checked");
    Ok(())
}

#[tokio::test]
async fn broken_links_fail_never_auto_fixable_with_the_remediation_and_the_first_listed() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("a.md"), "ok\n[[gone]]\n")?;

    let result = call_tool(server_over(&vault)?, "doctor", json!({})).await?;

    let check = link_check(&result)?;
    assert_eq!(check.get("status"), Some(&json!("fail")));
    assert_eq!(check.get("auto_fixable"), Some(&json!(false)));
    assert_eq!(check.get("remediation_tool"), None);
    assert!(message(&check).contains("a.md:2 -> gone (not-found)"), "{check:?}");
    let action = check.get("action").and_then(Value::as_str).unwrap_or_default();
    assert!(
        action.contains("query_graph") && action.contains("unresolved"),
        "{check:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_long_list_of_broken_links_is_capped_and_the_rest_counted() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut body = String::new();
    for index in 0..14 {
        std::fmt::Write::write_fmt(&mut body, format_args!("[[gone-{index:02}]]\n"))?;
    }
    std::fs::write(vault.path().join("a.md"), body)?;

    let result = call_tool(server_over(&vault)?, "doctor", json!({})).await?;

    let check = link_check(&result)?;
    assert!(message(&check).contains("14 not-found link(s)"), "{check:?}");
    assert!(message(&check).contains("and 4 more"), "{check:?}");
    assert!(!message(&check).contains("gone-10"), "{check:?}");
    Ok(())
}

#[tokio::test]
async fn a_link_broken_by_an_edit_outside_the_server_is_seen_by_the_next_doctor() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("a.md"), "[[b]]\n")?;
    std::fs::write(vault.path().join("b.md"), "x\n")?;
    let server = server_over(&vault)?;
    let before = call_tool(server.clone(), "doctor", json!({})).await?;
    assert_eq!(link_check(&before)?.get("status"), Some(&json!("pass")));

    std::fs::remove_file(vault.path().join("b.md"))?;

    let after = call_tool(server, "doctor", json!({})).await?;
    assert_eq!(link_check(&after)?.get("status"), Some(&json!("fail")));
    Ok(())
}

#[tokio::test]
async fn doctor_resolve_leaves_a_link_integrity_failure_alone() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("a.md"), "[[gone]]\n")?;

    let result = call_tool(server_over(&vault)?, "doctor_resolve", json!({})).await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let content = result
        .structured_content
        .as_ref()
        .ok_or_else(|| std::io::Error::other("doctor_resolve omitted structured content"))?;
    let touched_links = content
        .get("outcomes")
        .and_then(Value::as_array)
        .is_some_and(|outcomes| {
            outcomes
                .iter()
                .any(|outcome| outcome.to_string().contains("Link integrity"))
        });
    assert!(!touched_links, "{content}");
    assert_eq!(link_check(&result)?.get("status"), Some(&json!("fail")));
    assert_eq!(std::fs::read_to_string(vault.path().join("a.md"))?, "[[gone]]\n");
    Ok(())
}

#[tokio::test]
async fn a_vault_with_the_link_graph_off_passes_saying_it_was_not_checked() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("a.md"), "[[gone]]\n")?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.enabled = false;
    config.vaults[0].index_md.enabled = false;
    config.vaults[0].search.graph = false;

    let result = call_tool(ContextOsServer::try_from(config)?, "doctor", json!({})).await?;

    let check = link_check(&result)?;
    assert_eq!(check.get("status"), Some(&json!("pass")));
    assert!(message(&check).starts_with("not checked"), "{check:?}");
    Ok(())
}

fn server_ignoring_targets_under(vault: &tempfile::TempDir, folders: &[&str]) -> Result<ContextOsServer, TestError> {
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.enabled = false;
    config.vaults[0].index_md.enabled = false;
    config.vaults[0].links.ignore_targets = folders.iter().map(|folder| (*folder).to_owned()).collect();
    Ok(ContextOsServer::try_from(config)?)
}

#[tokio::test]
async fn links_into_ignored_target_folders_pass_and_are_counted_apart_from_the_links_checked() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("inbox"))?;
    std::fs::write(vault.path().join("a.md"), "[[b]]\n[[inbox/gone]]\n[[kept]]\n")?;
    std::fs::write(vault.path().join("b.md"), "x\n")?;
    std::fs::write(vault.path().join("inbox/kept.md"), "x\n")?;

    let result = call_tool(server_ignoring_targets_under(&vault, &["inbox"])?, "doctor", json!({})).await?;

    let check = link_check(&result)?;
    assert_eq!(check.get("status"), Some(&json!("pass")), "{check:?}");
    assert_eq!(
        message(&check),
        "1 links checked, and 2 links into the folders [vault.links] ignore_targets names not checked"
    );
    Ok(())
}

#[tokio::test]
async fn a_failing_check_still_says_how_many_links_into_ignored_target_folders_it_left_unchecked() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("a.md"), "[[gone]]\n[[inbox/gone]]\n")?;

    let result = call_tool(server_ignoring_targets_under(&vault, &["inbox"])?, "doctor", json!({})).await?;

    let check = link_check(&result)?;
    assert_eq!(check.get("status"), Some(&json!("fail")), "{check:?}");
    assert!(
        message(&check).starts_with("1 not-found link(s), 0 ambiguous link(s)"),
        "{}",
        message(&check)
    );
    assert!(
        message(&check).ends_with(" 1 link(s) into the folders [vault.links] ignore_targets names were not checked."),
        "{}",
        message(&check)
    );
    Ok(())
}
