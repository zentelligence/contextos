//! Where the authoring commands send their diagnostics, what they warn about, and what their errors never repeat, as a
//! real process. The host's data directory is redirected to a temporary directory, so nothing here reads or writes the
//! operator's own.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use contextos_workflows::{
    Actor, JobFingerprint, JobSchedule, ReleaseFingerprintInput, StoreKind, Transition, TransitionError,
};
use serde_json::Value;
use time::macros::datetime;
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;

mod support;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

const JOB: &[u8] = b"name = \"Nightly\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n";
const REVISED: &[u8] = b"name = \"Nightly, revised\"\ntimeout_secs = 300\nversion = \"1.0.0\"\n";
const CREATE: [&str; 9] = [
    "jobs", "create", "--shared", "acme", "sync", "nightly", "--file", "-", "--format",
];

struct Host {
    root: tempfile::TempDir,
    data: PathBuf,
}

impl Host {
    fn new() -> Result<Self, BoxError> {
        let root = tempfile::tempdir()?;
        let data = root.path().join("data");
        std::fs::create_dir_all(&data)?;
        let host = Self { root, data };
        host.install_package(&format!(
            "name = \"acme\"\nversion = \"1.0.0\"\ndigest = \"{DIGEST}\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n"
        ))?;
        std::fs::write(
            host.data.join("contextos/packages/acme/1.0.0/sync.rhai"),
            "log_info(\"a\");",
        )?;
        support::release_fixture::seal_io(&host.data.join("contextos/packages/acme/1.0.0"))?;
        Ok(host)
    }

    fn log_path(&self) -> PathBuf {
        self.root.path().join("contextos-web.log")
    }

    fn logged(&self) -> String {
        std::fs::read_to_string(self.log_path()).unwrap_or_default()
    }

    /// A web configuration that sends the log to a file at `level`, and needs no server.
    fn logging_config(&self, level: &str) -> Result<PathBuf, BoxError> {
        let path = self.root.path().join("web.toml");
        std::fs::write(
            &path,
            format!(
                "[server]\nlog_level = \"{level}\"\nlog_file = \"{}\"\n",
                self.log_path().display()
            ),
        )?;
        Ok(path)
    }

    fn install_package(&self, manifest: &str) -> Result<(), BoxError> {
        let directory = self.data.join("contextos/packages/acme/1.0.0");
        std::fs::create_dir_all(&directory)?;
        std::fs::write(directory.join("manifest.toml"), manifest)?;
        Ok(())
    }

    fn job_path(&self) -> PathBuf {
        self.data.join("contextos/workflows/acme/sync/jobs/nightly.toml")
    }

    /// Takes a job to `retired` in the host state, as the lifecycle commands would, so a delete is allowed.
    fn retire(&self) -> Result<(), BoxError> {
        let version = JobFingerprint::from(&ReleaseFingerprintInput {
            store: StoreKind::Host,
            package: "acme",
            release: "1.0.0",
            digest: "sha256:0",
            job_toml: "",
            release_manifest: "",
            libraries: &[],
        });
        let now = datetime!(2026-10-04 03:00:00 UTC);
        let schedule = JobSchedule::try_from("0 3 * * *")?;
        contextos_web::shared_state::SharedStateStore::new(self.data.join("contextos/state")).update(
            |jobs| -> Result<(), TransitionError> {
                for (actor, transition) in [
                    (Actor::Validator, Transition::Validated(version.clone())),
                    (Actor::DryRun, Transition::DryRunCompleted(version.clone())),
                    (Actor::Operator, Transition::Enable(schedule, version.clone())),
                    (Actor::Operator, Transition::Retire),
                ] {
                    jobs.transition("acme", "sync", "nightly", actor, &transition, now)?;
                }
                Ok(())
            },
        )??;
        Ok(())
    }

