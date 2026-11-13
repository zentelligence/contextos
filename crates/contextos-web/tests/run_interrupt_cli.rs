//! Interrupting a command-line run as a real process: SIGINT and SIGTERM stop a vault or shared run, which is recorded
//! `cancelled`, frees its claim, prints its summary, and exits 130; a second signal exits at once with no summary. The
//! host's data directory is redirected to a temporary directory, so nothing here reads or writes the operator's own.
#![cfg(target_os = "linux")]

#[path = "support/run_host.rs"]
mod run_host;
mod support;

use run_host::{BoxError, RunHost, children_of, jobs_run, signal, stderr, stdout, wait_until, workflows_run};
use serde_json::json;

/// Starts a run of `command` and waits until its record says `running`, which is written before its script starts.
async fn started_run(
    host: &RunHost,
    command: &[String],
    shared: bool,
    workflow: &str,
) -> Result<tokio::process::Child, BoxError> {
    let child = host.spawn(command)?;
    let running = wait_until(|| {
        let record = if shared {
            host.shared_record(workflow, 1)
        } else {
            host.vault_record(workflow, 1)
        };
        record.is_some_and(|text| text.contains("status: running\n"))
    })
    .await;
    if !running {
        return Err("the run never started".into());
    }
    Ok(child)
}

/// Signals the run, waits for it to end, and checks it was cancelled: exit 130, one `cancelled` summary on standard
/// output, a `cancelled` record, and a free claim.
async fn assert_cancelled_by(host: &RunHost, signal_name: &str, shared: bool, workflow: &str) -> Result<(), BoxError> {
    let command = jobs_run(shared, workflow, &["--format", "json"]);
    let child = started_run(host, &command, shared, workflow).await?;
    let pid = child.id().ok_or("the run has no process id")?;
    signal(signal_name, pid)?;

    let output = tokio::time::timeout(std::time::Duration::from_secs(30), child.wait_with_output())
        .await
        .map_err(|_| "the run did not stop when signalled")??;

    assert_eq!(
        output.status.code(),
        Some(130),
        "SIGTERM/SIGINT must end the run as a cancellation, not kill the process: {:?} {}",
        output.status,
        stderr(&output)
    );
    let document = run_host::parse_whole(&output)?;
    assert_eq!(document["outcome"], json!("cancelled"), "{document}");
    assert_eq!(document["code"], json!("workflow/cancelled"), "{document}");
    assert_eq!(document["exit_code"], json!(130));
    let record = if shared {
        host.shared_record(workflow, 1)
    } else {
        host.vault_record(workflow, 1)
    }
    .ok_or("the run left no record")?;
    assert!(record.contains("status: cancelled\n"), "{record}");
    let free = if shared {
        host.shared_claim_is_free(workflow)
    } else {
        host.vault_claim_is_free(workflow)
    };
    assert!(free, "the claim is free once the run was cancelled");
    Ok(())
}

#[tokio::test]
async fn sigint_cancels_a_vault_job_run_records_cancelled_exits_130_and_frees_the_claim() -> Result<(), BoxError> {
    assert_cancelled_by(&RunHost::new()?, "INT", false, "spin").await
}

#[tokio::test]
async fn sigterm_cancels_a_vault_job_run_records_cancelled_exits_130_and_frees_the_claim() -> Result<(), BoxError> {
    assert_cancelled_by(&RunHost::new()?, "TERM", false, "spin").await
}

#[tokio::test]
async fn sigterm_cancels_a_shared_job_run_as_sigint_does() -> Result<(), BoxError> {
    assert_cancelled_by(&RunHost::new()?, "TERM", true, "spin").await
}

#[tokio::test]
async fn sigint_and_sigterm_cancel_a_vault_one_off_run_and_exit_130() -> Result<(), BoxError> {
    for signal_name in ["INT", "TERM"] {
        let host = RunHost::new()?;
        let child = host.spawn(&workflows_run(false, "mark", &["--format", "json"]))?;
        // A one-off run has no record, so its script marks the vault before it spins.
        let started = wait_until(|| host.vault.join("started").is_dir()).await;
        assert!(started, "SIG{signal_name}: the script never started");
        let pid = child.id().ok_or("the run has no process id")?;
        signal(signal_name, pid)?;

        let output = tokio::time::timeout(std::time::Duration::from_secs(30), child.wait_with_output())
            .await
            .map_err(|_| "the run did not stop when signalled")??;

        assert_eq!(
            output.status.code(),
            Some(130),
            "SIG{signal_name}: {:?} {}",
            output.status,
            stderr(&output)
        );
        let document = run_host::parse_whole(&output)?;
        assert_eq!(document["outcome"], json!("cancelled"), "SIG{signal_name}: {document}");
        assert_eq!(document["command"], json!("workflows run"));
    }
    Ok(())
}

