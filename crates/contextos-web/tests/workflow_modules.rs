//! A vault workflow split into modules, against a real `contextos-mcp` session and a real temporary vault: validation
//! reads every declared module through the server and checks each, the version covers their text, and a run imports
//! them.

mod support;

use std::path::Path;
use std::sync::Arc;

use contextos_web::McpClientSet;
use contextos_web::cli_run::CliRunOptions;
use contextos_web::job_operation::{OperationOutcome, OperatorRequest, perform};
use contextos_web::job_operation_vault::VaultJobStore;
use contextos_workflows::{RunOutcome, StoredValidation};
use time::OffsetDateTime;
use time::macros::datetime;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";
const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n";
const PACKAGE: &str = "workflows = [\"sync\"]\nmodules = [\"lib/vault.rhai\"]\n";
const ENTRY: &str = "import \"lib/vault\" as vault;\nvault::info();\n";
const MODULE: &str = "fn info() { call_mcp(\"contextos\", \"vault_info\", #{}) }\n";
const JOB: &str = "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";
const NOW: OffsetDateTime = datetime!(2026-10-01 03:00:00 UTC);

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

struct Fixture {
    vault: tempfile::TempDir,
    config: tempfile::TempDir,
    clients: Arc<McpClientSet>,
}

/// A vault whose package `sync` has the workflow `sync` with `entry`, and the modules `PACKAGE` declares, which need not all exist.
async fn fixture(workflow: &str, entry: &str, modules: &[(&str, &str)]) -> Result<Fixture, BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    write(vault.path(), "index.md", "# Root\n")?;
    write(vault.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write(vault.path(), "registry/workflows/ops/workflows.toml", PACKAGE)?;
    write(vault.path(), "registry/workflows/ops/sync/workflow.toml", workflow)?;
    write(vault.path(), "registry/workflows/ops/sync/sync.rhai", entry)?;
    for (path, text) in modules {
        write(vault.path(), &format!("registry/workflows/ops/{path}"), text)?;
    }
    write(vault.path(), "registry/workflows/ops/sync/jobs/nightly.toml", JOB)?;
    support::release_fixture::install_vault_releases(vault.path()).map_err(|error| error.to_string())?;
    let config_path = support::write_vault_config(config.path(), vault.path())?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(Fixture { vault, config, clients })
}

impl Fixture {
    async fn validate(&self) -> Result<StoredValidation, BoxError> {
        let client = self.clients.get("contextos").ok_or("no primary server")?;
        let known = vec!["contextos".to_owned()];
        let store = VaultJobStore {
            client: &client,
            vault_name: VAULT_NAME,
            known_mcp_servers: &known,
            package: "ops",
            workflow_slug: "sync",
            job_slug: "nightly",
            claims_root: None,
        };
        let outcome = perform(&store, OperatorRequest::Validate, NOW).await?;
        let OperationOutcome::Validated { result, .. } = outcome else {
            return Err(format!("expected a validation outcome, got {outcome:?}").into());
        };
        Ok(*result)
    }
}

#[tokio::test]
async fn a_workflow_importing_its_declared_module_passes_and_a_change_to_the_module_released_as_a_later_version_is_a_new_version()
-> Result<(), BoxError> {
    let fixture = fixture(WORKFLOW, ENTRY, &[("lib/vault.rhai", MODULE)]).await?;

    let first = fixture.validate().await?;
    write(
        fixture.vault.path(),
        "registry/workflows/ops/lib/vault.rhai",
        "fn info() { call_mcp(\"contextos\", \"vault_info\", #{ }) }\n",
    )?;
    write(
        fixture.vault.path(),
        "registry/workflows/ops/sync/jobs/nightly.toml",
        &JOB.replace("1.0.0", "2.0.0"),
    )?;
    support::release_fixture::install_vault_releases(fixture.vault.path()).map_err(|error| error.to_string())?;
    let second = fixture.validate().await?;

    assert!(first.passed, "{:?}", first.failures);
    assert!(second.passed, "{:?}", second.failures);
    assert_ne!(
        first.fingerprint, second.fingerprint,
        "the module's text is part of the version"
    );
    Ok(())
}

#[tokio::test]
async fn a_declared_module_that_does_not_exist_fails_validation_naming_it() -> Result<(), BoxError> {
    let fixture = fixture(WORKFLOW, ENTRY, &[]).await?;

    let result = fixture.validate().await?;

    assert!(!result.passed);
    assert!(
        result
            .failures
            .iter()
            .any(|failure| failure.code == "validation/artefact-missing" && failure.artefact == "lib/vault.rhai"),
        "{:?}",
        result.failures
    );
    assert_eq!(
        result.fingerprint, None,
        "a workflow whose code cannot all be read has no version"
    );
    Ok(())
}

#[tokio::test]
async fn a_problem_in_a_module_is_reported_against_that_module_with_its_line() -> Result<(), BoxError> {
    let fixture = fixture(
        WORKFLOW,
        ENTRY,
        &[("lib/vault.rhai", "fn info() { 1 }\n\nlet ran = true;\n")],
    )
    .await?;

    let result = fixture.validate().await?;

    let failure = result
        .failures
        .iter()
        .find(|failure| failure.code == "validation/script-syntax")
        .ok_or("no script failure")?;
    assert_eq!(failure.artefact, "lib/vault.rhai");
    assert!(failure.message.contains("line 3"), "{}", failure.message);
    assert!(failure.message.contains("only functions"), "{}", failure.message);
    Ok(())
}

#[tokio::test]
async fn a_module_path_that_could_leave_the_workflow_fails_the_manifest_of_the_release() -> Result<(), BoxError> {
    let fixture = fixture(
        "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
        "()",
        &[],
    )
    .await?;
    let release = fixture.vault.path().join(".contextos/packages/ops/1.0.0");
    write(
        &release,
        "sync/workflow.toml",
        "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\nmodules = [\"../other/lib.rhai\"]\n",
    )?;
    support::release_fixture::seal_io(&release)?;

    let result = fixture.validate().await?;

    assert!(
        result
            .failures
            .iter()
            .any(|failure| failure.code == "validation/artefact-invalid" && failure.artefact == "sync/workflow.toml"),
        "{:?}",
        result.failures
    );
    Ok(())
}

#[tokio::test]
async fn a_run_imports_the_declared_module_and_its_call_is_made() -> Result<(), BoxError> {
    let fixture = fixture(WORKFLOW, ENTRY, &[("lib/vault.rhai", MODULE)]).await?;
    write(
        fixture.vault.path(),
        ".contextos/jobs.toml",
        "[ops.sync.nightly]\nlifecycle = \"registered\"\n",
    )?;

    let outcome = support::run_job(
        Arc::clone(&fixture.clients),
        &support::host(fixture.config.path()),
        VAULT_NAME,
        "ops",
        "sync",
        "nightly",
        CliRunOptions {
            dry_run_scenario: None,
            record: false,
        },
    )
    .await?;

    assert_eq!(outcome, RunOutcome::Succeeded);
    Ok(())
}
