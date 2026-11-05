use std::sync::Mutex;

use contextos_workflows::{
    CallEntry, CallJournal, CallOutcome, ContentDigest, PackageIdentity, RunFailure, RunHeader, RunOutcome, RunStatus,
    RunTrigger, WorkflowFailureKind,
};
use tempfile::tempdir;
use time::macros::datetime;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A clock the test moves by hand.
struct SettableClock(Mutex<OffsetDateTime>);

impl SettableClock {
    fn at(start: OffsetDateTime) -> Arc<Self> {
        Arc::new(Self(Mutex::new(start)))
    }

    fn set(&self, instant: OffsetDateTime) {
        *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = instant;
    }
}

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

struct Fixture {
    root: tempfile::TempDir,
    claims: tempfile::TempDir,
    clock: Arc<SettableClock>,
    ledger: SharedLedger,
}

fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let claims = tempdir()?;
    let clock = SettableClock::at(datetime!(2026-10-01 03:00:00 UTC));
    let ledger = SharedLedger::new(
        root.path().to_path_buf(),
        Arc::clone(&clock) as Arc<dyn Clock>,
        time::Duration::days(30),
    );
    Ok(Fixture {
        root,
        claims,
        clock,
        ledger,
    })
}

impl Fixture {
    fn claim(&self, job: &str) -> Result<SharedClaim, Box<dyn std::error::Error>> {
        Ok(SharedClaim::acquire(self.claims.path(), &key(job))?)
    }

    fn begin(&self, job: &str) -> Result<RunRecorder, Box<dyn std::error::Error>> {
        let claim = self.claim(job)?;
        Ok(self.ledger.begin(
            &claim,
            RunStart {
                identity: &identity()?,
                inputs: vec!["source_vault".to_owned()],
                libraries: &[],
                trigger: RunTrigger::Manual,
            },
        )?)
    }

    fn record_path(&self, job: &str, month: &str, id: u64) -> std::path::PathBuf {
        self.root.path().join(format!("acme/sync/{job}/{month}/R-{id}.md"))
    }
}

fn read(path: &std::path::Path) -> Result<String, std::io::Error> {
    std::fs::read_to_string(path)
}

fn call(tool: &str) -> CallEntry {
    CallEntry {
        server: "contextos".to_owned(),
        tool: tool.to_owned(),
        outcome: CallOutcome::Succeeded,
        duration_ms: 5,
    }
}

#[test]
fn a_run_starts_with_a_running_record_named_for_its_job_and_start() -> TestResult {
    let fixture = fixture()?;

    let recorder = fixture.begin("nightly")?;

    assert_eq!(recorder.run_id(), 1);
    let text = read(&fixture.record_path("nightly", "2026/10", 1))?;
    for expected in [
        "id: R-1\n",
        "workflow: sync\n",
        "job: nightly\n",
        "package: acme\n",
        "version: 1.2.0\n",
        "status: running\n",
        "inputs: [\"source_vault\"]\n",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }
    Ok(())
}

#[test]
fn runs_of_a_job_are_numbered_one_after_another() -> TestResult {
    let fixture = fixture()?;

    let numbers: Vec<u64> = (0..3)
        .map(|_| fixture.begin("nightly").map(|recorder| recorder.run_id()))
        .collect::<Result<_, _>>()?;

    assert_eq!(numbers, [1, 2, 3]);
    Ok(())
}

#[test]
fn each_job_numbers_its_own_runs() -> TestResult {
    let fixture = fixture()?;
    fixture.begin("nightly")?;
    fixture.begin("nightly")?;

    let other = fixture.begin("weekly")?;

    assert_eq!(other.run_id(), 1);
    Ok(())
}

#[test]
fn a_number_is_not_reused_after_its_record_is_deleted() -> TestResult {
    let fixture = fixture()?;
    fixture.begin("nightly")?;
    let second = fixture.begin("nightly")?;
    std::fs::remove_file(fixture.record_path("nightly", "2026/10", second.run_id()))?;

    let third = fixture.begin("nightly")?;

    assert_eq!(third.run_id(), 3);
    Ok(())
}

