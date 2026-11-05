//! Static validation of a vault job (`contextos_web::job_validation::validate_job`) against a real `contextos-mcp` session
//! and a real temporary vault: every failure is reported together with its code and the file it concerns, a script is
//! compiled and never run, and the job's version is its artefacts without the `ready` key.

mod support;

use std::path::Path;

use contextos_web::McpClientSet;
use contextos_web::job_checks::OwnerCheck;
use contextos_web::job_validation::{JobValidation, validate_job};
use contextos_web::team::load_roster;
use std::sync::Arc;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";
/// A valid team: `operator` is the one chief of staff.
const TEAM: &str = "[[member]]\nslug = \"operator\"\nname = \"Operator\"\ntype = \"human\"\nchief_of_staff = true\n";
const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n";

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

struct Fixture {
    vault: tempfile::TempDir,
    _config: tempfile::TempDir,
    clients: Arc<McpClientSet>,
}

async fn fixture(workflow: &str, script: Option<&str>, job: &str, team: Option<&str>) -> Result<Fixture, BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    write(vault.path(), "index.md", "# Root\n")?;
    write(
        vault.path(),
        "registry/workflows/ops/workflows.toml",
        "workflows = [\"sync\"]\n",
    )?;
    write(vault.path(), "registry/workflows/ops/sync/workflow.toml", workflow)?;
    if let Some(script) = script {
        write(vault.path(), "registry/workflows/ops/sync/sync.rhai", script)?;
    }
    write(vault.path(), "registry/workflows/ops/sync/jobs/nightly.toml", job)?;
    write(vault.path(), ".contextos/team.toml", team.unwrap_or(TEAM))?;
    support::release_fixture::install_vault_releases(vault.path()).map_err(|error| error.to_string())?;
    let config_path = support::write_vault_config(config.path(), vault.path())?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(Fixture {
        vault,
        _config: config,
        clients,
    })
}

async fn validate(fixture: &Fixture) -> Result<JobValidation, BoxError> {
    let client = fixture
        .clients
        .get("contextos")
        .ok_or("the fixture has no primary server")?;
    let roster = load_roster(&client, VAULT_NAME).await?;
    Ok(validate_job(
        &client,
        VAULT_NAME,
        contextos_web::jobs::WorkflowFolder {
            package: "ops",
            directory: "sync",
        },
        "nightly.toml",
        &["contextos".to_owned()],
        OwnerCheck::Roster(&roster),
    )
    .await?)
}

fn codes(validation: &JobValidation) -> Vec<&'static str> {
    validation
        .report
        .failures
        .iter()
        .map(|failure| failure.code.as_str())
        .collect()
}

#[tokio::test]
async fn a_complete_job_passes_and_has_a_version() -> Result<(), BoxError> {
    let fixture = fixture(
        WORKFLOW,
        Some("log_info(\"hello\");"),
        "name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        None,
    )
    .await?;

    let validation = validate(&fixture).await?;

    assert!(validation.report.passed(), "{:?}", validation.report);
    assert!(validation.fingerprint.is_some());
    assert!(!validation.ready);
    Ok(())
}

#[tokio::test]
async fn a_syntax_error_in_the_script_is_reported_with_its_file_and_line() -> Result<(), BoxError> {
    let fixture = fixture(
        WORKFLOW,
        Some("let x = 1;\nlet y = (2 +;\n"),
        "name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        None,
    )
    .await?;

    let validation = validate(&fixture).await?;

    assert_eq!(codes(&validation), ["validation/script-syntax"]);
    let failure = &validation.report.failures[0];
    assert_eq!(failure.artefact, "sync.rhai");
    assert!(failure.message.contains("line 2"), "{}", failure.message);
    Ok(())
}

#[tokio::test]
async fn every_failure_is_reported_together_not_just_the_first() -> Result<(), BoxError> {
    let fixture = fixture(
        WORKFLOW,
        Some("let y = (2 +;"),
        "name = \"Nightly\"\nowner = \"nobody\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        None,
    )
    .await?;

    let validation = validate(&fixture).await?;

    let found = codes(&validation);
    assert!(found.contains(&"validation/script-syntax"), "{found:?}");
    assert!(found.contains(&"validation/owner-unresolved"), "{found:?}");
    assert!(!validation.report.passed());
    Ok(())
}

