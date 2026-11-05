//! `contextos_web::cli_run::run_job_reported` (`contextos-web jobs run`): real
//! `contextos-mcp` sessions against real temporary vault fixtures,
//! no mocking of the vault itself (`testing.md`) - only a script's own
//! `call_mcp` calls are ever mocked, and only when `--dry-run` is
//! exercised.

mod support;

use std::path::Path;
use std::sync::Arc;

use contextos_web::McpClientSet;
use contextos_web::cli_run::{CliRunError, CliRunOptions};
use contextos_web::run_claim::{ClaimState, RunClaim};
use contextos_web::vault_runs::claim_slot;
use contextos_workflows::RunOutcome;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

fn write_workflow(
    vault_dir: &Path,
    workflow_slug: &str,
    workflow_toml: &str,
    entry_name: &str,
    entry_content: &str,
) -> std::io::Result<()> {
    write(
        vault_dir,
        &format!("registry/workflows/{workflow_slug}/workflows.toml"),
        &format!("workflows = [{workflow_slug:?}]\n"),
    )?;
    write(
        vault_dir,
        &format!("registry/workflows/{workflow_slug}/{workflow_slug}/workflow.toml"),
        workflow_toml,
    )?;
    write(
        vault_dir,
        &format!("registry/workflows/{workflow_slug}/{workflow_slug}/{entry_name}"),
        entry_content,
    )
}

fn write_job(vault_dir: &Path, workflow_slug: &str, job_slug: &str, job_toml: &str) -> std::io::Result<()> {
    write(
        vault_dir,
        &format!("registry/workflows/{workflow_slug}/{workflow_slug}/jobs/{job_slug}.toml"),
        job_toml,
    )
}

/// Records `lifecycle` for a job in the vault's `.contextos/jobs.toml`, as the Web-owned state store would hold it.
fn set_lifecycle(vault_dir: &Path, workflow_slug: &str, job_slug: &str, lifecycle: &str) -> std::io::Result<()> {
    write(
        vault_dir,
        ".contextos/jobs.toml",
        &format!("[{workflow_slug}.{workflow_slug}.{job_slug}]\nlifecycle = \"{lifecycle}\"\n"),
    )
}

async fn connected_clients(config_dir: &Path, vault_dir: &Path) -> Result<Arc<McpClientSet>, BoxError> {
    support::release_fixture::install_vault_releases(vault_dir).map_err(|error| error.to_string())?;
    let config_path = support::write_vault_config(config_dir, vault_dir)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    Ok(Arc::new(McpClientSet::connect(&[entry]).await?))
}

/// Finds the first run record markdown file anywhere under `root`
/// (`memory/jobs/{{workflow-slug}}/{{job-slug}}/{{YYYY}}/{{MM}}/`'s own
/// date partitioning): the test does not hardcode a run's own identifier
/// or date, since both are assigned at real run time.
fn find_a_run_record(root: &Path) -> Option<std::path::PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|extension| extension == "md") {
                return Some(path);
            }
        }
    }
    None
}

#[tokio::test]
async fn a_cli_triggered_run_succeeds_and_writes_a_run_record_by_default() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write_workflow(
        vault_dir.path(),
        "sync-crm-contacts",
        "name = \"Sync CRM contacts workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
        "workflow.rhai",
        "call_mcp(\"contextos\", \"vault_info\", #{});",
    )?;
    write_job(
        vault_dir.path(),
        "sync-crm-contacts",
        "nightly",
        "name = \"Nightly sync\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    set_lifecycle(vault_dir.path(), "sync-crm-contacts", "nightly", "registered")?;
    let clients = connected_clients(config_dir.path(), vault_dir.path()).await?;

    let outcome = support::run_job(
        clients,
        &support::host(config_dir.path()),
        VAULT_NAME,
        "sync-crm-contacts",
        "sync-crm-contacts",
        "nightly",
        CliRunOptions {
            dry_run_scenario: None,
            record: true,
        },
    )
    .await?;

    assert_eq!(outcome, RunOutcome::Succeeded);
    let record = find_a_run_record(&vault_dir.path().join("memory/jobs"));
    assert!(record.is_some(), "expected a run record to exist under memory/jobs");
    Ok(())
}