#[test]
fn each_call_is_on_disk_as_soon_as_it_ends() -> TestResult {
    let fixture = fixture()?;
    let recorder = fixture.begin("nightly")?;

    recorder.call_finished(call("vault_info"));
    recorder.call_finished(call("fs_write_file"));

    let text = read(&fixture.record_path("nightly", "2026/10", 1))?;
    assert!(
        text.contains("| 1 | contextos | vault_info | succeeded | 5 ms |"),
        "{text}"
    );
    assert!(
        text.contains("| 2 | contextos | fs_write_file | succeeded | 5 ms |"),
        "{text}"
    );
    assert!(
        text.contains("status: running\n"),
        "a run that has not ended is still running"
    );
    Ok(())
}

#[test]
fn finishing_a_run_writes_its_outcome() -> TestResult {
    let fixture = fixture()?;
    let recorder = fixture.begin("nightly")?;
    recorder.call_finished(call("vault_info"));
    fixture.clock.set(datetime!(2026-10-01 03:00:09 UTC));

    recorder.finish(
        &RunOutcome::Failed(RunFailure {
            kind: Some(WorkflowFailureKind::ToolError),
            message: "workflow/tool-error: nope".to_owned(),
        }),
        fixture.clock.now(),
    )?;

    let text = read(&fixture.record_path("nightly", "2026/10", 1))?;
    assert!(text.contains("status: failed\n"), "{text}");
    assert!(text.contains("completed: 2026-10-01T03:00:09Z\n"), "{text}");
    assert!(text.contains("error: \"workflow/tool-error: nope\"\n"), "{text}");
    assert!(
        text.contains("| 1 | contextos | vault_info |"),
        "the calls survive finishing"
    );
    Ok(())
}

#[test]
fn a_run_that_died_leaves_its_record_running_until_the_next_run_marks_it_interrupted() -> TestResult {
    let fixture = fixture()?;
    let dead = fixture.begin("nightly")?;
    dead.call_finished(call("vault_info"));
    drop(dead);
    let untouched_other_job = fixture.begin("weekly")?;
    fixture.clock.set(datetime!(2026-10-01 05:00:00 UTC));

    let next = fixture.begin("nightly")?;

    let old = read(&fixture.record_path("nightly", "2026/10", 1))?;
    assert!(old.contains("status: interrupted\n"), "{old}");
    assert!(old.contains("completed: 2026-10-01T05:00:00Z\n"), "{old}");
    assert!(
        old.contains("| 1 | contextos | vault_info |"),
        "the completed call is kept"
    );
    assert!(
        read(&fixture.record_path("weekly", "2026/10", untouched_other_job.run_id()))?.contains("status: running\n"),
        "another job's record is not touched"
    );
    assert!(read(&fixture.record_path("nightly", "2026/10", next.run_id()))?.contains("status: running\n"));
    Ok(())
}

#[test]
fn a_finished_record_is_not_marked_interrupted() -> TestResult {
    let fixture = fixture()?;
    let finished = fixture.begin("nightly")?;
    finished.finish(&RunOutcome::Succeeded, fixture.clock.now())?;
    fixture.clock.set(datetime!(2026-10-01 05:00:00 UTC));

    fixture.begin("nightly")?;

    assert!(read(&fixture.record_path("nightly", "2026/10", 1))?.contains("status: succeeded\n"));
    Ok(())
}

#[test]
fn a_counter_that_is_not_a_number_stops_the_record_and_is_not_repaired() -> TestResult {
    let fixture = fixture()?;
    let job_dir = fixture.root.path().join("acme/sync/nightly");
    std::fs::create_dir_all(&job_dir)?;
    std::fs::write(job_dir.join("next-run-id"), "soon")?;

    let result = fixture.begin("nightly");

    assert!(result.is_err(), "a counter nobody can read must not guess a number");
    assert_eq!(read(&job_dir.join("next-run-id"))?, "soon");
    assert!(!job_dir.join("2026").exists(), "nothing was recorded");
    Ok(())
}

fn finished_at(fixture: &Fixture, job: &str, completed: OffsetDateTime, outcome: &RunOutcome) -> TestResult {
    fixture.clock.set(completed);
    let recorder = fixture.begin(job)?;
    recorder.finish(outcome, completed)?;
    Ok(())
}

