//! Contract for the vault attribution and check kind every `doctor` finding carries, and for the vault selector
//! `doctor` accepts, so a client can show one vault's health without parsing prose.
mod support;
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

fn array<'a>(
    result: &'a CallToolResult,
    field: &str,
) -> Result<Vec<&'a Map<String, Value>>, Box<dyn std::error::Error + Send + Sync>> {
    structured(result)?
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| std::io::Error::other(format!("result omitted {field}")).into())
        .map(|items| items.iter().filter_map(Value::as_object).collect())
}

/// Two managed vaults named `alpha` and `beta`, neither with Git, so the checks are deterministic.
fn two_named_vaults()
-> Result<(ContextOsServer, tempfile::TempDir, tempfile::TempDir), Box<dyn std::error::Error + Send + Sync>> {
    let alpha = tempfile::Builder::new().prefix("alpha").tempdir()?;
    let beta = tempfile::Builder::new().prefix("beta").tempdir()?;
    let mut config = support::scoped_config(vec![alpha.path().to_path_buf(), beta.path().to_path_buf()])?;
    for (vault, name) in config.vaults.iter_mut().zip(["alpha", "beta"]) {
        vault.name = Some(name.to_owned());
        vault.git.enabled = false;
    }
    Ok((ContextOsServer::try_from(config)?, alpha, beta))
}

#[tokio::test]
async fn every_check_names_its_kind_and_the_vault_it_concerns() -> TestResult {
    let (server, _alpha, _beta) = two_named_vaults()?;

    let result = call_tool(server, "doctor", Map::new()).await?;

    assert_eq!(result.is_error, Some(false));
    let checks = array(&result, "checks")?;
    let configuration = checks
        .iter()
        .find(|check| check.get("kind") == Some(&json!("configuration")))
        .ok_or_else(|| std::io::Error::other("no configuration check reported"))?;
    assert_eq!(
        configuration.get("vault_name"),
        None,
        "the global check belongs to no vault"
    );
    for check in checks
        .iter()
        .filter(|check| check.get("kind") != Some(&json!("configuration")))
    {
        let vault = check.get("vault_name").and_then(Value::as_str);
        assert!(matches!(vault, Some("alpha" | "beta")), "unattributed check: {check:?}");
    }
    let alpha_kinds = checks
        .iter()
        .filter(|check| check.get("vault_name") == Some(&json!("alpha")))
        .filter_map(|check| check.get("kind").and_then(Value::as_str))
        .collect::<Vec<_>>();
    for expected in [
        "vault_access",
        "managed_indexes",
        "git_recovery",
        "uncommitted_changes",
        "semantic_search",
        "frontmatter",
        "duplicate_ids",
        "record_conformance",
        "team_config",
        "fence_vocabulary",
        "text_index_store",
        "link_graph_store",
        "vector_store",
    ] {
        assert!(alpha_kinds.contains(&expected), "missing {expected} in {alpha_kinds:?}");
    }
    Ok(())
}

#[tokio::test]
async fn path_limits_doctor_to_one_vault_and_the_global_check() -> TestResult {
    let (server, _alpha, _beta) = two_named_vaults()?;

    let result = call_tool(server, "doctor", serde_json::from_value(json!({"path": "beta"}))?).await?;

    assert_eq!(result.is_error, Some(false));
    let checks = array(&result, "checks")?;
    assert!(!checks.is_empty(), "`checks` should not be empty");
    for check in &checks {
        let vault = check.get("vault_name");
        assert!(
            vault.is_none() || vault == Some(&json!("beta")),
            "a check for another vault leaked: {check:?}"
        );
    }
    assert!(
        checks
            .iter()
            .any(|check| check.get("vault_name") == Some(&json!("beta")))
    );
    Ok(())
}

#[tokio::test]
async fn path_naming_no_configured_vault_is_rejected() -> TestResult {
    let (server, _alpha, _beta) = two_named_vaults()?;

    let result = call_tool(server, "doctor", serde_json::from_value(json!({"path": "gamma://."}))?).await?;

    assert_eq!(result.is_error, Some(true));
    assert!(structured(&result)?.get("code").and_then(Value::as_str).is_some());
    Ok(())
}

#[tokio::test]
async fn unknown_input_fields_are_rejected() -> TestResult {
    let (server, _alpha, _beta) = two_named_vaults()?;

    let outcome = call_tool(server, "doctor", serde_json::from_value(json!({"vault": "alpha"}))?).await;

    assert!(outcome.is_err() || outcome?.is_error == Some(true));
    Ok(())
}

#[tokio::test]
async fn doctor_resolve_outcomes_name_their_kind_and_vault() -> TestResult {
    let (server, _alpha, _beta) = two_named_vaults()?;

    let result = call_tool(
        server,
        "doctor_resolve",
        serde_json::from_value(json!({"path": "alpha", "dry_run": true}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    let outcomes = array(&result, "outcomes")?;
    assert!(
        !outcomes.is_empty(),
        "a vault without indexes has an auto-fixable finding"
    );
    for outcome in outcomes {
        assert_eq!(outcome.get("kind"), Some(&json!("managed_indexes")));
        assert_eq!(outcome.get("vault_name"), Some(&json!("alpha")));
        let path = outcome.get("vault_path").and_then(Value::as_str);
        assert!(path.is_some_and(|path| path.contains("alpha")), "vault_path: {path:?}");
        assert_eq!(outcome.get("vault"), None, "the old field name must be gone");
    }
    Ok(())
}

#[test]
fn catalogue_advertises_the_vault_selector_and_the_new_check_fields() -> TestResult {
    let catalogue = ContextOsServer::catalogue();
    let doctor = catalogue
        .get("doctor")
        .ok_or_else(|| std::io::Error::other("missing tool doctor"))?;

    let input = doctor
        .input_schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| std::io::Error::other("doctor omitted input properties"))?;
    assert!(input.contains_key("path"));
    assert_eq!(doctor.input_schema.get("additionalProperties"), Some(&json!(false)));

    let schema = serde_json::to_string(&doctor.output_schema)?;
    for field in ["kind", "vault_name", "managed_indexes", "team_config"] {
        assert!(schema.contains(field), "output schema omits {field}");
    }
    Ok(())
}
