//! The `contextos-web jobs run --shared` and `workflows run` commands as a real process: exit status, the success
//! line, and the `workflow/` code in the error. The host's data directory is redirected to a temporary directory,
//! so nothing here reads or writes the operator's own.
#![cfg(target_os = "linux")]

mod support;

use std::path::{Path, PathBuf};
use std::process::Output;

use tokio::process::Command;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// A workflow whose script survives the failure of its one call, so a dry run against an error scenario completes with a
/// warning and needs no scenario that matches a real tool's output.
const TOLERANT_SCRIPT: &str = "try { call_mcp(\"contextos\", \"fs_read_text_file\", #{ path: \"va://index.md\" }); } catch (e) { flag_degraded(\"unreadable\"); }";
const ERROR_SCENARIO: &str =
    "[[call]]\nserver = \"contextos\"\ntool = \"fs_read_text_file\"\nkind = \"error\"\nerror = \"offline\"\n";

/// A host with one installed package, two vaults, and a web configuration reaching a real `contextos` server.
struct Host {
    _root: tempfile::TempDir,
    data: PathBuf,
    web_config: PathBuf,
    vault_a: PathBuf,
}

/// Takes `acme`'s job `nightly` of `workflow` to `registered` in the host state, as validation and a dry run would, so it
/// may be run by hand.
fn register(data: &Path, workflow: &str, job_toml: &str) -> Result<(), BoxError> {
    use contextos_workflows::{Actor, Transition};
    let version = contextos_web::shared_version::shared_job_version(&data.join("contextos/packages"), "acme", job_toml)
        .ok_or("the job has no version")?;
    let at = time::macros::datetime!(2026-10-01 03:00:00 UTC);
    contextos_web::shared_state::SharedStateStore::new(data.join("contextos/state")).update(
        |jobs| -> Result<(), contextos_workflows::TransitionError> {
            jobs.transition(
                "acme",
                workflow,
                "nightly",
                Actor::Validator,
                &Transition::Validated(version.clone()),
                at,
            )?;
            jobs.transition(
                "acme",
                workflow,
                "nightly",
                Actor::DryRun,
                &Transition::DryRunCompleted(version),
                at,
            )?;
            Ok(())
        },
    )??;
    Ok(())
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
        let vault_a = root.path().join("va");
        let vault_b = root.path().join("vb");
        write(&vault_a.join("index.md"), "# A\n")?;
        write(&vault_b.join("index.md"), "# B\n")?;

        let package = data.join("contextos/packages/acme/1.0.0");
        write(
            &package.join("manifest.toml"),
            &format!(
                "name = \"acme\"\nversion = \"1.0.0\"\ndigest = \"{DIGEST}\"\n\n[[workflow]]\nslug = \"copy\"\nentry = \"copy.rhai\"\n\n\
                 [[workflow.inputs]]\nname = \"source\"\nkind = \"vault\"\n\n\
                 [[workflow.permitted_effects]]\nmcp_server_name = \"contextos\"\ntool = \"fs_read_text_file\"\n\n\
                 [[workflow]]\nslug = \"spin\"\nentry = \"spin.rhai\"\n\n\
                 [[workflow]]\nslug = \"tolerant\"\nentry = \"tolerant.rhai\"\n\n\
                 [[workflow.permitted_effects]]\nmcp_server_name = \"contextos\"\ntool = \"fs_read_text_file\"\n"
            ),
        )?;
        write(&package.join("tolerant.rhai"), TOLERANT_SCRIPT)?;
        write(
            &data.join("contextos/workflows/acme/tolerant/jobs/nightly.toml"),
            "name = \"Tolerant\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        )?;
        write(&package.join("spin.rhai"), "loop {}")?;
        write(
            &data.join("contextos/workflows/acme/spin/jobs/nightly.toml"),
            "name = \"Spin\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        )?;
        write(
            &package.join("copy.rhai"),
            "call_mcp(\"contextos\", \"fs_read_text_file\", #{ path: inputs.source + \"://index.md\" });",
        )?;
        write(
            &data.join("contextos/workflows/acme/copy/jobs/nightly.toml"),
            "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n[inputs]\nsource = \"va\"\n",
        )?;

        support::release_fixture::seal_io(&package)?;

        for (workflow, job) in [
            (
                "copy",
                "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n[inputs]\nsource = \"va\"\n",
            ),
            ("spin", "name = \"Spin\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n"),
        ] {
            register(&data, workflow, job)?;
        }

        let mcp_config = support::write_named_vaults_config(root.path(), &[("va", &vault_a), ("vb", &vault_b)])?;
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
            _root: root,
            data,
            web_config,
            vault_a,
        })
    }

    fn command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_contextos-web"));
        command
            .arg("--config")
            .arg(&self.web_config)
            .args(arguments)
            .env("XDG_DATA_HOME", &self.data)
            .env("HOME", &self.data)
            .env_remove("CONTEXTOS_WEB_CONFIG");
        command
    }

    async fn run(&self, arguments: &[&str]) -> Result<Output, BoxError> {
        Ok(self.command(arguments).output().await?)
    }

    /// Installs a copy of release 1.0.0 as `version`, sealed so that it is whole.
    fn add_release(&self, version: &str) -> Result<(), BoxError> {
        let store = self.data.join("contextos/packages/acme");
        let target = store.join(version);
        copy_folder(&store.join("1.0.0"), &target)?;
        let manifest = std::fs::read_to_string(target.join("manifest.toml"))?;
        write(
            &target.join("manifest.toml"),
            &manifest.replace("version = \"1.0.0\"", &format!("version = \"{version}\"")),
        )?;
        Ok(support::release_fixture::seal_io(&target)?)
    }

    fn claims_root(&self) -> PathBuf {
        self.data.join("contextos/claims")
    }

    /// Holds the claim of the `acme` / `copy` / `nightly` job, as a live run would.
    fn hold_claim(&self) -> Result<contextos_web::shared_claim::SharedClaim, BoxError> {
        Ok(contextos_web::shared_claim::SharedClaim::acquire(
            &self.claims_root(),
            &contextos_web::shared_claim::SharedJobKey {
                package: "acme".to_owned(),
                workflow: "copy".to_owned(),
                job: "nightly".to_owned(),
            },
        )?)
    }

    /// The text of run `id`'s record for `workflow`'s `nightly` job, wherever the date put it.
    fn record(&self, workflow: &str, id: u64) -> Option<String> {
        let job_dir = self.data.join(format!("contextos/runs/acme/{workflow}/nightly"));
        let name = format!("R-{id}.md");
        std::fs::read_dir(&job_dir).ok()?.flatten().find_map(|year| {
            std::fs::read_dir(year.path())
                .ok()?
                .flatten()
                .find_map(|month| std::fs::read_to_string(month.path().join(&name)).ok())
        })
    }
}

