//! `contextos-web workflows releases` as a real process over a real vault and a real host store: the arguments, the
//! result on standard output for a vault and for the host, the exit status, and what each release is reported as.
#![cfg(target_os = "linux")]

mod support;

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;
use tokio::process::Command;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\n";
const JOB: &str = "name = \"Nightly\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n";

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
    /// A vault `va` holding the source of the workflow package `ops`, a host store holding the package `acme`, and a web
    /// configuration reaching the vault.
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
        write(&vault.join("registry/workflows/ops/sync/sync.rhai"), "let x = 1;")?;
        support::release_fixture::write_whole_release(
            &root.path().join("bundle"),
            ("acme", "1.0.0"),
            "[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n",
            &[("sync.rhai", b"let x = 1;")],
        )
        .map_err(|error| error.to_string())?;
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

    async fn succeed(&self, arguments: &[&str]) -> Result<Output, BoxError> {
        let output = self.run(arguments).await?;
        if !output.status.success() {
            return Err(format!("{arguments:?} failed: {}{}", stdout(&output), stderr(&output)).into());
        }
        Ok(output)
    }

    /// Installs the package `acme` from the folder that holds it into the host store, so the ledger records it.
    async fn install_acme(&self) -> Result<(), BoxError> {
        let bundle = self.root.path().join("bundle");
        self.succeed(&["packages", "install", "--shared", &bundle.to_string_lossy()])
            .await?;
        Ok(())
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.vault.join(relative)
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn document(output: &Output) -> Result<Value, BoxError> {
    Ok(serde_json::from_str(&stdout(output))?)
}

#[tokio::test]
async fn a_vault_release_installed_from_source_is_reported_as_future_in_json() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.succeed(&["workflows", "install", "va", "ops"]).await?;

    let output = host
        .succeed(&["workflows", "releases", "va", "ops", "sync", "--format", "json"])
        .await?;

    let report = document(&output)?;
    assert_eq!(report["store"], "va");
    assert_eq!(report["package"], "ops");
    assert_eq!(report["workflow"], "sync");
    let release = &report["releases"][0];
    assert_eq!(release["version"], "1.0.0");
    assert_eq!(release["kind"], "future");
    assert_eq!(release["origin"], "source");
    assert_eq!(release["condition"], "whole");
    assert!(
        release["installed_at"].as_str().is_some_and(|at| at.ends_with('Z')),
        "{release}"
    );
    assert_eq!(release["lock"], Value::Null);
    assert_eq!(release["jobs"], serde_json::json!({}));
    let logged = stderr(&output);
    assert!(
        !logged.contains(" WARN ") && !logged.contains(" ERROR "),
        "the command logged a warning or an error: {logged}"
    );
    Ok(())
}

#[tokio::test]
async fn the_text_form_leads_with_the_kind_and_the_install_date() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.succeed(&["workflows", "install", "va", "ops"]).await?;

    let output = host.succeed(&["workflows", "releases", "va", "ops", "sync"]).await?;

    let text = stdout(&output);
    assert!(text.starts_with("ops/sync in va\nFuture, installed "), "{text}");
    assert!(text.contains("\n    version 1.0.0\n"), "{text}");
    Ok(())
}

#[tokio::test]
async fn a_release_recorded_as_current_and_a_newer_install_are_current_and_future() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.succeed(&["workflows", "install", "va", "ops"]).await?;
    write(&host.path("registry/workflows/ops/sync/sync.rhai"), "let x = 2;")?;
    host.succeed(&["workflows", "install", "va", "ops"]).await?;
    write(
        &host.path(".contextos/jobs.toml"),
        "[ops.sync]\ncurrent = \"1.0.0\"\nfuture = \"1.0.1\"\n",
    )?;

    let output = host
        .succeed(&["workflows", "releases", "va", "ops", "sync", "--format", "json"])
        .await?;

    let report = document(&output)?;
    let kinds: Vec<(String, String)> = report["releases"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|release| {
            (
                release["version"].as_str().unwrap_or_default().to_owned(),
                release["kind"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("1.0.1".to_owned(), "future".to_owned()),
            ("1.0.0".to_owned(), "current".to_owned())
        ]
    );
    Ok(())
}

#[tokio::test]
async fn a_job_file_naming_the_release_is_counted_by_stage() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.succeed(&["workflows", "install", "va", "ops"]).await?;
    write(&host.path("registry/workflows/ops/sync/jobs/nightly.toml"), JOB)?;

    let output = host
        .succeed(&["workflows", "releases", "va", "ops", "sync", "--format", "json"])
        .await?;

    assert_eq!(
        document(&output)?["releases"][0]["jobs"],
        serde_json::json!({"draft": 1})
    );
    Ok(())
}

#[tokio::test]
async fn a_damaged_vault_release_is_reported_damaged() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.succeed(&["workflows", "install", "va", "ops"]).await?;
    std::fs::write(host.path(".contextos/packages/ops/1.0.0/sync/sync.rhai"), "let x = 99;")?;

    let output = host
        .succeed(&["workflows", "releases", "va", "ops", "sync", "--format", "json"])
        .await?;

    let release = &document(&output)?["releases"][0];
    assert_eq!(release["condition"], "damaged");
    assert!(
        release["problems"]
            .as_array()
            .is_some_and(|problems| !problems.is_empty())
    );
    Ok(())
}