#[tokio::test]
async fn no_record_skips_writing_any_run_record() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write_workflow(
        vault_dir.path(),
        "sync-crm-contacts",
        "name = \"Sync CRM contacts workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
        "workflow.rhai",
        "call_mcp(\"contextos\", \"vault_info\", #{});",
    )?;
    write_job(
        vault_dir.path(),
        "sync-crm-contacts",
        "nightly",
        "name = \"Nightly sync\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    set_lifecycle(vault_dir.path(), "sync-crm-contacts", "nightly", "registered")?;
    let clients = connected_clients(config_dir.path(), vault_dir.path()).await?;

    let outcome = support::run_job(
        clients,
        &support::host(config_dir.path()),
        VAULT_NAME,
        "sync-crm-contacts",
        "sync-crm-contacts",
        "nightly",
        CliRunOptions {
            dry_run_scenario: None,
            record: false,
        },
    )
    .await?;

    assert_eq!(outcome, RunOutcome::Succeeded);
    assert!(
        !vault_dir.path().join("memory/jobs").exists(),
        "no run record should exist at all"
    );
    Ok(())
}

#[tokio::test]
async fn a_dry_run_uses_the_scenarios_own_mock_instead_of_a_real_call() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write_workflow(
        vault_dir.path(),
        "sync-crm-contacts",
        "name = \"Sync CRM contacts workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
        "workflow.rhai",
        // A script that would genuinely fail for real (no such file), but
        // the scenario below mocks a successful response instead - proving
        // the real tool was never actually invoked.
        "call_mcp(\"contextos\", \"fs_read_text_file\", #{ path: \"does-not-exist.md\" });",
    )?;
    write_job(
        vault_dir.path(),
        "sync-crm-contacts",
        "nightly",
        "name = \"Nightly sync\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    set_lifecycle(vault_dir.path(), "sync-crm-contacts", "nightly", "registered")?;
    write(
        vault_dir.path(),
        "registry/workflows/sync-crm-contacts/sync-crm-contacts/tests/happy-path.toml",
        "[[call]]\nserver = \"contextos\"\ntool = \"fs_read_text_file\"\nkind = \"valid\"\nresponse = { content = \"mocked\" }\n",
    )?;
    let clients = connected_clients(config_dir.path(), vault_dir.path()).await?;

    let outcome = support::run_job(
        clients,
        &support::host(config_dir.path()),
        VAULT_NAME,
        "sync-crm-contacts",
        "sync-crm-contacts",
        "nightly",
        CliRunOptions {
            dry_run_scenario: Some(Some("happy-path".to_owned())),
            record: false,
        },
    )
    .await?;

    assert_eq!(outcome, RunOutcome::Succeeded);
    Ok(())
}

#[tokio::test]
async fn a_dry_run_with_no_scenario_named_uses_the_sole_file_present() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write_workflow(
        vault_dir.path(),
        "sync-crm-contacts",
        "name = \"Sync CRM contacts workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
        "workflow.rhai",
        "call_mcp(\"contextos\", \"vault_info\", #{});",
    )?;
    write_job(
        vault_dir.path(),
        "sync-crm-contacts",
        "nightly",
        "name = \"Nightly sync\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    set_lifecycle(vault_dir.path(), "sync-crm-contacts", "nightly", "registered")?;
    write(
        vault_dir.path(),
        "registry/workflows/sync-crm-contacts/sync-crm-contacts/tests/only-scenario.toml",
        "[[call]]\nserver = \"contextos\"\ntool = \"vault_info\"\nkind = \"valid\"\nresponse = { ok = true }\n",
    )?;
    let clients = connected_clients(config_dir.path(), vault_dir.path()).await?;

    let outcome = support::run_job(
        clients,
        &support::host(config_dir.path()),
        VAULT_NAME,
        "sync-crm-contacts",
        "sync-crm-contacts",
        "nightly",
        CliRunOptions {
            dry_run_scenario: Some(None),
            record: false,
        },
    )
    .await?;

    assert_eq!(outcome, RunOutcome::Succeeded);
    Ok(())
}

