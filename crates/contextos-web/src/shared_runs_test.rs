use std::sync::{Arc, Mutex};

use contextos_workflows::{
    Clock, ContentDigest, PackageIdentity, RunFailure, RunOutcome, RunTrigger, WorkflowFailureKind,
};
use tempfile::tempdir;
use time::OffsetDateTime;
use time::macros::datetime;

use super::*;
use crate::shared_ledger::{RunStart, SharedLedger, SkippedRun};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct SettableClock(Mutex<OffsetDateTime>);

impl Clock for SettableClock {
    fn now(&self) -> OffsetDateTime {
        *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn key(job: &str) -> SharedJobKey {
    SharedJobKey {
        package: "acme".to_owned(),
        workflow: "sync".to_owned(),
        job: job.to_owned(),
    }
}

fn identity() -> Result<PackageIdentity, Box<dyn std::error::Error>> {
    Ok(PackageIdentity {
        name: contextos_workflows::PackageName::try_from("acme")?,
        version: semver::Version::parse("1.2.0")?,
        digest: ContentDigest::try_new("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")?,
    })
}

fn ledger(root: &std::path::Path, at: OffsetDateTime) -> (SharedLedger, Arc<SettableClock>) {
    let clock = Arc::new(SettableClock(Mutex::new(at)));
    let ledger = SharedLedger::new(
        root.to_path_buf(),
        Arc::clone(&clock) as Arc<dyn Clock>,
        time::Duration::days(365),
    );
    (ledger, clock)
}

fn skipped(ledger: &SharedLedger, job: &str) -> TestResult {
    ledger.record_skipped(
        &key(job),
        SkippedRun {
            identity: &identity()?,
            inputs: Vec::new(),
            reason: "the job's previous run still held its claim",
        },
    )?;
    Ok(())
}

#[test]
fn a_job_nothing_was_recorded_for_has_no_runs() -> TestResult {
    let root = tempdir()?;

    let runs = recent_shared_runs(root.path(), &key("nightly"), 20)?;

    assert!(runs.is_empty());
    Ok(())
}

#[test]
fn runs_are_listed_newest_first_with_their_status_trigger_times_and_error() -> TestResult {
    let root = tempdir()?;
    let (ledger, clock) = ledger(root.path(), datetime!(2026-10-01 03:00:00 UTC));
    skipped(&ledger, "nightly")?;
    *clock.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = datetime!(2026-11-02 03:00:00 UTC);
    let dry = ledger.begin_dry(
        &key("nightly"),
        RunStart {
            identity: &identity()?,
            inputs: Vec::new(),
            libraries: &[],
            trigger: RunTrigger::Manual,
        },
    )?;
    dry.finish(
        &RunOutcome::Failed(RunFailure {
            kind: Some(WorkflowFailureKind::TimedOut),
            message: "the run exceeded its timeout".to_owned(),
        }),
        datetime!(2026-11-02 03:00:09 UTC),
    )?;

    let runs = recent_shared_runs(root.path(), &key("nightly"), 20)?;

    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].run_id, "R-2", "the newest run is first, across month folders");
    assert_eq!(runs[0].status, "failed");
    assert_eq!(runs[0].trigger, "manual");
    assert!(
        runs[0].started.starts_with("2026-11-02T03:00:00"),
        "{}",
        runs[0].started
    );
    assert!(
        runs[0].completed.starts_with("2026-11-02T03:00:09"),
        "{}",
        runs[0].completed
    );
    assert!(runs[0].error.contains("exceeded its timeout"), "{}", runs[0].error);
    assert_eq!(runs[1].run_id, "R-1");
    assert_eq!(runs[1].status, "skipped");
    assert_eq!(runs[1].trigger, "scheduled");
    Ok(())
}

#[test]
fn a_run_names_its_job_and_the_release_it_read() -> TestResult {
    let root = tempdir()?;
    let (ledger, _clock) = ledger(root.path(), datetime!(2026-10-01 03:00:00 UTC));
    skipped(&ledger, "nightly")?;

    let runs = recent_shared_runs(root.path(), &key("nightly"), 20)?;

    assert_eq!(runs[0].job, "nightly");
    assert_eq!(runs[0].release, "1.2.0");
    Ok(())
}

#[test]
fn the_newest_runs_are_kept_when_there_are_more_than_the_limit() -> TestResult {
    let root = tempdir()?;
    let (ledger, _clock) = ledger(root.path(), datetime!(2026-10-01 03:00:00 UTC));
    for _ in 0..5 {
        skipped(&ledger, "nightly")?;
    }

    let runs = recent_shared_runs(root.path(), &key("nightly"), 2)?;

    let ids: Vec<&str> = runs.iter().map(|run| run.run_id.as_str()).collect();
    assert_eq!(ids, ["R-5", "R-4"]);
    Ok(())
}