#[tokio::test]
async fn a_second_signal_exits_130_at_once_with_no_summary_and_leaves_the_record_running() -> Result<(), BoxError> {
    let host = RunHost::new()?;
    let command = jobs_run(false, "spin", &["--format", "json"]);
    let mut child = started_run(&host, &command, false, "spin").await?;
    let pid = child.id().ok_or("the run has no process id")?;
    // The servers inherit the run's standard error, so they hold that pipe open after the run exits. The pipes are read
    // apart from the exit, which is the only thing this test times: end of file depends on how long the stopped servers
    // live, which is not the run's doing (whether the kernel ends them depends on what reparents them).
    let stdout_reader = read_to_end(child.stdout.take().ok_or("the run has no standard output")?);
    let stderr_reader = read_to_end(child.stderr.take().ok_or("the run has no standard error")?);
    // Stopping the server the run talks to means the run cannot finish its record after the first signal, so the second
    // signal arrives while the run is still ending.
    let servers = children_of(pid);
    for server in &servers {
        signal("STOP", *server)?;
    }
    // The two signals are of different kinds, which the process keeps apart: two of one kind sent close together are one
    // signal to it, and a pause long enough to tell them apart is a guess about timing.
    signal("INT", pid)?;
    signal("TERM", pid)?;

    let exited = tokio::time::timeout(std::time::Duration::from_secs(30), child.wait()).await;
    // Ending the servers closes their copies of the pipes, so both readers reach end of file.
    for server in &servers {
        let _ = signal("KILL", *server);
    }
    let status = exited.map_err(|_| "the run did not exit on its second signal")??;
    let output = std::process::Output {
        status,
        stdout: stdout_reader.await??,
        stderr: stderr_reader.await??,
    };

    assert_eq!(
        output.status.code(),
        Some(130),
        "{:?} {}",
        output.status,
        stderr(&output)
    );
    assert_eq!(stdout(&output), "", "a second signal prints no summary");
    let record = host.vault_record("spin", 1).ok_or("the run left no record")?;
    assert!(
        record.contains("status: running\n"),
        "the record is left for the next run to mark interrupted: {record}"
    );
    Ok(())
}

/// Reads `pipe` to its end on a task of its own, so a full pipe never holds up the process writing to it.
fn read_to_end(
    mut pipe: impl tokio::io::AsyncRead + Unpin + Send + 'static,
) -> tokio::task::JoinHandle<std::io::Result<Vec<u8>>> {
    use tokio::io::AsyncReadExt as _;
    tokio::spawn(async move {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).await?;
        Ok(bytes)
    })
}

/// Opens the write end of the named pipe `fifo` without waiting, which succeeds only once a reader has it open.
fn writer_once_read(fifo: &std::path::Path) -> Option<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    // O_NONBLOCK on Linux.
    std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(0o4000)
        .open(fifo)
        .ok()
}

#[tokio::test]
async fn an_interrupt_while_the_configuration_is_being_read_still_ends_the_run_cancelled_with_exit_130()
-> Result<(), BoxError> {
    use std::io::Write as _;
    let host = RunHost::new()?;
    // The configuration is a named pipe, so reading it waits until the test writes it, which holds the run in the step
    // before any server is connected.
    let fifo = host.root.path().join("web-fifo.toml");
    let made = std::process::Command::new("mkfifo").arg(&fifo).status()?;
    assert!(made.success(), "mkfifo failed");
    let configuration = std::fs::read_to_string(&host.web_config)?;
    let child = host
        .command_with(&fifo, &jobs_run(false, "emit", &["--format", "json"]))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let pid = child.id().ok_or("the run has no process id")?;
    let mut writer = None;
    let reading = wait_until(|| {
        writer = writer_once_read(&fifo);
        writer.is_some()
    })
    .await;
    assert!(reading, "the run never started reading its configuration");
    signal("INT", pid)?;
    if let Some(mut writer) = writer {
        writer.write_all(configuration.as_bytes())?;
    }

    let output = tokio::time::timeout(std::time::Duration::from_secs(30), child.wait_with_output())
        .await
        .map_err(|_| "the run did not stop when interrupted while reading its configuration")??;

    assert_eq!(
        output.status.code(),
        Some(130),
        "the signal must end the run as a cancellation, not kill the process: {:?} {}",
        output.status,
        stderr(&output)
    );
    let document = run_host::parse_whole(&output)?;
    assert_eq!(document["outcome"], json!("cancelled"), "{document}");
    assert_eq!(document["code"], json!("workflow/cancelled"), "{document}");
    Ok(())
}