#[test]
fn records_older_than_the_window_are_deleted_whatever_their_status_and_newer_ones_kept() -> TestResult {
    let fixture = fixture()?;
    finished_at(
        &fixture,
        "nightly",
        datetime!(2026-08-01 03:00:00 UTC),
        &RunOutcome::Succeeded,
    )?;
    finished_at(
        &fixture,
        "nightly",
        datetime!(2026-08-02 03:00:00 UTC),
        &RunOutcome::Failed(RunFailure::from("it broke".to_owned())),
    )?;
    finished_at(
        &fixture,
        "nightly",
        datetime!(2026-08-03 03:00:00 UTC),
        &RunOutcome::Cancelled,
    )?;
    finished_at(
        &fixture,
        "nightly",
        datetime!(2026-09-25 03:00:00 UTC),
        &RunOutcome::Succeeded,
    )?;
    fixture.clock.set(datetime!(2026-10-01 03:00:00 UTC));
    let current = fixture.begin("nightly")?;

    let swept = fixture.ledger.sweep(&fixture.claim("nightly")?, current.path())?;

    assert_eq!(swept.deleted, 3);
    assert!(!fixture.record_path("nightly", "2026/08", 1).exists());
    assert!(!fixture.record_path("nightly", "2026/08", 2).exists());
    assert!(!fixture.record_path("nightly", "2026/08", 3).exists());
    assert!(
        fixture.record_path("nightly", "2026/09", 4).exists(),
        "a record inside the window stays"
    );
    assert!(
        current.path().exists(),
        "the record of the run that just ended is never swept"
    );
    Ok(())
}

#[test]
fn a_running_record_is_never_swept_however_old() -> TestResult {
    let fixture = fixture()?;
    fixture.clock.set(datetime!(2026-01-01 03:00:00 UTC));
    drop(fixture.begin("nightly")?);
    fixture.clock.set(datetime!(2026-10-01 03:00:00 UTC));
    let claim = fixture.claim("nightly")?;

    let swept = fixture.ledger.sweep(&claim, std::path::Path::new("not-a-record"))?;

    assert_eq!(swept.deleted, 0);
    assert!(fixture.record_path("nightly", "2026/01", 1).exists());
    Ok(())
}

#[test]
fn a_sweep_touches_only_the_job_it_ran_for() -> TestResult {
    let fixture = fixture()?;
    finished_at(
        &fixture,
        "weekly",
        datetime!(2026-01-01 03:00:00 UTC),
        &RunOutcome::Succeeded,
    )?;
    fixture.clock.set(datetime!(2026-10-01 03:00:00 UTC));
    let claim = fixture.claim("nightly")?;

    fixture.ledger.sweep(&claim, std::path::Path::new("not-a-record"))?;

    assert!(fixture.record_path("weekly", "2026/01", 1).exists());
    Ok(())
}

#[test]
fn a_record_that_cannot_be_read_is_skipped_and_the_rest_are_still_swept() -> TestResult {
    let fixture = fixture()?;
    finished_at(
        &fixture,
        "nightly",
        datetime!(2026-01-01 03:00:00 UTC),
        &RunOutcome::Succeeded,
    )?;
    // A directory where a record should be: it cannot be read as one.
    std::fs::create_dir_all(fixture.record_path("nightly", "2026/01", 9))?;
    fixture.clock.set(datetime!(2026-10-01 03:00:00 UTC));
    let claim = fixture.claim("nightly")?;

    let swept = fixture.ledger.sweep(&claim, std::path::Path::new("not-a-record"))?;

    assert_eq!(swept.deleted, 1);
    assert_eq!(swept.skipped, 1);
    Ok(())
}

#[test]
fn an_emptied_month_folder_is_removed() -> TestResult {
    let fixture = fixture()?;
    finished_at(
        &fixture,
        "nightly",
        datetime!(2026-01-01 03:00:00 UTC),
        &RunOutcome::Succeeded,
    )?;
    fixture.clock.set(datetime!(2026-10-01 03:00:00 UTC));
    let claim = fixture.claim("nightly")?;

    fixture.ledger.sweep(&claim, std::path::Path::new("not-a-record"))?;

    assert!(!fixture.root.path().join("acme/sync/nightly/2026/01").exists());
    Ok(())
}

#[test]
fn a_record_names_its_status_as_a_run_status() -> TestResult {
    let fixture = fixture()?;
    let recorder = fixture.begin("nightly")?;
    recorder.finish(&RunOutcome::Cancelled, fixture.clock.now())?;

    let header = contextos_workflows::RunHeader::try_from(read(recorder.path())?.as_str())?;

    assert_eq!(header.status, RunStatus::Cancelled);
    Ok(())
}

