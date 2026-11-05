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

fn checks(
    result: &CallToolResult,
) -> Result<Vec<&serde_json::Map<String, serde_json::Value>>, Box<dyn std::error::Error + Send + Sync>> {
    structured(result)?
        .get("checks")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("doctor result omitted checks").into())
        .map(|checks| checks.iter().filter_map(serde_json::Value::as_object).collect())
}

#[tokio::test]
async fn catalogue_advertises_doctor_with_an_object_schema() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let catalogue = ContextOsServer::catalogue();

    let doctor = catalogue
        .get("doctor")
        .ok_or_else(|| std::io::Error::other("missing tool doctor"))?;
    assert_eq!(
        doctor.input_schema.get("type").and_then(serde_json::Value::as_str),
        Some("object")
    );
    assert!(doctor.description.is_some());
    let schema = doctor
        .output_schema
        .as_ref()
        .ok_or_else(|| std::io::Error::other("doctor omitted output schema"))?;
    let properties = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| std::io::Error::other("doctor omitted output properties"))?;
    let mut fields = properties.keys().map(String::as_str).collect::<Vec<_>>();
    fields.sort_unstable();
    // `doctor`'s output schema is the flat, merged success-and-`ToolFailure`
    // shape `fallible_output_schema_for` builds (`tool_contract.rs`'s
    // `every_remaining_tool_advertises_a_flat_fallible_output_schema`
    // covers this generally): `code`/`message`/`path`/`remediation` are
    // `ToolFailure`'s fields, present because `doctor`'s error path
    // populates `structured_content` with that shape too.
    assert_eq!(
        fields,
        vec!["checks", "code", "has_failures", "message", "path", "remediation"]
    );

    Ok(())
}

#[tokio::test]
async fn doctor_reports_a_healthy_vault_with_no_failures() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.enabled = false;
    config.vaults[0].index_md.enabled = false;
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(server, "doctor", Map::new()).await?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(structured(&result)?.get("has_failures"), Some(&json!(false)));
    let managed = checks(&result)?
        .into_iter()
        .find(|check| check.get("subject") == Some(&json!("Managed indexes")))
        .ok_or_else(|| std::io::Error::other("no Managed indexes check reported"))?;
    assert_eq!(managed.get("status"), Some(&json!("pass")));
    assert_eq!(managed.get("auto_fixable"), Some(&json!(false)));
    assert_eq!(managed.get("remediation_tool"), None);
    Ok(())
}

#[tokio::test]
async fn doctor_reports_a_missing_managed_index_as_auto_fixable() -> Result<(), Box<dyn std::error::Error + Send + Sync>>
{
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.enabled = false;
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(server, "doctor", Map::new()).await?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(structured(&result)?.get("has_failures"), Some(&json!(true)));
    let managed = checks(&result)?
        .into_iter()
        .find(|check| check.get("subject") == Some(&json!("Managed indexes")))
        .ok_or_else(|| std::io::Error::other("no Managed indexes check reported"))?;
    assert_eq!(managed.get("status"), Some(&json!("fail")));
    assert_eq!(managed.get("auto_fixable"), Some(&json!(true)));
    assert_eq!(managed.get("remediation_tool"), Some(&json!("vault_index_rebuild")));
    Ok(())
}

#[tokio::test]
async fn doctor_reports_invalid_frontmatter_as_never_auto_fixable()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(
        vault.path().join("broken.md"),
        "---\ntitle: Notes: something happened\n---\nBody\n",
    )?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.enabled = false;
    config.vaults[0].index_md.enabled = false;
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(server, "doctor", Map::new()).await?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(structured(&result)?.get("has_failures"), Some(&json!(true)));
    let frontmatter = checks(&result)?
        .into_iter()
        .find(|check| check.get("subject") == Some(&json!("Frontmatter validity")))
        .ok_or_else(|| std::io::Error::other("no Frontmatter validity check reported"))?;
    assert_eq!(frontmatter.get("status"), Some(&json!("fail")));
    assert_eq!(frontmatter.get("auto_fixable"), Some(&json!(false)));
    assert_eq!(frontmatter.get("remediation_tool"), None);
    let message = frontmatter
        .get("message")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other("Frontmatter validity omitted message"))?;
    assert!(message.contains("broken.md"), "{message}");
    Ok(())
}

