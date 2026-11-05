//! The `contextos-web jobs validate`, `dry-run`, `schedule`, `suspend`, `resume`, `retire`, `reactivate`, and `delete`
//! commands as a real process, for a shared job and for a vault job: exit status, the line printed, the `workflow/` code
//! in an error, and the state each leaves behind. The host's data directory is redirected to a temporary directory, so
//! nothing here reads or writes the operator's own.
#![cfg(target_os = "linux")]

mod support;

use std::path::{Path, PathBuf};
use std::process::Output;

use tokio::process::Command;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// A script that survives the failure of its one call, so a dry run against an error scenario completes with a warning
/// and needs no scenario that matches a real tool's output.
const SCRIPT: &str =
    "try { call_mcp(\"contextos\", \"vault_info\", #{}); } catch (e) { flag_degraded(\"the read failed\"); }";
const ERROR_SCENARIO: &str =
    "[[call]]\nserver = \"contextos\"\ntool = \"vault_info\"\nkind = \"error\"\nerror = \"offline\"\n";
const VAULT_WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n";

/// A host with one installed package whose workflow `sync` has the job `nightly`, a vault `va` with a workflow and job of
/// its own, and a web configuration reaching a real `contextos` server.
struct Host {
    root: tempfile::TempDir,
    data: PathBuf,
    web_config: PathBuf,
    vault: PathBuf,
}