#[tokio::test]
async fn a_run_for_an_unregistered_job_is_reported_as_not_found() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    let clients = connected_clients(config_dir.path(), vault_dir.path()).await?;

    let result = support::run_job(
        clients,
        &support::host(config_dir.path()),
        VAULT_NAME,
        "no-such-workflow",
        "no-such-workflow",
        "no-such-job",
        CliRunOptions::default(),
    )
    .await;

    assert!(matches!(result, Err(CliRunError::JobNotFound { .. })), "{result:?}");
    Ok(())
}

#[tokio::test]
async fn a_run_of_a_job_whose_previous_run_is_still_active_is_reported_as_busy() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write_workflow(
        vault_dir.path(),
        "sync-crm-contacts",
        "name = \"Sync CRM contacts workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
        "workflow.rhai",
        "call_mcp(\"contextos\", \"vault_info\", #{});",
    )?;
    write_job(
        vault_dir.path(),
        "sync-crm-contacts",
        "nightly",
        "name = \"Nightly sync\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    set_lifecycle(vault_dir.path(), "sync-crm-contacts", "nightly", "registered")?;
    // A run of this job is active in another process, which holds the job's claim.
    let host = support::host(config_dir.path());
    let _running = RunClaim::acquire(&claim_slot(
        &host,
        VAULT_NAME,
        "sync-crm-contacts",
        "sync-crm-contacts",
        "nightly",
    )?)?;
    let clients = connected_clients(config_dir.path(), vault_dir.path()).await?;

    let result = support::run_job(
        clients,
        &support::host(config_dir.path()),
        VAULT_NAME,
        "sync-crm-contacts",
        "sync-crm-contacts",
        "nightly",
        CliRunOptions::default(),
    )
    .await;

    assert!(matches!(result, Err(CliRunError::Busy)), "{result:?}");
    Ok(())
}

#[tokio::test]
async fn a_job_run_that_flags_a_problem_is_recorded_as_degraded_and_returns_its_reasons() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write_workflow(
        vault_dir.path(),
        "sync-crm-contacts",
        "name = \"Sync CRM contacts workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
        "workflow.rhai",
        "call_mcp(\"contextos\", \"vault_info\", #{});\nflag_degraded(\"the read was retried\");",
    )?;
    write_job(
        vault_dir.path(),
        "sync-crm-contacts",
        "nightly",
        "name = \"Nightly sync\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nlog_level = \"debug\"\n",
    )?;
    set_lifecycle(vault_dir.path(), "sync-crm-contacts", "nightly", "registered")?;
    let clients = connected_clients(config_dir.path(), vault_dir.path()).await?;

    let outcome = support::run_job(
        clients,
        &support::host(config_dir.path()),
        VAULT_NAME,
        "sync-crm-contacts",
        "sync-crm-contacts",
        "nightly",
        CliRunOptions {
            dry_run_scenario: None,
            record: true,
        },
    )
    .await?;

    let RunOutcome::Degraded(reasons) = outcome else {
        return Err(format!("expected a degraded run, got {outcome:?}").into());
    };
    assert_eq!(reasons.kept, vec!["the read was retried".to_owned()]);
    let record = find_a_run_record(&vault_dir.path().join("memory/jobs")).ok_or("no run record was written")?;
    let text = std::fs::read_to_string(record)?;
    assert!(text.contains("status: degraded"), "{text}");
    assert!(
        !text.contains("the read was retried"),
        "reasons must not be written to the record yet: {text}"
    );
    Ok(())
}