#[tokio::test]
async fn doctor_reports_a_duplicate_id_as_never_auto_fixable() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("a"))?;
    std::fs::create_dir_all(vault.path().join("b"))?;
    std::fs::write(
        vault.path().join("all.base"),
        "views:\n  - type: table\n    name: All\n",
    )?;
    std::fs::write(
        vault.path().join("a/first.md"),
        "---\nid: TASK-007\ntitle: First\n---\nBody\n",
    )?;
    std::fs::write(
        vault.path().join("b/second.md"),
        "---\nid: TASK-007\ntitle: Second\n---\nBody\n",
    )?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.enabled = false;
    config.vaults[0].index_md.enabled = false;
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(server, "doctor", Map::new()).await?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(structured(&result)?.get("has_failures"), Some(&json!(true)));
    let duplicate = checks(&result)?
        .into_iter()
        .find(|check| check.get("subject") == Some(&json!("Duplicate id detection")))
        .ok_or_else(|| std::io::Error::other("no Duplicate id detection check reported"))?;
    assert_eq!(duplicate.get("status"), Some(&json!("fail")));
    assert_eq!(duplicate.get("auto_fixable"), Some(&json!(false)));
    assert_eq!(duplicate.get("remediation_tool"), None);
    let message = duplicate
        .get("message")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other("Duplicate id detection omitted message"))?;
    assert!(message.contains("TASK-007"), "{message}");
    assert!(message.contains("all.base"), "{message}");
    assert!(message.contains("a/first.md"), "{message}");
    assert!(message.contains("b/second.md"), "{message}");
    Ok(())
}

#[tokio::test]
async fn doctor_reports_no_duplicate_ids_for_a_healthy_vault() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("a.md"), "---\nid: TASK-001\ntitle: A\n---\nBody\n")?;
    std::fs::write(vault.path().join("b.md"), "---\nid: TASK-002\ntitle: B\n---\nBody\n")?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.enabled = false;
    config.vaults[0].index_md.enabled = false;
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(server, "doctor", Map::new()).await?;

    assert_eq!(result.is_error, Some(false));
    let duplicate = checks(&result)?
        .into_iter()
        .find(|check| check.get("subject") == Some(&json!("Duplicate id detection")))
        .ok_or_else(|| std::io::Error::other("no Duplicate id detection check reported"))?;
    assert_eq!(duplicate.get("status"), Some(&json!("pass")));
    Ok(())
}

/// `doctor`, and the dry run of a resolution that reuses it, only read: with semantic search configured they check
/// the provider and report the vector store, without creating the store or the state directory that holds it.
/// The key is written in the configuration, and the report never shows it.
#[test]
fn doctor_checks_semantic_search_without_creating_the_vector_store()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("note.md"), "# Note\n")?;
    let stub = support::embedding_stub::EmbeddingStub::start()?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    let settings = &mut config.vaults[0];
    settings.index_md.enabled = false;
    settings.git.enabled = false;
    settings.search.text = false;
    settings.search.graph = false;
    settings.search.semantic = true;
    settings.search.embedding = contextos_mcp::EmbeddingConfig {
        provider: contextos_mcp::EmbeddingProvider::OpenaiCompatible,
        model: "stub".to_owned(),
        endpoint: stub.endpoint(),
        api_key: "sk-doctor-must-not-appear".to_owned(),
        api_key_env: String::new(),
        model_directory: None,
    };

    let report = contextos_mcp::DoctorReport::try_from(&config)?.to_string();
    let preview = contextos_mcp::preview_resolution_for_cli(&config)?;

    assert!(!report.contains("sk-doctor-must-not-appear"), "{report}");

    assert!(
        !vault.path().join(".contextos").exists(),
        "the doctor created the vault's derived state"
    );
    assert!(report.contains("Semantic search | PASS"), "{report}");
    assert!(report.contains("not been built yet"), "{report}");
    assert!(preview.is_empty(), "{preview:?}");
    Ok(())
}