fn write(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

impl Host {
    fn new() -> Result<Self, BoxError> {
        let root = tempfile::tempdir()?;
        let data = root.path().join("data");
        let vault = root.path().join("va");
        write(&vault.join("index.md"), "# A\n")?;
        write(
            &vault.join("registry/workflows/ops/workflows.toml"),
            "workflows = [\"sync\"]\n",
        )?;
        write(&vault.join("registry/workflows/ops/sync/workflow.toml"), VAULT_WORKFLOW)?;
        write(&vault.join("registry/workflows/ops/sync/sync.rhai"), SCRIPT)?;
        write(
            &vault.join("registry/workflows/ops/sync/jobs/nightly.toml"),
            "name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        )?;
        write(
            &vault.join(".contextos/team.toml"),
            "[[member]]\nslug = \"operator\"\nname = \"Operator\"\ntype = \"human\"\nchief_of_staff = true\n",
        )?;
        write(
            &vault.join("registry/workflows/ops/sync/tests/offline.toml"),
            ERROR_SCENARIO,
        )?;
        support::release_fixture::install_vault_releases(&vault).map_err(|error| error.to_string())?;

        let package = data.join("contextos/packages/acme/1.0.0");
        write(
            &package.join("manifest.toml"),
            &format!(
                "name = \"acme\"\nversion = \"1.0.0\"\ndigest = \"{DIGEST}\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n\n\
                 [[workflow.permitted_effects]]\nmcp_server_name = \"contextos\"\ntool = \"vault_info\"\n"
            ),
        )?;
        write(&package.join("sync.rhai"), SCRIPT)?;
        write(&package.join("sync/tests/offline.toml"), ERROR_SCENARIO)?;
        support::release_fixture::seal_io(&package)?;
        write(
            &data.join("contextos/workflows/acme/sync/jobs/nightly.toml"),
            "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        )?;

        let mcp_config = support::write_named_vaults_config(root.path(), &[("va", &vault)])?;
        let mcp_binary = support::contextos_mcp_binary()?;
        let web_config = root.path().join("web.toml");
        write(
            &web_config,
            &format!(
                "[[mcp_server]]\ntransport = \"stdio\"\nname = \"contextos\"\ncommand = {:?}\nargs = [\"--config\", {:?}]\n",
                mcp_binary.to_string_lossy(),
                mcp_config.to_string_lossy()
            ),
        )?;
        Ok(Self {
            root,
            data,
            web_config,
            vault,
        })
    }

    fn command(&self, config: &Path, arguments: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_contextos-web"));
        command
            .arg("--config")
            .arg(config)
            .args(arguments)
            .env("XDG_DATA_HOME", &self.data)
            .env("HOME", &self.data)
            .env_remove("CONTEXTOS_WEB_CONFIG");
        command
    }

    async fn run(&self, arguments: &[String]) -> Result<Output, BoxError> {
        let borrowed: Vec<&str> = arguments.iter().map(String::as_str).collect();
        Ok(self.command(&self.web_config, &borrowed).output().await?)
    }

    /// Runs a command against a web configuration that does not exist, to show it never needed one.
    async fn run_without_config(&self, arguments: &[String]) -> Result<Output, BoxError> {
        let missing = self.root.path().join("no-such-web.toml");
        let borrowed: Vec<&str> = arguments.iter().map(String::as_str).collect();
        Ok(self.command(&missing, &borrowed).output().await?)
    }

    fn shared_state(&self) -> Result<contextos_workflows::JobsState, BoxError> {
        Ok(contextos_web::shared_state::SharedStateStore::new(self.data.join("contextos/state")).read()?)
    }

    fn shared_lifecycle(&self) -> Result<String, BoxError> {
        Ok(self
            .shared_state()?
            .lifecycle_of("acme", "sync", "nightly")
            .as_str()
            .to_owned())
    }

    fn vault_state(&self) -> Result<contextos_workflows::JobsState, BoxError> {
        let path = self.vault.join(".contextos/jobs.toml");
        if !path.exists() {
            return Ok(contextos_workflows::JobsState::default());
        }
        Ok(toml::from_str(&std::fs::read_to_string(path)?)?)
    }

    fn vault_lifecycle(&self) -> Result<String, BoxError> {
        Ok(self
            .vault_state()?
            .lifecycle_of("ops", "sync", "nightly")
            .as_str()
            .to_owned())
    }

    /// Succeeds only when the command does, with its output for the caller to inspect.
    async fn succeed(&self, arguments: &[String]) -> Result<Output, BoxError> {
        let output = self.run(arguments).await?;
        if !output.status.success() {
            return Err(format!("{arguments:?} failed: {}", stderr(&output)).into());
        }
        Ok(output)
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// `jobs {command} --shared acme sync nightly {extra}`.
fn shared(command: &str, extra: &[&str]) -> Vec<String> {
    let mut arguments: Vec<String> = ["jobs", command, "--shared", "acme", "sync", "nightly"]
        .iter()
        .map(|part| (*part).to_owned())
        .collect();
    arguments.extend(extra.iter().map(|part| (*part).to_owned()));
    arguments
}

/// `jobs {command} va sync nightly {extra}`.
fn vault(command: &str, extra: &[&str]) -> Vec<String> {
    let mut arguments: Vec<String> = ["jobs", command, "va", "ops", "sync", "nightly"]
        .iter()
        .map(|part| (*part).to_owned())
        .collect();
    arguments.extend(extra.iter().map(|part| (*part).to_owned()));
    arguments
}

/// Takes the shared job to `registered` through the real commands: validation, then a dry run.
async fn register_shared(host: &Host) -> Result<(), BoxError> {
    host.succeed(&shared("validate", &[])).await?;
    host.succeed(&shared("dry-run", &[])).await?;
    assert_eq!(host.shared_lifecycle()?, "registered");
    Ok(())
}

async fn register_vault(host: &Host) -> Result<(), BoxError> {
    host.succeed(&vault("validate", &[])).await?;
    host.succeed(&vault("dry-run", &[])).await?;
    assert_eq!(host.vault_lifecycle()?, "registered");
    Ok(())
}

#[tokio::test]
async fn validating_a_shared_job_prints_its_state_and_records_the_result() -> Result<(), BoxError> {
    let host = Host::new()?;

    let output = host.succeed(&shared("validate", &[])).await?;

    let text = stdout(&output);
    assert!(
        text.contains("Validation passed for shared job acme/sync/nightly"),
        "{text}"
    );
    assert!(text.contains("It is validated"), "{text}");
    assert_eq!(host.shared_lifecycle()?, "validated");
    Ok(())
}

#[tokio::test]
async fn validating_a_shared_job_that_fails_exits_unsuccessfully_listing_every_failure() -> Result<(), BoxError> {
    let host = Host::new()?;
    write(
        &host.data.join("contextos/packages/acme/1.0.0/sync/sync.rhai"),
        "let y = (2 +;",
    )?;
    support::release_fixture::seal_io(&host.data.join("contextos/packages/acme/1.0.0"))?;

    let output = host.run(&shared("validate", &[])).await?;

    assert!(!output.status.success());
    let error = stderr(&output);
    assert!(error.contains("workflow/validation-failed"), "{error}");
    assert!(error.contains("validation/script-syntax"), "{error}");
    assert_eq!(host.shared_lifecycle()?, "draft");
    Ok(())
}

#[tokio::test]
async fn a_shared_job_goes_through_schedule_suspend_resume_retire_reactivate_and_delete() -> Result<(), BoxError> {
    let host = Host::new()?;
    register_shared(&host).await?;

    let enabled = host.succeed(&shared("schedule", &["0 3 * * *"])).await?;
    assert!(stdout(&enabled).contains("is now enabled."), "{}", stdout(&enabled));
    assert_eq!(
        host.shared_state()?.scheduled_expression("acme", "sync", "nightly"),
        Some("0 3 * * *")
    );

    host.succeed(&shared("suspend", &["--reason", "the upstream API is down"]))
        .await?;
    assert_eq!(host.shared_lifecycle()?, "suspended");
    let state = host.shared_state()?;
    let suspension = state
        .job("acme", "sync", "nightly")
        .and_then(contextos_workflows::JobState::suspension);
    assert_eq!(
        suspension.map(|suspension| suspension.code.as_str()),
        Some("workflow/operator-suspended")
    );
    assert_eq!(
        suspension.and_then(|suspension| suspension.by),
        Some(contextos_workflows::Actor::Operator)
    );
    assert_eq!(
        suspension.map(|suspension| suspension.reason.as_str()),
        Some("the upstream API is down")
    );

    host.succeed(&shared("resume", &[])).await?;
    assert_eq!(host.shared_lifecycle()?, "enabled");

    host.succeed(&shared("retire", &[])).await?;
    assert_eq!(host.shared_lifecycle()?, "retired");

    host.succeed(&shared("reactivate", &[])).await?;
    assert_eq!(host.shared_lifecycle()?, "draft");

    register_shared(&host).await?;
    host.succeed(&shared("schedule", &["0 4 * * *"])).await?;
    host.succeed(&shared("retire", &[])).await?;
    host.succeed(&shared("delete", &[])).await?;
    assert!(host.shared_state()?.job("acme", "sync", "nightly").is_none());
    Ok(())
}

#[tokio::test]
async fn a_dry_run_command_registers_a_validated_job_and_says_its_results_were_mocked() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.succeed(&shared("validate", &[])).await?;

    let output = host.succeed(&shared("dry-run", &[])).await?;

    assert!(
        stdout(&output).contains("Scenario: offline (call results were mocked, so nothing was written)\n"),
        "{}",
        stdout(&output)
    );
    assert!(
        stdout(&output).contains("Registration: registered\n"),
        "{}",
        stdout(&output)
    );
    assert_eq!(host.shared_lifecycle()?, "registered");
    Ok(())
}

#[tokio::test]
async fn a_refused_command_exits_unsuccessfully_with_the_code_the_state_and_what_would_allow_it() -> Result<(), BoxError>
{
    let host = Host::new()?;

    let output = host.run(&shared("schedule", &["0 3 * * *"])).await?;

    assert!(!output.status.success());
    let error = stderr(&output);
    assert!(error.contains("workflow/transition-refused"), "{error}");
    assert!(error.contains("draft") && error.contains("registered"), "{error}");
    assert!(
        !host.data.join("contextos/state/shared-jobs.toml").exists(),
        "nothing was written"
    );
    Ok(())
}

#[tokio::test]
async fn a_schedule_that_is_not_a_cron_expression_is_refused_and_changes_nothing() -> Result<(), BoxError> {
    let host = Host::new()?;
    register_shared(&host).await?;

    let output = host.run(&shared("schedule", &["every tuesday"])).await?;

    assert!(!output.status.success());
    assert_eq!(host.shared_lifecycle()?, "registered");
    Ok(())
}

#[tokio::test]
async fn clearing_a_schedule_returns_an_enabled_job_to_registered() -> Result<(), BoxError> {
    let host = Host::new()?;
    register_shared(&host).await?;
    host.succeed(&shared("schedule", &["0 3 * * *"])).await?;

    host.succeed(&shared("schedule", &["--clear"])).await?;

    assert_eq!(host.shared_lifecycle()?, "registered");
    Ok(())
}

#[tokio::test]
async fn a_schedule_command_needs_an_expression_or_clear_and_not_both() -> Result<(), BoxError> {
    let host = Host::new()?;
    register_shared(&host).await?;

    for extra in [&[][..], &["0 3 * * *", "--clear"][..]] {
        let output = host.run(&shared("schedule", extra)).await?;
        assert!(!output.status.success(), "{extra:?}");
    }
    assert_eq!(host.shared_lifecycle()?, "registered");
    Ok(())
}

#[tokio::test]
async fn a_suspension_without_a_reason_is_refused_by_the_command() -> Result<(), BoxError> {
    let host = Host::new()?;
    register_shared(&host).await?;
    host.succeed(&shared("schedule", &["0 3 * * *"])).await?;

    let output = host.run(&shared("suspend", &[])).await?;

    assert!(!output.status.success());
    assert_eq!(host.shared_lifecycle()?, "enabled");
    Ok(())
}

#[tokio::test]
async fn a_shared_job_is_suspended_and_retired_with_no_web_configuration_at_all() -> Result<(), BoxError> {
    let host = Host::new()?;
    register_shared(&host).await?;
    host.succeed(&shared("schedule", &["0 3 * * *"])).await?;

    for (command, extra, expected) in [
        ("suspend", vec!["--reason", "held"], "suspended"),
        ("retire", vec![], "retired"),
    ] {
        let output = host.run_without_config(&shared(command, &extra)).await?;
        assert!(output.status.success(), "{command}: {}", stderr(&output));
        assert_eq!(host.shared_lifecycle()?, expected);
    }
    Ok(())
}

#[tokio::test]
async fn resuming_a_shared_job_whose_release_no_longer_validates_is_refused_and_it_stays_suspended()
-> Result<(), BoxError> {
    let host = Host::new()?;
    register_shared(&host).await?;
    host.succeed(&shared("schedule", &["0 3 * * *"])).await?;
    host.succeed(&shared("suspend", &["--reason", "held"])).await?;
    write(
        &host.data.join("contextos/packages/acme/1.0.0/sync/sync.rhai"),
        "let y = (2 +;",
    )?;
    support::release_fixture::seal_io(&host.data.join("contextos/packages/acme/1.0.0"))?;

    let output = host.run(&shared("resume", &[])).await?;

    assert!(!output.status.success());
    let error = stderr(&output);
    assert!(error.contains("workflow/validation-failed"), "{error}");
    assert!(error.contains("validation/script-syntax"), "{error}");
    assert_eq!(host.shared_lifecycle()?, "suspended");
    Ok(())
}

#[tokio::test]
async fn a_vault_job_goes_through_schedule_suspend_resume_retire_reactivate_and_delete() -> Result<(), BoxError> {
    let host = Host::new()?;

    host.succeed(&vault("validate", &[])).await?;
    assert_eq!(host.vault_lifecycle()?, "validated");
    let early = host.run(&vault("schedule", &["0 3 * * *"])).await?;
    assert!(!early.status.success(), "a validated job is not yet registered");
    assert!(stderr(&early).contains("registered"), "{}", stderr(&early));

    host.succeed(&vault("dry-run", &[])).await?;
    assert_eq!(host.vault_lifecycle()?, "registered");

    let enabled = host.succeed(&vault("schedule", &["0 3 * * *"])).await?;
    assert!(
        stdout(&enabled).contains("job ops/sync/nightly of vault va is now enabled."),
        "{}",
        stdout(&enabled)
    );
    assert_eq!(
        host.vault_state()?.scheduled_expression("ops", "sync", "nightly"),
        Some("0 3 * * *")
    );

    host.succeed(&vault("suspend", &["--reason", "held"])).await?;
    assert_eq!(host.vault_lifecycle()?, "suspended");
    host.succeed(&vault("resume", &[])).await?;
    assert_eq!(host.vault_lifecycle()?, "enabled");
    host.succeed(&vault("retire", &[])).await?;
    assert_eq!(host.vault_lifecycle()?, "retired");
    host.succeed(&vault("reactivate", &[])).await?;
    assert_eq!(host.vault_lifecycle()?, "draft");

    register_vault(&host).await?;
    host.succeed(&vault("schedule", &["0 4 * * *"])).await?;
    host.succeed(&vault("retire", &[])).await?;
    host.succeed(&vault("delete", &[])).await?;
    assert!(host.vault_state()?.job("ops", "sync", "nightly").is_none());
    Ok(())
}

#[tokio::test]
async fn a_vault_job_that_does_not_exist_is_reported_as_not_found_and_nothing_is_written() -> Result<(), BoxError> {
    let host = Host::new()?;

    let output = host
        .run(&["jobs", "validate", "va", "ops", "sync", "no-such-job"].map(str::to_owned))
        .await?;

    assert!(!output.status.success());
    let error = stderr(&output);
    assert!(
        error.contains("workflow/job-not-found") && error.contains("no-such-job"),
        "{error}"
    );
    assert!(!host.vault.join(".contextos/jobs.toml").exists());
    Ok(())
}

/// The path and bytes of every file at or beneath `folder`, sorted by path, so a record that changed is told from one
/// that did not.
fn run_record_files(folder: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap_or_default();
                found.push((path, bytes));
            }
        }
    }
    found.sort();
    found
}

/// The number the next live run of the vault job `va`/`sync`/`nightly` would take, read from the host's own bookkeeping.
fn next_run_number(host: &Host) -> Result<u64, BoxError> {
    let context = support::host(&host.data.join("contextos"));
    let slot = contextos_web::vault_runs::claim_slot(&context, "va", "ops", "sync", "nightly")?;
    Ok(slot.peek_run_number()?)
}

const TEAM_WITH_VELA: &str = "[[member]]\nslug = \"atlas\"\nname = \"Atlas\"\nchief_of_staff = true\n\n\
                              [[member]]\nslug = \"vela\"\nname = \"Vela\"\n";
const TEAM_WITHOUT_VELA: &str = "[[member]]\nslug = \"atlas\"\nname = \"Atlas\"\nchief_of_staff = true\n";
const OWNED_JOB: &str = "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";

#[tokio::test]
async fn jobs_run_of_a_job_whose_owner_left_is_refused_with_owner_unresolved_and_writes_nothing() -> Result<(), BoxError>
{
    let host = Host::new()?;
    write(&host.vault.join(".contextos/team.toml"), TEAM_WITH_VELA)?;
    write(
        &host.vault.join("registry/workflows/ops/sync/jobs/nightly.toml"),
        OWNED_JOB,
    )?;
    register_vault(&host).await?;
    // The owner leaves the team after the job was registered.
    write(&host.vault.join(".contextos/team.toml"), TEAM_WITHOUT_VELA)?;
    let state_before = std::fs::read(host.vault.join(".contextos/jobs.toml"))?;
    // Registering the job dry-ran it, which left a record of its own, so what must not change is that folder's content.
    let records_before = run_record_files(&host.vault.join("memory/jobs/ops/sync/nightly"));
    let next_number_before = next_run_number(&host)?;

    let output = host.run(&vault("run", &["--format", "json"])).await?;

    let summary: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "standard output is not one JSON document ({error}): {}",
            stdout(&output)
        )
    })?;
    assert_eq!(output.status.code(), Some(3), "a refusal exits 3: {}", stderr(&output));
    assert_eq!(summary["outcome"], "refused", "{summary}");
    assert_eq!(summary["code"], "workflow/owner-unresolved", "{summary}");
    assert_eq!(
        std::fs::read(host.vault.join(".contextos/jobs.toml"))?,
        state_before,
        "the refusal changes no job state, and the job keeps its lifecycle"
    );
    assert_eq!(host.vault_lifecycle()?, "registered");
    assert_eq!(
        run_record_files(&host.vault.join("memory/jobs/ops/sync/nightly")),
        records_before,
        "no run record is written or changed"
    );
    assert_eq!(
        next_run_number(&host)?,
        next_number_before,
        "the refusal came before a run number was taken"
    );

    let dry = host.run(&vault("dry-run", &["--format", "json"])).await?;
    assert_eq!(
        dry.status.code(),
        Some(0),
        "a dry run does not need an owner: {}",
        stderr(&dry)
    );
    let dry_summary: serde_json::Value = serde_json::from_slice(&dry.stdout)?;
    assert_eq!(dry_summary["outcome"], "degraded", "the dry run ran: {dry_summary}");
    Ok(())
}

