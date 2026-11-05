//! The `contextos-web jobs create`, `show`, `replace`, and `list` commands as a real process: what each prints, the
//! file each leaves in the host's jobs folder, the JSON documents, and the exit status of a refusal. The host's data
//! directory is redirected to a temporary directory, so nothing here reads or writes the operator's own.
#![cfg(target_os = "linux")]

mod support;

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use serde_json::Value;
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SCRIPT: &str =
    "try { call_mcp(\"contextos\", \"vault_info\", #{}); } catch (e) { flag_degraded(\"the read failed\"); }";
const ERROR_SCENARIO: &str =
    "[[call]]\nserver = \"contextos\"\ntool = \"vault_info\"\nkind = \"error\"\nerror = \"offline\"\n";

/// A job file with a comment and CRLF line endings, which a normalising writer would alter.
const JOB: &[u8] = b"# the nightly sync\r\nname = \"Nightly\"\r\ntimeout_secs = 120\r\nversion = \"1.0.0\"\r\n";
const OTHER_JOB: &[u8] = b"name = \"Nightly, revised\"\ntimeout_secs = 300\nversion = \"1.0.0\"\n";

/// A host with one installed package whose workflow `sync` can be validated, and a web configuration reaching a real
/// `contextos` server for the commands that validate.
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
        let package = data.join("contextos/packages/acme/1.0.0");
        write(
            &package.join("manifest.toml"),
            &format!(
                "name = \"acme\"\nversion = \"1.0.0\"\ndigest = \"{DIGEST}\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n\n\
                 [[workflow.permitted_effects]]\nmcp_server_name = \"contextos\"\ntool = \"vault_info\"\n"
            ),
        )?;
        write(&package.join("sync.rhai"), SCRIPT)?;
        write(&package.join("sync/tests/offline.toml"), ERROR_SCENARIO)?;
        support::release_fixture::seal_io(&package)?;

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

    fn job_path(&self) -> PathBuf {
        self.data.join("contextos/workflows/acme/sync/jobs/nightly.toml")
    }

    fn command(&self, config: &Path, arguments: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_contextos-web"));
        command
            .arg("--config")
            .arg(config)
            .args(arguments)
            .env("XDG_DATA_HOME", &self.data)
            .env("HOME", &self.data)
            .env_remove("CONTEXTOS_WEB_CONFIG");
        command
    }

    /// Runs `arguments` against a web configuration that does not exist, so what it does is shown to need none.
    async fn run(&self, arguments: &[&str], stdin: Option<&[u8]>) -> Result<Output, BoxError> {
        let missing = self.root.path().join("no-such-web.toml");
        spawn(self.command(&missing, arguments), stdin).await
    }

    /// Runs `arguments` against the web configuration that reaches a real server, as validation needs.
    async fn run_configured(&self, arguments: &[&str]) -> Result<Output, BoxError> {
        spawn(self.command(&self.web_config, arguments), None).await
    }

    async fn succeed(&self, arguments: &[&str], stdin: Option<&[u8]>) -> Result<Output, BoxError> {
        let output = self.run(arguments, stdin).await?;
        require_success(arguments, &output)?;
        Ok(output)
    }

    async fn succeed_configured(&self, arguments: &[&str]) -> Result<Output, BoxError> {
        let output = self.run_configured(arguments).await?;
        require_success(arguments, &output)?;
        Ok(output)
    }

    fn state(&self) -> Result<contextos_workflows::JobsState, BoxError> {
        Ok(contextos_web::shared_state::SharedStateStore::new(self.data.join("contextos/state")).read()?)
    }

    fn lifecycle(&self) -> Result<String, BoxError> {
        Ok(self
            .state()?
            .lifecycle_of("acme", "sync", "nightly")
            .as_str()
            .to_owned())
    }
}

async fn spawn(mut command: Command, stdin: Option<&[u8]>) -> Result<Output, BoxError> {
    command
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(bytes).await?;
        pipe.shutdown().await?;
    }
    Ok(child.wait_with_output().await?)
}

fn require_success(arguments: &[&str], output: &Output) -> Result<(), BoxError> {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("{arguments:?} failed: {}", stderr(output)).into())
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The one JSON document a command printed on standard output.
fn document(output: &Output) -> Result<Value, BoxError> {
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("standard output is not one JSON document ({error}): {}", stdout(output)).into())
}

