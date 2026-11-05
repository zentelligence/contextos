//! `contextos-web packages list`, `inspect`, and `remove` as a real process over a real vault and a real host store: how a
//! store is named, what each command prints in text and in JSON, the guard on removal and `--force`, and rebuilding a
//! ledger that is missing.
#![cfg(target_os = "linux")]

mod support;

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;
use tokio::process::Command;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\n";

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
    /// A vault `va` holding the source of the workflow package `ops`, a bundle of the package `acme`, and a web
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

    /// Installs the vault's `ops` package from source into the vault's store.
    async fn install_ops(&self) -> Result<(), BoxError> {
        self.succeed(&["workflows", "install", "va", "ops"]).await?;
        Ok(())
    }

    /// Installs the package `acme` from its folder into the host store.
    async fn install_acme(&self) -> Result<(), BoxError> {
        let bundle = self.root.path().join("bundle");
        self.succeed(&["packages", "install", "--shared", &bundle.to_string_lossy()])
            .await?;
        Ok(())
    }

    fn vault_file(&self, relative: &str) -> PathBuf {
        self.vault.join(relative)
    }

    fn host_file(&self, relative: &str) -> PathBuf {
        self.data.join("contextos").join(relative)
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
async fn a_vault_store_is_listed_with_its_release_origin_workflow_kind_and_reason() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.install_ops().await?;

    let output = host.succeed(&["packages", "list", "va"]).await?;

    let text = stdout(&output);
    for line in [
        "Package store va",
        "Ledger: 1 release recorded",
        "ops 1.0.0",
        "    sync: future, no lock yet",
        "    kept: it is the newest future release of sync",
    ] {
        assert!(text.lines().any(|found| found == line), "missing {line:?} in\n{text}");
    }
    assert!(text.contains("whole, from a source, installed "), "{text}");
    Ok(())
}

#[tokio::test]
async fn a_bundle_is_installed_into_a_vault_store_and_listed_with_a_bundle_origin() -> Result<(), BoxError> {
    let host = Host::new()?;
    let bundle = host.root.path().join("bundle");

    let installed = host
        .succeed(&["packages", "install", "va", &bundle.to_string_lossy()])
        .await?;

    assert_eq!(stdout(&installed), "Installed acme 1.0.0.\n");
    assert!(host.vault_file(".contextos/packages/acme/1.0.0").is_dir());
    assert!(!host.host_file("packages/acme/1.0.0").exists());
    let listed = document(&host.succeed(&["packages", "list", "va", "--format", "json"]).await?)?;
    assert_eq!(listed["releases"][0]["name"], "acme");
    assert_eq!(listed["releases"][0]["origin"], "bundle");
    Ok(())
}

#[tokio::test]
async fn installing_a_bundle_that_is_not_there_into_a_vault_store_is_refused_with_its_code() -> Result<(), BoxError> {
    let host = Host::new()?;
    let missing = host.root.path().join("no-such-bundle");

    let refused = host
        .run(&["packages", "install", "va", &missing.to_string_lossy()])
        .await?;

    assert_eq!(refused.status.code(), Some(1));
    assert!(
        stdout(&refused).starts_with("refused packages/store-unavailable: "),
        "{}",
        stdout(&refused)
    );
    assert!(!host.vault_file(".contextos/packages/acme").exists());
    Ok(())
}

/// A second release of `acme` whose workflow gains a required input, which a job has to follow.
fn write_acme_with_a_required_input(host: &Host, version: &str) -> Result<PathBuf, BoxError> {
    let folder = host.root.path().join(format!("bundle-{version}"));
    support::release_fixture::write_whole_release(
        &folder,
        ("acme", version),
        "[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\ninputs = [{ name = \"day\", kind = \"string\" }]\n",
        &[("sync.rhai", b"let x = 1;")],
    )
    .map_err(|error| error.to_string())?;
    Ok(folder)
}

async fn install_into(host: &Host, store: &str, bundle: &Path) -> Result<Output, BoxError> {
    host.run(&["packages", "install", store, &bundle.to_string_lossy()])
        .await
}