#[test]
fn the_record_of_the_run_that_just_ended_survives_even_a_window_that_has_already_passed() -> TestResult {
    let fixture = fixture()?;
    let no_window = SharedLedger::new(
        fixture.root.path().to_path_buf(),
        Arc::clone(&fixture.clock) as Arc<dyn Clock>,
        time::Duration::ZERO,
    );
    let claim = fixture.claim("nightly")?;
    let recorder = no_window.begin(
        &claim,
        RunStart {
            identity: &identity()?,
            inputs: Vec::new(),
            libraries: &[],
            trigger: RunTrigger::Manual,
        },
    )?;
    recorder.finish(&RunOutcome::Succeeded, fixture.clock.now())?;

    let swept = no_window.sweep(&claim, recorder.path())?;

    assert_eq!(swept.deleted, 0);
    assert!(recorder.path().exists());
    Ok(())
}

#[test]
fn a_missing_counter_continues_after_the_records_already_there_and_never_reuses_a_number() -> TestResult {
    let fixture = fixture()?;
    fixture.begin("nightly")?;
    fixture.begin("nightly")?;
    std::fs::remove_file(fixture.root.path().join("acme/sync/nightly/next-run-id"))?;

    let next = fixture.begin("nightly")?;

    assert_eq!(next.run_id(), 3);
    assert!(
        read(&fixture.record_path("nightly", "2026/10", 1))?.contains("id: R-1\n"),
        "R-1 is untouched"
    );
    Ok(())
}

#[test]
fn deleting_a_counter_that_is_not_a_number_repairs_it_from_the_records() -> TestResult {
    let fixture = fixture()?;
    fixture.begin("nightly")?;
    let counter = fixture.root.path().join("acme/sync/nightly/next-run-id");
    std::fs::write(&counter, "x".repeat(10_000))?;

    let message = fixture
        .begin("nightly")
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(message.contains("delete it"), "{message}");
    assert!(
        message.len() < 400,
        "the counter is not echoed in full: {} bytes",
        message.len()
    );

    std::fs::remove_file(&counter)?;
    assert_eq!(fixture.begin("nightly")?.run_id(), 2);
    Ok(())
}

#[test]
fn a_record_that_cannot_be_read_does_not_stop_the_next_run_starting() -> TestResult {
    let fixture = fixture()?;
    fixture.begin("nightly")?;
    std::fs::write(fixture.record_path("nightly", "2026/10", 1), "not a run record")?;

    let next = fixture.begin("nightly")?;

    assert_eq!(next.run_id(), 2);
    assert_eq!(
        read(&fixture.record_path("nightly", "2026/10", 1))?,
        "not a run record",
        "it is left as found"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_record_that_cannot_be_deleted_is_left_and_counted_and_the_sweep_goes_on() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let fixture = fixture()?;
    finished_at(
        &fixture,
        "nightly",
        datetime!(2026-01-01 03:00:00 UTC),
        &RunOutcome::Succeeded,
    )?;
    finished_at(
        &fixture,
        "nightly",
        datetime!(2026-02-01 03:00:00 UTC),
        &RunOutcome::Succeeded,
    )?;
    fixture.clock.set(datetime!(2026-10-01 03:00:00 UTC));
    let locked = fixture.root.path().join("acme/sync/nightly/2026/01");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555))?;
    let writable = std::fs::write(locked.join("probe"), "").is_ok();
    let claim = fixture.claim("nightly")?;

    let swept = fixture.ledger.sweep(&claim, std::path::Path::new("not-a-record"));

    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))?;
    if writable {
        // Running as a user the permission does not bind, so the deletion could not be made to fail.
        return Ok(());
    }
    let swept = swept?;
    assert_eq!(swept.skipped, 1);
    assert_eq!(swept.deleted, 1, "the other record is still swept");
    assert!(fixture.record_path("nightly", "2026/01", 1).exists());
    Ok(())
}

fn skipped(ledger: &SharedLedger, job: &str) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    Ok(ledger.record_skipped(
        &key(job),
        SkippedRun {
            identity: &identity()?,
            inputs: vec!["source_vault".to_owned()],
            reason: "the job's previous run still held its claim",
        },
    )?)
}

#[test]
fn a_scheduled_run_is_recorded_as_scheduled() -> TestResult {
    let fixture = fixture()?;
    let claim = fixture.claim("nightly")?;

    let recorder = fixture.ledger.begin(
        &claim,
        RunStart {
            identity: &identity()?,
            inputs: Vec::new(),
            libraries: &[],
            trigger: RunTrigger::Scheduled,
        },
    )?;

    assert!(read(recorder.path())?.contains("trigger: scheduled\n"));
    Ok(())
}

