use super::*;

/// The folder of the workflow `name` in the package `crm`, so a package and a workflow are never mistaken for each other.
fn folder(name: &str) -> WorkflowFolder<'_> {
    WorkflowFolder {
        package: "ops",
        directory: name,
    }
}

#[test]
fn a_lowercase_hyphenated_slug_is_url_safe() {
    assert!(is_url_safe_slug("sync-crm-contacts"));
    assert!(is_url_safe_slug("a1-b2"));
}

#[test]
fn an_empty_slug_is_not_url_safe() {
    assert!(!is_url_safe_slug(""));
}

#[test]
fn a_slug_with_uppercase_or_symbols_is_not_url_safe() {
    assert!(!is_url_safe_slug("Sync-Crm"));
    assert!(!is_url_safe_slug("sync_crm"));
    assert!(!is_url_safe_slug("sync crm"));
    assert!(!is_url_safe_slug("sync/crm"));
}

#[test]
fn a_workflow_manifest_parses_every_documented_field() -> Result<(), Box<dyn std::error::Error>> {
    let raw = declarations_from_text(
        r#"
            name         = "Sync CRM contacts"
            entry        = "workflow.rhai"
            timeout_secs = 120

            [mcp_servers]
            allow = ["contextos", "crm"]
        "#,
        folder("sync"),
    )
    .map_err(|found| found.message)?;
    assert_eq!(raw.name, "Sync CRM contacts");
    assert_eq!(raw.slug, "sync");
    assert_eq!(raw.entry, "workflow.rhai");
    assert_eq!(raw.timeout_secs, 120);
    assert!(raw.calls.permits("crm", "anything"));
    assert!(!raw.calls.permits("mail", "send"));
    Ok(())
}

#[test]
fn a_workflow_manifest_has_no_schedule_field_at_all() {
    // Scheduling belongs to a job, not its parent workflow: an
    // unrecognised `schedule` key in `workflow.toml` is a schema
    // violation, not silently ignored.
    let result = declarations_from_text(
        r#"
            name        = "Sync CRM contacts"
            entry       = "workflow.rhai"
            schedule    = "0 3 * * *"
        "#,
        folder("sync"),
    );
    assert!(result.is_err());
}

#[test]
fn a_workflow_manifest_that_declares_no_call_rules_may_call_every_configured_server() -> Result<(), String> {
    let declared = declarations_from_text(
        r#"
            name  = "One-off backfill"
            entry = "workflow.rhai"
        "#,
        folder("backfill"),
    )
    .map_err(|found| found.message)?;
    assert!(declared.calls.permits("any", "tool"));
    Ok(())
}

#[test]
fn an_unknown_workflow_manifest_field_is_a_schema_violation() {
    let result = declarations_from_text(
        r#"
            name        = "Workflow"
            entry       = "workflow.rhai"
            surprise    = "field"
        "#,
        folder("workflow"),
    );
    assert!(result.is_err());
}

#[test]
fn a_missing_required_workflow_manifest_field_is_a_schema_violation() {
    let result = declarations_from_text("name = \"Workflow\"\n", folder("workflow"));
    assert!(result.is_err());
}

#[test]
fn a_job_manifest_parses_every_documented_field() -> Result<(), Box<dyn std::error::Error>> {
    let raw: JobManifestRaw = toml::from_str(
        r#"
            name         = "Nightly sync"
            slug         = "nightly-sync"
            timeout_secs = 120
            version      = "1.0.0"
            owner        = "operator"
        "#,
    )?;
    assert_eq!(raw.name, "Nightly sync");
    assert_eq!(raw.slug.as_deref(), Some("nightly-sync"));
    assert_eq!(raw.timeout_secs.get(), 120);
    assert_eq!(raw.owner.as_deref(), Some("operator"));
    Ok(())
}

/// A schedule is set by the operator in the state store, never authored, so a job manifest that carries
/// one is a schema violation like any other unknown key.
#[test]
fn a_job_manifest_with_a_schedule_key_is_a_schema_violation() {
    let result: Result<JobManifestRaw, _> = toml::from_str(
        r#"
            name     = "Nightly sync"
            schedule = "0 3 * * *"
        "#,
    );

    assert!(result.is_err());
}