#[tokio::test]
async fn a_saved_job_run_may_not_name_another_configured_vault_and_records_the_refusal() -> Result<(), BoxError> {
    let home = tempfile::tempdir()?;
    let other = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(home.path(), "index.md", "# Home\n")?;
    write(home.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write(other.path(), "index.md", "# Other\n")?;
    write(other.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write_workflow(
        home.path(),
        "reach",
        "name = \"Reach workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
        "workflow.rhai",
        "call_mcp(\"contextos\", \"fs_delete_file\", #{ path: \"other-vault://index.md\" });",
    )?;
    write_job(
        home.path(),
        "reach",
        "nightly",
        "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    set_lifecycle(home.path(), "reach", "nightly", "registered")?;
    support::release_fixture::install_vault_releases(home.path()).map_err(|error| error.to_string())?;
    let config_path = support::write_named_vaults_config(
        config_dir.path(),
        &[(VAULT_NAME, home.path()), ("other-vault", other.path())],
    )?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);

    let outcome = support::run_job(
        clients,
        &support::host(config_dir.path()),
        VAULT_NAME,
        "reach",
        "reach",
        "nightly",
        CliRunOptions {
            dry_run_scenario: None,
            record: true,
        },
    )
    .await?;

    assert!(
        matches!(&outcome, RunOutcome::Failed(failure)
            if failure.kind == Some(contextos_workflows::WorkflowFailureKind::VaultOutOfScope)),
        "{outcome:?}"
    );
    assert!(
        other.path().join("index.md").is_file(),
        "the other vault must be untouched"
    );
    let record = find_a_run_record(&home.path().join("memory/jobs"))
        .ok_or("the refused run should still leave a failed run record")?;
    let text = std::fs::read_to_string(record)?;
    assert!(text.contains("status: failed"), "{text}");
    Ok(())
}

/// Runs the one job of a vault in `lifecycle` as a live run, returning the result.
async fn run_in_state(
    lifecycle: &str,
) -> Result<(Result<RunOutcome, CliRunError>, tempfile::TempDir, tempfile::TempDir), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write_workflow(
        vault_dir.path(),
        "sync-crm-contacts",
        "name = \"Sync CRM contacts workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
        "workflow.rhai",
        "call_mcp(\"contextos\", \"vault_info\", #{});",
    )?;
    write_job(
        vault_dir.path(),
        "sync-crm-contacts",
        "nightly",
        "name = \"Nightly sync\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    set_lifecycle(vault_dir.path(), "sync-crm-contacts", "nightly", lifecycle)?;
    let clients = connected_clients(config_dir.path(), vault_dir.path()).await?;
    let result = support::run_job(
        clients,
        &support::host(config_dir.path()),
        VAULT_NAME,
        "sync-crm-contacts",
        "sync-crm-contacts",
        "nightly",
        CliRunOptions::default(),
    )
    .await;
    Ok((result, vault_dir, config_dir))
}

#[tokio::test]
async fn a_live_run_of_a_job_that_is_not_registered_is_refused_naming_its_state_and_writes_nothing()
-> Result<(), BoxError> {
    for lifecycle in ["draft", "validated", "retired"] {
        let (result, vault_dir, config_dir) = run_in_state(lifecycle).await?;

        assert!(
            matches!(&result, Err(CliRunError::NotRunnable { dry_run: false, .. })),
            "{lifecycle}: {result:?}"
        );
        let message = result.err().map(|error| error.to_string()).unwrap_or_default();
        assert!(message.contains("workflow/job-not-runnable"), "{message}");
        assert!(message.contains(lifecycle), "{message}");
        assert!(
            find_a_run_record(&vault_dir.path().join("memory/jobs")).is_none(),
            "{lifecycle}: a refused run leaves no record"
        );
        // `run_in_state` keeps no claim of its own, so a claim held now would be a run that was started and never ended.
        let slot = claim_slot(
            &support::host(config_dir.path()),
            VAULT_NAME,
            "sync-crm-contacts",
            "sync-crm-contacts",
            "nightly",
        )?;
        assert_eq!(slot.state()?, ClaimState::Idle, "{lifecycle}: no run was claimed");
    }
    Ok(())
}

#[tokio::test]
async fn a_live_run_of_a_registered_enabled_or_suspended_job_is_allowed() -> Result<(), BoxError> {
    for lifecycle in ["registered", "enabled", "suspended"] {
        let (result, _vault_dir, _config_dir) = run_in_state(lifecycle).await?;

        assert_eq!(result.ok(), Some(RunOutcome::Succeeded), "{lifecycle}");
    }
    Ok(())
}

#[tokio::test]
async fn a_dry_run_of_a_draft_job_is_allowed_and_of_a_retired_job_is_refused() -> Result<(), BoxError> {
    for (lifecycle, allowed) in [("draft", true), ("validated", true), ("retired", false)] {
        let vault_dir = tempfile::tempdir()?;
        let config_dir = tempfile::tempdir()?;
        write(vault_dir.path(), "index.md", "# Root\n")?;
        write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
        write_workflow(
            vault_dir.path(),
            "sync-crm-contacts",
            "name = \"Sync CRM contacts workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
            "workflow.rhai",
            "call_mcp(\"contextos\", \"fs_read_text_file\", #{ path: \"does-not-exist.md\" });",
        )?;
        write_job(
            vault_dir.path(),
            "sync-crm-contacts",
            "nightly",
            "name = \"Nightly sync\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        )?;
        set_lifecycle(vault_dir.path(), "sync-crm-contacts", "nightly", lifecycle)?;
        write(
            vault_dir.path(),
            "registry/workflows/sync-crm-contacts/sync-crm-contacts/tests/happy-path.toml",
            "[[call]]\nserver = \"contextos\"\ntool = \"fs_read_text_file\"\nkind = \"valid\"\nresponse = { content = \"mocked\" }\n",
        )?;
        let clients = connected_clients(config_dir.path(), vault_dir.path()).await?;

        let result = support::run_job(
            clients,
            &support::host(config_dir.path()),
            VAULT_NAME,
            "sync-crm-contacts",
            "sync-crm-contacts",
            "nightly",
            CliRunOptions {
                dry_run_scenario: Some(Some("happy-path".to_owned())),
                record: false,
            },
        )
        .await;

        if allowed {
            assert_eq!(result.ok(), Some(RunOutcome::Succeeded), "{lifecycle}");
        } else {
            assert!(
                matches!(&result, Err(CliRunError::NotRunnable { dry_run: true, .. })),
                "{lifecycle}: {result:?}"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn the_state_refusal_comes_before_the_busy_check() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write_workflow(
        vault_dir.path(),
        "sync-crm-contacts",
        "name = \"Sync CRM contacts workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
        "workflow.rhai",
        "call_mcp(\"contextos\", \"vault_info\", #{});",
    )?;
    write_job(
        vault_dir.path(),
        "sync-crm-contacts",
        "nightly",
        "name = \"Nightly sync\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    // A run of the job is active, and the job is a draft: the refusal is about the job, not about it being busy.
    let host = support::host(config_dir.path());
    let _running = RunClaim::acquire(&claim_slot(
        &host,
        VAULT_NAME,
        "sync-crm-contacts",
        "sync-crm-contacts",
        "nightly",
    )?)?;
    let clients = connected_clients(config_dir.path(), vault_dir.path()).await?;

    let result = support::run_job(
        clients,
        &support::host(config_dir.path()),
        VAULT_NAME,
        "sync-crm-contacts",
        "sync-crm-contacts",
        "nightly",
        CliRunOptions::default(),
    )
    .await;

    assert!(matches!(result, Err(CliRunError::NotRunnable { .. })), "{result:?}");
    Ok(())
}

/// A workflow whose script makes the one call a scenario declares, with the job already `ready` and validated by a real
/// validation pass, so a dry run is what decides whether it becomes registered.
async fn validated_job_with_scenario(
    scenario: &str,
    ready: bool,
) -> Result<(tempfile::TempDir, tempfile::TempDir, Arc<McpClientSet>), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write_workflow(
        vault_dir.path(),
        "sync-crm-contacts",
        "name = \"Sync CRM contacts workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
        "workflow.rhai",
        "call_mcp(\"contextos\", \"fs_read_text_file\", #{ path: \"does-not-exist.md\" });",
    )?;
    write_job(
        vault_dir.path(),
        "sync-crm-contacts",
        "nightly",
        &format!(
            "name = \"Nightly sync\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n{}",
            if ready { "ready = true\n" } else { "" }
        ),
    )?;
    write(
        vault_dir.path(),
        "registry/workflows/sync-crm-contacts/sync-crm-contacts/tests/happy-path.toml",
        scenario,
    )?;
    let clients = connected_clients(config_dir.path(), vault_dir.path()).await?;
    let client = clients.get("contextos").ok_or("no primary server")?;
    contextos_web::job_validation::validation_pass(
        &client,
        VAULT_NAME,
        &["contextos".to_owned()],
        time::macros::datetime!(2026-10-01 03:00:00 UTC),
    )
    .await?;
    Ok((vault_dir, config_dir, clients))
}

fn state_text(vault_dir: &Path) -> Result<String, std::io::Error> {
    std::fs::read_to_string(vault_dir.join(".contextos/jobs.toml"))
}

async fn dry_run(clients: Arc<McpClientSet>, config_dir: &Path) -> Result<RunOutcome, CliRunError> {
    support::run_job(
        clients,
        &support::host(config_dir),
        VAULT_NAME,
        "sync-crm-contacts",
        "sync-crm-contacts",
        "nightly",
        CliRunOptions {
            dry_run_scenario: Some(Some("happy-path".to_owned())),
            record: false,
        },
    )
    .await
}

const ONE_CALL: &str = "[[call]]\nserver = \"contextos\"\ntool = \"fs_read_text_file\"\nkind = \"valid\"\nresponse = { content = \"mocked\" }\n";

#[tokio::test]
async fn a_completed_dry_run_of_a_validated_job_registers_it_for_its_version() -> Result<(), BoxError> {
    let (vault_dir, config, clients) = validated_job_with_scenario(ONE_CALL, true).await?;
    assert!(state_text(vault_dir.path())?.contains("lifecycle = \"validated\""));

    let outcome = dry_run(clients, config.path()).await?;

    assert_eq!(outcome, RunOutcome::Succeeded);
    let state = state_text(vault_dir.path())?;
    assert!(state.contains("lifecycle = \"registered\""), "{state}");
    assert!(state.contains("dry_run_at"), "{state}");
    Ok(())
}

#[tokio::test]
async fn a_dry_run_that_leaves_declared_calls_unconsumed_does_not_register_the_job() -> Result<(), BoxError> {
    let two_calls = format!("{ONE_CALL}\n{ONE_CALL}");
    let (vault_dir, config, clients) = validated_job_with_scenario(&two_calls, true).await?;

    let outcome = dry_run(clients, config.path()).await?;

    assert_eq!(outcome, RunOutcome::Succeeded, "the script itself ran fine");
    assert!(state_text(vault_dir.path())?.contains("lifecycle = \"validated\""));
    Ok(())
}

#[tokio::test]
async fn a_dry_run_that_fails_does_not_register_the_job() -> Result<(), BoxError> {
    let wrong_tool = "[[call]]\nserver = \"contextos\"\ntool = \"vault_info\"\nkind = \"valid\"\nresponse = { content = \"mocked\" }\n";
    let (vault_dir, config, clients) = validated_job_with_scenario(wrong_tool, true).await?;

    let outcome = dry_run(clients, config.path()).await?;

    assert!(matches!(outcome, RunOutcome::Failed(_)), "{outcome:?}");
    assert!(state_text(vault_dir.path())?.contains("lifecycle = \"validated\""));
    Ok(())
}

#[tokio::test]
async fn a_dry_run_of_a_draft_job_runs_but_registers_nothing() -> Result<(), BoxError> {
    let (vault_dir, config, clients) = validated_job_with_scenario(ONE_CALL, false).await?;

    let outcome = dry_run(clients, config.path()).await?;

    assert_eq!(outcome, RunOutcome::Succeeded);
    assert!(
        !vault_dir.path().join(".contextos/jobs.toml").exists()
            || !state_text(vault_dir.path())?.contains("registered"),
        "a draft is not registered by a dry run"
    );
    Ok(())
}

#[tokio::test]
async fn a_dry_run_after_the_release_changed_since_it_was_validated_does_not_register_the_job() -> Result<(), BoxError>
{
    let (vault_dir, config, clients) = validated_job_with_scenario(ONE_CALL, true).await?;
    write(
        vault_dir.path(),
        "registry/workflows/sync-crm-contacts/sync-crm-contacts/workflow.rhai",
        "call_mcp(\"contextos\", \"fs_read_text_file\", #{ path: \"another.md\" });",
    )?;
    // The source only matters once it is installed again as the release the job names.
    support::release_fixture::install_vault_releases(vault_dir.path()).map_err(|error| error.to_string())?;

    let outcome = dry_run(clients, config.path()).await?;

    assert_eq!(outcome, RunOutcome::Succeeded);
    assert!(
        !state_text(vault_dir.path())?.contains("lifecycle = \"registered\""),
        "the version that was dry-run is not the version that was validated"
    );
    Ok(())
}