const CREATE: [&str; 8] = ["jobs", "create", "--shared", "acme", "sync", "nightly", "--file", "-"];

fn create_with(extra: &[&'static str]) -> Vec<&'static str> {
    CREATE.iter().copied().chain(extra.iter().copied()).collect()
}

#[tokio::test]
async fn a_job_piped_to_create_is_listed_shown_and_validated_without_writing_host_storage_directly()
-> Result<(), BoxError> {
    let host = Host::new()?;

    let created = host.succeed(&create_with(&[]), Some(JOB)).await?;
    assert!(stdout(&created).contains("created"), "{}", stdout(&created));

    let listed = host
        .succeed(&["jobs", "list", "--shared", "--format", "json"], None)
        .await?;
    let rows = document(&listed)?;
    let jobs = rows["jobs"].as_array().ok_or("jobs is not an array")?;
    assert_eq!(jobs.len(), 1, "{rows}");
    assert_eq!(jobs[0]["job"], "nightly");
    assert_eq!(jobs[0]["state"], "draft");
    assert_eq!(jobs[0]["name"], "Nightly");

    let shown = host
        .succeed(&["jobs", "show", "--shared", "acme", "sync", "nightly"], None)
        .await?;
    assert!(stdout(&shown).contains("name = \"Nightly\""), "{}", stdout(&shown));

    host.succeed_configured(&["jobs", "validate", "--shared", "acme", "sync", "nightly"])
        .await?;
    assert_eq!(host.lifecycle()?, "validated");
    Ok(())
}

#[tokio::test]
async fn a_job_created_from_a_file_is_byte_for_byte_that_file() -> Result<(), BoxError> {
    let host = Host::new()?;
    let source = host.root.path().join("authored.toml");
    let bytes = b"# no trailing newline, CRLF, odd spacing\r\nname   =   \"Nightly\"\r\ntimeout_secs = 120\nversion = \"1.0.0\"";
    std::fs::write(&source, bytes)?;
    let source_text = source.to_string_lossy().into_owned();

    host.succeed(
        &[
            "jobs",
            "create",
            "--shared",
            "acme",
            "sync",
            "nightly",
            "--file",
            &source_text,
        ],
        None,
    )
    .await?;

    assert_eq!(std::fs::read(host.job_path())?, bytes);
    Ok(())
}

#[tokio::test]
async fn creating_the_same_job_twice_reports_unchanged_and_a_different_job_exits_with_job_exists()
-> Result<(), BoxError> {
    let host = Host::new()?;
    let first = host.succeed(&create_with(&["--format", "json"]), Some(JOB)).await?;
    assert_eq!(document(&first)?["outcome"], "created");

    let again = host.succeed(&create_with(&["--format", "json"]), Some(JOB)).await?;
    assert_eq!(document(&again)?["outcome"], "unchanged");

    let different = host.run(&create_with(&[]), Some(OTHER_JOB)).await?;
    assert!(!different.status.success());
    assert_eq!(different.status.code(), Some(1));
    assert!(
        stdout(&different).contains("workflow/job-exists"),
        "{}",
        stdout(&different)
    );
    assert!(stderr(&different).is_empty(), "{}", stderr(&different));
    assert_eq!(std::fs::read(host.job_path())?, JOB);
    Ok(())
}

#[tokio::test]
async fn each_authoring_command_prints_one_json_document_on_stdout_with_format_json() -> Result<(), BoxError> {
    let host = Host::new()?;

    let created = host.succeed(&create_with(&["--format", "json"]), Some(JOB)).await?;
    let created = document(&created)?;
    assert_eq!(created["outcome"], "created");
    assert_eq!(created["package"], "acme");
    assert_eq!(created["job"], "nightly");
    let hash = created["hash"].as_str().ok_or("no hash")?.to_owned();
    assert_eq!(hash.len(), 64);

    let shown = host
        .succeed(
            &[
                "jobs", "show", "--shared", "acme", "sync", "nightly", "--format", "json",
            ],
            None,
        )
        .await?;
    let shown = document(&shown)?;
    assert_eq!(shown["hash"], hash);
    assert_eq!(shown["content"].as_str().map(str::as_bytes), Some(JOB));

    let replaced = host
        .succeed(
            &[
                "jobs",
                "replace",
                "--shared",
                "acme",
                "sync",
                "nightly",
                "--file",
                "-",
                "--expected-hash",
                &hash,
                "--format",
                "json",
            ],
            Some(OTHER_JOB),
        )
        .await?;
    let replaced = document(&replaced)?;
    assert_eq!(replaced["outcome"], "replaced");
    assert_ne!(replaced["hash"], hash.as_str());

    let listed = host
        .succeed(&["jobs", "list", "--shared", "--format", "json"], None)
        .await?;
    assert_eq!(document(&listed)?["jobs"][0]["hash"], replaced["hash"]);
    Ok(())
}

#[tokio::test]
async fn a_refusal_with_format_json_prints_one_error_document_and_exits_unsuccessfully() -> Result<(), BoxError> {
    let host = Host::new()?;

    let missing = host
        .run(
            &[
                "jobs", "show", "--shared", "acme", "sync", "nightly", "--format", "json",
            ],
            None,
        )
        .await?;
    assert_eq!(missing.status.code(), Some(1));
    let refused = document(&missing)?;
    assert_eq!(refused["error"]["code"], "workflow/job-not-found");
    assert!(refused["error"]["message"].is_string(), "{refused}");
    assert!(
        !stderr(&missing).contains("Error:"),
        "a refusal is reported in the result alone: {}",
        stderr(&missing)
    );

    let invalid = host
        .run(
            &create_with(&["--format", "json"]),
            Some(b"name = \"N\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nschedule = \"0 3 * * *\"\n"),
        )
        .await?;
    assert_eq!(invalid.status.code(), Some(1));
    let refused = document(&invalid)?;
    assert_eq!(refused["error"]["code"], "workflow/job-file-invalid");
    assert_eq!(refused["error"]["cause"], "validation/artefact-invalid");
    assert!(!host.job_path().exists());
    Ok(())
}

#[tokio::test]
async fn replacing_with_the_hash_show_printed_stores_the_new_file_and_writes_no_state() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.succeed(&create_with(&[]), Some(JOB)).await?;
    host.succeed_configured(&["jobs", "validate", "--shared", "acme", "sync", "nightly"])
        .await?;
    let state_before = std::fs::read(host.data.join("contextos/state/shared-jobs.toml"))?;
    let shown = host
        .succeed(
            &[
                "jobs", "show", "--shared", "acme", "sync", "nightly", "--format", "json",
            ],
            None,
        )
        .await?;
    let hash = document(&shown)?["hash"].as_str().ok_or("no hash")?.to_owned();

    host.succeed(
        &[
            "jobs",
            "replace",
            "--shared",
            "acme",
            "sync",
            "nightly",
            "--file",
            "-",
            "--expected-hash",
            &hash,
        ],
        Some(OTHER_JOB),
    )
    .await?;

    assert_eq!(std::fs::read(host.job_path())?, OTHER_JOB);
    assert_eq!(
        std::fs::read(host.data.join("contextos/state/shared-jobs.toml"))?,
        state_before,
        "replace changes only the file"
    );
    host.succeed_configured(&["jobs", "validate", "--shared", "acme", "sync", "nightly"])
        .await?;
    Ok(())
}

#[tokio::test]
async fn a_stale_expected_hash_exits_with_job_changed_and_leaves_the_file() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.succeed(&create_with(&[]), Some(JOB)).await?;
    let stale = "0".repeat(64);

    let output = host
        .run(
            &[
                "jobs",
                "replace",
                "--shared",
                "acme",
                "sync",
                "nightly",
                "--file",
                "-",
                "--expected-hash",
                &stale,
            ],
            Some(OTHER_JOB),
        )
        .await?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).contains("workflow/job-changed"), "{}", stdout(&output));
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    assert_eq!(std::fs::read(host.job_path())?, JOB);
    Ok(())
}