/// An openai-compatible provider with neither a key nor a variable naming one fails its check, and the remediation
/// offers both ways of supplying it.
#[test]
fn doctor_reports_an_openai_compatible_provider_with_no_key_naming_both_ways_to_supply_one()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    let settings = &mut config.vaults[0];
    settings.index_md.enabled = false;
    settings.git.enabled = false;
    settings.search.semantic = true;
    settings.search.embedding = contextos_mcp::EmbeddingConfig {
        provider: contextos_mcp::EmbeddingProvider::OpenaiCompatible,
        model: "stub".to_owned(),
        endpoint: "https://embeddings.example.com/v1/embeddings".to_owned(),
        ..contextos_mcp::EmbeddingConfig::default()
    };

    let report = contextos_mcp::DoctorReport::try_from(&config)?.to_string();

    let semantic = report
        .lines()
        .find(|line| line.starts_with("Semantic search |"))
        .ok_or("no Semantic search line")?;
    assert!(semantic.starts_with("Semantic search | FAIL"), "{report}");
    assert!(semantic.contains("api_key or api_key_env"), "{report}");
    Ok(())
}

/// A repository whose branch reference is corrupt is damaged, not merely new: the doctor must leave it to the operator
/// rather than offer `git_init`, which would build history on top of the damage.
#[test]
fn doctor_reports_a_corrupt_branch_reference_as_needing_the_operator()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    git2::Repository::init(vault.path())?;
    let head = std::fs::read_to_string(vault.path().join(".git/HEAD"))?;
    let branch = head.trim().strip_prefix("ref: ").ok_or("HEAD is not symbolic")?;
    std::fs::write(vault.path().join(".git").join(branch), "not an object id\n")?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;

    let report = contextos_mcp::DoctorReport::try_from(&config)?.to_string();
    let preview = contextos_mcp::preview_resolution_for_cli(&config)?;

    let git_line = report
        .lines()
        .find(|line| line.starts_with("Git recovery |"))
        .ok_or("no Git recovery line")?;
    assert!(git_line.starts_with("Git recovery | FAIL"), "{report}");
    assert!(!git_line.contains("no commits yet"), "{report}");
    assert!(
        preview.iter().all(|outcome| outcome.subject != "Git recovery"),
        "{preview:?}"
    );
    Ok(())
}

#[tokio::test]
async fn doctor_reports_an_invalid_team_as_a_team_config_failure_through_the_tool()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join(".contextos"))?;
    std::fs::write(
        vault.path().join(".contextos/team.toml"),
        "[[member]]\nslug = \"atlas\"\nname = \"Atlas\"\nchief_of_staff = true\n\
         [[member]]\nslug = \"vela\"\nname = \"Vela\"\nchief_of_staff = true\n",
    )?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.enabled = false;
    config.vaults[0].index_md.enabled = false;
    let server = ContextOsServer::try_from(config)?;

    let result = call_tool(server, "doctor", Map::new()).await?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(structured(&result)?.get("has_failures"), Some(&json!(true)));
    let team = checks(&result)?
        .into_iter()
        .find(|check| check.get("subject") == Some(&json!("Team configuration")))
        .ok_or_else(|| std::io::Error::other("no Team configuration check reported"))?;
    assert_eq!(team.get("status"), Some(&json!("fail")));
    assert_eq!(team.get("auto_fixable"), Some(&json!(false)));
    let message = team
        .get("message")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other("Team configuration omitted message"))?;
    assert!(message.contains("several-chiefs-of-staff"), "{message}");
    assert!(!message.contains("Atlas") && !message.contains("Vela"), "{message}");
    Ok(())
}