/// Every field beyond `name` and `timeout_secs` is optional, and a job with nothing stored for it runs by hand only. A
/// missing owner is not a schema error: the job loads and validation says it has no owner.
#[test]
fn a_job_manifest_needs_only_a_name_and_a_timeout() -> Result<(), Box<dyn std::error::Error>> {
    let raw: JobManifestRaw = toml::from_str(
        r#"
            name         = "One-off backfill"
            timeout_secs = 30
            version      = "1.0.0"
        "#,
    )?;
    assert_eq!(raw.timeout_secs.get(), 30);
    assert_eq!(raw.owner, None);
    Ok(())
}

#[test]
fn a_job_manifest_has_no_entry_or_mcp_servers_field_of_its_own() {
    // Both belong to the parent workflow; a job manifest declaring either
    // is a schema violation, not a silently ignored per-job override.
    let result: Result<JobManifestRaw, _> = toml::from_str(
        r#"
            name  = "Nightly sync"
            entry = "workflow.rhai"
        "#,
    );
    assert!(result.is_err());
}

#[test]
fn an_unknown_job_manifest_field_is_a_schema_violation() {
    let result: Result<JobManifestRaw, _> = toml::from_str(
        r#"
            name     = "Job"
            surprise = "field"
        "#,
    );
    assert!(result.is_err());
}

#[test]
fn a_missing_required_job_manifest_field_is_a_schema_violation() {
    let result: Result<JobManifestRaw, _> = toml::from_str("");
    assert!(result.is_err());
}

#[test]
fn an_optional_owner_slug_is_accepted_by_the_job_manifest_schema() {
    let raw = toml::from_str::<JobManifestRaw>(
        "name = \"Owned job\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nowner = \"operator\"\n",
    );
    assert!(raw.is_ok(), "owner must be a supported manifest field: {raw:?}");
}

fn a_team_member(slug: &str, kind: crate::team::TeamMemberKind) -> crate::team::TeamMember {
    crate::team::TeamMember {
        slug: slug.to_owned(),
        name: slug.to_owned(),
        kind,
        domain: None,
        team: None,
        chief_of_staff: false,
        state: crate::team::MemberState::Idle,
    }
}

#[test]
fn an_absent_owner_resolves_to_none() {
    let team = vec![a_team_member("operator", crate::team::TeamMemberKind::Human)];
    assert!(resolve_owner(None, &team).is_none());
}

#[test]
fn a_configured_owner_slug_resolves_to_the_matching_team_member() -> Result<(), Box<dyn std::error::Error>> {
    let team = vec![
        a_team_member("assistant", crate::team::TeamMemberKind::Agent),
        a_team_member("operator", crate::team::TeamMemberKind::Human),
    ];
    let owner = resolve_owner(Some("operator"), &team).ok_or("owner resolved")?;
    assert_eq!(owner.slug, "operator");
    assert_eq!(owner.kind, crate::team::TeamMemberKind::Human);
    Ok(())
}

#[test]
fn an_owner_slug_absent_from_team_toml_resolves_to_no_member_and_does_not_refuse_the_job() {
    let team = vec![a_team_member("operator", crate::team::TeamMemberKind::Human)];
    assert!(resolve_owner(Some("missing"), &team).is_none());
}

#[test]
fn an_owner_slug_resolves_to_no_member_in_an_empty_team() {
    assert!(resolve_owner(Some("operator"), &[]).is_none());
}

#[test]
fn an_owner_is_matched_by_its_exact_slug_only() {
    let team = vec![a_team_member("operator", crate::team::TeamMemberKind::Human)];
    assert!(resolve_owner(Some("Operator"), &team).is_none());
}

fn tool_error_with_code(code: &str) -> ToolErrorResult {
    let result = rmcp::model::CallToolResult::structured_error(serde_json::json!({
        "code": code,
        "message": "the server's own explanation",
    }));
    ToolErrorResult::from(&result)
}