#[test]
fn a_skipped_tick_leaves_a_finished_record_with_its_own_run_number_and_the_reason() -> TestResult {
    let fixture = fixture()?;

    let path = skipped(&fixture.ledger, "nightly")?;

    assert_eq!(path, fixture.record_path("nightly", "2026/10", 1));
    let text = read(&path)?;
    assert!(text.contains("status: skipped\n"), "{text}");
    assert!(text.contains("trigger: scheduled\n"), "{text}");
    assert!(text.contains("previous run still held its claim"), "{text}");
    let header = RunHeader::try_from(text.as_str())?;
    assert_eq!(
        header.completed,
        Some(header.started),
        "it ran nothing, so it ended as it began"
    );
    Ok(())
}

#[test]
fn a_skipped_tick_can_be_recorded_while_another_run_holds_the_claim_and_never_reuses_its_number() -> TestResult {
    let fixture = fixture()?;
    let running = fixture.begin("nightly")?;
    assert_eq!(running.run_id(), 1);

    let skipped_path = skipped(&fixture.ledger, "nightly")?;
    let later = skipped(&fixture.ledger, "nightly")?;

    assert_eq!(skipped_path, fixture.record_path("nightly", "2026/10", 2));
    assert_eq!(later, fixture.record_path("nightly", "2026/10", 3));
    assert!(
        read(running.path())?.contains("status: running"),
        "the running record is untouched"
    );
    running.finish(&RunOutcome::Succeeded, fixture.clock.now())?;
    drop(fixture.claim("nightly").ok());
    Ok(())
}

#[test]
fn the_next_run_after_skipped_ticks_continues_the_numbering() -> TestResult {
    let fixture = fixture()?;
    skipped(&fixture.ledger, "nightly")?;
    skipped(&fixture.ledger, "nightly")?;

    let recorder = fixture.begin("nightly")?;

    assert_eq!(recorder.run_id(), 3);
    Ok(())
}

#[test]
fn a_run_that_begins_does_not_interrupt_a_skipped_record() -> TestResult {
    let fixture = fixture()?;
    let path = skipped(&fixture.ledger, "nightly")?;

    fixture.begin("nightly")?;

    assert!(read(&path)?.contains("status: skipped\n"));
    Ok(())
}

#[test]
fn simultaneous_skipped_ticks_each_get_a_different_number() -> TestResult {
    let fixture = fixture()?;
    let ledger = Arc::new(SharedLedger::new(
        fixture.root.path().to_path_buf(),
        Arc::clone(&fixture.clock) as Arc<dyn Clock>,
        time::Duration::days(30),
    ));
    let writers = 10;
    let start = Arc::new(std::sync::Barrier::new(writers));
    let handles: Vec<_> = (0..writers)
        .map(|_| {
            let ledger = Arc::clone(&ledger);
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                skipped(&ledger, "nightly").map_err(|error| error.to_string())
            })
        })
        .collect();
    let mut paths = Vec::new();
    for handle in handles {
        paths.push(handle.join().map_err(|_| "a writer panicked")??);
    }

    paths.sort();
    paths.dedup();
    assert_eq!(paths.len(), writers, "every record has its own number");
    Ok(())
}

#[test]
fn a_skipped_record_ages_out_like_any_other_that_is_not_running() -> TestResult {
    let fixture = fixture()?;
    let old = skipped(&fixture.ledger, "nightly")?;
    fixture.clock.set(datetime!(2026-12-01 03:00:00 UTC));
    let claim = fixture.claim("nightly")?;
    let recorder = fixture.ledger.begin(
        &claim,
        RunStart {
            identity: &identity()?,
            inputs: Vec::new(),
            libraries: &[],
            trigger: RunTrigger::Manual,
        },
    )?;

    let swept = fixture.ledger.sweep(&claim, recorder.path())?;

    assert_eq!(swept.deleted, 1);
    assert!(!old.exists());
    Ok(())
}

#[test]
fn a_skipped_record_for_a_job_whose_names_could_leave_the_runs_folder_is_refused() -> TestResult {
    let fixture = fixture()?;

    let refused = fixture.ledger.record_skipped(
        &key(".."),
        SkippedRun {
            identity: &identity()?,
            inputs: Vec::new(),
            reason: "x",
        },
    );

    assert!(refused.is_err());
    assert!(
        std::fs::read_dir(fixture.root.path())?.next().is_none(),
        "nothing was written"
    );
    Ok(())
}