    async fn run(&self, config: &Path, arguments: &[&str], stdin: Option<&[u8]>) -> Result<Output, BoxError> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_contextos-web"));
        command
            .arg("--config")
            .arg(config)
            .args(arguments)
            .env("XDG_DATA_HOME", &self.data)
            .env("HOME", &self.data)
            .env_remove("CONTEXTOS_WEB_CONFIG")
            .env_remove("RUST_LOG")
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
            // A command that refuses before it reads its input closes the pipe first, which is not a failure here.
            match pipe.write_all(bytes).await {
                Err(error) if error.kind() != std::io::ErrorKind::BrokenPipe => return Err(error.into()),
                _ => {}
            }
            let _ = pipe.shutdown().await;
        }
        Ok(child.wait_with_output().await?)
    }

    fn missing_config(&self) -> PathBuf {
        self.root.path().join("no-such-web.toml")
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn document(output: &Output) -> Result<Value, BoxError> {
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("standard output is not one JSON document ({error}): {}", stdout(output)).into())
}

#[tokio::test]
async fn creating_replacing_and_deleting_a_shared_job_are_logged_at_info_to_the_configured_log_file()
-> Result<(), BoxError> {
    let host = Host::new()?;
    let config = host.logging_config("info")?;

    let created = host
        .run(&config, &[&CREATE[..], &["json"][..]].concat(), Some(JOB))
        .await?;
    assert!(created.status.success(), "{}", stderr(&created));
    let hash = document(&created)?["hash"].as_str().ok_or("no hash")?.to_owned();
    assert!(stderr(&created).is_empty(), "{}", stderr(&created));
    assert!(host.logged().contains("shared job created"), "{}", host.logged());

    let replaced = host
        .run(
            &config,
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
            Some(REVISED),
        )
        .await?;
    assert!(replaced.status.success(), "{}", stderr(&replaced));
    document(&replaced)?;
    assert!(host.logged().contains("shared job replaced"), "{}", host.logged());

    host.retire()?;
    let deleted = host
        .run(
            &config,
            &["jobs", "delete", "--shared", "acme", "sync", "nightly"],
            None,
        )
        .await?;
    assert!(deleted.status.success(), "{}", stderr(&deleted));
    assert!(!host.job_path().exists());
    assert!(host.logged().contains("shared job deleted"), "{}", host.logged());

    let logged = host.logged();
    for field in ["acme", "sync", "nightly"] {
        assert!(logged.contains(field), "{field} is named: {logged}");
    }
    let data = host.data.to_string_lossy().into_owned();
    assert!(
        !logged.contains("Nightly") && !logged.contains("timeout_secs") && !logged.contains(&data),
        "no job content and no host path is logged: {logged}"
    );
    Ok(())
}

#[tokio::test]
async fn a_refusal_is_logged_at_debug_only_so_the_default_level_writes_nothing_and_stderr_stays_empty()
-> Result<(), BoxError> {
    let host = Host::new()?;
    let bad = b"name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nowner = \"x\"\n";

    let at_info = host
        .run(
            &host.logging_config("info")?,
            &[&CREATE[..], &["json"][..]].concat(),
            Some(bad),
        )
        .await?;
    assert_eq!(at_info.status.code(), Some(1));
    assert_eq!(document(&at_info)?["error"]["code"], "workflow/job-file-invalid");
    assert!(stderr(&at_info).is_empty(), "{}", stderr(&at_info));
    assert!(!host.logged().contains("refused"), "{}", host.logged());

    let at_debug = host
        .run(
            &host.logging_config("debug")?,
            &[&CREATE[..], &["json"][..]].concat(),
            Some(bad),
        )
        .await?;
    assert_eq!(at_debug.status.code(), Some(1));
    assert!(host.logged().contains("workflow/job-file-invalid"), "{}", host.logged());
    assert!(host.logged().contains("DEBUG"), "{}", host.logged());
    assert!(!host.logged().contains("WARN"), "{}", host.logged());

    let without_config = host
        .run(
            &host.missing_config(),
            &[&CREATE[..], &["json"][..]].concat(),
            Some(bad),
        )
        .await?;
    assert_eq!(without_config.status.code(), Some(1));
    assert!(stderr(&without_config).is_empty(), "{}", stderr(&without_config));
    Ok(())
}

