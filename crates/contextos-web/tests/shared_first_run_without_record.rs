//! A shared job's first live run fails while its record cannot be started, as a real process: the suspension it causes
//! names no run, because none was numbered, and says so in the state and in the listing instead of naming run 0. The
//! host's data directory is redirected to a temporary directory, so nothing here reads or writes the operator's own.
#![cfg(target_os = "linux")]

#[path = "support/run_host.rs"]
mod run_host;
mod support;

use contextos_workflows::{Actor, JobSchedule, Transition};
use run_host::{BoxError, PACKAGE, RunHost, jobs_run, parse_whole};

/// Enables the shared `nightly` job of the `fail` workflow, which puts its first live run on trial.
fn enable_on_trial(host: &RunHost) -> Result<(), BoxError> {
    let store = contextos_web::shared_state::SharedStateStore::new(host.data.join("contextos/state"));
    let version = {
        let job_toml = "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";
        contextos_web::shared_version::shared_job_version(&host.data.join("contextos/packages"), PACKAGE, job_toml)
            .ok_or("the job has no version")?
    };
    store.update(|jobs| -> Result<(), BoxError> {
        jobs.transition(
            PACKAGE,
            "fail",
            "nightly",
            Actor::Operator,
            &Transition::Enable(JobSchedule::try_from("0 3 * * *")?, version),
            time::macros::datetime!(2026-10-01 03:00:00 UTC),
        )?;
        Ok(())
    })??;
    Ok(())
}

#[tokio::test]
async fn a_failed_first_run_whose_record_could_not_be_started_suspends_the_job_naming_no_run() -> Result<(), BoxError> {
    let host = RunHost::new()?;
    enable_on_trial(&host)?;
    // A counter that holds no number, which the ledger refuses, so the run goes ahead with no record and no number.
    run_host::write(
        &host.data.join("contextos/runs/acme/fail/nightly/next-run-id"),
        "not a number",
    )?;

    let output = host.run(&jobs_run(true, "fail", &["--format", "json"])).await?;

    let summary = parse_whole(&output)?;
    assert_eq!(
        summary["outcome"], "failed",
        "the script threw, so the run failed: {summary}"
    );
    let state = contextos_web::shared_state::SharedStateStore::new(host.data.join("contextos/state")).read()?;
    let job = state
        .job(PACKAGE, "fail", "nightly")
        .ok_or("the job has stored state")?;
    assert_eq!(
        job.lifecycle().as_str(),
        "suspended",
        "the failed trial suspends the job"
    );
    let record = job.suspension().ok_or("the suspension is a stored record")?;
    assert_eq!(record.code, "workflow/probation-failed");
    assert_eq!(
        record.run, None,
        "no record was started, so no run was numbered and none is named"
    );

    let (_, listed) = host
        .json(&run_host::args(&["jobs", "list", "--shared", "--format", "json"]))
        .await?;
    let row = listed["jobs"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["workflow"] == "fail"))
        .ok_or_else(|| format!("the listing has no row for the fail workflow: {listed}"))?;
    assert!(
        row["suspension"].get("run").is_none_or(serde_json::Value::is_null),
        "the listing names no run: {row}"
    );
    Ok(())
}
