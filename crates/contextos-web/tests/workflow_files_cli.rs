//! A workflow's script reads the files its workflow declares, through `workflow_file` and `workflow_files`, and writes
//! them to a vault through `call_mcp`: a shared workflow from the installed release its run resolved, a vault workflow
//! from its own folder under `registry/workflows/`. Run as a real `contextos-web` process against a real `contextos`
//! server, with the host's data directory redirected to a temporary directory.
#![cfg(target_os = "linux")]

mod support;

use std::path::{Path, PathBuf};
use std::process::Output;

use tokio::process::Command;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// Writes every declared file under `scaffold` into the target vault beneath `out/`.
const DEPLOY: &str = r#"
for path in workflow_files("scaffold") {
    let file = workflow_file(path);
    call_mcp("contextos", "fs_write_file", #{ path: inputs.target + "://out/" + path, content: file.content });
}
"#;

const AGENTS: &str = "# Agents\n\nCafé, naïve, 日本語.\n";

struct Host {
    root: tempfile::TempDir,
    data: PathBuf,
    web_config: PathBuf,
    vault_a: PathBuf,
    vault_b: PathBuf,
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
        let host = Self {
            web_config: root.path().join("web.toml"),
            data,
            vault_a,
            vault_b,
            root,
        };
        host.install("1.0.0", AGENTS)?;

        let mcp_config =
            support::write_named_vaults_config(host.root.path(), &[("va", &host.vault_a), ("vb", &host.vault_b)])?;
        let mcp_binary = support::contextos_mcp_binary()?;
        write(
            &host.web_config,
            &format!(
                "[[mcp_server]]\ntransport = \"stdio\"\nname = \"contextos\"\ncommand = {:?}\nargs = [\"--config\", {:?}]\n",
                mcp_binary.to_string_lossy(),
                mcp_config.to_string_lossy()
            ),
        )?;
        Ok(host)
    }

    /// Installs release `version` of the `kit` package, whose `scaffold` folder holds `AGENTS.md` with `agents` and
    /// `memory/notes.md`.
    fn install(&self, version: &str, agents: &str) -> Result<(), BoxError> {
        let package = self.data.join(format!("contextos/packages/kit/{version}"));
        write(
            &package.join("manifest.toml"),
            &format!(
                "name = \"kit\"\nversion = \"{version}\"\ndigest = \"{DIGEST}\"\ntemplates = [\"scaffold\"]\n\n\
                 [[workflow]]\nslug = \"deploy\"\nentry = \"deploy.rhai\"\n\n\
                 [[workflow.inputs]]\nname = \"target\"\nkind = \"vault\"\n\n\
                 [[workflow.permitted_effects]]\nmcp_server_name = \"contextos\"\ntool = \"fs_write_file\"\n\n\
                 [[workflow]]\nslug = \"peek\"\nentry = \"peek.rhai\"\n"
            ),
        )?;
        write(&package.join("deploy.rhai"), DEPLOY)?;
        write(&package.join("peek.rhai"), "workflow_file(\"deploy.rhai\");")?;
        write(&package.join("scaffold/AGENTS.md"), agents)?;
        write(&package.join("scaffold/memory/notes.md"), "# Memory\n")?;
        let mut scenario = String::new();
        for path in ["AGENTS.md", "memory/notes.md"] {
            scenario.push_str("[[call]]\nserver = \"contextos\"\ntool = \"fs_write_file\"\nkind = \"valid\"\n");
            scenario.push_str(&["response = { path = \"out/scaffold/", path, "\", content_hash = \""].concat());
            scenario.push_str(&"0".repeat(64));
            scenario.push_str("\", warnings = [] }\n");
        }
        write(&package.join("deploy/tests/writes.toml"), &scenario)?;
        support::release_fixture::seal_io(&package)?;
        Ok(())
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
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[tokio::test]
async fn a_shared_workflow_writes_its_declared_files_to_a_vault_byte_for_byte() -> Result<(), BoxError> {
    let host = Host::new()?;

    let output = host
        .run(&[
            "workflows",
            "run",
            "--shared",
            "--release",
            "1.0.0",
            "kit",
            "deploy",
            "--input",
            "target=va",
        ])
        .await?;

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        std::fs::read_to_string(host.vault_a.join("out/scaffold/AGENTS.md"))?,
        AGENTS
    );
    assert_eq!(
        std::fs::read_to_string(host.vault_a.join("out/scaffold/memory/notes.md"))?,
        "# Memory\n"
    );
    Ok(())
}

#[tokio::test]
async fn a_shared_workflow_cannot_read_a_file_its_package_does_not_declare() -> Result<(), BoxError> {
    let host = Host::new()?;

    let output = host
        .run(&["workflows", "run", "--shared", "--release", "1.0.0", "kit", "peek"])
        .await?;

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("workflow/file-undeclared"),
        "{}",
        stderr(&output)
    );
    Ok(())
}