#[tokio::test]
async fn deleting_a_retired_job_removes_its_file_so_it_is_not_listed_and_does_not_return_as_a_draft()
-> Result<(), BoxError> {
    let host = Host::new()?;
    // Placed by hand, so this test reaches the delete itself and not the create it does not concern.
    std::fs::create_dir_all(host.job_path().parent().ok_or("no parent")?)?;
    std::fs::write(host.job_path(), JOB)?;
    let job = ["--shared", "acme", "sync", "nightly"];
    for command in ["validate", "dry-run"] {
        host.succeed_configured(&[&["jobs", command][..], &job[..]].concat())
            .await?;
    }
    host.succeed_configured(&[&["jobs", "schedule"][..], &job[..], &["0 3 * * *"][..]].concat())
        .await?;
    host.succeed(&[&["jobs", "retire"][..], &job[..]].concat(), None)
        .await?;
    assert!(host.job_path().exists());

    host.succeed(&[&["jobs", "delete"][..], &job[..]].concat(), None)
        .await?;

    assert!(!host.job_path().exists(), "the job file is removed with its state");
    assert!(host.state()?.job("acme", "sync", "nightly").is_none());
    let listed = host
        .succeed(&["jobs", "list", "--shared", "--format", "json"], None)
        .await?;
    assert_eq!(document(&listed)?["jobs"], serde_json::json!([]));
    Ok(())
}

