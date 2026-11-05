//! Two `contextos-web` processes installing into one vault's package store at the same time are serialised by the store's
//! lock: each either installs whole or is turned away with `packages/store-locked`, and nothing partial is left behind.
//! Which of the two wins is not asserted, because it is up to the operating system; every outcome allowed is.
#![cfg(target_os = "linux")]

mod support;

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;
use tokio::process::Command;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const WORKFLOW: &str = "[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n";

fn write(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

struct Host {
    root: tempfile::TempDir,
    data: PathBuf,
    web_config: PathBuf,
    vault: PathBuf,
}

impl Host {
    fn new() -> Result<Self, BoxError> {
        let root = tempfile::tempdir()?;
        let data = root.path().join("data");
        let vault = root.path().join("va");
        write(&vault.join("index.md"), "# A\n")?;
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

    fn bundle(&self, name: &str) -> Result<PathBuf, BoxError> {
        let folder = self.root.path().join(format!("bundle-{name}"));
        support::release_fixture::write_whole_release(
            &folder,
            (name, "1.0.0"),
            WORKFLOW,
            &[("sync.rhai", b"let x = 1;")],
        )
        .map_err(|error| error.to_string())?;
        Ok(folder)
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
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[tokio::test]
async fn two_processes_installing_into_one_vault_store_are_serialised_and_leave_nothing_partial() -> Result<(), BoxError>
{
    let host = Host::new()?;
    let first = host.bundle("alpha")?;
    let second = host.bundle("beta")?;
    let first_text = first.to_string_lossy().into_owned();
    let second_text = second.to_string_lossy().into_owned();

    let first_arguments = ["packages", "install", "va", first_text.as_str()];
    let second_arguments = ["packages", "install", "va", second_text.as_str()];
    let (a, b) = tokio::join!(host.run(&first_arguments), host.run(&second_arguments));
    let outcomes = [("alpha", a?), ("beta", b?)];

    let mut installed = Vec::new();
    for (name, output) in &outcomes {
        if output.status.success() {
            assert_eq!(stdout(output), format!("Installed {name} 1.0.0.\n"));
            installed.push(*name);
        } else {
            assert_eq!(output.status.code(), Some(1), "{name}: {}", stdout(output));
            assert!(
                stdout(output).starts_with("refused packages/store-locked: "),
                "{name}: {}",
                stdout(output)
            );
        }
    }
    assert!(
        !installed.is_empty(),
        "at least one install gets the lock: {outcomes:?}"
    );
    let store = host.vault.join(".contextos/packages");
    for entry in std::fs::read_dir(&store)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        assert!(
            name == "ledger.toml" || name.starts_with(".lock") || installed.contains(&name.as_str()),
            "nothing partial is left in the store: {name}"
        );
    }
    let listed = host.run(&["packages", "list", "va", "--format", "json"]).await?;
    let document: Value = serde_json::from_str(&stdout(&listed))?;
    assert_eq!(document["ledger"]["state"], "present");
    let mut recorded: Vec<&str> = document["releases"]
        .as_array()
        .ok_or("no releases")?
        .iter()
        .filter_map(|release| release["name"].as_str())
        .collect();
    recorded.sort_unstable();
    assert_eq!(recorded, installed, "the ledger records exactly what was installed");
    // The one that was turned away installs on its own afterwards: the store was left whole.
    for (name, output) in &outcomes {
        if !output.status.success() {
            let bundle = if *name == "alpha" { &first_text } else { &second_text };
            let retried = host.run(&["packages", "install", "va", bundle]).await?;
            assert!(retried.status.success(), "{name}: {}", stdout(&retried));
        }
    }
    Ok(())
}
