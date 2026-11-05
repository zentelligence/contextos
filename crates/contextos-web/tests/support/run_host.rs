//! A host for the command-line run tests: one installed package `acme` and one vault `va`, each with the same set of
//! workflows and a saved job `nightly` for every one of them, and a web configuration reaching a real `contextos`
//! server. The host's data directory is redirected into a temporary directory, so nothing here reads or writes the
//! operator's own.
//!
//! Every workflow's script first makes one call that a scenario answers with an error, which the script catches, so a
//! dry run against the one scenario each workflow ships completes without a scenario that matches a real tool's output.
//! Included by the test targets that run the real binary, with `#[path = "support/run_host.rs"] mod run_host;`.
#![allow(dead_code)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use serde_json::Value;
use tokio::process::{Child, Command};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

pub const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const VAULT: &str = "va";
pub const PACKAGE: &str = "acme";
/// What a script prints and debugs, so a test can look for it in the wrong places.
pub const PRINTED: &str = "hello-from-the-script";
pub const DEBUGGED: &str = "debug-from-the-script";
/// An output value that must never reach a record or a log.
pub const SECRET_OUTPUT: &str = "secret-output-value-77";
pub const DEGRADED_REASON: &str = "one source note was unreadable";

/// A valid team: `atlas` is the one chief of staff, and `vela` owns the vault jobs.
const TEAM: &str = "[[member]]\nslug = \"atlas\"\nname = \"Atlas\"\nchief_of_staff = true\n\n[[member]]\nslug = \"vela\"\nname = \"Vela\"\n";
const HEAD: &str = "try { call_mcp(\"contextos\", \"vault_info\", #{}); } catch (e) { }\n";
const ERROR_SCENARIO: &str =
    "[[call]]\nserver = \"contextos\"\ntool = \"vault_info\"\nkind = \"error\"\nerror = \"offline\"\n";

/// One workflow of the host: its slug, the script after the common head, and the declarations a manifest carries.
struct Workflow {
    slug: &'static str,
    tail: &'static str,
    /// `(name, kind)` for each declared input.
    inputs: &'static [(&'static str, &'static str)],
    /// `(name, kind)` for each declared output.
    outputs: &'static [(&'static str, &'static str)],
    /// The saved job's file.
    job: &'static str,
}

