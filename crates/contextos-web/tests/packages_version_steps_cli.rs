//! `contextos-web packages install` of host bundles as a real process, in both stores: a version that steps further than
//! the interface changes need is accepted, and a version is judged against the highest release the ledger records for the
//! package even after that release has been removed.
#![cfg(target_os = "linux")]

mod support;

use std::path::{Path, PathBuf};
use std::process::Output;

use tokio::process::Command;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const PLAIN: &str = "[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n";
const WITH_DAY: &str =
    "[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\ninputs = [{ name = \"day\", kind = \"string\" }]\n";

struct Host {
    root: tempfile::TempDir,
    data: PathBuf,
    web_config: PathBuf,
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
        Ok(Self { root, data, web_config })
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

    /// A bundle of `acme` at `version` whose workflow `sync` has `workflow` as its manifest entry and `script` as its code.
    fn bundle(&self, version: &str, workflow: &str, script: &str) -> Result<PathBuf, BoxError> {
        let folder = self.root.path().join(format!("bundle-{version}-{}", script.len()));
        support::release_fixture::write_whole_release(
            &folder,
            ("acme", version),
            workflow,
            &[("sync.rhai", script.as_bytes())],
        )
        .map_err(|error| error.to_string())?;
        Ok(folder)
    }

    async fn install(&self, store: &str, bundle: &Path) -> Result<Output, BoxError> {
        self.run(&["packages", "install", store, &bundle.to_string_lossy()])
            .await
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[tokio::test]
async fn a_bundle_whose_version_steps_further_than_its_changes_need_is_accepted_in_either_store() -> Result<(), BoxError>
{
    let host = Host::new()?;
    let first = host.bundle("1.0.0", PLAIN, "let x = 1;")?;
    // Only code differs, which needs a patch step; a major step is more than that and is accepted.
    let larger = host.bundle("3.0.0", PLAIN, "let x = 2;")?;
    for store in ["va", "--shared"] {
        assert!(host.install(store, &first).await?.status.success(), "{store}");

        let installed = host.install(store, &larger).await?;

        assert_eq!(installed.status.code(), Some(0), "{store}: {}", stdout(&installed));
        assert_eq!(stdout(&installed), "Installed acme 3.0.0.\n", "{store}");
    }
    Ok(())
}

#[tokio::test]
async fn a_bundle_is_judged_against_the_highest_release_the_ledger_records_even_after_it_was_removed()
-> Result<(), BoxError> {
    let host = Host::new()?;
    let first = host.bundle("1.0.0", PLAIN, "let x = 1;")?;
    let with_day = host.bundle("2.0.0", WITH_DAY, "let x = 2;")?;
    // Against 1.0.0 (no input) this differs by code only and would need a patch step; against the removed 2.0.0 it
    // removes an input and needs a major step.
    let without_day = host.bundle("2.0.1", PLAIN, "let x = 3;")?;
    let major = host.bundle("3.0.0", PLAIN, "let x = 3;")?;
    for store in ["va", "--shared"] {
        assert!(host.install(store, &first).await?.status.success(), "{store}");
        assert!(host.install(store, &with_day).await?.status.success(), "{store}");
        let removed = host.run(&["packages", "remove", store, "acme", "2.0.0"]).await?;
        assert!(removed.status.success(), "{store}: {}", stdout(&removed));

        let refused = host.install(store, &without_day).await?;

        assert_eq!(refused.status.code(), Some(1), "{store}");
        let text = stdout(&refused);
        assert!(
            text.starts_with("refused packages/version-step-invalid: "),
            "{store}: {text}"
        );
        assert!(text.contains("input day removed"), "{store}: {text}");
        assert!(host.install(store, &major).await?.status.success(), "{store}");
    }
    Ok(())
}