#[test]
fn a_tool_error_for_an_unknown_vault_name_is_reported_as_the_vault_being_unavailable() {
    let error = tool_error_with_code("path/unknown-vault-name");

    assert_eq!(error.failure_kind(), WorkflowFailureKind::VaultUnavailable);
}

#[test]
fn any_other_tool_error_is_reported_as_a_tool_error() {
    let error = tool_error_with_code("io/not-found");

    assert_eq!(error.failure_kind(), WorkflowFailureKind::ToolError);
}

#[test]
fn a_tool_error_without_structured_content_is_a_tool_error_with_a_fallback_message() {
    let result = rmcp::model::CallToolResult::error(Vec::new());

    let error = ToolErrorResult::from(&result);

    assert_eq!(error.failure_kind(), WorkflowFailureKind::ToolError);
    assert_eq!(error.message, "the tool call failed");
}

#[test]
fn a_workflow_manifest_parses_its_declared_inputs() -> Result<(), Box<dyn std::error::Error>> {
    let raw = declarations_from_text(
        r#"
            name        = "Sync CRM contacts"
            entry       = "workflow.rhai"

            [[input]]
            name     = "since"
            kind     = "date"
            required = false
        "#,
        folder("sync"),
    )
    .map_err(|found| found.message)?;
    assert_eq!(raw.inputs.fields().len(), 1);
    assert_eq!(raw.inputs.fields()[0].name, "since");
    Ok(())
}

#[test]
fn a_workflow_manifest_with_a_bad_input_declaration_is_a_schema_violation() {
    let result = declarations_from_text(
        "name = \"A\"\nentry = \"a.rhai\"\n[[input]]\nname = \"x\"\nkind = \"blob\"\n",
        folder("a"),
    );
    assert!(result.is_err());
}

#[test]
fn a_job_manifest_parses_its_input_values() -> Result<(), Box<dyn std::error::Error>> {
    let raw: JobManifestRaw = toml::from_str(
        "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n[inputs]\nsince = \"2026-01-01\"\n",
    )?;
    assert_eq!(raw.inputs.get("since"), Some(&serde_json::json!("2026-01-01")));
    Ok(())
}

fn registered_workflow_declaring(inputs: &str) -> Result<RegisteredWorkflow, Box<dyn std::error::Error>> {
    declarations_from_text(&format!("name = \"W\"\nentry = \"w.rhai\"\n{inputs}"), folder("w"))
        .map_err(|found| found.message.into())
}

#[test]
fn job_values_are_validated_against_the_workflow_declaration_in_normalised_form()
-> Result<(), Box<dyn std::error::Error>> {
    let workflow = registered_workflow_declaring("[[input]]\nname = \"at\"\nkind = \"datetime\"\n")?;
    let mut values = Map::new();
    values.insert("at".to_owned(), Value::String("2026-09-23T10:00:00+00:00".to_owned()));

    let validated = validate_job_inputs(&workflow, values)?;

    assert_eq!(
        validated.values().get("at"),
        Some(&Value::String("2026-09-23T10:00:00Z".to_owned()))
    );
    Ok(())
}

#[test]
fn a_job_missing_a_required_input_is_refused_naming_it() -> Result<(), Box<dyn std::error::Error>> {
    let workflow = registered_workflow_declaring("[[input]]\nname = \"at\"\nkind = \"string\"\n")?;

    let message = validate_job_inputs(&workflow, Map::new()).err().unwrap_or_default();

    assert!(message.contains("\"at\""), "{message}");
    Ok(())
}

#[test]
fn a_job_supplying_an_undeclared_input_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let workflow = registered_workflow_declaring("")?;
    let mut values = Map::new();
    values.insert("extra".to_owned(), Value::Bool(true));

    assert!(validate_job_inputs(&workflow, values).is_err());
    Ok(())
}

#[test]
fn a_workflow_and_a_job_manifest_may_each_set_a_log_level() -> Result<(), Box<dyn std::error::Error>> {
    let workflow = declarations_from_text("name = \"W\"\nentry = \"w.rhai\"\nlog_level = \"debug\"\n", folder("w"))
        .map_err(|found| found.message)?;
    let job: JobManifestRaw =
        toml::from_str("name = \"J\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nlog_level = \"error\"\n")?;

    assert_eq!(workflow.log_level, Some(contextos_workflows::LogLevel::Debug));
    assert_eq!(job.log_level, Some(contextos_workflows::LogLevel::Error));
    Ok(())
}