#[tokio::test]
async fn each_run_reads_the_files_of_the_release_it_resolved_across_an_upgrade() -> Result<(), BoxError> {
    let host = Host::new()?;
    let before = host
        .run(&[
            "workflows",
            "run",
            "--shared",
            "--release",
            "1.0.0",
            "kit",
            "deploy",
            "--input",
            "target=va",
        ])
        .await?;
    host.install("1.1.0", "# Agents, upgraded\n")?;

    let after = host
        .run(&[
            "workflows",
            "run",
            "--shared",
            "--release",
            "1.1.0",
            "kit",
            "deploy",
            "--input",
            "target=vb",
        ])
        .await?;

    assert!(before.status.success(), "{}", stderr(&before));
    assert!(after.status.success(), "{}", stderr(&after));
    assert_eq!(
        std::fs::read_to_string(host.vault_a.join("out/scaffold/AGENTS.md"))?,
        AGENTS
    );
    assert_eq!(
        std::fs::read_to_string(host.vault_b.join("out/scaffold/AGENTS.md"))?,
        "# Agents, upgraded\n"
    );
    Ok(())
}

#[tokio::test]
async fn a_dry_run_reads_the_declared_files_and_writes_nothing_to_the_vault() -> Result<(), BoxError> {
    let host = Host::new()?;

    let output = host
        .run(&[
            "workflows",
            "run",
            "--shared",
            "--release",
            "1.0.0",
            "kit",
            "deploy",
            "--input",
            "target=va",
            "--dry-run",
        ])
        .await?;

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("Dry run succeeded."), "{}", stdout(&output));
    assert!(!host.vault_a.join("out").exists(), "a dry run wrote to the vault");
    Ok(())
}

/// Registers the vault workflow `seed` in vault `va`, declaring `templates` and holding `files/hello.md`.
fn register_vault_workflow(host: &Host, templates: &str) -> Result<(), BoxError> {
    let package = host.vault_a.join("registry/workflows/seed");
    let folder = package.join("seed");
    write(
        &package.join("workflows.toml"),
        &format!("workflows = [\"seed\"]\ntemplates = {templates}\n"),
    )?;
    write(
        &folder.join("workflow.toml"),
        "name = \"Seed\"\nentry = \"seed.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
    )?;
    write(
        &folder.join("seed.rhai"),
        r#"
        let paths = workflow_files();
        if paths.is_empty() { throw "the workflow's declared files were not found"; }
        for path in paths {
            call_mcp("contextos", "fs_write_file", #{ path: "va://out/" + path, content: workflow_file(path).content });
        }
        "#,
    )?;
    write(&package.join("files/hello.md"), "# Hello\n")?;
    write(
        &folder.join("jobs/nightly.toml"),
        "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    write(
        &host.vault_a.join(".contextos/team.toml"),
        "[[member]]\nslug = \"atlas\"\nname = \"Atlas\"\nchief_of_staff = true\n\n[[member]]\nslug = \"vela\"\nname = \"Vela\"\n",
    )?;
    support::release_fixture::install_vault_releases(&host.vault_a).map_err(|error| error.to_string())?;
    Ok(())
}

#[tokio::test]
async fn a_vault_workflow_reads_the_files_its_package_declares() -> Result<(), BoxError> {
    let host = Host::new()?;
    register_vault_workflow(&host, "[\"files\"]")?;

    let output = host
        .run(&["workflows", "run", "va", "seed", "seed", "--source"])
        .await?;

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        std::fs::read_to_string(host.vault_a.join("out/files/hello.md"))?,
        "# Hello\n"
    );
    Ok(())
}

#[tokio::test]
async fn a_vault_workflow_that_declares_a_template_it_does_not_hold_still_validates_because_templates_are_read_live()
-> Result<(), BoxError> {
    let host = Host::new()?;
    register_vault_workflow(&host, "[\"files\", \"absent.md\"]")?;

    let output = host.run(&["jobs", "validate", "va", "seed", "seed", "nightly"]).await?;

    assert!(output.status.success(), "{}{}", stdout(&output), stderr(&output));
    Ok(())
}

#[tokio::test]
async fn a_vault_workflow_whose_templates_could_leave_its_folder_fails_validation() -> Result<(), BoxError> {
    let host = Host::new()?;
    register_vault_workflow(&host, "[\"../../../index.md\"]")?;

    let output = host.run(&["jobs", "validate", "va", "seed", "seed", "nightly"]).await?;

    assert!(!output.status.success());
    let reported = format!("{}{}", stdout(&output), stderr(&output));
    assert!(reported.contains("validation/artefact-invalid"), "{reported}");
    assert!(reported.contains("templates entry"), "{reported}");
    Ok(())
}

