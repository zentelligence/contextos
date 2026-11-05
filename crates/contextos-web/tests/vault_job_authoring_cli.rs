//! `contextos-web jobs create`, `show`, `replace`, and `list` for a vault job, as a real process over a real vault: the
//! arguments, the one document on standard output, the exit status, and what is left on disk.
#![cfg(target_os = "linux")]

mod support;

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;
use tokio::process::Command;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n";
const JOB: &str = "name = \"Weekly\"\nowner = \"vela\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n";

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
    /// A vault `va` whose workflow `sync` has the release 1.0.0 installed, and a web configuration reaching it.
    fn new() -> Result<Self, BoxError> {
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
        write(
            &vault.join(".contextos/packages/ops/1.0.0/manifest.toml"),
            "name = \"sync\"\nversion = \"1.0.0\"\n",
        )?;
        write(
            &vault.join(".contextos/packages/ops/1.0.0/workflows.toml"),
            "workflows = [\"sync\"]\n",
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

    fn file(&self, name: &str, content: &str) -> Result<PathBuf, BoxError> {
        let path = self.root.path().join(name);
        write(&path, content)?;
        Ok(path)
    }

    fn job_file(&self) -> PathBuf {
        self.vault.join("registry/workflows/ops/sync/jobs/weekly.toml")
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn document(output: &Output) -> Result<Value, BoxError> {
    Ok(serde_json::from_str(&stdout(output))?)
}

#[tokio::test]
async fn a_vault_job_is_created_shown_replaced_and_listed_through_the_commands() -> Result<(), BoxError> {
    let host = Host::new()?;
    let file = host.file("weekly.toml", JOB)?;
    let file = file.to_string_lossy().into_owned();

    let created = host
        .run(&[
            "jobs", "create", "va", "ops", "sync", "weekly", "--file", &file, "--format", "json",
        ])
        .await?;
    assert!(created.status.success(), "{}", stdout(&created));
    let created = document(&created)?;
    assert_eq!(created["outcome"], "created");
    assert_eq!(created["store"], "va");
    assert_eq!(created["workflow"], "sync");
    assert_eq!(created["job"], "weekly");
    assert_eq!(std::fs::read_to_string(host.job_file())?, JOB);

    let shown = host
        .run(&["jobs", "show", "va", "ops", "sync", "weekly", "--format", "json"])
        .await?;
    assert!(shown.status.success());
    let shown = document(&shown)?;
    assert_eq!(shown["content"], JOB);
    let hash = shown["hash"].as_str().ok_or("no hash")?.to_owned();

    let revised = host.file("revised.toml", &JOB.replace("Weekly", "Revised"))?;
    let revised = revised.to_string_lossy().into_owned();
    let replaced = host
        .run(&[
            "jobs",
            "replace",
            "va",
            "ops",
            "sync",
            "weekly",
            "--file",
            &revised,
            "--expected-hash",
            &hash,
            "--format",
            "json",
        ])
        .await?;
    assert!(replaced.status.success(), "{}", stdout(&replaced));
    assert_eq!(document(&replaced)?["outcome"], "replaced");

    let listed = host.run(&["jobs", "list", "va", "--format", "json"]).await?;
    assert!(listed.status.success());
    let listed = document(&listed)?;
    let job = &listed["jobs"][0];
    assert_eq!(job["job"], "weekly");
    assert_eq!(job["state"], "draft");
    assert_eq!(job["version"], "1.0.0");
    assert_eq!(job["release_kind"], "future");
    assert_eq!(job["name"], "Revised");
    Ok(())
}

#[tokio::test]
async fn a_refusal_is_the_one_document_on_standard_output_and_exits_unsuccessfully_without_an_error_line()
-> Result<(), BoxError> {
    let host = Host::new()?;
    let file = host.file("weekly.toml", &JOB.replace("1.0.0", "9.9.9"))?;
    let file = file.to_string_lossy().into_owned();

    let refused = host
        .run(&[
            "jobs", "create", "va", "ops", "sync", "weekly", "--file", &file, "--format", "json",
        ])
        .await?;

    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(document(&refused)?["error"]["code"], "workflow/package-unavailable");
    // The connection to the server logs as every command that needs one does, but a refusal prints no `Error:` line.
    assert!(
        !String::from_utf8_lossy(&refused.stderr).contains("Error:"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(!host.job_file().exists());
    let text = host
        .run(&["jobs", "create", "va", "ops", "sync", "weekly", "--file", &file])
        .await?;
    assert!(
        stdout(&text).starts_with("refused workflow/package-unavailable: "),
        "{}",
        stdout(&text)
    );
    Ok(())
}

#[tokio::test]
async fn a_list_names_a_vault_or_the_host_and_never_both_or_neither() -> Result<(), BoxError> {
    let host = Host::new()?;

    let neither = host.run(&["jobs", "list"]).await?;
    let both = host.run(&["jobs", "list", "va", "--shared"]).await?;
    let shared = host.run(&["jobs", "list", "--shared", "--format", "json"]).await?;

    assert_eq!(neither.status.code(), Some(2));
    assert_eq!(both.status.code(), Some(2));
    assert!(shared.status.success(), "{}", stdout(&shared));
    Ok(())
}