#[test]
fn an_unknown_log_level_in_either_manifest_is_a_schema_violation() {
    let workflow = declarations_from_text("name = \"W\"\nentry = \"w.rhai\"\nlog_level = \"loud\"\n", folder("w"));
    let job = toml::from_str::<JobManifestRaw>(
        "name = \"J\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nlog_level = \"loud\"\n",
    );

    assert!(workflow.is_err());
    assert!(job.is_err());
}

#[tokio::test]
async fn a_call_to_a_server_that_is_not_connected_is_reported_as_the_server_being_unavailable() -> Result<(), String> {
    let clients = McpClientSet::connect(&[])
        .await
        .map_err(|source| format!("an empty set should connect: {source}"))?;
    let caller = WebMcpCaller::new(Arc::new(clients));

    let Err(failure) = caller.call("absent", "vault_info", Map::new()).await else {
        return Err("a call to an unconnected server should fail".to_owned());
    };

    assert_eq!(failure.kind, WorkflowFailureKind::ServerUnavailable);
    assert_eq!(failure.server, "absent");
    assert_eq!(failure.tool, "vault_info");
    Ok(())
}

#[test]
fn a_job_file_with_a_schedule_key_is_refused_as_an_unknown_key() -> Result<(), String> {
    let workflow = RegisteredWorkflow::without_declarations("ops", "sync");
    let text = "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nschedule = \"0 3 * * *\"\n";
    let JobLoadOutcome::Invalid(found) = job_from_text(&workflow, "nightly.toml", text, &[]) else {
        return Err("a job file with a schedule loaded".to_owned());
    };
    assert_eq!(found.code, ValidationCode::ArtefactInvalid);
    assert!(found.message.contains("unknown field `schedule`"), "{}", found.message);
    Ok(())
}

#[test]
fn a_job_file_without_timeout_secs_does_not_load_and_the_failure_names_the_key() {
    let error = toml::from_str::<JobManifestRaw>("name = \"Nightly\"\nowner = \"operator\"\n")
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();

    assert!(error.contains("timeout_secs"), "{error:?}");
}