#[tokio::test]
async fn a_dry_run_of_a_saved_vault_job_reads_its_workflows_files_and_writes_nothing() -> Result<(), BoxError> {
    let host = Host::new()?;
    register_vault_workflow(&host, "[\"files\"]")?;
    write(
        &host.vault_a.join("registry/workflows/seed/seed/tests/writes.toml"),
        &format!(
            "[[call]]\nserver = \"contextos\"\ntool = \"fs_write_file\"\nkind = \"valid\"\n\
             response = {{ path = \"out/files/hello.md\", content_hash = \"{}\", warnings = [] }}\n",
            "0".repeat(64)
        ),
    )?;

    let output = host.run(&["jobs", "dry-run", "va", "seed", "seed", "nightly"]).await?;

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("Dry run succeeded."), "{}", stdout(&output));
    assert!(!host.vault_a.join("out").exists(), "a dry run wrote to the vault");
    Ok(())
}

#[tokio::test]
async fn a_vault_workflow_cannot_read_a_file_in_its_folder_it_does_not_declare() -> Result<(), BoxError> {
    let host = Host::new()?;
    register_vault_workflow(&host, "[\"files\"]")?;
    write(
        &host.vault_a.join("registry/workflows/seed/seed/seed.rhai"),
        "workflow_file(\"jobs/nightly.toml\");",
    )?;

    let output = host
        .run(&["workflows", "run", "va", "seed", "seed", "--source"])
        .await?;

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("workflow/file-undeclared"),
        "{}",
        stderr(&output)
    );
    Ok(())
}

#[tokio::test]
async fn a_vault_workflow_is_refused_a_declared_file_that_is_binary_or_over_the_limit() -> Result<(), BoxError> {
    let host = Host::new()?;
    register_vault_workflow(&host, "[\"files\", \"logo.png\", \"big.md\"]")?;
    let folder = host.vault_a.join("registry/workflows/seed");
    std::fs::write(folder.join("logo.png"), [0x89, b'P', b'N', b'G', 0x00, 0xff, 0xfe])?;
    write(&folder.join("big.md"), &"a".repeat(1024 * 1024 + 1))?;

    let mut codes = Vec::new();
    for (path, code) in [
        ("logo.png", "workflow/file-not-text"),
        ("big.md", "workflow/file-too-large"),
    ] {
        write(&folder.join("seed/seed.rhai"), &format!("workflow_file(\"{path}\");"))?;
        let output = host
            .run(&["workflows", "run", "va", "seed", "seed", "--source"])
            .await?;
        assert!(!output.status.success());
        codes.push((code, stderr(&output).contains(code), stderr(&output)));
    }

    for (code, found, reported) in codes {
        assert!(found, "expected {code}: {reported}");
    }
    Ok(())
}

/// What a link in a vault workflow's folder does: the primary server follows it within the vault, as it does for every
/// MCP read, and refuses one that leaves the vault. A link inside an installed package release is refused outright.
#[tokio::test]
async fn a_link_in_a_vault_workflows_folder_is_followed_within_the_vault_and_refused_beyond_it() -> Result<(), BoxError>
{
    let host = Host::new()?;
    register_vault_workflow(&host, "[\"files\"]")?;
    let folder = host.vault_a.join("registry/workflows/seed/files");
    write(&host.vault_a.join("shared/notes.md"), "# Shared\n")?;
    std::os::unix::fs::symlink(host.vault_a.join("shared/notes.md"), folder.join("inside.md"))?;
    std::os::unix::fs::symlink(host.vault_b.join("index.md"), folder.join("outside.md"))?;
    let script = host.vault_a.join("registry/workflows/seed/seed/seed.rhai");

    write(
        &script,
        "if workflow_file(\"files/inside.md\").content != \"# Shared\\n\" { throw \"not followed\"; }",
    )?;
    let inside = host
        .run(&["workflows", "run", "va", "seed", "seed", "--source"])
        .await?;
    write(&script, "workflow_file(\"files/outside.md\");")?;
    let outside = host
        .run(&["workflows", "run", "va", "seed", "seed", "--source"])
        .await?;

    assert!(inside.status.success(), "{}", stderr(&inside));
    assert!(!outside.status.success());
    assert!(
        stderr(&outside).contains("workflow/file-unreadable") || stderr(&outside).contains("workflow/file-not-found"),
        "{}",
        stderr(&outside)
    );
    assert!(
        !stderr(&outside).contains("# B"),
        "content beyond the vault was returned"
    );
    Ok(())
}

#[tokio::test]
async fn a_vault_workflow_lists_files_in_nested_folders() -> Result<(), BoxError> {
    let host = Host::new()?;
    register_vault_workflow(&host, "[\"files\"]")?;
    write(
        &host.vault_a.join("registry/workflows/seed/files/deep/er/notes.md"),
        "# Deep\n",
    )?;

    let output = host
        .run(&["workflows", "run", "va", "seed", "seed", "--source"])
        .await?;

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        std::fs::read_to_string(host.vault_a.join("out/files/deep/er/notes.md"))?,
        "# Deep\n"
    );
    Ok(())
}