const WORKFLOWS: &[Workflow] = &[
    Workflow {
        slug: "emit",
        tail: "set_output(\"created\", 3);\nset_output(\"label\", \"secret-output-value-77\");",
        inputs: &[],
        outputs: &[("created", "number"), ("label", "string")],
        job: "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    },
    Workflow {
        slug: "chatty",
        tail: "print(\"hello-from-the-script\");\ndebug(\"debug-from-the-script\");",
        inputs: &[],
        outputs: &[],
        job: "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    },
    Workflow {
        slug: "degrade",
        tail: "flag_degraded(\"one source note was unreadable\");",
        inputs: &[],
        outputs: &[],
        job: "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    },
    Workflow {
        slug: "fail",
        tail: "throw \"the script gave up\";",
        inputs: &[],
        outputs: &[],
        job: "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    },
    Workflow {
        slug: "spin",
        tail: "loop {}",
        inputs: &[],
        outputs: &[],
        job: "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    },
    Workflow {
        slug: "slow",
        tail: "loop {}",
        inputs: &[],
        outputs: &[],
        job: "name = \"Nightly\"\ntimeout_secs = 1\nversion = \"1.0.0\"\n",
    },
    Workflow {
        slug: "echo",
        tail: "set_output(\"seen\", inputs.target);",
        inputs: &[("target", "string")],
        outputs: &[("seen", "string")],
        job: "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n[inputs]\ntarget = \"from-the-job\"\n",
    },
];

/// A host with the package, the vault, and a web configuration.
pub struct RunHost {
    pub root: tempfile::TempDir,
    pub data: PathBuf,
    pub web_config: PathBuf,
    pub vault: PathBuf,
}

pub fn write(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A saved job's file as a vault job has it: the same text with an owner, a member of the host's team, named right after
/// the name line so it stays above any table.
fn owned(job: &str) -> String {
    job.replacen('\n', "\nowner = \"vela\"\n", 1)
}

/// Arguments from string slices.
pub fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

/// `jobs run {scope} {workflow} nightly {extra}`, for a vault job or, with `shared`, a shared one.
pub fn jobs_run(shared: bool, workflow: &str, extra: &[&str]) -> Vec<String> {
    scoped("run", shared, workflow, Some("nightly"), extra, "jobs")
}

/// `jobs dry-run {scope} {workflow} nightly {extra}`.
pub fn jobs_dry_run(shared: bool, workflow: &str, extra: &[&str]) -> Vec<String> {
    scoped("dry-run", shared, workflow, Some("nightly"), extra, "jobs")
}

/// `workflows run {scope} {workflow} {extra}`.
pub fn workflows_run(shared: bool, workflow: &str, extra: &[&str]) -> Vec<String> {
    scoped("run", shared, workflow, None, extra, "workflows")
}

fn scoped(verb: &str, shared: bool, workflow: &str, job: Option<&str>, extra: &[&str], group: &str) -> Vec<String> {
    let mut arguments = args(&[group, verb]);
    if shared {
        arguments.push("--shared".to_owned());
        if group == "workflows" && !extra.contains(&"--release") {
            // A shared one-off run of a package whose workflow no job has enabled names the release it runs.
            arguments.extend(args(&["--release", "1.0.0"]));
        }
        arguments.push(PACKAGE.to_owned());
    } else {
        arguments.push(VAULT.to_owned());
        // A vault workflow is its own package in these fixtures, and these fixtures write its source only.
        arguments.push(workflow.to_owned());
    }
    arguments.push(workflow.to_owned());
    arguments.extend(job.map(str::to_owned));
    arguments.extend(extra.iter().map(|part| (*part).to_owned()));
    if group == "workflows" && !shared && !extra.contains(&"--release") {
        arguments.push("--source".to_owned());
    }
    arguments
}

fn declarations(table: &str, fields: &[(&str, &str)]) -> String {
    let mut text = String::new();
    for (name, kind) in fields {
        let _ = write!(text, "\n[[{table}]]\nname = \"{name}\"\nkind = \"{kind}\"\n");
    }
    text
}

impl RunHost {
    pub fn new() -> Result<Self, BoxError> {
        let root = tempfile::tempdir()?;
        let data = root.path().join("data");
        let vault = root.path().join(VAULT);
        write(&vault.join("index.md"), "# A\n")?;
        write(&vault.join(".contextos/team.toml"), TEAM)?;

        let package = data.join("contextos/packages/acme/1.0.0");
        let mut manifest = format!("name = \"acme\"\nversion = \"1.0.0\"\ndigest = \"{DIGEST}\"\n");
        let mut vault_state = String::new();
        for workflow in WORKFLOWS {
            let slug = workflow.slug;
            let script = format!("{HEAD}{}\n", workflow.tail);
            let _ = write!(
                manifest,
                "\n[[workflow]]\nslug = \"{slug}\"\nentry = \"{slug}.rhai\"\n\n\
                 [[workflow.permitted_effects]]\nmcp_server_name = \"contextos\"\ntool = \"vault_info\"\n"
            );
            manifest.push_str(&declarations("workflow.inputs", workflow.inputs));
            manifest.push_str(&declarations("workflow.outputs", workflow.outputs));
            write(&package.join(format!("{slug}.rhai")), &script)?;
            write(&package.join(format!("{slug}/tests/offline.toml")), ERROR_SCENARIO)?;
            write(
                &data.join(format!("contextos/workflows/acme/{slug}/jobs/nightly.toml")),
                workflow.job,
            )?;

            let package_dir = vault.join(format!("registry/workflows/{slug}"));
            write(
                &package_dir.join("workflows.toml"),
                &format!("workflows = [{slug:?}]\n"),
            )?;
            let directory = package_dir.join(slug);
            let mut workflow_toml =
                format!("name = \"{slug}\"\nentry = \"{slug}.rhai\"\nmcp_servers.allow = [\"contextos\"]\n");
            workflow_toml.push_str(&declarations("input", workflow.inputs));
            workflow_toml.push_str(&declarations("output", workflow.outputs));
            write(&directory.join("workflow.toml"), &workflow_toml)?;
            write(&directory.join(format!("{slug}.rhai")), &script)?;
            write(&directory.join("jobs/nightly.toml"), &owned(workflow.job))?;
            write(&directory.join("tests/offline.toml"), ERROR_SCENARIO)?;
            let _ = write!(vault_state, "[{slug}.{slug}.nightly]\nlifecycle = \"registered\"\n\n");
        }
        write(&package.join("manifest.toml"), &manifest)?;
        crate::support::release_fixture::seal_io(&package)?;
        write(&vault.join(".contextos/jobs.toml"), &vault_state)?;
        // A job that exists and has never been validated, to be refused as not runnable.
        write(
            &data.join("contextos/workflows/acme/emit/jobs/draft.toml"),
            "name = \"Draft\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        )?;
        write(
            &vault.join("registry/workflows/emit/emit/jobs/draft.toml"),
            "name = \"Draft\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        )?;
        // A workflow that marks the vault before it spins, so a test can tell its script is running.
        write(
            &vault.join("registry/workflows/mark/workflows.toml"),
            "workflows = [\"mark\"]\n",
        )?;
        write(
            &vault.join("registry/workflows/mark/mark/workflow.toml"),
            "name = \"mark\"\nentry = \"mark.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
        )?;
        write(
            &vault.join("registry/workflows/mark/mark/mark.rhai"),
            "call_mcp(\"contextos\", \"fs_create_directory\", #{ path: \"va://started\" });\nloop {}\n",
        )?;
        crate::support::release_fixture::install_vault_releases(&vault).map_err(|error| error.to_string())?;

        let host = Self {
            web_config: root.path().join("web.toml"),
            root,
            data,
            vault,
        };
        let mcp_config = crate::support::write_named_vaults_config(host.root.path(), &[(VAULT, &host.vault)])?;
        let mcp_binary = crate::support::contextos_mcp_binary()?;
        write(
            &host.web_config,
            &format!(
                "[[mcp_server]]\ntransport = \"stdio\"\nname = \"contextos\"\ncommand = {:?}\nargs = [\"--config\", {:?}]\n",
                mcp_binary.to_string_lossy(),
                mcp_config.to_string_lossy()
            ),
        )?;
        for workflow in WORKFLOWS {
            host.register_shared(workflow.slug, workflow.job)?;
        }
        Ok(host)
    }

    /// Takes the shared `nightly` job of `workflow` to `registered` in the host state, as validation and a dry run would.
    fn register_shared(&self, workflow: &str, job_toml: &str) -> Result<(), BoxError> {
        use contextos_workflows::{Actor, Transition};
        let version =
            contextos_web::shared_version::shared_job_version(&self.data.join("contextos/packages"), PACKAGE, job_toml)
                .ok_or("the job has no version")?;
        let at = time::macros::datetime!(2026-10-01 03:00:00 UTC);
        contextos_web::shared_state::SharedStateStore::new(self.data.join("contextos/state")).update(
            |jobs| -> Result<(), contextos_workflows::TransitionError> {
                jobs.transition(
                    PACKAGE,
                    workflow,
                    "nightly",
                    Actor::Validator,
                    &Transition::Validated(version.clone()),
                    at,
                )?;
                jobs.transition(
                    PACKAGE,
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

    /// The real `contextos-web`, against `config`, with its data directory redirected.
    pub fn command_with(&self, config: &Path, arguments: &[String]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_contextos-web"));
        command
            .arg("--config")
            .arg(config)
            .args(arguments)
            .env("XDG_DATA_HOME", &self.data)
            .env("HOME", &self.data)
            .env_remove("CONTEXTOS_WEB_CONFIG")
            .env_remove("RUST_LOG");
        command
    }

    pub fn command(&self, arguments: &[String]) -> Command {
        self.command_with(&self.web_config, arguments)
    }

    pub async fn run(&self, arguments: &[String]) -> Result<Output, BoxError> {
        Ok(self.command(arguments).output().await?)
    }

    /// Runs `arguments`, and parses the whole of standard output as one JSON value.
    pub async fn json(&self, arguments: &[String]) -> Result<(Output, Value), BoxError> {
        let output = self.run(arguments).await?;
        let document = parse_whole(&output)?;
        Ok((output, document))
    }

    /// Starts a run whose standard output and error are piped, so a test can signal it and read what it printed.
    pub fn spawn(&self, arguments: &[String]) -> Result<Child, BoxError> {
        Ok(self
            .command(arguments)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?)
    }

    pub fn shared_claims_root(&self) -> PathBuf {
        self.data.join("contextos/claims")
    }

    /// Holds the claim of the shared `nightly` job of `workflow`, as a live run would.
    pub fn hold_shared_claim(&self, workflow: &str) -> Result<contextos_web::shared_claim::SharedClaim, BoxError> {
        Ok(contextos_web::shared_claim::SharedClaim::acquire(
            &self.shared_claims_root(),
            &contextos_web::shared_claim::SharedJobKey {
                package: PACKAGE.to_owned(),
                workflow: workflow.to_owned(),
                job: "nightly".to_owned(),
            },
        )?)
    }

    /// Holds the claim of the vault `nightly` job of `workflow`, as a live run would.
    pub fn hold_vault_claim(&self, workflow: &str) -> Result<contextos_web::run_claim::RunClaim, BoxError> {
        let host = crate::support::host(&self.data.join("contextos"));
        let slot = contextos_web::vault_runs::claim_slot(&host, VAULT, workflow, workflow, "nightly")?;
        Ok(contextos_web::run_claim::RunClaim::acquire(&slot)?)
    }

    /// Whether the vault `nightly` job of `workflow` can be claimed, so no run still holds it.
    pub fn vault_claim_is_free(&self, workflow: &str) -> bool {
        self.hold_vault_claim(workflow).is_ok()
    }

    pub fn shared_claim_is_free(&self, workflow: &str) -> bool {
        self.hold_shared_claim(workflow).is_ok()
    }

    /// The text of run `id`'s record for the shared `nightly` job of `workflow`, wherever the date put it.
    pub fn shared_record(&self, workflow: &str, id: u64) -> Option<String> {
        find_record(&self.data.join(format!("contextos/runs/acme/{workflow}/nightly")), id)
    }

    /// The text of run `id`'s record for the vault `nightly` job of `workflow`.
    pub fn vault_record(&self, workflow: &str, id: u64) -> Option<String> {
        find_record(
            &self.vault.join(format!("memory/jobs/{workflow}/{workflow}/nightly")),
            id,
        )
    }
}

fn find_record(job_dir: &Path, id: u64) -> Option<String> {
    let name = format!("R-{id}.md");
    std::fs::read_dir(job_dir).ok()?.flatten().find_map(|year| {
        std::fs::read_dir(year.path())
            .ok()?
            .flatten()
            .find_map(|month| std::fs::read_to_string(month.path().join(&name)).ok())
    })
}

/// Standard output as exactly one JSON value, or why it is not.
pub fn parse_whole(output: &Output) -> Result<Value, BoxError> {
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "standard output is not exactly one JSON value ({error}): {:?}\nstderr: {}",
            stdout(output),
            stderr(output)
        )
        .into()
    })
}

/// Waits, up to the bound, until `ready` holds. A bound only turns a run that never got there into a failure.
pub async fn wait_until(mut ready: impl FnMut() -> bool) -> bool {
    for _ in 0..200 {
        if ready() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    false
}

/// Sends `signal` (`INT`, `TERM`, `STOP`, `CONT`, or `KILL`) to `pid`.
pub fn signal(name: &str, pid: u32) -> Result<(), BoxError> {
    let sent = std::process::Command::new("kill")
        .args([format!("-{name}"), pid.to_string()])
        .status()?;
    if sent.success() {
        Ok(())
    } else {
        Err(format!("kill -{name} {pid} failed").into())
    }
}

/// The process ids of `pid`'s children, as `pgrep -P` lists them.
pub fn children_of(pid: u32) -> Vec<u32> {
    std::process::Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| line.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default()
}