#[test]
fn another_jobs_runs_are_not_listed() -> TestResult {
    let root = tempdir()?;
    let (ledger, _clock) = ledger(root.path(), datetime!(2026-10-01 03:00:00 UTC));
    skipped(&ledger, "weekly")?;

    let runs = recent_shared_runs(root.path(), &key("nightly"), 20)?;

    assert!(runs.is_empty());
    Ok(())
}

#[test]
fn a_record_that_cannot_be_read_is_left_out_and_the_rest_are_listed() -> TestResult {
    let root = tempdir()?;
    let (ledger, _clock) = ledger(root.path(), datetime!(2026-10-01 03:00:00 UTC));
    skipped(&ledger, "nightly")?;
    skipped(&ledger, "nightly")?;
    std::fs::write(
        root.path().join("acme/sync/nightly/2026/10/R-2.md"),
        "no frontmatter at all",
    )?;

    let runs = recent_shared_runs(root.path(), &key("nightly"), 20)?;

    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].run_id, "R-1");
    Ok(())
}

#[test]
fn a_key_that_could_leave_the_runs_folder_is_refused() {
    let mut escaping = key("nightly");
    escaping.job = "../outside".to_owned();

    let refused = recent_shared_runs(std::path::Path::new("/nonexistent"), &escaping, 20);

    assert!(refused.is_err());
}

#[test]
fn an_unreadable_record_is_named_by_its_job_and_run_and_never_the_host_folder() -> TestResult {
    let root = tempdir()?;
    let record = root.path().join("acme/sync/nightly/2026/10/R-1.md");

    let label = record_label(&record).to_string();

    assert_eq!(label, "nightly/2026/10/R-1.md");
    assert!(!label.contains(&root.path().display().to_string()), "{label}");
    Ok(())
}

fn sources<'a>(
    jobs_root: &'a std::path::Path,
    store_root: &'a std::path::Path,
    state_root: &std::path::Path,
    runs_root: &'a std::path::Path,
) -> ChainSources<'a> {
    ChainSources {
        jobs_root,
        store_root,
        state: SharedStateStore::new(state_root.to_path_buf()),
        runs_root,
    }
}

#[test]
fn a_chain_that_cannot_be_listed_leaves_the_jobs_own_runs_and_says_so() -> TestResult {
    let root = tempdir()?;
    let (ledger, _clock) = ledger(&root.path().join("runs"), datetime!(2026-10-01 03:00:00 UTC));
    skipped(&ledger, "nightly")?;
    skipped(&ledger, "nightly")?;
    // A file where the jobs folder should be cannot be listed.
    let jobs_root = root.path().join("jobs");
    std::fs::write(&jobs_root, b"not a folder")?;

    let history = recent_chain_runs(
        &sources(
            &jobs_root,
            &root.path().join("packages"),
            &root.path().join("state"),
            &root.path().join("runs"),
        ),
        &key("nightly"),
        20,
    )?;

    assert!(history.chain_unread);
    let ids: Vec<&str> = history.runs.iter().map(|run| run.run_id.as_str()).collect();
    assert_eq!(ids, ["R-2", "R-1"]);
    assert_eq!(history.own_latest.map(|run| run.run_id), Some("R-2".to_owned()));
    Ok(())
}

#[test]
fn a_jobs_own_latest_run_is_kept_when_its_successors_newer_runs_fill_the_log() -> TestResult {
    let root = tempdir()?;
    let runs_root = root.path().join("runs");
    let (ledger, clock) = ledger(&runs_root, datetime!(2026-10-01 03:00:00 UTC));
    skipped(&ledger, "nightly")?;
    *clock.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = datetime!(2026-10-02 03:00:00 UTC);
    for _ in 0..3 {
        skipped(&ledger, "nightly-v2")?;
    }
    let state: contextos_workflows::JobsState = toml::from_str(
        "[acme.sync.nightly]\nlifecycle = \"retired\"\n\n[acme.sync.nightly.retirement]\nby = \"system\"\n\
         cause = \"successor-enabled\"\nat = \"2026-10-01T10:00:00Z\"\n\
         replaced_by = { job = \"nightly-v2\", identity = \"job-2\" }\n\n\
         [acme.sync.nightly-v2]\nlifecycle = \"enabled\"\n",
    )?;
    let store = SharedStateStore::new(root.path().join("state"));
    store.update(|jobs| *jobs = state)?;
    let jobs_root = root.path().join("jobs");
    std::fs::create_dir_all(&jobs_root)?;

    let history = recent_chain_runs(
        &sources(
            &jobs_root,
            &root.path().join("packages"),
            &root.path().join("state"),
            &runs_root,
        ),
        &key("nightly"),
        2,
    )?;

    assert!(!history.chain_unread);
    let log: Vec<(&str, &str)> = history
        .runs
        .iter()
        .map(|run| (run.job.as_str(), run.run_id.as_str()))
        .collect();
    assert_eq!(log, [("nightly-v2", "R-3"), ("nightly-v2", "R-2")]);
    let own = history.own_latest.ok_or("the job's own latest run is lost")?;
    assert_eq!((own.job.as_str(), own.run_id.as_str()), ("nightly", "R-1"));
    Ok(())
}
