//! `contextos-web workflows install` and `contextos-web libraries install` as a real process over a real vault: the
//! arguments, the result on standard output, the exit status, and what is left on disk.
#![cfg(target_os = "linux")]

mod support;

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;
use tokio::process::Command;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\n";

struct Host {
    _root: tempfile::TempDir,
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
    /// A vault `va` holding the source of the workflow package `ops`, and a web configuration reaching it.
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

    fn path(&self, relative: &str) -> PathBuf {
        self.vault.join(relative)
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn document(output: &Output) -> Result<Value, BoxError> {
    Ok(serde_json::from_str(&stdout(output))?)
}

#[tokio::test]
async fn a_workflow_package_is_installed_from_source_and_reported_as_json() -> Result<(), BoxError> {
    let host = Host::new()?;

    let output = host
        .run(&["workflows", "install", "va", "ops", "--format", "json"])
        .await?;

    assert!(output.status.success(), "{}", stdout(&output));
    let report = document(&output)?;
    assert_eq!(report["release"]["package"], "ops");
    assert_eq!(report["release"]["version"], "1.0.0");
    assert_eq!(report["release"]["outcome"], "installed");
    assert_eq!(report["standing"]["sync"], "future");
    assert!(host.path(".contextos/packages/ops/1.0.0/manifest.toml").exists());
    let state = std::fs::read_to_string(host.path(".contextos/jobs.toml"))?;
    assert!(state.contains("future = \"1.0.0\""), "{state}");
    Ok(())
}

#[tokio::test]
async fn the_text_form_names_the_release_and_what_it_is() -> Result<(), BoxError> {
    let host = Host::new()?;

    let output = host.run(&["workflows", "install", "va", "ops"]).await?;

    assert!(output.status.success());
    let text = stdout(&output);
    assert!(text.starts_with("installed ops 1.0.0 (workflows)\n"), "{text}");
    assert!(text.contains("sync: future"), "{text}");
    Ok(())
}

#[tokio::test]
async fn a_library_is_installed_from_source_by_its_own_command() -> Result<(), BoxError> {
    let host = Host::new()?;
    write(
        &host.path("registry/libraries/crm/library.toml"),
        "modules = [\"crm.rhai\"]\nexports = [\"crm.rhai\"]\n",
    )?;
    write(&host.path("registry/libraries/crm/crm.rhai"), "fn contacts() { 1 }")?;

    let output = host
        .run(&["libraries", "install", "va", "crm", "--format", "json"])
        .await?;

    assert!(output.status.success(), "{}", stdout(&output));
    let report = document(&output)?;
    assert_eq!(report["kind"], "library");
    assert_eq!(report["release"]["version"], "1.0.0");
    assert!(host.path(".contextos/packages/crm/1.0.0/library.toml").exists());
    assert!(
        !host.path(".contextos/jobs.toml").exists(),
        "a library has no workflow to record"
    );
    Ok(())
}

#[tokio::test]
async fn source_that_fails_a_check_is_refused_with_its_code_and_exit_status_one() -> Result<(), BoxError> {
    let host = Host::new()?;
    write(&host.path("registry/workflows/ops/sync/sync.rhai"), "let x = ;")?;

    let output = host
        .run(&["workflows", "install", "va", "ops", "--format", "json"])
        .await?;

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(document(&output)?["error"]["code"], "validation/script-syntax");
    assert!(!host.path(".contextos/packages/ops").exists());
    Ok(())
}

#[tokio::test]
async fn a_removed_release_is_restored_at_its_version_and_a_version_never_installed_is_not_found()
-> Result<(), BoxError> {
    let host = Host::new()?;
    host.run(&["workflows", "install", "va", "ops"]).await?;
    std::fs::remove_dir_all(host.path(".contextos/packages/ops/1.0.0"))?;

    let restored = host
        .run(&[
            "workflows",
            "install",
            "va",
            "ops",
            "--version",
            "1.0.0",
            "--format",
            "json",
        ])
        .await?;
    let missing = host
        .run(&[
            "workflows",
            "install",
            "va",
            "ops",
            "--version",
            "4.0.0",
            "--format",
            "json",
        ])
        .await?;

    assert!(restored.status.success(), "{}", stdout(&restored));
    assert_eq!(document(&restored)?["release"]["outcome"], "repaired");
    assert!(host.path(".contextos/packages/ops/1.0.0/sync/sync.rhai").exists());
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(document(&missing)?["error"]["code"], "packages/not-found");
    Ok(())
}

#[tokio::test]
async fn a_vault_the_server_does_not_have_is_refused() -> Result<(), BoxError> {
    let host = Host::new()?;

    let output = host
        .run(&["workflows", "install", "nowhere", "ops", "--format", "json"])
        .await?;

    assert_eq!(output.status.code(), Some(1));
    assert!(document(&output)?["error"]["code"].is_string());
    Ok(())
}

#[tokio::test]
async fn a_version_that_is_not_an_exact_version_is_a_usage_error() -> Result<(), BoxError> {
    let host = Host::new()?;

    let output = host
        .run(&["workflows", "install", "va", "ops", "--version", "^1.0"])
        .await?;

    assert_eq!(output.status.code(), Some(2));
    assert!(!host.path(".contextos/packages").exists());
    Ok(())
}

#[tokio::test]
async fn a_vault_workflow_run_without_a_choice_runs_the_current_release_not_the_source_edited_since()
-> Result<(), BoxError> {
    let host = Host::new()?;
    let installed = host.run(&["workflows", "install", "va", "ops"]).await?;
    assert!(installed.status.success(), "{}", stdout(&installed));
    let state = std::fs::read_to_string(host.path(".contextos/jobs.toml"))?;
    std::fs::write(
        host.path(".contextos/jobs.toml"),
        state.replace("future = \"1.0.0\"", "current = \"1.0.0\""),
    )?;
    write(
        &host.path("registry/workflows/ops/sync/sync.rhai"),
        "throw \"the source ran\";",
    )?;

    let default = host
        .run(&["workflows", "run", "va", "ops", "sync", "--format", "json"])
        .await?;
    let source = host
        .run(&["workflows", "run", "va", "ops", "sync", "--source", "--format", "json"])
        .await?;

    assert!(default.status.success(), "{}", stdout(&default));
    let report = document(&default)?;
    assert_eq!(report["outcome"], "succeeded");
    assert_eq!(report["scope"]["package_version"], "1.0.0");
    assert!(!source.status.success(), "the source script throws");
    assert_eq!(document(&source)?["outcome"], "failed");
    Ok(())
}

#[tokio::test]
async fn a_library_that_calls_a_tool_it_does_not_declare_is_refused_at_install() -> Result<(), BoxError> {
    let host = Host::new()?;
    write(
        &host.path("registry/libraries/crm/library.toml"),
        "modules = [\"crm.rhai\"]\nexports = [\"crm.rhai\"]\n",
    )?;
    write(
        &host.path("registry/libraries/crm/crm.rhai"),
        "fn bill() { call_mcp(\"billing\", \"list_invoices\", #{}) }",
    )?;

    let output = host
        .run(&["libraries", "install", "va", "crm", "--format", "json"])
        .await?;

    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    let report = document(&output)?;
    assert_eq!(report["error"]["code"], "validation/artefact-invalid");
    assert!(
        report["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("list_invoices"),
        "{report}"
    );
    assert!(!host.path(".contextos/packages/crm").exists());
    Ok(())
}
