mod support;
use contextos_mcp::ContextOsServer;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, json};

async fn call_tool(
    server: ContextOsServer,
    name: &'static str,
    arguments: Map<String, serde_json::Value>,
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

fn structured(
    result: &CallToolResult,
) -> Result<&serde_json::Map<String, serde_json::Value>, Box<dyn std::error::Error + Send + Sync>> {
    result
        .structured_content
        .as_ref()
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| std::io::Error::other("tool result omitted structured content").into())
}

fn outcomes(
    result: &CallToolResult,
) -> Result<Vec<&serde_json::Map<String, serde_json::Value>>, Box<dyn std::error::Error + Send + Sync>> {
    structured(result)?
        .get("outcomes")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("doctor_resolve result omitted outcomes").into())
        .map(|outcomes| outcomes.iter().filter_map(serde_json::Value::as_object).collect())
}

#[tokio::test]
async fn catalogue_advertises_doctor_resolve_with_an_object_schema()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let catalogue = ContextOsServer::catalogue();

    let tool = catalogue
        .get("doctor_resolve")
        .ok_or_else(|| std::io::Error::other("missing tool doctor_resolve"))?;
    assert_eq!(
        tool.input_schema.get("type").and_then(serde_json::Value::as_str),
        Some("object")
    );
    assert!(tool.description.is_some());
    let schema = tool
        .output_schema
        .as_ref()
        .ok_or_else(|| std::io::Error::other("doctor_resolve omitted output schema"))?;
    let properties = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| std::io::Error::other("doctor_resolve omitted output properties"))?;
    let mut fields = properties.keys().map(String::as_str).collect::<Vec<_>>();
    fields.sort_unstable();
    // `doctor_resolve`'s output schema is the flat, merged success-and-
    // `ToolFailure` shape `fallible_output_schema_for` builds
    // (`tool_contract.rs`'s
    // `every_remaining_tool_advertises_a_flat_fallible_output_schema`
    // covers this generally): `code`/`message`/`path`/`remediation` are
    // `ToolFailure`'s fields, present because `doctor_resolve`'s error
    // path populates `structured_content` with that shape too.
    assert_eq!(
        fields,
        vec!["code", "message", "outcomes", "path", "remediation", "report"]
    );

    Ok(())
}

#[tokio::test]
async fn doctor_resolve_rebuilds_a_stale_index_and_reports_it_resolved()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.enabled = false;
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(server, "doctor_resolve", Map::new()).await?;

    assert_eq!(result.is_error, Some(false));
    assert!(vault.path().join("index.md").exists());
    let resolved = outcomes(&result)?
        .into_iter()
        .find(|outcome| outcome.get("subject") == Some(&json!("Managed indexes")))
        .ok_or_else(|| std::io::Error::other("no Managed indexes outcome reported"))?;
    assert_eq!(resolved.get("resolved"), Some(&json!(true)));
    let report = structured(&result)?
        .get("report")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| std::io::Error::other("doctor_resolve omitted report"))?;
    assert_eq!(report.get("has_failures"), Some(&json!(false)));
    Ok(())
}

#[tokio::test]
async fn doctor_resolve_initialises_git_for_a_repository_free_vault()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(server, "doctor_resolve", Map::new()).await?;

    assert_eq!(result.is_error, Some(false));
    assert!(vault.path().join(".git").exists());
    let resolved = outcomes(&result)?
        .into_iter()
        .find(|outcome| outcome.get("subject") == Some(&json!("Git recovery")))
        .ok_or_else(|| std::io::Error::other("no Git recovery outcome reported"))?;
    assert_eq!(resolved.get("resolved"), Some(&json!(true)));
    Ok(())
}