#[tokio::test]
async fn jobs_list_shows_the_suspension_code_and_run() -> Result<(), BoxError> {
    let host = Host::new()?;
    write(
        &host.data.join("contextos/workflows/acme/sync/jobs/nightly.toml"),
        "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    write(
        &host.data.join("contextos/state/shared-jobs.toml"),
        "[acme.sync.nightly]\nlifecycle = \"suspended\"\nschedule = \"0 3 * * *\"\n\
         first_run = true\nlifecycle_changed = \"2026-10-01T03:00:00Z\"\nlifecycle_actor = \"system\"\n\n\
         [acme.sync.nightly.suspension]\ncode = \"workflow/probation-failed\"\n\
         by = \"system\"\nat = \"2026-10-01T03:00:00Z\"\nreason = \"the first live run failed\"\nrun = 4\n\
         failure = \"workflow/script-error\"\n",
    )?;

    let json = host
        .succeed(&["jobs", "list", "--shared", "--format", "json"].map(str::to_owned))
        .await?;
    let listed: serde_json::Value = serde_json::from_slice(&json.stdout)?;
    let row = &listed["jobs"][0];
    assert_eq!(row["state"], "suspended", "{listed}");
    assert_eq!(row["suspension"]["code"], "workflow/probation-failed", "{listed}");
    assert_eq!(row["suspension"]["run"], 4, "{listed}");
    assert_eq!(row["suspension"]["by"], "system", "{listed}");
    assert_eq!(row["suspension"]["reason"], "the first live run failed", "{listed}");
    assert_eq!(row["suspension"]["failure"], "workflow/script-error", "{listed}");

    let text = host.succeed(&["jobs", "list", "--shared"].map(str::to_owned)).await?;
    let lines = stdout(&text);
    assert!(lines.contains("workflow/probation-failed"), "{lines}");
    assert!(
        lines.contains("R-4") || lines.contains("run 4"),
        "the run that failed is named: {lines}"
    );
    for shown in [
        "system",
        "2026-10-01",
        "workflow/script-error",
        "the first live run failed",
    ] {
        assert!(lines.contains(shown), "the listing lacks {shown:?}: {lines}");
    }
    Ok(())
}