#[tokio::test]
async fn authoring_commands_need_no_web_configuration_and_no_server() -> Result<(), BoxError> {
    let host = Host::new()?;
    assert!(!host.root.path().join("no-such-web.toml").exists());

    host.succeed(&create_with(&[]), Some(JOB)).await?;
    host.succeed(&["jobs", "show", "--shared", "acme", "sync", "nightly"], None)
        .await?;
    host.succeed(&["jobs", "list", "--shared"], None).await?;
    let shown = host
        .succeed(
            &[
                "jobs", "show", "--shared", "acme", "sync", "nightly", "--format", "json",
            ],
            None,
        )
        .await?;
    let hash = document(&shown)?["hash"].as_str().ok_or("no hash")?.to_owned();
    host.succeed(
        &[
            "jobs",
            "replace",
            "--shared",
            "acme",
            "sync",
            "nightly",
            "--file",
            "-",
            "--expected-hash",
            &hash,
        ],
        Some(OTHER_JOB),
    )
    .await?;
    assert_eq!(std::fs::read(host.job_path())?, OTHER_JOB);
    Ok(())
}

#[tokio::test]
async fn errors_name_the_job_by_key_and_never_print_the_host_data_folder() -> Result<(), BoxError> {
    let host = Host::new()?;
    let data = host.data.to_string_lossy().into_owned();
    let root = host.root.path().to_string_lossy().into_owned();
    host.succeed(&create_with(&[]), Some(JOB)).await?;

    let refusals = [
        host.run(&["jobs", "show", "--shared", "acme", "sync", "missing"], None)
            .await?,
        host.run(&create_with(&[]), Some(OTHER_JOB)).await?,
        host.run(
            &["jobs", "create", "--shared", "acme", "sync", "con", "--file", "-"],
            Some(JOB),
        )
        .await?,
        host.run(
            &create_with(&[]),
            Some(b"name = \"N\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nowner = \"x\"\n"),
        )
        .await?,
        host.run(
            &create_with(&[]),
            Some(b"name = \"N\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nslug = \"secret-value\"\n"),
        )
        .await?,
    ];

    for refusal in &refusals {
        assert_eq!(refusal.status.code(), Some(1));
        let result = stdout(refusal);
        assert!(result.contains("workflow/"), "{result}");
        for printed in [&result, &stderr(refusal)] {
            assert!(!printed.contains(&data) && !printed.contains(&root), "{printed}");
        }
    }
    assert!(stdout(&refusals[0]).contains("missing"), "{}", stdout(&refusals[0]));
    assert!(stdout(&refusals[1]).contains("nightly"), "{}", stdout(&refusals[1]));
    Ok(())
}