#[tokio::test]
async fn doctor_resolve_gives_a_repository_created_after_start_up_its_first_commit()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("note.md"), "# Note\n")?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;
    let server = ContextOsServer::try_from(config)?;
    // An operator running `git init` while the server is up leaves a repository with no commits, which no change
    // made through MCP can be committed to until it has a first one.
    let repository = git2::Repository::init(vault.path())?;

    let report = call_tool(server.clone(), "doctor", Map::new()).await?;
    let result = call_tool(server, "doctor_resolve", Map::new()).await?;

    let finding = structured(&report)?
        .get("checks")
        .and_then(serde_json::Value::as_array)
        .and_then(|checks| {
            checks
                .iter()
                .find(|check| check.get("kind") == Some(&json!("git_recovery")))
        })
        .ok_or_else(|| std::io::Error::other("no Git recovery check reported"))?;
    assert_eq!(finding.get("status"), Some(&json!("fail")), "{finding}");
    assert_eq!(finding.get("auto_fixable"), Some(&json!(true)), "{finding}");
    assert_eq!(finding.get("remediation_tool"), Some(&json!("git_init")), "{finding}");
    let resolved = outcomes(&result)?
        .into_iter()
        .find(|outcome| outcome.get("kind") == Some(&json!("git_recovery")))
        .ok_or_else(|| std::io::Error::other("no Git recovery outcome reported"))?;
    assert_eq!(resolved.get("resolved"), Some(&json!(true)));
    assert!(repository.head()?.peel_to_commit().is_ok());
    Ok(())
}

#[tokio::test]
async fn a_git_initialisation_that_fails_is_reported_as_not_resolved()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("note.md"), "# Note\n")?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;
    let server = ContextOsServer::try_from(config)?;
    let repository = git2::Repository::init(vault.path())?;
    // Another Git process holding the index lock makes the first commit impossible.
    std::fs::write(vault.path().join(".git/index.lock"), "")?;

    let result = call_tool(server, "doctor_resolve", Map::new()).await?;

    assert_eq!(result.is_error, Some(false));
    let outcome = outcomes(&result)?
        .into_iter()
        .find(|outcome| outcome.get("kind") == Some(&json!("git_recovery")))
        .ok_or_else(|| std::io::Error::other("no Git recovery outcome reported"))?;
    assert_eq!(outcome.get("resolved"), Some(&json!(false)), "{outcome:?}");
    assert!(repository.head().is_err(), "no commit should have been made");
    Ok(())
}

/// Unix only: a read-only directory is how the fixture makes the rebuild's write fail, and Windows has no equivalent
/// directory permission that a test can set portably.
#[cfg(unix)]
#[tokio::test]
async fn an_index_rebuild_that_fails_is_reported_as_not_resolved()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use std::os::unix::fs::PermissionsExt;

    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let folder = vault.path().join("alpha");
    std::fs::create_dir(&folder)?;
    std::fs::write(folder.join("note.md"), "# Note\n")?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.enabled = false;
    let server = ContextOsServer::try_from(config)?;
    // A folder that cannot be written to cannot take its missing managed index.
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555))?;

    let result = call_tool(server, "doctor_resolve", Map::new()).await;
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755))?;
    let result = result?;

    assert_eq!(result.is_error, Some(false));
    let outcome = outcomes(&result)?
        .into_iter()
        .find(|outcome| outcome.get("kind") == Some(&json!("managed_indexes")))
        .ok_or_else(|| std::io::Error::other("no Managed indexes outcome reported"))?;
    assert_eq!(outcome.get("resolved"), Some(&json!(false)), "{outcome:?}");
    Ok(())
}

#[tokio::test]
async fn doctor_resolve_never_acts_on_a_non_auto_fixable_finding()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // A legacy `_index.md` alongside a real `index.md` is an index conflict
    // (`index/legacy-conflict`) that `contextos-index` deliberately refuses
    // to resolve automatically, per `crates/contextos-index/src/lib.rs`'s
    // `migrate_legacy_index`. Unlike a bad `[vault.search.embedding]`
    // config (which fails server construction outright, so a live server
    // can never actually reach that finding), this is genuine, reachable
    // runtime state for a running server: both files simply exist.
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("index.md"), "# Root\n")?;
    std::fs::write(vault.path().join("_index.md"), "legacy bytes\n")?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.enabled = false;
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(server, "doctor_resolve", Map::new()).await?;

    assert_eq!(result.is_error, Some(false));
    assert!(
        outcomes(&result)?
            .into_iter()
            .all(|outcome| outcome.get("subject") != Some(&json!("Managed indexes")))
    );
    assert_eq!(std::fs::read(vault.path().join("_index.md"))?, b"legacy bytes\n");
    let report = structured(&result)?
        .get("report")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| std::io::Error::other("doctor_resolve omitted report"))?;
    assert_eq!(report.get("has_failures"), Some(&json!(true)));
    Ok(())
}