const TEAM_WITHOUT_CHIEF: &str =
    "[[member]]\nslug = \"atlas\"\nname = \"Atlas\"\n\n[[member]]\nslug = \"vela\"\nname = \"Vela\"\n";

#[tokio::test]
async fn jobs_validate_while_the_team_is_invalid_reports_the_owner_check_as_unverifiable_and_records_no_defect()
-> Result<(), BoxError> {
    // A usable team first, so the unverifiable result below is the team's doing and not the fixture's.
    let control = Host::new()?;
    write(&control.vault.join(".contextos/team.toml"), TEAM_WITH_VELA)?;
    write(
        &control.vault.join("registry/workflows/ops/sync/jobs/nightly.toml"),
        OWNED_JOB,
    )?;
    control.succeed(&vault("validate", &[])).await?;
    assert_eq!(control.vault_lifecycle()?, "validated");

    let fresh = Host::new()?;
    write(&fresh.vault.join(".contextos/team.toml"), TEAM_WITHOUT_CHIEF)?;
    write(
        &fresh.vault.join("registry/workflows/ops/sync/jobs/nightly.toml"),
        OWNED_JOB,
    )?;

    let state_before = std::fs::read_to_string(fresh.vault.join(".contextos/jobs.toml")).ok();

    let output = fresh.run(&vault("validate", &[])).await?;

    let report = format!("{}{}", stdout(&output), stderr(&output));
    assert!(
        !output.status.success(),
        "a check that could not be made is not a pass: {report}"
    );
    assert!(
        report.contains("unverifiable"),
        "the owner check is reported as unverifiable: {report}"
    );
    assert!(
        !report.contains("validation/owner-unresolved"),
        "an outage is not recorded as the owner's defect: {report}"
    );
    assert_eq!(fresh.vault_lifecycle()?, "draft", "the job does not become validated");
    let stored = std::fs::read_to_string(fresh.vault.join(".contextos/jobs.toml")).unwrap_or_default();
    assert!(!stored.contains("validation/owner-unresolved"), "{stored}");
    assert_eq!(
        std::fs::read_to_string(fresh.vault.join(".contextos/jobs.toml")).ok(),
        state_before,
        "no validation result is stored while the owner check cannot be made, so jobs.toml is as it was"
    );
    Ok(())
}