#[tokio::test]
async fn a_missing_entry_script_is_reported_as_missing() -> Result<(), BoxError> {
    let fixture = fixture(
        WORKFLOW,
        None,
        "name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        None,
    )
    .await?;

    let validation = validate(&fixture).await?;

    // The release is the only place the entry script is looked for, so it is reported once.
    assert_eq!(codes(&validation), ["validation/artefact-missing"]);
    assert_eq!(validation.report.failures[0].artefact, "sync/sync.rhai");
    assert!(
        validation.fingerprint.is_none(),
        "a job with a file missing has no version to record facts for"
    );
    Ok(())
}

#[tokio::test]
async fn a_workflow_naming_a_server_that_is_not_configured_is_reported_and_its_script_is_still_checked()
-> Result<(), BoxError> {
    let fixture = fixture(
        "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"elsewhere\"]\n",
        Some("let y = (2 +;"),
        "name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        None,
    )
    .await?;

    let validation = validate(&fixture).await?;

    let found = codes(&validation);
    assert!(found.contains(&"validation/server-not-configured"), "{found:?}");
    assert!(found.contains(&"validation/script-syntax"), "{found:?}");
    Ok(())
}

#[tokio::test]
async fn job_values_that_do_not_fit_the_workflows_inputs_are_reported() -> Result<(), BoxError> {
    let fixture = fixture(
        WORKFLOW,
        Some("()"),
        "name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n[inputs]\nundeclared = 1\n",
        None,
    )
    .await?;

    let validation = validate(&fixture).await?;

    assert_eq!(codes(&validation), ["validation/input-invalid"]);
    assert_eq!(validation.report.failures[0].artefact, "nightly.toml");
    Ok(())
}

#[tokio::test]
async fn a_job_file_that_authors_a_schedule_is_reported_as_such() -> Result<(), BoxError> {
    let fixture = fixture(
        WORKFLOW,
        Some("()"),
        "name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nschedule = \"0 3 * * *\"\n",
        None,
    )
    .await?;

    let validation = validate(&fixture).await?;

    assert_eq!(codes(&validation), ["validation/artefact-invalid"]);
    Ok(())
}

#[tokio::test]
async fn validation_never_runs_the_script() -> Result<(), BoxError> {
    let fixture = fixture(
        WORKFLOW,
        Some("call_mcp(\"contextos\", \"fs_write_file\", #{ path: \"written-by-the-script.md\", content: \"x\" });"),
        "name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        None,
    )
    .await?;

    let validation = validate(&fixture).await?;

    assert!(validation.report.passed(), "{:?}", validation.report);
    assert!(
        !fixture.vault.path().join("written-by-the-script.md").exists(),
        "a script that would write a file was compiled, not run"
    );
    Ok(())
}

#[tokio::test]
async fn the_ready_key_is_read_and_is_not_part_of_the_version() -> Result<(), BoxError> {
    let without = fixture(
        WORKFLOW,
        Some("()"),
        "name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        None,
    )
    .await?;
    let with = fixture(
        WORKFLOW,
        Some("()"),
        "ready = true\nname = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        None,
    )
    .await?;

    let a = validate(&without).await?;
    let b = validate(&with).await?;

    assert!(!a.ready);
    assert!(b.ready);
    assert_eq!(a.fingerprint, b.fingerprint);
    Ok(())
}

#[tokio::test]
async fn changing_the_script_changes_the_version() -> Result<(), BoxError> {
    let one = fixture(
        WORKFLOW,
        Some("log_info(\"a\");"),
        "name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        None,
    )
    .await?;
    let two = fixture(
        WORKFLOW,
        Some("log_info(\"b\");"),
        "name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        None,
    )
    .await?;

    assert_ne!(validate(&one).await?.fingerprint, validate(&two).await?.fingerprint);
    Ok(())
}
