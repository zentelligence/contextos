//! `contextos-web jobs successor` as a real process, for a vault and for the host: what it prints, the job files it
//! leaves, and the exit status of a refusal. The host's data directory is redirected to a temporary directory, so
//! nothing here reads or writes the operator's own.
#![cfg(target_os = "linux")]

mod support;

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;
use tokio::process::Command;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n";
const JOB: &str =
    "# every night\nname = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nready = true\n";

/// A shared job has no owner.
const SHARED_JOB: &str = "# every night\nname = \"Nightly\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nready = true\n";

/// Releases 1.0.0 current and `future` future, with `nightly` and `weekly` enabled on 1.0.0.
fn state(package: &str, future: &str) -> String {
    format!(
        "[{package}.sync]\ncurrent = \"1.0.0\"\nfuture = \"{future}\"\n\n[{package}.sync.nightly]\nlifecycle = \"enabled\"\n\n[{package}.sync.weekly]\nlifecycle = \"enabled\"\n"
    )
}

fn write(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

struct Host {
    _root: tempfile::TempDir,
    data: PathBuf,
    web_config: PathBuf,
    vault: PathBuf,
}

impl Host {
    /// A vault `va` and a host, each with the workflow `sync` released as 1.0.0 and `future`, and two enabled jobs.
    fn new(future: &str) -> Result<Self, BoxError> {
        let root = tempfile::tempdir()?;
        let data = root.path().join("data");
        let vault = root.path().join("va");
        write(&vault.join("index.md"), "# A\n")?;
        write(
            &vault.join("registry/workflows/ops/workflows.toml"),
            "workflows = [\"sync\"]\n",
        )?;
        write(&vault.join("registry/workflows/ops/sync/workflow.toml"), WORKFLOW)?;
        write(&vault.join("registry/workflows/ops/sync/sync.rhai"), "log_info(\"a\");")?;
        for job in ["nightly", "weekly"] {
            write(&vault.join(format!("registry/workflows/ops/sync/jobs/{job}.toml")), JOB)?;
        }
        write(&vault.join(".contextos/jobs.toml"), &state("ops", future))?;
        for version in ["1.0.0", future] {
            write(
                &vault.join(format!(".contextos/packages/ops/{version}/manifest.toml")),
                &format!("name = \"ops\"\nversion = \"{version}\"\n"),
            )?;
            write(
                &vault.join(format!(".contextos/packages/ops/{version}/workflows.toml")),
                "workflows = [\"sync\"]\n",
            )?;
        }
        for version in ["1.0.0", future] {
            let package = data.join(format!("contextos/packages/acme/{version}"));
            write(
                &package.join("manifest.toml"),
                &format!(
                    "name = \"acme\"\nversion = \"{version}\"\ndigest = \"{DIGEST}\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n"
                ),
            )?;
            write(&package.join("sync.rhai"), "log_info(\"a\");")?;
            support::release_fixture::seal_io(&package)?;
        }
        for job in ["nightly", "weekly"] {
            write(
                &data.join(format!("contextos/workflows/acme/sync/jobs/{job}.toml")),
                SHARED_JOB,
            )?;
        }
        write(&data.join("contextos/state/shared-jobs.toml"), &state("acme", future))?;

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
            _root: root,
            data,
            web_config,
            vault,
        })
    }

    async fn run(&self, arguments: &[&str]) -> Result<Output, BoxError> {
        Ok(Command::new(env!("CARGO_BIN_EXE_contextos-web"))
            .arg("--config")
            .arg(&self.web_config)
            .args(arguments)
            .env("XDG_DATA_HOME", &self.data)
            .env("HOME", &self.data)
            .env_remove("CONTEXTOS_WEB_CONFIG")
            .output()
            .await?)
    }

    fn vault_job(&self, job: &str) -> PathBuf {
        self.vault.join(format!("registry/workflows/ops/sync/jobs/{job}.toml"))
    }

    fn shared_job(&self, job: &str) -> PathBuf {
        self.data.join(format!("contextos/workflows/acme/sync/jobs/{job}.toml"))
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn document(output: &Output) -> Result<Value, BoxError> {
    serde_json::from_str(&stdout(output)).map_err(|error| format!("{error}: {}", stdout(output)).into())
}

#[tokio::test]
async fn a_vault_successor_is_the_predecessors_file_naming_the_future_release() -> Result<(), BoxError> {
    let host = Host::new("1.1.0")?;

    let output = host
        .run(&[
            "jobs",
            "successor",
            "va",
            "ops",
            "sync",
            "nightly",
            "nightly-next",
            "--format",
            "json",
        ])
        .await?;

    assert!(output.status.success(), "{}", stdout(&output));
    let result = document(&output)?;
    assert_eq!(result["store"], "va");
    assert_eq!(result["release"]["version"], "1.1.0");
    assert_eq!(result["release"]["kind"], "future");
    assert_eq!(result["successors"][0]["predecessor"], "nightly");
    assert_eq!(result["successors"][0]["job"], "nightly-next");
    assert_eq!(result["successors"][0]["step"], "minor");
    assert_eq!(
        result["successors"][0]["carries_over"], false,
        "no ledger holds the interfaces to compare"
    );
    assert!(result["successors"][0]["interface_changes"].is_null(), "{result}");
    let written = std::fs::read_to_string(host.vault_job("nightly-next"))?;
    assert!(written.starts_with("# every night\n"), "{written}");
    assert!(written.contains("version = \"1.1.0\""), "{written}");
    assert!(written.contains("predecessor = \"nightly\""), "{written}");
    assert_eq!(std::fs::read_to_string(host.vault_job("nightly"))?, JOB);
    Ok(())
}

#[tokio::test]
async fn a_shared_successor_is_written_in_the_hosts_jobs_folder() -> Result<(), BoxError> {
    let host = Host::new("1.0.1")?;

    let output = host
        .run(&[
            "jobs",
            "successor",
            "--shared",
            "acme",
            "sync",
            "nightly",
            "nightly-next",
        ])
        .await?;

    let text = stdout(&output);
    assert!(output.status.success(), "{text}");
    assert!(
        text.contains("made 1 successor in shared/acme/sync on release 1.0.1 (future)"),
        "{text}"
    );
    assert!(
        text.contains(
            "nightly to nightly-next, 1.0.0 to 1.0.1, patch step: take up the changes to inputs and outputs while it is draft; the store's ledger does not say what changed"
        ),
        "{text}"
    );
    let written = std::fs::read_to_string(host.shared_job("nightly-next"))?;
    assert!(
        written.contains("version = \"1.0.1\"") && written.contains("predecessor = \"nightly\""),
        "{written}"
    );
    Ok(())
}

#[tokio::test]
async fn all_makes_a_successor_for_each_job_and_says_what_a_major_step_asks_of_them() -> Result<(), BoxError> {
    let host = Host::new("2.0.0")?;

    let vault = host
        .run(&["jobs", "successor", "va", "ops", "sync", "--all", "--format", "json"])
        .await?;
    let shared = host
        .run(&["jobs", "successor", "--shared", "acme", "sync", "--all"])
        .await?;

    assert!(vault.status.success(), "{}", stdout(&vault));
    let result = document(&vault)?;
    let keys: Vec<&str> = result["successors"]
        .as_array()
        .ok_or("no successors")?
        .iter()
        .filter_map(|made| made["job"].as_str())
        .collect();
    assert_eq!(keys, ["nightly-v2-0-0", "weekly-v2-0-0"]);
    assert_eq!(result["successors"][0]["step"], "major");
    assert_eq!(result["successors"][0]["carries_over"], false);
    assert!(host.vault_job("weekly-v2-0-0").is_file());
    let text = stdout(&shared);
    assert!(shared.status.success(), "{text}");
    assert!(
        text.contains("major step: take up the changes to inputs and outputs while it is draft"),
        "{text}"
    );
    assert!(host.shared_job("weekly-v2-0-0").is_file());
    Ok(())
}

#[tokio::test]
async fn all_to_previous_makes_return_successors() -> Result<(), BoxError> {
    let host = Host::new("1.1.0")?;
    write(
        &host.data.join("contextos/packages/acme/0.9.0/sync.rhai"),
        "log_info(\"a\");",
    )?;
    let package = host.data.join("contextos/packages/acme/0.9.0");
    write(
        &package.join("manifest.toml"),
        &format!(
            "name = \"acme\"\nversion = \"0.9.0\"\ndigest = \"{DIGEST}\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n"
        ),
    )?;
    support::release_fixture::seal_io(&package)?;
    write(
        &host.data.join("contextos/state/shared-jobs.toml"),
        &state("acme", "1.1.0").replace("current = \"1.0.0\"", "current = \"1.0.0\"\nprevious = \"0.9.0\""),
    )?;

    let output = host
        .run(&[
            "jobs",
            "successor",
            "--shared",
            "acme",
            "sync",
            "--all",
            "--to",
            "previous",
            "--format",
            "json",
        ])
        .await?;

    assert!(output.status.success(), "{}", stdout(&output));
    let result = document(&output)?;
    assert_eq!(result["release"]["kind"], "previous");
    assert_eq!(result["successors"][0]["job"], "nightly-v0-9-0");
    assert!(host.shared_job("nightly-v0-9-0").is_file());
    Ok(())
}

#[tokio::test]
async fn a_refusal_is_the_one_document_with_exit_status_1_and_writes_nothing() -> Result<(), BoxError> {
    let host = Host::new("1.1.0")?;
    write(&host.vault_job("weekly-v1-1-0"), JOB)?;

    let output = host
        .run(&["jobs", "successor", "va", "ops", "sync", "--all", "--format", "json"])
        .await?;

    assert_eq!(output.status.code(), Some(1));
    let result = document(&output)?;
    assert_eq!(result["error"]["code"], "workflow/job-exists");
    assert_eq!(result["refusals"][0]["job"], "weekly");
    assert!(
        !host.vault_job("nightly-v1-1-0").exists(),
        "nightly's successor was written beside weekly's refusal"
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("Error:"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = host.run(&["jobs", "successor", "va", "ops", "sync", "--all"]).await?;
    assert!(
        stdout(&text).starts_with("refused in va/ops/sync, and nothing was written"),
        "{}",
        stdout(&text)
    );
    Ok(())
}

#[tokio::test]
async fn a_release_not_installed_is_package_unavailable_for_both_stores() -> Result<(), BoxError> {
    let host = Host::new("1.1.0")?;

    for arguments in [
        &[
            "jobs",
            "successor",
            "va",
            "ops",
            "sync",
            "nightly",
            "next",
            "--to",
            "7.0.0",
            "--format",
            "json",
        ][..],
        &[
            "jobs",
            "successor",
            "--shared",
            "acme",
            "sync",
            "nightly",
            "next",
            "--to",
            "7.0.0",
            "--format",
            "json",
        ][..],
    ] {
        let output = host.run(arguments).await?;
        assert_eq!(output.status.code(), Some(1), "{arguments:?}");
        assert_eq!(
            document(&output)?["error"]["code"],
            "workflow/package-unavailable",
            "{arguments:?}"
        );
    }
    Ok(())
}