#[tokio::test]
async fn a_package_store_that_cannot_be_read_is_refused_saying_it_could_not_be_checked_not_that_the_package_is_missing()
-> Result<(), BoxError> {
    let broken = Host::new()?;
    broken.install_package("this is not a manifest")?;
    let missing = Host::new()?;
    std::fs::remove_dir_all(missing.data.join("contextos/packages"))?;

    let broken_result = broken
        .run(
            &broken.missing_config(),
            &[&CREATE[..], &["json"][..]].concat(),
            Some(JOB),
        )
        .await?;
    let missing_result = missing
        .run(
            &missing.missing_config(),
            &[&CREATE[..], &["json"][..]].concat(),
            Some(JOB),
        )
        .await?;

    for result in [&broken_result, &missing_result] {
        assert_eq!(result.status.code(), Some(1));
        assert_eq!(document(result)?["error"]["code"], "workflow/package-unavailable");
    }
    let message = document(&broken_result)?["error"]["message"]
        .as_str()
        .ok_or("no message")?
        .to_owned();
    assert!(message.contains("could not be checked"), "{message}");
    assert!(!message.contains("not installed"), "{message}");
    let message = document(&missing_result)?["error"]["message"]
        .as_str()
        .ok_or("no message")?
        .to_owned();
    assert!(message.contains("not installed"), "{message}");
    assert!(!message.contains("could not be checked"), "{message}");
    Ok(())
}

#[tokio::test]
async fn an_error_never_repeats_a_value_from_the_jobs_inputs_table() -> Result<(), BoxError> {
    let host = Host::new()?;
    let bytes = b"name = \"Nightly\"\ntimeout_secs = \"soon\"\n\n[inputs]\ntoken = \"hunter2-value\"\n";
    let unknown = b"name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nowner = \"x\"\n\n[inputs]\ntoken = \"hunter2-value\"\n";
    let mismatched =
        b"name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nslug = \"other\"\n\n[inputs]\ntoken = \"hunter2-value\"\n";

    for (label, content) in [
        ("type error", &bytes[..]),
        ("unknown field", &unknown[..]),
        ("slug", &mismatched[..]),
    ] {
        for format in ["text", "json"] {
            let output = host
                .run(
                    &host.missing_config(),
                    &[&CREATE[..], &[format][..]].concat(),
                    Some(content),
                )
                .await?;
            assert_eq!(output.status.code(), Some(1), "{label} {format}: {}", stdout(&output));
            for printed in [stdout(&output), stderr(&output)] {
                assert!(!printed.contains("hunter2-value"), "{label} {format}: {printed}");
            }
        }
    }
    Ok(())
}

const REPLACE: [&str; 11] = [
    "jobs",
    "replace",
    "--shared",
    "acme",
    "sync",
    "nightly",
    "--file",
    "-",
    "--expected-hash",
    "HASH",
    "--format",
];

fn replace_with(hash: &str, format: &'static str) -> Vec<String> {
    REPLACE
        .iter()
        .map(|argument| {
            if *argument == "HASH" {
                hash.to_owned()
            } else {
                (*argument).to_owned()
            }
        })
        .chain([format.to_owned()])
        .collect()
}

fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[tokio::test]
async fn an_unchanged_create_or_replace_logs_nothing_at_info_because_no_bytes_were_written() -> Result<(), BoxError> {
    let host = Host::new()?;
    let config = host.logging_config("info")?;
    let create = [&CREATE[..], &["json"][..]].concat();

    let first = host.run(&config, &create, Some(JOB)).await?;
    assert!(first.status.success(), "{}", stderr(&first));
    let hash = document(&first)?["hash"].as_str().ok_or("no hash")?.to_owned();
    let again = host.run(&config, &create, Some(JOB)).await?;
    assert!(again.status.success(), "{}", stderr(&again));
    assert_eq!(document(&again)?["outcome"], "unchanged");
    assert_eq!(count(&host.logged(), "shared job created"), 1, "{}", host.logged());

    let arguments = replace_with(&hash, "json");
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let unchanged = host.run(&config, &arguments, Some(JOB)).await?;
    assert!(unchanged.status.success(), "{}", stderr(&unchanged));
    assert_eq!(document(&unchanged)?["outcome"], "unchanged");
    assert_eq!(count(&host.logged(), "shared job replaced"), 0, "{}", host.logged());
    Ok(())
}