fn copy_folder(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_folder(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[tokio::test]
async fn a_saved_shared_job_prints_the_success_line_leaves_a_record_and_a_free_claim() -> Result<(), BoxError> {
    let host = Host::new()?;

    let output = host
        .run(&["jobs", "run", "--shared", "acme", "copy", "nightly"])
        .await?;

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("Run succeeded."), "{}", stdout(&output));
    let record = host.record("copy", 1).ok_or("the run left no record")?;
    assert!(record.contains("status: succeeded\n"), "{record}");
    assert!(
        record.contains("| 1 | contextos | fs_read_text_file | succeeded |"),
        "{record}"
    );
    assert!(
        record.contains("inputs: [\"source\"]\n"),
        "an input is named, and its value never recorded: {record}"
    );
    let again = host
        .run(&["jobs", "run", "--shared", "acme", "copy", "nightly"])
        .await?;
    assert!(
        again.status.success(),
        "the claim is free for the next run: {}",
        stderr(&again)
    );
    assert!(host.record("copy", 2).is_some());
    Ok(())
}

#[tokio::test]
async fn a_claim_held_by_a_live_run_exits_75_naming_job_busy_and_not_the_claim_path() -> Result<(), BoxError> {
    let host = Host::new()?;
    let _held = host.hold_claim()?;

    let output = host
        .run(&["jobs", "run", "--shared", "acme", "copy", "nightly"])
        .await?;

    assert_eq!(output.status.code(), Some(75), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("Code: workflow/job-busy"),
        "{}",
        stdout(&output)
    );
    assert!(!stdout(&output).contains("nightly.lock"), "{}", stdout(&output));
    assert!(!stderr(&output).contains("nightly.lock"), "{}", stderr(&output));
    assert!(
        host.record("copy", 1).is_none(),
        "a run that never started leaves no record"
    );
    Ok(())
}

#[tokio::test]
async fn a_claim_file_left_by_a_dead_run_does_not_stop_the_next() -> Result<(), BoxError> {
    let host = Host::new()?;
    write(&host.claims_root().join("acme/copy/nightly.lock"), "")?;

    let output = host
        .run(&["jobs", "run", "--shared", "acme", "copy", "nightly"])
        .await?;

    assert!(output.status.success(), "{}", stderr(&output));
    Ok(())
}

#[tokio::test]
async fn interrupting_a_shared_run_stops_it_records_it_cancelled_and_frees_its_claim() -> Result<(), BoxError> {
    let host = Host::new()?;
    let child = host
        .command(&["jobs", "run", "--shared", "acme", "spin", "nightly"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    // The record is written, `running`, before the script starts, so it is the signal that the run is under way.
    let mut running = false;
    for _ in 0..400 {
        if host
            .record("spin", 1)
            .is_some_and(|text| text.contains("status: running\n"))
        {
            running = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(running, "the run never started");
    let pid = child.id().ok_or("the run has no process id")?;
    let signalled = std::process::Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .status()?;
    assert!(signalled.success());

    let output = child.wait_with_output().await?;

    assert_eq!(output.status.code(), Some(130), "{}", stderr(&output));
    assert!(stdout(&output).contains("Run cancelled."), "{}", stdout(&output));
    let record = host.record("spin", 1).ok_or("the run left no record")?;
    assert!(record.contains("status: cancelled\n"), "{record}");
    assert!(
        contextos_web::shared_claim::SharedClaim::acquire(
            &host.claims_root(),
            &contextos_web::shared_claim::SharedJobKey {
                package: "acme".to_owned(),
                workflow: "spin".to_owned(),
                job: "nightly".to_owned(),
            },
        )
        .is_ok(),
        "the claim is free once the run was cancelled"
    );
    Ok(())
}

#[tokio::test]
async fn a_one_off_shared_run_takes_its_inputs_from_the_command_line() -> Result<(), BoxError> {
    let host = Host::new()?;

    let output = host
        .run(&[
            "workflows",
            "run",
            "--shared",
            "--release",
            "1.0.0",
            "acme",
            "copy",
            "--input",
            "source=vb",
        ])
        .await?;

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("Run succeeded."));
    Ok(())
}

#[tokio::test]
async fn each_start_failure_exits_non_zero_with_its_own_code() -> Result<(), BoxError> {
    let host = Host::new()?;
    let cases: [(&[&str], &str); 3] = [
        (
            &["workflows", "run", "--shared", "--release", "1.0.0", "acme", "copy"],
            "workflow/input-invalid",
        ),
        (
            &[
                "workflows",
                "run",
                "--shared",
                "--release",
                "1.0.0",
                "acme",
                "copy",
                "--input",
                "source=vz",
            ],
            "workflow/vault-unavailable",
        ),
        (
            &[
                "workflows",
                "run",
                "--shared",
                "acme",
                "copy",
                "--release",
                "2.0.0",
                "--input",
                "source=va",
            ],
            "workflow/job-not-runnable",
        ),
    ];
    host.run(&["packages", "inspect", "--shared", "--rebuild-ledger"])
        .await?;

    for (arguments, code) in cases {
        let output = host.run(arguments).await?;
        assert!(!output.status.success(), "{arguments:?}");
        assert!(stderr(&output).contains(code), "{arguments:?}: {}", stderr(&output));
    }
    Ok(())
}

#[tokio::test]
async fn a_dry_run_of_a_shared_workflow_that_ships_no_scenario_is_refused_naming_it() -> Result<(), BoxError> {
    let host = Host::new()?;

    for arguments in [
        &[
            "workflows",
            "run",
            "--shared",
            "--release",
            "1.0.0",
            "--dry-run",
            "acme",
            "tolerant",
        ][..],
        &["jobs", "run", "--shared", "--dry-run", "acme", "tolerant", "nightly"][..],
    ] {
        let output = host.run(arguments).await?;
        assert!(!output.status.success(), "{arguments:?}");
        let error = stdout(&output);
        assert!(
            error.contains("ships no scenario") && error.contains("tolerant"),
            "{arguments:?}: {error}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_dry_run_of_a_shared_workflow_uses_the_scenario_its_package_ships() -> Result<(), BoxError> {
    let host = Host::new()?;
    write(
        &host
            .data
            .join("contextos/packages/acme/1.0.0/tolerant/tests/offline.toml"),
        ERROR_SCENARIO,
    )?;
    support::release_fixture::seal_io(&host.data.join("contextos/packages/acme/1.0.0"))?;

    for arguments in [
        &[
            "workflows",
            "run",
            "--shared",
            "--release",
            "1.0.0",
            "--dry-run",
            "acme",
            "tolerant",
        ][..],
        &["jobs", "run", "--shared", "--dry-run", "acme", "tolerant", "nightly"][..],
    ] {
        let output = host.run(arguments).await?;
        assert!(output.status.success(), "{arguments:?}: {}", stderr(&output));
        assert!(
            stdout(&output).starts_with("Dry run succeeded with warnings.\n"),
            "{arguments:?}: {}",
            stdout(&output)
        );
        assert!(
            stdout(&output).contains("Scenario: offline (call results were mocked, so nothing was written)\n"),
            "{arguments:?}: {}",
            stdout(&output)
        );
    }
    let record = host
        .record("tolerant", 1)
        .ok_or("the saved job's dry run left no record")?;
    assert!(record.contains("dry_run: true\n"), "{record}");
    assert!(
        host.record("tolerant", 2).is_none(),
        "the one-off dry run, which has no saved job, leaves no record"
    );
    Ok(())
}

#[tokio::test]
async fn a_shared_job_that_is_still_a_draft_is_refused_with_the_stable_code() -> Result<(), BoxError> {
    let host = Host::new()?;

    let output = host
        .run(&["jobs", "run", "--shared", "acme", "tolerant", "nightly"])
        .await?;

    assert!(!output.status.success());
    let error = stdout(&output);
    assert!(
        error.contains("workflow/job-not-runnable") && error.contains("draft"),
        "{error}"
    );
    Ok(())
}

#[tokio::test]
async fn a_vault_registered_one_off_run_succeeds_and_writes_no_run_record() -> Result<(), BoxError> {
    let host = Host::new()?;
    write(
        &host.vault_a.join("registry/workflows/greet/workflows.toml"),
        "workflows = [\"greet\"]\n",
    )?;
    write(
        &host.vault_a.join("registry/workflows/greet/greet/workflow.toml"),
        "name = \"Greet\"\nentry = \"greet.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
    )?;
    write(
        &host.vault_a.join("registry/workflows/greet/greet/greet.rhai"),
        "call_mcp(\"contextos\", \"fs_read_text_file\", #{ path: \"va://index.md\" });",
    )?;

    let output = host
        .run(&["workflows", "run", "va", "greet", "greet", "--source"])
        .await?;

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("Run succeeded."));
    assert!(!host.vault_a.join("memory/jobs").exists());
    Ok(())
}

#[tokio::test]
async fn a_vault_dry_run_says_its_results_were_mocked_and_a_live_run_does_not() -> Result<(), BoxError> {
    let host = Host::new()?;
    write(
        &host.vault_a.join("registry/workflows/greet/workflows.toml"),
        "workflows = [\"greet\"]\n",
    )?;
    write(
        &host.vault_a.join("registry/workflows/greet/greet/workflow.toml"),
        "name = \"Greet\"\nentry = \"greet.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
    )?;
    write(
        &host.vault_a.join("registry/workflows/greet/greet/greet.rhai"),
        TOLERANT_SCRIPT,
    )?;
    write(
        &host.vault_a.join("registry/workflows/greet/greet/jobs/nightly.toml"),
        "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    write(
        &host.vault_a.join("registry/workflows/greet/greet/tests/offline.toml"),
        ERROR_SCENARIO,
    )?;
    support::release_fixture::install_vault_releases(&host.vault_a).map_err(|error| error.to_string())?;

    let dry = host
        .run(&["jobs", "run", "--dry-run", "va", "greet", "greet", "nightly"])
        .await?;
    let one_off = host
        .run(&["workflows", "run", "--dry-run", "--source", "va", "greet", "greet"])
        .await?;

    for output in [&dry, &one_off] {
        assert!(output.status.success(), "{}", stderr(output));
        assert!(
            stdout(output).starts_with("Dry run succeeded with warnings.\n"),
            "{}",
            stdout(output)
        );
        assert!(
            stdout(output).contains("Scenario: offline (call results were mocked, so nothing was written)\n"),
            "{}",
            stdout(output)
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_failed_saved_shared_job_names_the_owner_from_web_toml_as_its_own_line() -> Result<(), BoxError> {
    let host = Host::new()?;
    let mut config = std::fs::read_to_string(&host.web_config)?;
    config.push_str("\n[owner]\nname = \"Piper\"\n");
    std::fs::write(&host.web_config, config)?;
    let package = host.data.join("contextos/packages/acme/1.0.0");
    let manifest = std::fs::read_to_string(package.join("manifest.toml"))?;
    // The new workflow goes before the table of files, which sealing rebuilds.
    let (head, _files) = manifest
        .split_once("\n[[files]]")
        .ok_or("the manifest keeps no table of files")?;
    std::fs::write(
        package.join("manifest.toml"),
        format!("{head}\n[[workflow]]\nslug = \"boom\"\nentry = \"boom.rhai\"\n"),
    )?;
    write(&package.join("boom.rhai"), "throw \"the script gave up\";")?;
    support::release_fixture::seal_io(&package)?;
    write(
        &host.data.join("contextos/workflows/acme/boom/jobs/nightly.toml"),
        "name = \"Boom\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    register(
        &host.data,
        "boom",
        "name = \"Boom\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;

    let output = host
        .run(&["jobs", "run", "--shared", "acme", "boom", "nightly"])
        .await?;

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(stdout(&output).contains("\nOwner: Piper\n"), "{}", stdout(&output));
    assert!(!stdout(&output).contains("(owner:"), "{}", stdout(&output));
    Ok(())
}

fn write_greeting_workflow(host: &Host, manifest_extra: &str, script: &str) -> std::io::Result<()> {
    write(
        &host.vault_a.join("registry/workflows/greet/workflows.toml"),
        "workflows = [\"greet\"]\n",
    )?;
    write(
        &host.vault_a.join("registry/workflows/greet/greet/workflow.toml"),
        &format!("name = \"Greet\"\nentry = \"greet.rhai\"\nmcp_servers.allow = [\"contextos\"]\n{manifest_extra}"),
    )?;
    write(&host.vault_a.join("registry/workflows/greet/greet/greet.rhai"), script)
}

#[tokio::test]
async fn a_run_that_flags_a_problem_succeeds_with_warnings_and_lists_the_reasons() -> Result<(), BoxError> {
    let host = Host::new()?;
    write_greeting_workflow(
        &host,
        "",
        "flag_degraded(\"retried the read twice\"); log_warn(\"read was slow\");",
    )?;

    let output = host
        .run(&["workflows", "run", "va", "greet", "greet", "--source"])
        .await?;

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("Run succeeded with warnings."),
        "{}",
        stdout(&output)
    );
    assert!(!stdout(&output).contains("Run succeeded.\n"), "{}", stdout(&output));
    assert!(
        stdout(&output).contains("Warning: retried the read twice\n"),
        "the reasons are in the summary: {}",
        stdout(&output)
    );
    assert!(
        !stderr(&output).contains("retried the read twice"),
        "and are no longer written to standard error: {}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("read was slow"),
        "the log line stays in the log: {}",
        stderr(&output)
    );
    Ok(())
}

#[tokio::test]
async fn logging_that_is_only_informational_leaves_the_run_plainly_succeeded() -> Result<(), BoxError> {
    let host = Host::new()?;
    write_greeting_workflow(&host, "", "log_info(\"all fine\");")?;

    let output = host
        .run(&["workflows", "run", "va", "greet", "greet", "--source"])
        .await?;

    assert!(stdout(&output).contains("Run succeeded."), "{}", stdout(&output));
    assert!(!stdout(&output).contains("with warnings"), "{}", stdout(&output));
    Ok(())
}

#[tokio::test]
async fn the_minimum_level_is_the_hosts_unless_the_workflow_sets_its_own() -> Result<(), BoxError> {
    let host = Host::new()?;
    let mut config = std::fs::read_to_string(&host.web_config)?;
    config.push_str("\n[workflows]\nlog_level = \"warn\"\n");
    std::fs::write(&host.web_config, config)?;
    let script = "log_info(\"an information line\"); log_warn(\"a warning line\");";

    write_greeting_workflow(&host, "", script)?;
    let by_host = host
        .run(&["workflows", "run", "va", "greet", "greet", "--source"])
        .await?;
    write_greeting_workflow(&host, "log_level = \"info\"\n", script)?;
    let by_workflow = host
        .run(&["workflows", "run", "va", "greet", "greet", "--source"])
        .await?;

    assert!(
        !stderr(&by_host).contains("an information line"),
        "{}",
        stderr(&by_host)
    );
    assert!(stderr(&by_host).contains("a warning line"), "{}", stderr(&by_host));
    assert!(
        stderr(&by_workflow).contains("an information line"),
        "{}",
        stderr(&by_workflow)
    );
    Ok(())
}

#[tokio::test]
async fn an_unknown_level_in_a_workflow_manifest_stops_that_workflow_registering() -> Result<(), BoxError> {
    let host = Host::new()?;
    write_greeting_workflow(&host, "log_level = \"loud\"\n", "log_info(\"x\");")?;

    let output = host
        .run(&["workflows", "run", "va", "greet", "greet", "--source"])
        .await?;

    assert!(!output.status.success());
    assert!(stderr(&output).contains("greet"), "{}", stderr(&output));
    Ok(())
}

#[tokio::test]
async fn a_saved_shared_jobs_own_minimum_level_beats_the_hosts() -> Result<(), BoxError> {
    let host = Host::new()?;
    let mut config = std::fs::read_to_string(&host.web_config)?;
    config.push_str("\n[workflows]\nlog_level = \"debug\"\n");
    std::fs::write(&host.web_config, config)?;
    write(
        &host.data.join("contextos/packages/acme/1.0.0/copy/copy.rhai"),
        "log_info(\"an information line\"); log_warn(\"a warning line\");",
    )?;
    support::release_fixture::seal_io(&host.data.join("contextos/packages/acme/1.0.0"))?;
    write(
        &host.data.join("contextos/workflows/acme/copy/jobs/nightly.toml"),
        "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nlog_level = \"warn\"\n[inputs]\nsource = \"va\"\n",
    )?;

    let output = host
        .run(&["jobs", "run", "--shared", "acme", "copy", "nightly"])
        .await?;

    assert!(
        !stderr(&output).contains("an information line"),
        "the host's debug minimum must not override the job's warn: {}",
        stderr(&output)
    );
    assert!(stderr(&output).contains("a warning line"), "{}", stderr(&output));
    Ok(())
}

#[tokio::test]
async fn a_one_off_shared_run_of_a_version_recorded_but_removed_is_package_unavailable() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.run(&["packages", "inspect", "--shared", "--rebuild-ledger"])
        .await?;
    let removed = host
        .run(&["packages", "remove", "--shared", "acme", "1.0.0", "--force"])
        .await?;
    assert!(removed.status.success(), "{}", stderr(&removed));

    let output = host
        .run(&[
            "workflows",
            "run",
            "--shared",
            "--release",
            "1.0.0",
            "acme",
            "copy",
            "--input",
            "source=va",
        ])
        .await?;

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("workflow/package-unavailable"),
        "{}",
        stderr(&output)
    );
    Ok(())
}

#[tokio::test]
async fn a_one_off_shared_run_of_future_or_previous_runs_that_release() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.add_release("0.9.0")?;
    host.add_release("2.0.0")?;
    let state = host.data.join("contextos/state/shared-jobs.toml");
    let kept = std::fs::read_to_string(&state)?;
    write(
        &state,
        &format!("{kept}\n[acme.copy]\ncurrent = \"1.0.0\"\nprevious = \"0.9.0\"\nfuture = \"2.0.0\"\n"),
    )?;

    for (choice, version) in [("future", "2.0.0"), ("previous", "0.9.0"), ("current", "1.0.0")] {
        let output = host
            .run(&[
                "workflows",
                "run",
                "--shared",
                "--release",
                choice,
                "acme",
                "copy",
                "--input",
                "source=va",
                "--format",
                "json",
            ])
            .await?;

        assert!(output.status.success(), "{choice}: {}", stderr(&output));
        let document: serde_json::Value = serde_json::from_str(&stdout(&output))?;
        assert_eq!(document["scope"]["package_version"], version, "{choice}: {document}");
    }
    Ok(())
}