#[tokio::test]
async fn jobs_resume_while_the_team_is_invalid_reports_the_owner_check_as_unverifiable_and_leaves_the_job_suspended()
-> Result<(), BoxError> {
    let host = Host::new()?;
    write(&host.vault.join(".contextos/team.toml"), TEAM_WITH_VELA)?;
    write(
        &host.vault.join("registry/workflows/ops/sync/jobs/nightly.toml"),
        OWNED_JOB,
    )?;
    register_vault(&host).await?;
    host.succeed(&vault("schedule", &["0 3 * * *"])).await?;
    host.succeed(&vault("suspend", &["--reason", "held"])).await?;
    assert_eq!(host.vault_lifecycle()?, "suspended");
    write(&host.vault.join(".contextos/team.toml"), TEAM_WITHOUT_CHIEF)?;
    let state_before = std::fs::read_to_string(host.vault.join(".contextos/jobs.toml"))?;

    let output = host.run(&vault("resume", &[])).await?;

    let report = format!("{}{}", stdout(&output), stderr(&output));
    assert!(
        !output.status.success(),
        "a resume that cannot be checked is refused: {report}"
    );
    assert!(
        report.contains("unverifiable"),
        "the owner check is reported as unverifiable: {report}"
    );
    assert!(
        !report.contains("validation/owner-unresolved"),
        "an outage is not recorded as the owner's defect: {report}"
    );
    assert_eq!(host.vault_lifecycle()?, "suspended", "the job stays suspended");
    let stored = std::fs::read_to_string(host.vault.join(".contextos/jobs.toml"))?;
    assert!(!stored.contains("validation/owner-unresolved"), "{stored}");
    assert_eq!(
        std::fs::read_to_string(host.vault.join(".contextos/jobs.toml"))?,
        state_before,
        "the refused resume stores no validation result and changes no state"
    );
    Ok(())
}