#[tokio::test]
async fn a_host_release_is_reported_with_the_same_shape_as_a_vault_release() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.install_acme().await?;

    let output = host
        .succeed(&["workflows", "releases", "--shared", "acme", "sync", "--format", "json"])
        .await?;

    let report = document(&output)?;
    assert_eq!(report["store"], "shared");
    assert_eq!(report["package"], "acme");
    assert_eq!(report["workflow"], "sync");
    assert_eq!(report["releases"][0]["version"], "1.0.0");
    assert_eq!(report["releases"][0]["kind"], "future");
    assert_eq!(report["releases"][0]["condition"], "whole");
    assert_eq!(report["releases"][0]["origin"], "bundle");
    assert!(report["releases"][0]["installed_at"].as_str().is_some());
    Ok(())
}

#[tokio::test]
async fn the_host_text_form_names_the_store_shared() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.install_acme().await?;

    let output = host
        .succeed(&["workflows", "releases", "--shared", "acme", "sync"])
        .await?;

    let text = stdout(&output);
    assert!(text.starts_with("acme/sync (shared)\nFuture, installed "), "{text}");
    Ok(())
}

#[tokio::test]
async fn the_host_report_needs_no_web_configuration() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.install_acme().await?;
    std::fs::remove_file(&host.web_config)?;

    let output = host
        .succeed(&["workflows", "releases", "--shared", "acme", "sync", "--format", "json"])
        .await?;

    assert_eq!(document(&output)?["releases"][0]["version"], "1.0.0");
    Ok(())
}

#[tokio::test]
async fn a_workflow_with_no_releases_is_reported_as_having_none() -> Result<(), BoxError> {
    let host = Host::new()?;

    let output = host.succeed(&["workflows", "releases", "va", "ops", "sync"]).await?;

    assert_eq!(stdout(&output).trim_end(), "No releases of ops/sync in va.");
    Ok(())
}

#[tokio::test]
async fn a_package_name_that_cannot_name_one_is_refused_with_its_code() -> Result<(), BoxError> {
    let host = Host::new()?;

    let output = host
        .run(&["workflows", "releases", "va", "Bad Name", "sync", "--format", "json"])
        .await?;

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(document(&output)?["error"]["code"], "packages/invalid-name");
    Ok(())
}

#[tokio::test]
async fn a_vault_the_server_does_not_serve_is_refused_with_its_code() -> Result<(), BoxError> {
    let host = Host::new()?;

    let output = host
        .run(&["workflows", "releases", "nowhere", "ops", "sync", "--format", "json"])
        .await?;

    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    let code = document(&output)?["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(code.starts_with("workflow/") || code.starts_with("packages/"), "{code}");
    Ok(())
}

#[tokio::test]
async fn the_wrong_number_of_words_is_a_usage_error() -> Result<(), BoxError> {
    let host = Host::new()?;

    let vault_missing_a_word = host.run(&["workflows", "releases", "va", "ops"]).await?;
    let shared_with_a_vault = host
        .run(&["workflows", "releases", "--shared", "va", "ops", "sync"])
        .await?;

    assert_eq!(vault_missing_a_word.status.code(), Some(2));
    assert!(
        stderr(&vault_missing_a_word).contains("a vault workflow is named by <vault> <package> <workflow>"),
        "{}",
        stderr(&vault_missing_a_word)
    );
    assert_eq!(shared_with_a_vault.status.code(), Some(2));
    assert!(
        stderr(&shared_with_a_vault).contains("a shared workflow is named by <package> <workflow>"),
        "{}",
        stderr(&shared_with_a_vault)
    );
    Ok(())
}

#[tokio::test]
async fn a_template_the_release_declares_and_source_has_lost_is_reported_missing() -> Result<(), BoxError> {
    let host = Host::new()?;
    write(
        &host.path("registry/workflows/ops/workflows.toml"),
        "workflows = [\"sync\"]\ntemplates = [\"letter.md\"]\n",
    )?;
    write(&host.path("registry/workflows/ops/letter.md"), "Dear you\n")?;
    host.succeed(&["workflows", "install", "va", "ops"]).await?;
    let intact = host
        .succeed(&["workflows", "releases", "va", "ops", "sync", "--format", "json"])
        .await?;
    std::fs::remove_file(host.path("registry/workflows/ops/letter.md"))?;

    let json = host
        .succeed(&["workflows", "releases", "va", "ops", "sync", "--format", "json"])
        .await?;
    let text = host.succeed(&["workflows", "releases", "va", "ops", "sync"]).await?;

    assert_eq!(
        document(&intact)?["releases"][0]["missing_templates"],
        serde_json::json!([])
    );
    assert_eq!(
        document(&json)?["releases"][0]["missing_templates"],
        serde_json::json!(["letter.md"])
    );
    assert!(
        stdout(&text).contains("missing template: letter.md"),
        "{}",
        stdout(&text)
    );
    Ok(())
}
