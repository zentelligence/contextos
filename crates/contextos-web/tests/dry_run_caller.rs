//! `DryRunMcpCaller` against a real `contextos-mcp` stdio session
//! (`testing.md`'s "no mocking" standard for meaningful MCP semantics):
//! proves the live `tools/list` schema fetch and validation actually work
//! against a genuine server, not just the pure matching/validation logic
//! already covered by `src/dry_run_caller_test.rs`.

mod support;

use std::sync::Arc;

use contextos_web::dry_run_caller::DryRunMcpCaller;
use contextos_web::dry_run_scenario::{Scenario, ScenarioMatcher};
use contextos_web::mcp_client::McpClientSet;
use contextos_workflows::McpCaller;
use serde_json::{Map, Value, json};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

async fn connected_clients(dir: &std::path::Path) -> Result<Arc<McpClientSet>, BoxError> {
    let vault = dir.join("vault");
    std::fs::create_dir_all(&vault)?;
    let config_path = support::write_vault_config(dir, &vault)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    Ok(Arc::new(McpClientSet::connect(&[entry]).await?))
}

#[tokio::test]
async fn a_valid_mock_matching_the_real_tools_input_schema_is_returned() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let clients = connected_clients(dir.path()).await?;
    let scenario = Scenario::parse(
        r#"
            [[call]]
            server = "contextos"
            tool = "vault_info"
            kind = "valid"
            response = { ok = true }
        "#,
    )?;
    let caller = DryRunMcpCaller::new(clients, ScenarioMatcher::new(scenario));

    let result = caller.call("contextos", "vault_info", Map::new()).await;

    assert_eq!(result?, json!({"ok": true}));
    Ok(())
}

#[tokio::test]
async fn a_call_with_arguments_not_matching_the_real_tools_input_schema_is_rejected() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let clients = connected_clients(dir.path()).await?;
    let scenario = Scenario::parse(
        r#"
            [[call]]
            server = "contextos"
            tool = "fs_read_text_file"
            kind = "valid"
            response = { content = "hello" }
        "#,
    )?;
    let caller = DryRunMcpCaller::new(clients, ScenarioMatcher::new(scenario));

    // `fs_read_text_file` requires a `path` argument; the script here
    // supplies none at all, which must be caught against the tool's own
    // real, live-fetched schema before anything else happens.
    let result = caller.call("contextos", "fs_read_text_file", Map::new()).await;

    let Err(failure) = result else {
        return Err("missing required argument should fail dry-run validation".into());
    };
    assert!(failure.message.contains("input schema"), "{failure:?}");
    Ok(())
}

#[tokio::test]
async fn an_error_kind_call_fails_immediately_with_no_real_tool_touched() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let clients = connected_clients(dir.path()).await?;
    let scenario = Scenario::parse(
        r#"
            [[call]]
            server = "contextos"
            tool = "this_tool_does_not_exist_at_all"
            kind = "error"
            error = "simulated failure"
        "#,
    )?;
    let caller = DryRunMcpCaller::new(clients, ScenarioMatcher::new(scenario));

    // The tool name is deliberately nonexistent: an `error`-kind call must
    // never attempt a schema fetch at all, so this must fail with the
    // scenario's own declared message, not a "tool not found" message.
    let result = caller
        .call("contextos", "this_tool_does_not_exist_at_all", Map::new())
        .await;

    let Err(failure) = result else {
        return Err("error-kind call must fail".into());
    };
    assert_eq!(failure.message, "simulated failure");
    Ok(())
}

#[tokio::test]
async fn a_call_out_of_the_scenarios_declared_order_is_rejected_before_any_schema_fetch() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let clients = connected_clients(dir.path()).await?;
    let scenario = Scenario::parse(
        r#"
            [[call]]
            server = "contextos"
            tool = "vault_info"
            kind = "valid"
            response = { ok = true }
        "#,
    )?;
    let caller = DryRunMcpCaller::new(clients, ScenarioMatcher::new(scenario));

    let result = caller
        .call("contextos", "fs_list_allowed_directories", Map::new())
        .await;

    let Err(failure) = result else {
        return Err("out-of-order call must be rejected".into());
    };
    assert!(failure.message.contains("expected call 0"), "{failure:?}");
    Ok(())
}

#[tokio::test]
async fn an_invalid_kind_response_bypasses_output_schema_validation() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let clients = connected_clients(dir.path()).await?;
    // `vault_info` genuinely returns an object shaped nothing like this;
    // `kind = "invalid"` must return it verbatim regardless.
    let scenario = Scenario::parse(
        r#"
            [[call]]
            server = "contextos"
            tool = "vault_info"
            kind = "invalid"
            response = "not even an object"
        "#,
    )?;
    let caller = DryRunMcpCaller::new(clients, ScenarioMatcher::new(scenario));

    let result = caller.call("contextos", "vault_info", Map::new()).await;

    assert_eq!(result?, Value::String("not even an object".to_owned()));
    Ok(())
}