#[tokio::test]
async fn a_bundle_whose_version_steps_less_than_its_interface_changes_need_is_refused_in_either_store()
-> Result<(), BoxError> {
    let host = Host::new()?;
    let first = host.root.path().join("bundle");
    let patch = write_acme_with_a_required_input(&host, "1.0.1")?;
    let major = write_acme_with_a_required_input(&host, "2.0.0")?;

    for store in ["va", "--shared"] {
        assert!(install_into(&host, store, &first).await?.status.success());

        let refused = install_into(&host, store, &patch).await?;

        assert_eq!(refused.status.code(), Some(1), "{store}");
        let text = stdout(&refused);
        assert!(
            text.starts_with("refused packages/version-step-invalid: "),
            "{store}: {text}"
        );
        assert!(text.contains("required input day added"), "{store}: {text}");
        assert!(install_into(&host, store, &major).await?.status.success(), "{store}");
    }
    Ok(())
}

#[tokio::test]
async fn a_vault_store_is_listed_as_one_json_document() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.install_ops().await?;

    let output = host.succeed(&["packages", "list", "va", "--format", "json"]).await?;

    let listed = document(&output)?;
    assert_eq!(listed["store"], "va");
    assert_eq!(listed["ledger"]["state"], "present");
    assert_eq!(listed["releases"][0]["name"], "ops");
    assert_eq!(listed["releases"][0]["origin"], "source");
    assert_eq!(listed["releases"][0]["workflows"][0]["kind"], "future");
    assert_eq!(listed["releases"][0]["kept_by"][0]["reason"], "newest-future");
    assert_eq!(listed["releases"][0]["kept_by"][0]["guards_removal"], false);
    Ok(())
}

#[tokio::test]
async fn the_host_store_is_named_with_shared_and_listed_the_same_way() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.install_acme().await?;

    let output = host
        .succeed(&["packages", "list", "--shared", "--format", "json"])
        .await?;

    let listed = document(&output)?;
    assert_eq!(listed["store"], "shared");
    assert_eq!(listed["releases"][0]["name"], "acme");
    assert_eq!(listed["releases"][0]["origin"], "bundle");
    let text = stdout(&host.succeed(&["packages", "list", "--shared"]).await?);
    assert!(text.starts_with("Package store shared\n"), "{text}");
    Ok(())
}

#[tokio::test]
async fn one_release_is_inspected_and_one_that_is_not_there_is_refused_with_its_code() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.install_ops().await?;

    let found = host
        .succeed(&["packages", "inspect", "va", "ops", "1.0.0", "--format", "json"])
        .await?;
    let missing = host
        .run(&["packages", "inspect", "va", "ops", "9.0.0", "--format", "json"])
        .await?;

    let release = document(&found)?;
    assert_eq!(
        (release["name"].as_str(), release["version"].as_str()),
        (Some("ops"), Some("1.0.0"))
    );
    assert!(
        release["digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:"))
    );
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(document(&missing)?["error"]["code"], "packages/not-found");
    Ok(())
}

#[tokio::test]
async fn removing_a_release_a_job_file_names_is_refused_naming_the_job_until_forced() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.install_ops().await?;
    write(
        &host.vault_file("registry/workflows/ops/sync/jobs/nightly.toml"),
        "version = \"1.0.0\"\n",
    )?;

    let refused = host.run(&["packages", "remove", "va", "ops", "1.0.0"]).await?;

    assert_eq!(refused.status.code(), Some(1));
    let text = stdout(&refused);
    assert!(text.starts_with("refused packages/in-use: "), "{text}");
    assert!(text.contains("job nightly of sync names it"), "{text}");
    assert!(host.vault_file(".contextos/packages/ops/1.0.0").is_dir());

    let forced = host
        .succeed(&["packages", "remove", "va", "ops", "1.0.0", "--force"])
        .await?;

    assert!(
        stdout(&forced).starts_with("Removed ops 1.0.0 from va, forced.\n"),
        "{}",
        stdout(&forced)
    );
    assert!(!host.vault_file(".contextos/packages/ops/1.0.0").exists());
    Ok(())
}

