//! A shared run whose record cannot be written, as a real process: the run goes ahead, and nothing it logs, on standard
//! error or in the configured log file, names a folder of the host. The host's data directory is redirected to a
//! temporary directory, so nothing here reads or writes the operator's own.
#![cfg(target_os = "linux")]

#[path = "support/run_host.rs"]
mod run_host;
mod support;

use run_host::{BoxError, RunHost, jobs_run, stderr, stdout};

/// A copy of the host's web configuration that also writes its log to a file, written beside it.
fn config_logging_to(host: &RunHost, log_file: &std::path::Path) -> Result<std::path::PathBuf, BoxError> {
    let path = host.root.path().join("web-with-log-file.toml");
    let base = std::fs::read_to_string(&host.web_config)?;
    run_host::write(
        &path,
        &format!("{base}\n[server]\nlog_file = {:?}\n", log_file.to_string_lossy()),
    )?;
    Ok(path)
}

/// Puts a host into the state a case is about.
type Prepare = fn(&RunHost) -> Result<(), BoxError>;

/// The folder holding the shared `emit` job's records and counter.
fn emit_job_folder(host: &RunHost) -> std::path::PathBuf {
    host.data.join("contextos/runs/acme/emit/nightly")
}

/// Leaves a counter that holds no number, which the ledger refuses and does not repair.
fn corrupt_the_counter(host: &RunHost) -> Result<(), BoxError> {
    run_host::write(&emit_job_folder(host).join("next-run-id"), "not a number")?;
    Ok(())
}

/// Leaves a record the ledger cannot read where the next run looks for runs a dead process left behind.
fn leave_an_unreadable_record(host: &RunHost) -> Result<(), BoxError> {
    run_host::write(
        &emit_job_folder(host).join("2026/10/R-1.md"),
        "this is not a run record",
    )?;
    Ok(())
}

#[tokio::test]
async fn a_shared_run_whose_record_cannot_be_written_or_read_logs_no_host_folder_to_standard_error_or_the_log_file()
-> Result<(), BoxError> {
    let cases: [(&str, Prepare); 2] = [
        ("a counter that holds no number", corrupt_the_counter),
        ("a record the ledger cannot read", leave_an_unreadable_record),
    ];
    let mut leaks = Vec::new();
    for (case, prepare) in cases {
        let host = RunHost::new()?;
        prepare(&host)?;
        let root = host.data.to_string_lossy().into_owned();
        let log_file = host.root.path().join("run.log");
        let logging = config_logging_to(&host, &log_file)?;
        let command = jobs_run(true, "emit", &["--format", "json"]);

        let to_stderr = host.run(&command).await?;
        let to_file = host.command_with(&logging, &command).output().await?;

        assert_eq!(to_stderr.status.code(), Some(0), "{case}: {}", stderr(&to_stderr));
        assert!(
            stderr(&to_stderr).contains(" WARN "),
            "{case}: nothing was logged, so this proves nothing: {}",
            stderr(&to_stderr)
        );
        let logged = std::fs::read_to_string(&log_file).unwrap_or_default();
        assert!(
            logged.contains(" WARN "),
            "{case}: the log file holds no warning: {logged:?}"
        );
        for (stream, text) in [
            ("stdout", stdout(&to_stderr)),
            ("stderr", stderr(&to_stderr)),
            ("log file", logged),
            ("stdout with a log file", stdout(&to_file)),
            ("stderr with a log file", stderr(&to_file)),
        ] {
            if text.contains(&root) {
                leaks.push(format!("{case}: the host folder is on {stream}: {text}"));
            }
        }
    }
    assert!(leaks.is_empty(), "{}", leaks.join("\n"));
    Ok(())
}