// ---------------------------------------------------------------------
// Dry runs
// ---------------------------------------------------------------------

impl Fixture {
    fn begin_dry(&self, job: &str) -> Result<RunRecorder, Box<dyn std::error::Error>> {
        Ok(self.ledger.begin_dry(
            &key(job),
            RunStart {
                identity: &identity()?,
                inputs: vec!["source_vault".to_owned()],
                libraries: &[],
                trigger: RunTrigger::Manual,
            },
        )?)
    }
}

#[test]
fn a_dry_run_is_recorded_flagged_without_the_jobs_claim() -> TestResult {
    let fixture = fixture()?;
    let _held_by_a_live_run = fixture.claim("nightly")?;

    let recorder = fixture.begin_dry("nightly")?;
    recorder.finish(&RunOutcome::Succeeded, fixture.clock.now())?;

    let text = read(&fixture.record_path("nightly", "2026/10", 1))?;
    assert!(text.contains("dry_run: true\n"), "{text}");
    assert!(text.contains("status: succeeded\n"), "{text}");
    Ok(())
}

#[test]
fn a_dry_run_leaves_nothing_on_disk_until_it_ends() -> TestResult {
    let fixture = fixture()?;

    let recorder = fixture.begin_dry("nightly")?;
    recorder.call_finished(call("vault_info"));

    assert!(
        !recorder.path().exists(),
        "a record that is never `running` cannot be mistaken for a dead run"
    );
    recorder.finish(&RunOutcome::Succeeded, fixture.clock.now())?;
    let text = read(recorder.path())?;
    assert!(text.contains("vault_info"), "the calls it made are kept:\n{text}");
    Ok(())
}

#[test]
fn a_live_run_beginning_does_not_interrupt_a_dry_run_still_in_progress() -> TestResult {
    let fixture = fixture()?;
    let dry = fixture.begin_dry("nightly")?;

    let live = fixture.begin("nightly")?;

    assert_ne!(
        dry.run_id(),
        live.run_id(),
        "they share the job's numbering without clashing"
    );
    dry.finish(&RunOutcome::Succeeded, fixture.clock.now())?;
    let text = read(dry.path())?;
    assert!(text.contains("status: succeeded\n"), "{text}");
    Ok(())
}

#[test]
fn a_dry_run_beginning_does_not_interrupt_a_live_run_in_progress() -> TestResult {
    let fixture = fixture()?;
    let live = fixture.begin("nightly")?;

    fixture.begin_dry("nightly")?;

    let text = read(live.path())?;
    assert!(text.contains("status: running\n"), "{text}");
    Ok(())
}

#[test]
fn a_dry_run_refuses_a_job_key_that_could_leave_the_runs_folder() -> TestResult {
    let fixture = fixture()?;
    let mut escaping = key("nightly");
    escaping.job = "../outside".to_owned();

    let refused = fixture.ledger.begin_dry(
        &escaping,
        RunStart {
            identity: &identity()?,
            inputs: Vec::new(),
            libraries: &[],
            trigger: RunTrigger::Manual,
        },
    );

    assert!(matches!(refused, Err(LedgerError::InvalidName { .. })));
    Ok(())
}

#[test]
fn no_ledger_error_names_the_absolute_folder_it_was_raised_in() -> TestResult {
    let folder = tempdir()?;
    let root = folder.path().to_string_lossy().into_owned();
    let counter = folder.path().join("acme").join("sync").join("nightly").join(".counter");
    let wrapped = std::io::Error::from(contextos_fs::FsError::NotFound {
        path: folder.path().join("acme").join("R-1.md"),
    });
    let errors = [
        LedgerError::Io(wrapped),
        LedgerError::InvalidName {
            name: "../escape".to_owned(),
        },
        LedgerError::Counter {
            path: counter,
            value: "twelve".to_owned(),
        },
    ];

    for error in &errors {
        let shown = error.to_string();
        assert!(!shown.contains(&root), "{error:?} shows the folder: {shown}");
        assert!(!shown.is_empty(), "{error:?}");
    }
    let shown = errors[2].to_string();
    assert!(shown.contains("nightly/.counter"), "it still says which file: {shown}");
    Ok(())
}