#[tokio::test]
async fn a_forced_removal_in_json_names_the_cause_and_what_was_overridden() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.install_ops().await?;
    write(
        &host.vault_file("registry/workflows/ops/sync/jobs/nightly.toml"),
        "version = \"1.0.0\"\n",
    )?;

    let forced = host
        .succeed(&[
            "packages", "remove", "va", "ops", "1.0.0", "--force", "--format", "json",
        ])
        .await?;

    let removed = document(&forced)?;
    assert_eq!(removed["cause"], "forced");
    assert_eq!(removed["overridden"][0]["job"], "nightly");
    assert_eq!(removed["store"], "va");
    Ok(())
}

#[tokio::test]
async fn the_host_guards_removal_the_same_way() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.install_acme().await?;
    write(
        &host.host_file("workflows/acme/sync/jobs/nightly.toml"),
        "name = \"J\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;

    let refused = host.run(&["packages", "remove", "--shared", "acme", "1.0.0"]).await?;
    assert_eq!(refused.status.code(), Some(1));
    assert!(stdout(&refused).contains("packages/in-use"), "{}", stdout(&refused));
    assert!(host.host_file("packages/acme/1.0.0").is_dir());

    host.succeed(&["packages", "remove", "--shared", "acme", "1.0.0", "--force"])
        .await?;

    assert!(!host.host_file("packages/acme/1.0.0").exists());
    Ok(())
}

#[tokio::test]
async fn a_missing_ledger_refuses_a_removal_until_it_is_rebuilt_from_the_releases_present() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.install_ops().await?;
    std::fs::remove_file(host.vault_file(".contextos/packages/ledger.toml"))?;

    let refused = host
        .run(&["packages", "remove", "va", "ops", "1.0.0", "--format", "json"])
        .await?;
    assert_eq!(document(&refused)?["error"]["code"], "packages/ledger-unreadable");

    let rebuilt = host.succeed(&["packages", "inspect", "va", "--rebuild-ledger"]).await?;
    assert_eq!(
        stdout(&rebuilt),
        "Rebuilt the ledger of va from 1 release.\n    ops 1.0.0\n"
    );

    host.succeed(&["packages", "remove", "va", "ops", "1.0.0", "--force"])
        .await?;
    assert!(!host.vault_file(".contextos/packages/ops/1.0.0").exists());
    Ok(())
}

#[tokio::test]
async fn a_ledger_that_is_present_is_not_rebuilt() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.install_acme().await?;

    let refused = host
        .run(&[
            "packages",
            "inspect",
            "--shared",
            "--rebuild-ledger",
            "--format",
            "json",
        ])
        .await?;

    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(document(&refused)?["error"]["code"], "packages/ledger-present");
    Ok(())
}

#[tokio::test]
async fn the_wrong_number_of_words_is_a_usage_error() -> Result<(), BoxError> {
    let host = Host::new()?;

    for arguments in [
        &["packages", "list"][..],
        &["packages", "list", "va", "extra"],
        &["packages", "remove", "va", "ops"],
        &["packages", "remove", "--shared", "va", "ops", "1.0.0"],
        &["packages", "inspect", "va", "ops"],
        &["packages", "install", "some-folder"],
        &["packages", "install", "--shared", "one-folder", "another"],
        &["packages", "install", "va", "one-folder", "another"],
        &["packages", "install"],
    ] {
        let output = host.run(arguments).await?;
        assert_eq!(output.status.code(), Some(2), "{arguments:?}: {}", stderr(&output));
    }
    Ok(())
}

#[tokio::test]
async fn the_remove_help_explains_the_guard_and_force() -> Result<(), BoxError> {
    let host = Host::new()?;

    let help = host.succeed(&["packages", "remove", "--help"]).await?;

    let text = stdout(&help);
    assert!(text.contains("--force"), "{text}");
    assert!(text.contains("packages/in-use"), "{text}");
    Ok(())
}
