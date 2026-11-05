//! The edges of the run commands as a real process: what a failure logs at each level, what a bad `--input` argument
//! echoes, which flags cannot be combined, which zone times are written in, what an unreadable host file puts in the
//! output, and what an interrupt during start-up does. The host's data directory is redirected to a temporary
//! directory, so nothing here reads or writes the operator's own.
#![cfg(target_os = "linux")]

#[path = "support/run_host.rs"]
mod run_host;
mod support;

use run_host::{
    BoxError, RunHost, children_of, jobs_dry_run, jobs_run, signal, stderr, stdout, wait_until, workflows_run,
};
use serde_json::{Value, json};

const SECRET_MESSAGE: &str = "the script gave up";

/// A copy of the host's web configuration that also sets the log level, written beside it.
fn config_with(host: &RunHost, extra: &str) -> Result<std::path::PathBuf, BoxError> {
    let path = host.root.path().join("web-with-extras.toml");
    let base = std::fs::read_to_string(&host.web_config)?;
    run_host::write(&path, &format!("{base}\n{extra}\n"))?;
    Ok(path)
}

#[tokio::test]
async fn the_error_event_of_a_failed_run_carries_the_outcome_and_code_and_not_the_failure_message()
-> Result<(), BoxError> {
    let host = RunHost::new()?;
    let debug_config = config_with(&host, "[server]\nlog_level = \"debug\"")?;

    for shared in [false, true] {
        let scope = if shared { "shared" } else { "vault" };
        let command = jobs_run(shared, "fail", &["--format", "json"]);

        let info = host.run(&command).await?;
        let debug = host.command_with(&debug_config, &command).output().await?;

        let info_log = stderr(&info);
        // Only the command's own events are counted: the MCP library logs its own shutdown at ERROR now and then.
        let errors: Vec<&str> = info_log
            .lines()
            .filter(|line| line.contains(" ERROR contextos_web::run_commands:"))
            .collect();
        assert_eq!(errors.len(), 1, "{scope}: one error event: {info_log}");
        assert!(
            !info_log
                .lines()
                .any(|line| line.contains(" ERROR ") && line.contains(SECRET_MESSAGE)),
            "{scope}: no error event from any source holds the failure message: {info_log}"
        );
        assert!(errors[0].contains("workflow/script-error"), "{scope}: {}", errors[0]);
        assert!(
            !errors[0].contains(SECRET_MESSAGE),
            "{scope}: the error event holds the failure message, which can quote the job's data: {}",
            errors[0]
        );
        let debug_log = stderr(&debug);
        assert!(
            debug_log
                .lines()
                .any(|line| line.contains(" DEBUG ") && line.contains(SECRET_MESSAGE)),
            "{scope}: the message is logged at debug: {debug_log}"
        );
        assert!(
            !debug_log
                .lines()
                .any(|line| line.contains(" ERROR ") && line.contains(SECRET_MESSAGE)),
            "{scope}: and never at error: {debug_log}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_shared_run_cannot_be_combined_with_no_record_while_a_vault_run_can() -> Result<(), BoxError> {
    let host = RunHost::new()?;

    let shared = host
        .run(&jobs_run(true, "emit", &["--no-record", "--format", "json"]))
        .await?;
    let vault = host
        .run(&jobs_run(false, "emit", &["--no-record", "--format", "json"]))
        .await?;

    assert_eq!(
        shared.status.code(),
        Some(2),
        "a shared run always leaves a record: {}",
        stderr(&shared)
    );
    assert_eq!(stdout(&shared), "");
    assert_eq!(vault.status.code(), Some(0), "{}", stderr(&vault));
    Ok(())
}

#[tokio::test]
async fn a_malformed_input_argument_is_named_by_position_and_never_echoed_by_any_run_command() -> Result<(), BoxError> {
    let host = RunHost::new()?;
    let secret = "sk-live-4242-secret";

    for shared in [false, true] {
        let scope = if shared { "shared" } else { "vault" };
        let commands = [
            jobs_run(
                shared,
                "echo",
                &["--input", "target=ok", "--input", secret, "--format", "json"],
            ),
            jobs_dry_run(
                shared,
                "echo",
                &["--input", "target=ok", "--input", secret, "--format", "json"],
            ),
            workflows_run(
                shared,
                "echo",
                &["--input", "target=ok", "--input", secret, "--format", "json"],
            ),
        ];
        for command in commands {
            let output = host.run(&command).await?;

            let document: Value = serde_json::from_slice(&output.stdout)?;
            assert_eq!(
                output.status.code(),
                Some(3),
                "{scope} {command:?}: {}",
                stderr(&output)
            );
            assert_eq!(document["code"], json!("workflow/input-invalid"), "{scope}: {document}");
            let message = document["message"].as_str().unwrap_or_default();
            assert!(
                message.contains("position 2") && message.contains("no `=`"),
                "{scope}: {message}"
            );
            for text in [stdout(&output), stderr(&output)] {
                assert!(
                    !text.contains(secret),
                    "{scope} {command:?}: the argument is echoed: {text}"
                );
            }
        }
    }
    Ok(())
}

/// The UTC offset a timestamp is written in, as `+HH:MM`, or `Z`.
fn offset_of(timestamp: &Value) -> String {
    let text = timestamp.as_str().unwrap_or_default();
    if text.ends_with('Z') {
        return "Z".to_owned();
    }
    text.get(text.len().saturating_sub(6)..).unwrap_or_default().to_owned()
}

#[tokio::test]
async fn a_refusal_is_timed_in_the_hosts_zone_like_a_run_that_finished() -> Result<(), BoxError> {
    let host = RunHost::new()?;
    // The zone of the server the run talks to is the zone the host writes times in. Chatham is never on a whole hour.
    let mcp_config = host.root.path().join("config.toml");
    let text = std::fs::read_to_string(&mcp_config)?;
    std::fs::write(
        &mcp_config,
        format!("[server]\ntimezone = \"Pacific/Chatham\"\n\n{text}"),
    )?;

    for shared in [false, true] {
        let scope = if shared { "shared" } else { "vault" };
        let (_, finished) = host.json(&jobs_run(shared, "emit", &["--format", "json"])).await?;
        let mut missing = jobs_run(shared, "emit", &["--format", "json"]);
        if let Some(job) = missing.iter_mut().find(|part| *part == "nightly") {
            "no-such-job".clone_into(job);
        }
        let (_, refused) = host.json(&missing).await?;

        let zone = offset_of(&finished["timings"]["started"]);
        assert!(
            zone == "+12:45" || zone == "+13:45",
            "{scope}: the finished run is not in the host zone: {finished}"
        );
        assert_eq!(refused["outcome"], json!("refused"), "{scope}: {refused}");
        for field in ["started", "ended"] {
            assert_eq!(
                offset_of(&refused["timings"][field]),
                zone,
                "{scope}: a refusal's {field} time is in another zone than a finished run's: {refused}"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_refusal_before_the_scenario_is_resolved_names_no_scenario() -> Result<(), BoxError> {
    let host = RunHost::new()?;

    for shared in [false, true] {
        let scope = if shared { "shared" } else { "vault" };
        let mut unknown_job = jobs_run(
            shared,
            "emit",
            &["--dry-run", "--scenario", "offline", "--format", "json"],
        );
        if let Some(job) = unknown_job.iter_mut().find(|part| *part == "nightly") {
            "no-such-job".clone_into(job);
        }
        let missing_scenario = jobs_run(
            shared,
            "emit",
            &["--dry-run", "--scenario", "no-such-scenario", "--format", "json"],
        );
        for command in [unknown_job, missing_scenario] {
            let (_, document) = host.json(&command).await?;

            assert_eq!(document["outcome"], json!("refused"), "{scope}: {document}");
            assert_eq!(
                document["scenario"],
                Value::Null,
                "{scope}: no scenario was resolved, so none is named: {document}"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn an_invalid_web_configuration_is_logged_as_one_error_event_with_its_code() -> Result<(), BoxError> {
    let host = RunHost::new()?;
    let broken = host.root.path().join("broken-web.toml");
    run_host::write(&broken, "this is = not [ valid")?;

    let output = host
        .command_with(&broken, &jobs_run(false, "emit", &["--format", "json"]))
        .output()
        .await?;

    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let log = stderr(&output);
    let errors: Vec<&str> = log.lines().filter(|line| line.contains(" ERROR ")).collect();
    assert_eq!(errors.len(), 1, "one error event: {log}");
    assert!(errors[0].contains("workflow/config-invalid"), "{}", errors[0]);
    Ok(())
}

#[tokio::test]
async fn a_run_command_with_no_web_configuration_file_is_a_refusal_and_not_an_error_line() -> Result<(), BoxError> {
    let host = RunHost::new()?;
    // No `--config`, and a home folder with no `web.toml` in it.
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_contextos-web"))
        .args(jobs_run(false, "emit", &["--format", "json"]))
        .env_clear()
        .env("HOME", &host.data)
        .env("XDG_DATA_HOME", &host.data)
        .output()
        .await?;

    let document: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert_eq!(document["outcome"], json!("refused"), "{document}");
    assert_eq!(document["code"], json!("workflow/config-invalid"), "{document}");
    assert!(
        !stderr(&output).lines().any(|line| line.starts_with("Error:")),
        "the command writes no line of its own: {}",
        stderr(&output)
    );
    Ok(())
}

/// Appends a shared job, workflow, and scenario that are unusable, and returns the commands that load each of them.
fn unusable_shared_files(host: &RunHost) -> Result<Vec<Vec<String>>, BoxError> {
    let jobs = host.data.join("contextos/workflows/acme/emit/jobs");
    run_host::write(
        &jobs.join("oversized.toml"),
        &format!("name = \"x\"\n# {}\n", "x".repeat(70_000)),
    )?;
    std::fs::write(jobs.join("binary.toml"), [0xff_u8, 0xfe, 0xfd, 0x00])?;
    let release = host.data.join("contextos/packages/acme/1.0.0");
    let mut manifest = std::fs::read_to_string(release.join("manifest.toml"))?;
    manifest.push_str(
        "\n[[workflow]]\nslug = \"bigscript\"\nentry = \"bigscript.rhai\"\n\n[[workflow]]\nslug = \"binscript\"\nentry = \"binscript.rhai\"\n",
    );
    run_host::write(&release.join("manifest.toml"), &manifest)?;
    run_host::write(
        &release.join("bigscript.rhai"),
        &format!("// {}\n", "x".repeat(1_100_000)),
    )?;
    std::fs::write(release.join("binscript.rhai"), [0xff_u8, 0xfe, 0xfd, 0x00])?;
    run_host::write(
        &release.join("emit/tests/huge.toml"),
        &format!("# {}\n", "x".repeat(300_000)),
    )?;
    std::fs::write(release.join("emit/tests/binary.toml"), [0xff_u8, 0xfe, 0xfd, 0x00])?;
    let mut commands = Vec::new();
    for job in ["oversized", "binary"] {
        let mut command = jobs_run(true, "emit", &["--format", "json"]);
        if let Some(part) = command.iter_mut().find(|part| *part == "nightly") {
            job.clone_into(part);
        }
        commands.push(command);
    }
    for workflow in ["bigscript", "binscript"] {
        commands.push(workflows_run(true, workflow, &["--format", "json"]));
    }
    for scenario in ["huge", "binary"] {
        commands.push(workflows_run(
            true,
            "emit",
            &["--dry-run", "--scenario", scenario, "--format", "json"],
        ));
        commands.push(jobs_dry_run(
            true,
            "emit",
            &["--scenario", scenario, "--format", "json"],
        ));
    }
    Ok(commands)
}

#[tokio::test]
async fn an_unreadable_shared_file_never_puts_the_hosts_folders_on_standard_output_or_error() -> Result<(), BoxError> {
    let host = RunHost::new()?;
    let commands = unusable_shared_files(&host)?;
    let root = host.data.to_string_lossy().into_owned();

    let mut leaks = Vec::new();
    for command in commands {
        let output = host.run(&command).await?;

        assert!(
            output.status.code().is_some_and(|code| code != 0),
            "{command:?} was expected to be refused: {} {}",
            stdout(&output),
            stderr(&output)
        );
        for (stream, text) in [("stdout", stdout(&output)), ("stderr", stderr(&output))] {
            if text.contains(&root) {
                leaks.push(format!("{command:?} puts the host folder on {stream}: {text}"));
            }
        }
    }
    assert!(leaks.is_empty(), "{}", leaks.join("\n"));
    Ok(())
}

#[tokio::test]
async fn an_interrupt_while_the_servers_are_connecting_ends_the_run_cancelled_with_exit_130() -> Result<(), BoxError> {
    let host = RunHost::new()?;
    // A server that starts and never answers, so the run is in its connect phase until it is interrupted.
    let silent = host.root.path().join("silent-web.toml");
    run_host::write(
        &silent,
        "[[mcp_server]]\ntransport = \"stdio\"\nname = \"contextos\"\ncommand = \"sleep\"\nargs = [\"600\"]\n",
    )?;
    let child = host
        .command_with(&silent, &jobs_run(false, "emit", &["--format", "json"]))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let pid = child.id().ok_or("the run has no process id")?;
    // The silent server is a child of the run once the run is connecting to it.
    let connecting = wait_until(|| !children_of(pid).is_empty()).await;
    assert!(connecting, "the run never started connecting");
    signal("INT", pid)?;

    let output = tokio::time::timeout(std::time::Duration::from_secs(30), child.wait_with_output())
        .await
        .map_err(|_| "the run did not stop when interrupted while connecting")??;

    let document: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(output.status.code(), Some(130), "{}", stderr(&output));
    assert_eq!(document["outcome"], json!("cancelled"), "{document}");
    assert_eq!(document["code"], json!("workflow/cancelled"), "{document}");
    Ok(())
}