#[test]
fn a_timeout_of_zero_or_a_negative_or_fractional_timeout_is_refused() {
    for bad in ["0", "-30", "1.5", "\"30\""] {
        let parsed = toml::from_str::<JobManifestRaw>(&format!(
            "name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = {bad}\nversion = \"1.0.0\"\n"
        ));
        assert!(parsed.is_err(), "timeout_secs = {bad} should be refused");
    }
    let accepted = toml::from_str::<JobManifestRaw>(
        "name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    );
    assert!(
        accepted.is_ok(),
        "a whole number of seconds above zero is accepted: {accepted:?}"
    );
}

#[test]
fn an_entity_key_is_accepted_in_a_job_file_and_in_a_workflow_manifest() {
    let job = toml::from_str::<JobManifestRaw>(
        "name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nentity = \"personal\"\n",
    );
    let workflow = declarations_from_text("name = \"W\"\nentry = \"w.rhai\"\nentity = \"personal\"\n", folder("w"));

    assert!(job.is_ok(), "{job:?}");
    assert!(workflow.is_ok(), "{workflow:?}");
}

#[test]
fn the_resolved_entity_is_the_jobs_else_the_workflows_else_vault() {
    assert_eq!(
        ResolvedEntity::from((Some("personal"), Some("zenith"))).as_str(),
        "personal"
    );
    assert_eq!(ResolvedEntity::from((None, Some("zenith"))).as_str(), "zenith");
    assert_eq!(ResolvedEntity::from((None, None)).as_str(), contextos_core::VAULT_SCOPE);
    assert_eq!(ResolvedEntity::from((Some("personal"), None)).as_str(), "personal");
}

mod job_listing {
    use super::*;
    use crate::fake_vault::{FakeVault, Listing};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn vault_with_two_jobs() -> FakeVault {
        FakeVault::new()
            .with_file("registry/workflows/ops/sync/jobs/nightly.toml", "ready = true\n")
            .with_file("registry/workflows/ops/sync/jobs/hourly.toml", "ready = true\n")
            .with_file("registry/workflows/ops/sync/jobs/notes.md", "not a job")
    }

    #[tokio::test]
    async fn a_listing_the_server_refuses_for_any_reason_but_a_missing_folder_is_an_outage_not_no_jobs() -> TestResult {
        let vault = vault_with_two_jobs().listing(Listing::Refused("io/permission-denied"));
        let client = vault.session().await?;

        let listed = list_job_files(&client, "vault", folder("sync")).await;

        assert!(matches!(listed, Err(McpCallError::Refused { .. })), "{listed:?}");
        Ok(())
    }

    #[tokio::test]
    async fn a_listing_that_is_not_a_listing_is_an_outage_not_no_jobs() -> TestResult {
        let vault = vault_with_two_jobs().listing(Listing::Garbled);
        let client = vault.session().await?;

        let listed = list_job_files(&client, "vault", folder("sync")).await;

        assert!(matches!(listed, Err(McpCallError::Refused { .. })), "{listed:?}");
        Ok(())
    }

    #[tokio::test]
    async fn a_workflow_with_no_jobs_folder_has_no_jobs() -> TestResult {
        let vault = FakeVault::new().with_file("registry/workflows/ops/sync/workflow.toml", "name = \"Sync\"\n");
        let client = vault.session().await?;

        let listed = list_job_files(&client, "vault", folder("sync")).await;

        assert_eq!(listed.map_err(|error| error.to_string())?, Vec::<String>::new());
        Ok(())
    }

    #[tokio::test]
    async fn only_toml_files_of_the_jobs_folder_are_jobs() -> TestResult {
        let client = vault_with_two_jobs().session().await?;

        let mut listed = list_job_files(&client, "vault", folder("sync"))
            .await
            .map_err(|error| error.to_string())?;
        listed.sort();

        assert_eq!(listed, ["hourly.toml", "nightly.toml"]);
        Ok(())
    }
}

#[test]
fn a_job_file_without_version_does_not_load_and_the_failure_names_the_key() {
    let error = toml::from_str::<JobManifestRaw>("name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\n")
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();

    assert!(error.contains("version"), "{error:?}");
}

#[test]
fn a_job_file_names_its_release_with_a_version_and_a_successor_names_its_predecessor()
-> Result<(), Box<dyn std::error::Error>> {
    let raw: JobManifestRaw =
        toml::from_str("name = \"N\"\ntimeout_secs = 30\nversion = \"1.2.0\"\npredecessor = \"nightly-v1-0-0\"\n")?;

    assert_eq!(raw.version, "1.2.0");
    assert_eq!(raw.predecessor.as_deref(), Some("nightly-v1-0-0"));
    Ok(())
}

#[test]
fn a_libraries_table_in_a_job_file_is_an_unknown_key() {
    let error = toml::from_str::<JobManifestRaw>(
        "name = \"N\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n[libraries]\nacme = \"1.0.0\"\n",
    )
    .err()
    .map(|error| error.to_string())
    .unwrap_or_default();

    assert!(error.contains("libraries"), "{error:?}");
}

#[test]
fn a_job_slug_the_state_file_uses_for_a_workflows_versions_is_refused_whether_declared_or_taken_from_the_file_name() {
    let workflow = RegisteredWorkflow::without_declarations("ops", "sync");
    for reserved in ["current", "previous", "future"] {
        let declared = format!("name = \"N\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nslug = \"{reserved}\"\n");
        let from_name = "name = \"N\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";

        assert!(
            matches!(
                job_from_text(&workflow, "nightly.toml", &declared, &[]),
                JobLoadOutcome::Invalid(_)
            ),
            "declared {reserved}"
        );
        assert!(
            matches!(
                job_from_text(&workflow, &format!("{reserved}.toml"), from_name, &[]),
                JobLoadOutcome::Invalid(_)
            ),
            "named {reserved}"
        );
    }
}