#[tokio::test]
async fn two_processes_creating_one_job_with_different_bytes_leave_one_file_and_one_job_exists_refusal()
-> Result<(), BoxError> {
    let host = Host::new()?;
    let arguments = create_with(&["--format", "json"]);

    let (first, second) = tokio::join!(host.run(&arguments, Some(JOB)), host.run(&arguments, Some(OTHER_JOB)));
    let (first, second) = (first?, second?);

    let (winner, loser, winning_bytes) = match (first.status.success(), second.status.success()) {
        (true, false) => (&first, &second, JOB),
        (false, true) => (&second, &first, OTHER_JOB),
        other => {
            return Err(format!(
                "exactly one must win, got {other:?}: {} / {}",
                stderr(&first),
                stderr(&second)
            )
            .into());
        }
    };
    assert_eq!(document(winner)?["outcome"], "created");
    assert_eq!(document(loser)?["error"]["code"], "workflow/job-exists");
    assert_eq!(std::fs::read(host.job_path())?, winning_bytes);
    let leftovers: Vec<_> = std::fs::read_dir(host.job_path().parent().ok_or("no parent")?)?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(leftovers, ["nightly.toml"], "no temporary file is left");
    Ok(())
}

#[tokio::test]
async fn replacing_a_validated_jobs_file_returns_it_to_draft_on_the_next_validation_pass() -> Result<(), BoxError> {
    use support::shared::{FIRST, SharedFixture};

    let fixture =
        SharedFixture::new("ready = true\nname = \"Nightly\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n").await?;
    fixture.pass(FIRST).await?;
    assert_eq!(fixture.lifecycle()?, "validated");
    let authoring = contextos_web::shared_job_authoring::SharedJobAuthoring {
        jobs_root: fixture.jobs_root(),
        store_root: fixture.store_root(),
        state: fixture.state(),
    };
    let key = support::shared::key();
    let before = std::fs::read(fixture.job_path())?;
    let expected = {
        use sha2::{Digest as _, Sha256};
        contextos_core::ContentHash::from(<[u8; 32]>::from(Sha256::digest(&before)))
    };
    let state_before = fixture.state_text()?;

    let replaced = tokio::task::spawn_blocking(move || {
        authoring.replace(
            &key,
            b"name = \"Nightly\"\ntimeout_secs = 300\nversion = \"1.0.0\"\n",
            &expected,
        )
    })
    .await??;

    assert_eq!(
        replaced.outcome,
        contextos_web::shared_job_authoring::AuthoredOutcome::Replaced
    );
    assert_eq!(fixture.state_text()?, state_before, "replace wrote no state");
    fixture.pass(FIRST).await?;
    assert_eq!(
        fixture.lifecycle()?,
        "draft",
        "the new version is not the one that was validated"
    );
    Ok(())
}

#[tokio::test]
async fn a_usage_error_exits_with_status_two_and_writes_nothing_to_standard_output() -> Result<(), BoxError> {
    let host = Host::new()?;

    for arguments in [
        &["jobs", "list", "--shared", "--format", "yaml"][..],
        &["jobs", "show", "--shared", "acme", "sync"][..],
        &[
            "jobs",
            "replace",
            "--shared",
            "acme",
            "sync",
            "nightly",
            "--file",
            "-",
            "--expected-hash",
            "not-a-hash",
        ][..],
    ] {
        let output = host.run(arguments, None).await?;
        assert_eq!(output.status.code(), Some(2), "{arguments:?}: {}", stderr(&output));
        assert!(stdout(&output).is_empty(), "{arguments:?}: {}", stdout(&output));
    }
    Ok(())
}

#[tokio::test]
async fn a_text_list_is_one_aligned_line_per_job_with_each_problem_indented_beneath_it() -> Result<(), BoxError> {
    let host = Host::new()?;
    host.succeed(&create_with(&[]), Some(JOB)).await?;
    let broken = host.job_path().with_file_name("broken.toml");
    std::fs::write(
        &broken,
        b"name = \"B\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nowner = \"x\"\n",
    )?;

    let listed = host.succeed(&["jobs", "list", "--shared"], None).await?;

    let text = stdout(&listed);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert!(lines[0].starts_with("acme/sync/broken"), "{text}");
    assert!(
        lines[1].starts_with("    problem validation/artefact-invalid"),
        "{text}"
    );
    assert!(lines[2].starts_with("acme/sync/nightly"), "{text}");
    assert!(stderr(&listed).is_empty(), "{}", stderr(&listed));
    Ok(())
}