#[tokio::test]
async fn dry_run_reports_without_writing() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.enabled = false;
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(
        server,
        "doctor_resolve",
        serde_json::from_value(json!({"dry_run": true}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    assert!(!vault.path().join("index.md").exists());
    let previewed = outcomes(&result)?
        .into_iter()
        .find(|outcome| outcome.get("subject") == Some(&json!("Managed indexes")))
        .ok_or_else(|| std::io::Error::other("no Managed indexes outcome reported"))?;
    assert_eq!(previewed.get("resolved"), Some(&json!(false)));
    let report = structured(&result)?
        .get("report")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| std::io::Error::other("doctor_resolve omitted report"))?;
    assert_eq!(report.get("has_failures"), Some(&json!(true)));
    Ok(())
}

#[tokio::test]
async fn doctor_resolve_run_twice_on_an_already_healthy_vault_writes_nothing_the_second_time()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.enabled = false;
    let server = ContextOsServer::try_from(config)?;

    let first = call_tool(server.clone(), "doctor_resolve", Map::new()).await?;
    assert_eq!(first.is_error, Some(false));
    let index_md = vault.path().join("index.md");
    let mtime_after_first = std::fs::metadata(&index_md)?.modified()?;

    let second = call_tool(server, "doctor_resolve", Map::new()).await?;

    assert_eq!(second.is_error, Some(false));
    assert!(
        outcomes(&second)?.is_empty(),
        "`outcomes(&second)?` should be empty, got {:?}",
        outcomes(&second)?
    );
    assert_eq!(std::fs::metadata(&index_md)?.modified()?, mtime_after_first);
    let report = structured(&second)?
        .get("report")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| std::io::Error::other("doctor_resolve omitted report"))?;
    assert_eq!(report.get("has_failures"), Some(&json!(false)));
    Ok(())
}

#[tokio::test]
async fn path_scopes_resolution_to_one_configured_vault() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let first = tempfile::Builder::new().prefix("first").tempdir()?;
    let second = tempfile::Builder::new().prefix("second").tempdir()?;
    let mut config = support::scoped_config(vec![first.path().to_path_buf(), second.path().to_path_buf()])?;
    for vault in &mut config.vaults {
        vault.git.enabled = false;
    }
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(
        server,
        "doctor_resolve",
        serde_json::from_value(json!({"path": first.path().to_str()}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    assert!(first.path().join("index.md").exists());
    assert!(!second.path().join("index.md").exists());
    Ok(())
}

#[tokio::test]
async fn the_cli_dry_run_previews_exactly_the_outcomes_the_tool_dry_run_reports()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Stale indexes and an absent repository give two auto-fixable findings of different kinds, so the comparison
    // covers the selection, its order, and each remediation's preview.
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    let cli_preview = serde_json::to_value(contextos_mcp::preview_resolution_for_cli(&config)?)?;
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(
        server,
        "doctor_resolve",
        serde_json::from_value(json!({"dry_run": true}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    let tool_preview = structured(&result)?
        .get("outcomes")
        .cloned()
        .ok_or_else(|| std::io::Error::other("doctor_resolve result omitted outcomes"))?;
    let kinds: Vec<_> = outcomes(&result)?
        .iter()
        .filter_map(|outcome| outcome.get("kind").cloned())
        .collect();
    assert_eq!(kinds, vec![json!("managed_indexes"), json!("git_recovery")]);
    assert_eq!(cli_preview, tool_preview);
    assert!(!vault.path().join("index.md").exists());
    assert!(!vault.path().join(".git").exists());
    Ok(())
}