/// A web configuration that is there but cannot be used must stop an authoring command, not be ignored.
fn assert_config_refusal(output: &Output, host: &Host, format: &str) -> Result<(), BoxError> {
    assert_eq!(output.status.code(), Some(1), "{}", stdout(output));
    let printed = stdout(output);
    let root = host.root.path().to_string_lossy().into_owned();
    assert!(!printed.contains(&root), "no path is printed: {printed}");
    assert!(
        !stderr(output).contains(&root),
        "no path is printed: {}",
        stderr(output)
    );
    if format == "json" {
        let result = document(output)?;
        assert_eq!(result["error"]["code"], "workflow/config-invalid", "{printed}");
    } else {
        assert!(printed.starts_with("refused workflow/config-invalid: "), "{printed}");
        assert_eq!(printed.trim_end().lines().count(), 1, "{printed}");
    }
    Ok(())
}

#[tokio::test]
async fn an_authoring_command_refuses_with_config_invalid_when_the_web_configuration_cannot_be_used()
-> Result<(), BoxError> {
    let host = Host::new()?;
    let malformed = host.root.path().join("malformed.toml");
    std::fs::write(&malformed, "[server\nlog_level = ")?;
    let unreadable = host.root.path().join("a-folder.toml");
    std::fs::create_dir(&unreadable)?;
    let unknown_key = host.root.path().join("unknown.toml");
    std::fs::write(&unknown_key, "[server]\nno_such_field = 1\n")?;

    for (label, config) in [
        ("malformed", &malformed),
        ("a folder where the file should be", &unreadable),
        ("an unknown key", &unknown_key),
    ] {
        for format in ["json", "text"] {
            let output = host
                .run(config, &[&CREATE[..], &[format][..]].concat(), Some(JOB))
                .await?;
            assert_config_refusal(&output, &host, format).map_err(|error| format!("{label} {format}: {error}"))?;
            assert!(!host.job_path().exists(), "{label} {format}: nothing is created");
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_log_file_that_cannot_be_opened_is_a_config_invalid_refusal_in_the_result_and_one_json_document()
-> Result<(), BoxError> {
    let host = Host::new()?;
    let config = host.root.path().join("web.toml");
    let missing_parent = host.root.path().join("no-such-folder/contextos-web.log");
    std::fs::write(
        &config,
        format!("[server]\nlog_file = \"{}\"\n", missing_parent.display()),
    )?;

    for format in ["json", "text"] {
        let output = host
            .run(&config, &[&CREATE[..], &[format][..]].concat(), Some(JOB))
            .await?;
        assert_config_refusal(&output, &host, format).map_err(|error| format!("{format}: {error}"))?;
        assert!(!host.job_path().exists(), "{format}: nothing is created");
    }
    Ok(())
}

#[tokio::test]
async fn a_server_less_operator_command_refuses_with_config_invalid_when_the_web_configuration_cannot_be_used()
-> Result<(), BoxError> {
    let host = Host::new()?;
    host.retire()?;
    std::fs::create_dir_all(host.job_path().parent().ok_or("no parent")?)?;
    std::fs::write(host.job_path(), JOB)?;
    let malformed = host.root.path().join("malformed.toml");
    std::fs::write(&malformed, "[server\nlog_level = ")?;

    let output = host
        .run(
            &malformed,
            &["jobs", "delete", "--shared", "acme", "sync", "nightly"],
            None,
        )
        .await?;

    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    assert!(stdout(&output).is_empty(), "{}", stdout(&output));
    assert!(
        stderr(&output).contains("workflow/config-invalid"),
        "{}",
        stderr(&output)
    );
    let root = host.root.path().to_string_lossy().into_owned();
    assert!(!stderr(&output).contains(&root), "{}", stderr(&output));
    assert!(host.job_path().exists(), "the job file is not removed");
    Ok(())
}

#[tokio::test]
async fn a_server_less_operator_command_refuses_when_the_configured_log_file_cannot_be_opened() -> Result<(), BoxError>
{
    let host = Host::new()?;
    host.retire()?;
    let config = host.root.path().join("web.toml");
    let missing_parent = host.root.path().join("no-such-folder/contextos-web.log");
    std::fs::write(
        &config,
        format!("[server]\nlog_file = \"{}\"\n", missing_parent.display()),
    )?;

    let output = host
        .run(
            &config,
            &["jobs", "delete", "--shared", "acme", "sync", "nightly"],
            None,
        )
        .await?;

    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    assert!(stdout(&output).is_empty(), "{}", stdout(&output));
    assert!(
        stderr(&output).contains("workflow/config-invalid"),
        "{}",
        stderr(&output)
    );
    Ok(())
}