/// A suspension the system recorded for a failed first run, as the state file of a vault or a shared host stores it.
const PROBATION_FAILED: &str = "code = \"workflow/probation-failed\"\nby = \"system\"\nat = \"2026-10-01T03:00:00Z\"\n\
     reason = \"the first live run failed\"\nrun = 4\nfailure = \"workflow/script-error\"\n";

fn assert_suspension_in_text(report: &str, whose: &str) {
    for shown in [
        "workflow/probation-failed",
        "system",
        "2026-10-01",
        "the first live run failed",
    ] {
        assert!(report.contains(shown), "{whose}: the refusal lacks {shown:?}: {report}");
    }
    assert!(
        report.contains("R-4") || report.contains("run 4"),
        "{whose}: the refusal names the run that failed: {report}"
    );
}

/// The text a refused command reports, for each of the commands that refuse a job in the state `suspended`.
async fn refusals_of_a_suspended_job(
    host: &Host,
    command: fn(&str, &[&str]) -> Vec<String>,
) -> Result<Vec<String>, BoxError> {
    let mut reports = Vec::new();
    for (verb, extra) in [("reactivate", Vec::new()), ("suspend", vec!["--reason", "again"])] {
        let output = host.run(&command(verb, &extra)).await?;
        assert!(!output.status.success(), "{verb} of a suspended job is refused");
        let report = format!("{}{}", stdout(&output), stderr(&output));
        assert!(report.contains("suspended"), "{verb} names the state: {report}");
        reports.push(report);
    }
    Ok(reports)
}

