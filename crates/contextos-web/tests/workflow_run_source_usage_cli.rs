//! `workflows run --source` as a real process: it runs a vault workflow's source, so it cannot be combined with a release
//! or with a shared package. Both are clap's own usage errors (exit status 2) raised before any configuration is read,
//! so nothing is run, claimed, or recorded.
#![cfg(target_os = "linux")]

use std::process::Output;

use tokio::process::Command;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

async fn run(arguments: &[&str]) -> Result<Output, BoxError> {
    let empty = tempfile::tempdir()?;
    Ok(Command::new(env!("CARGO_BIN_EXE_contextos-web"))
        .arg("--config")
        .arg(empty.path().join("absent.toml"))
        .args(arguments)
        .env("XDG_DATA_HOME", empty.path())
        .env("HOME", empty.path())
        .env_remove("CONTEXTOS_WEB_CONFIG")
        .output()
        .await?)
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[tokio::test]
async fn source_is_refused_for_a_shared_workflow_as_a_usage_error() -> Result<(), BoxError> {
    let output = run(&["workflows", "run", "--shared", "acme", "sync", "--source"]).await?;

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let message = stderr(&output);
    assert!(
        message.contains("--source") && message.contains("--shared"),
        "{message}"
    );
    assert!(output.stdout.is_empty(), "a usage error writes no summary");
    Ok(())
}

#[tokio::test]
async fn source_together_with_a_release_is_refused_as_a_usage_error() -> Result<(), BoxError> {
    for release in ["current", "future", "previous", "1.0.0"] {
        let output = run(&[
            "workflows",
            "run",
            "va",
            "ops",
            "sync",
            "--source",
            "--release",
            release,
        ])
        .await?;

        assert_eq!(output.status.code(), Some(2), "{release}: {}", stderr(&output));
        let message = stderr(&output);
        assert!(
            message.contains("--source") && message.contains("--release"),
            "{release}: {message}"
        );
        assert!(output.stdout.is_empty(), "{release}: a usage error writes no summary");
    }
    Ok(())
}