#[tokio::test]
async fn a_refusal_that_names_a_suspended_vault_job_carries_the_suspension_record() -> Result<(), BoxError> {
    let host = Host::new()?;
    register_vault(&host).await?;
    host.succeed(&vault("schedule", &["0 3 * * *"])).await?;
    let state = std::fs::read_to_string(host.vault.join(".contextos/jobs.toml"))?
        .replace("lifecycle = \"enabled\"", "lifecycle = \"suspended\"");
    write(
        &host.vault.join(".contextos/jobs.toml"),
        &format!("{state}\n[ops.sync.nightly.suspension]\n{PROBATION_FAILED}"),
    )?;
    assert_eq!(host.vault_lifecycle()?, "suspended");

    for report in refusals_of_a_suspended_job(&host, vault).await? {
        assert_suspension_in_text(&report, "vault");
    }
    Ok(())
}

#[tokio::test]
async fn a_refusal_that_names_a_suspended_shared_job_carries_the_suspension_record() -> Result<(), BoxError> {
    let host = Host::new()?;
    write(
        &host.data.join("contextos/state/shared-jobs.toml"),
        &format!(
            "[acme.sync.nightly]\nlifecycle = \"suspended\"\nschedule = \"0 3 * * *\"\n\
             first_run = true\nlifecycle_changed = \"2026-10-01T03:00:00Z\"\nlifecycle_actor = \"system\"\n\n\
             [acme.sync.nightly.suspension]\n{PROBATION_FAILED}"
        ),
    )?;

    for report in refusals_of_a_suspended_job(&host, shared).await? {
        assert_suspension_in_text(&report, "shared");
    }
    Ok(())
}
