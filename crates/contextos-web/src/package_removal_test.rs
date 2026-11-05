use contextos_packages::{LedgerState, PackageStore, RecordedCause, ReleaseName, ReleaseVersion};
use contextos_workflows::{JobFileReading, JobLifecycle, LibraryKeep, WorkflowKeep};
use tempfile::{TempDir, tempdir};

use super::*;
use crate::host_store::HostStore;
use crate::retention_fixture::{
    FixedHome, archive, archive_needing, job_file, library_archive, names, put_without_retention,
};

type BoxError = Box<dyn std::error::Error>;

/// A host store holding the given releases, put there without any retention.
struct Held {
    root: TempDir,
}

impl Held {
    fn with(archives: &[Vec<u8>]) -> Result<Self, BoxError> {
        let held = Self { root: tempdir()? };
        for archive in archives {
            put_without_retention(held.root.path(), archive)?;
        }
        Ok(held)
    }

    fn versions(&self, package: &str) -> Vec<String> {
        let mut found: Vec<String> = std::fs::read_dir(self.root.path().join(package))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        found.sort();
        found
    }

    async fn remove(
        &self,
        home: &impl crate::package_retention::RetentionHome,
        (package, version): (&str, &str),
        force: bool,
    ) -> Result<Removal, PackageOpError> {
        let store = HostStore::new(self.root.path());
        let lock = store.lock().map_err(PackageOpError::from)?;
        let key = ReleaseKey {
            name: ReleaseName::try_from(package)?,
            version: ReleaseVersion::try_from(version)?,
        };
        remove_guarded(store.engine(), &lock, home, &key, force).await
    }

    async fn ledger_cause(&self, package: &str, version: &str) -> Result<Option<RecordedCause>, BoxError> {
        let store = HostStore::new(self.root.path());
        let LedgerState::Present(ledger) = store.engine().ledger().await?.state else {
            return Ok(None);
        };
        Ok(ledger
            .entries
            .iter()
            .find(|entry| {
                entry.release.key.name.as_str() == package && entry.release.key.version.to_string() == version
            })
            .and_then(|entry| entry.removal)
            .map(|removal| removal.cause))
    }
}

/// A home whose job files change after the first time they are read, as they do when a person writes a job file while a
/// removal is under way: the first reading is `before`, every later one `after`, or fails when `after` is `None`.
struct ChangingHome {
    state: contextos_workflows::JobsState,
    before: Vec<contextos_workflows::JobFile>,
    after: Option<Vec<contextos_workflows::JobFile>>,
    reads: std::sync::atomic::AtomicUsize,
}

impl ChangingHome {
    fn new(
        state: &str,
        before: Vec<contextos_workflows::JobFile>,
        after: Option<Vec<contextos_workflows::JobFile>>,
    ) -> Result<Self, BoxError> {
        Ok(Self {
            state: toml::from_str(state)?,
            before,
            after,
            reads: std::sync::atomic::AtomicUsize::new(0),
        })
    }
}

impl crate::package_retention::RetentionHome for ChangingHome {
    fn state(
        &self,
    ) -> impl std::future::Future<
        Output = Result<crate::package_retention::StateReading, crate::package_retention::RetentionError>,
    > + Send {
        std::future::ready(Ok(crate::package_retention::StateReading {
            state: self.state.clone(),
            recorded: true,
        }))
    }

    fn job_files(
        &self,
    ) -> impl std::future::Future<
        Output = Result<Vec<contextos_workflows::JobFile>, crate::package_retention::RetentionError>,
    > + Send {
        let first = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
        std::future::ready(match (&self.after, first) {
            (_, true) => Ok(self.before.clone()),
            (Some(after), false) => Ok(after.clone()),
            (None, false) => Err(crate::package_retention::RetentionError::JobsFolder(
                std::io::Error::other("the jobs folder went away"),
            )),
        })
    }
}

const CURRENT_1_2: &str = "[crm.sync]\ncurrent = \"1.2.0\"\n";

fn three() -> Result<Held, BoxError> {
    Held::with(&[archive("1.0.0")?, archive("1.1.0")?, archive("1.2.0")?])
}

fn in_use_reasons(error: PackageOpError) -> Result<Vec<KeepReason>, BoxError> {
    match error {
        PackageOpError::InUse { reasons, .. } => Ok(reasons),
        other => Err(format!("expected a release in use, got {other}").into()),
    }
}

fn current(workflow: &str) -> KeepReason {
    KeepReason::Workflow {
        workflow: workflow.to_owned(),
        reason: WorkflowKeep::Current,
    }
}

#[tokio::test]
async fn a_release_no_rule_keeps_is_removed_and_recorded_as_removed_by_request() -> Result<(), BoxError> {
    let held = three()?;
    let home = FixedHome::new(CURRENT_1_2, Vec::new())?;

    let removal = held.remove(&home, ("crm", "1.0.0"), false).await?;

    assert_eq!(held.versions("crm"), ["1.1.0", "1.2.0"]);
    assert_eq!(removal.cause, RecordedCause::ByRequest);
    assert_eq!(held.ledger_cause("crm", "1.0.0").await?, Some(RecordedCause::ByRequest));
    assert!(removal.overridden.is_empty() && removal.affected_jobs.is_empty());
    Ok(())
}

#[tokio::test]
async fn the_current_release_is_refused_and_left_in_place() -> Result<(), BoxError> {
    let held = three()?;
    let home = FixedHome::new(CURRENT_1_2, Vec::new())?;

    let error = held.remove(&home, ("crm", "1.2.0"), false).await.err();

    let reasons = in_use_reasons(error.ok_or("the removal was not refused")?)?;
    assert_eq!(reasons, [current("sync")]);
    assert_eq!(held.versions("crm"), ["1.0.0", "1.1.0", "1.2.0"]);
    assert_eq!(held.ledger_cause("crm", "1.2.0").await?, None);
    Ok(())
}

#[tokio::test]
async fn a_release_a_job_file_names_is_refused_naming_the_job() -> Result<(), BoxError> {
    let held = three()?;
    let home = FixedHome::new(CURRENT_1_2, vec![job_file("nightly", names("1.0.0"))])?;

    let error = held.remove(&home, ("crm", "1.0.0"), false).await.err();

    let reasons = in_use_reasons(error.ok_or("the removal was not refused")?)?;
    assert!(
        reasons.iter().any(|reason| matches!(
            reason,
            KeepReason::Workflow { reason: WorkflowKeep::NamedByJob { job, .. }, .. } if job == "nightly"
        )),
        "{reasons:?}"
    );
    assert_eq!(held.versions("crm"), ["1.0.0", "1.1.0", "1.2.0"]);
    Ok(())
}

#[tokio::test]
async fn a_job_file_that_cannot_be_read_refuses_a_release_of_its_workflow() -> Result<(), BoxError> {
    let held = three()?;
    let home = FixedHome::new(CURRENT_1_2, vec![job_file("broken", JobFileReading::Unreadable)])?;

    let error = held.remove(&home, ("crm", "1.0.0"), false).await.err();

    let reasons = in_use_reasons(error.ok_or("the removal was not refused")?)?;
    assert!(
        reasons.iter().any(|reason| matches!(
            reason,
            KeepReason::Workflow { reason: WorkflowKeep::UnreadableJobFile { job }, .. } if job == "broken"
        )),
        "{reasons:?}"
    );
    Ok(())
}

#[tokio::test]
async fn the_newest_future_release_is_removed_without_force() -> Result<(), BoxError> {
    let held = three()?;
    let home = FixedHome::new("[crm.sync]\ncurrent = \"1.1.0\"\nfuture = \"1.2.0\"\n", Vec::new())?;

    held.remove(&home, ("crm", "1.2.0"), false).await?;

    assert_eq!(held.versions("crm"), ["1.0.0", "1.1.0"]);
    Ok(())
}

#[tokio::test]
async fn forcing_removes_a_kept_release_records_forced_and_reports_the_jobs_that_lose_their_release()
-> Result<(), BoxError> {
    let held = three()?;
    let home = FixedHome::new(
        "[crm.sync]\ncurrent = \"1.2.0\"\n\n[crm.sync.nightly]\nlifecycle = \"enabled\"\nrelease = \"1.2.0\"\n",
        vec![job_file("nightly", names("1.2.0"))],
    )?;

    let removal = held.remove(&home, ("crm", "1.2.0"), true).await?;

    assert_eq!(held.versions("crm"), ["1.0.0", "1.1.0"]);
    assert_eq!(removal.cause, RecordedCause::Forced);
    assert_eq!(held.ledger_cause("crm", "1.2.0").await?, Some(RecordedCause::Forced));
    assert!(
        removal.overridden.contains(&current("sync")),
        "{:?}",
        removal.overridden
    );
    assert_eq!(
        removal.affected_jobs,
        [AffectedJob {
            package: "crm".to_owned(),
            workflow: "sync".to_owned(),
            job: "nightly".to_owned(),
            lifecycle: JobLifecycle::Enabled,
        }]
    );
    Ok(())
}

#[tokio::test]
async fn forcing_a_release_no_rule_keeps_is_recorded_as_removed_by_request() -> Result<(), BoxError> {
    let held = three()?;
    let home = FixedHome::new(CURRENT_1_2, Vec::new())?;

    let removal = held.remove(&home, ("crm", "1.0.0"), true).await?;

    assert_eq!(removal.cause, RecordedCause::ByRequest);
    assert_eq!(removal.overridden, []);
    Ok(())
}

const LOCKED: &str = "[crm.sync]\ncurrent = \"1.2.0\"\n\n[crm.sync.\"1.2.0\".lock.text]\nversion = \"1.0.0\"\ndigest = \
                      \"sha256:aaaa\"\n\n[crm.sync.nightly]\nlifecycle = \"enabled\"\nrelease = \"1.2.0\"\n";

fn with_library() -> Result<Held, BoxError> {
    Held::with(&[
        library_archive("text", "1.0.0")?,
        library_archive("text", "1.1.0")?,
        archive_needing("1.2.0", "text", "^1")?,
    ])
}

#[tokio::test]
async fn a_library_release_a_lock_names_is_refused_naming_the_workflow_release_that_locks_it() -> Result<(), BoxError> {
    let held = with_library()?;
    let home = FixedHome::new(LOCKED, vec![job_file("nightly", names("1.2.0"))])?;

    let error = held.remove(&home, ("text", "1.0.0"), false).await.err();

    let reasons = in_use_reasons(error.ok_or("the removal was not refused")?)?;
    assert!(
        reasons.iter().any(|reason| matches!(
            reason,
            KeepReason::Library(LibraryKeep::LockedBy { release, workflow })
                if release.to_string() == "crm 1.2.0" && workflow == "sync"
        )),
        "{reasons:?}"
    );
    assert_eq!(held.versions("text"), ["1.0.0", "1.1.0"]);
    Ok(())
}

#[tokio::test]
async fn forcing_a_locked_library_release_reports_the_jobs_on_the_release_that_locks_it() -> Result<(), BoxError> {
    let held = with_library()?;
    let home = FixedHome::new(LOCKED, vec![job_file("nightly", names("1.2.0"))])?;

    let removal = held.remove(&home, ("text", "1.0.0"), true).await?;

    assert_eq!(held.versions("text"), ["1.1.0"]);
    assert_eq!(
        removal
            .affected_jobs
            .iter()
            .map(|job| job.job.as_str())
            .collect::<Vec<_>>(),
        ["nightly"]
    );
    Ok(())
}

#[tokio::test]
async fn the_newest_release_of_a_library_is_removed_without_force() -> Result<(), BoxError> {
    let held = Held::with(&[library_archive("text", "1.0.0")?, library_archive("text", "1.1.0")?])?;
    let home = FixedHome::new("", Vec::new())?;

    held.remove(&home, ("text", "1.1.0"), false).await?;

    assert_eq!(held.versions("text"), ["1.0.0"]);
    Ok(())
}

#[tokio::test]
async fn a_damaged_release_no_rule_holds_is_removed_without_force() -> Result<(), BoxError> {
    let held = three()?;
    std::fs::write(held.root.path().join("crm/1.0.0/sync/sync.rhai"), b"tampered")?;
    let home = FixedHome::new(CURRENT_1_2, Vec::new())?;

    held.remove(&home, ("crm", "1.0.0"), false).await?;

    assert_eq!(held.versions("crm"), ["1.1.0", "1.2.0"]);
    Ok(())
}

#[tokio::test]
async fn a_damaged_current_release_is_refused_without_force_and_left_in_place() -> Result<(), BoxError> {
    let held = three()?;
    std::fs::write(held.root.path().join("crm/1.2.0/sync/sync.rhai"), b"tampered")?;
    let home = FixedHome::new(CURRENT_1_2, Vec::new())?;

    let error = held.remove(&home, ("crm", "1.2.0"), false).await.err();

    let reasons = in_use_reasons(error.ok_or("the removal was not refused")?)?;
    assert_eq!(reasons, [current("sync")]);
    assert_eq!(held.versions("crm"), ["1.0.0", "1.1.0", "1.2.0"]);
    assert_eq!(held.ledger_cause("crm", "1.2.0").await?, None);
    Ok(())
}

#[tokio::test]
async fn forcing_the_removal_of_a_damaged_release_a_job_names_records_forced_and_names_the_job() -> Result<(), BoxError>
{
    let held = three()?;
    std::fs::write(held.root.path().join("crm/1.2.0/sync/sync.rhai"), b"tampered")?;
    let home = FixedHome::new(
        "[crm.sync]\ncurrent = \"1.2.0\"\n\n[crm.sync.nightly]\nlifecycle = \"enabled\"\nrelease = \"1.2.0\"\n",
        vec![job_file("nightly", names("1.2.0"))],
    )?;

    let removal = held.remove(&home, ("crm", "1.2.0"), true).await?;

    assert_eq!(held.versions("crm"), ["1.0.0", "1.1.0"]);
    assert_eq!(removal.cause, RecordedCause::Forced);
    assert_eq!(held.ledger_cause("crm", "1.2.0").await?, Some(RecordedCause::Forced));
    assert_eq!(
        removal
            .affected_jobs
            .iter()
            .map(|job| job.job.as_str())
            .collect::<Vec<_>>(),
        ["nightly"]
    );
    Ok(())
}

#[tokio::test]
async fn a_release_that_is_not_in_the_store_is_not_found() -> Result<(), BoxError> {
    let held = three()?;
    let home = FixedHome::new(CURRENT_1_2, Vec::new())?;

    let error = held.remove(&home, ("crm", "9.9.9"), false).await.err();

    assert_eq!(error.map(|error| error.code()), Some("packages/not-found"));
    Ok(())
}

#[tokio::test]
async fn a_ledger_that_cannot_be_read_refuses_the_removal_and_changes_nothing() -> Result<(), BoxError> {
    let held = three()?;
    std::fs::write(held.root.path().join("ledger.toml"), "this is not a ledger")?;
    let home = FixedHome::new(CURRENT_1_2, Vec::new())?;

    let error = held.remove(&home, ("crm", "1.0.0"), false).await.err();

    assert_eq!(error.map(|error| error.code()), Some("packages/ledger-unreadable"));
    assert_eq!(held.versions("crm"), ["1.0.0", "1.1.0", "1.2.0"]);
    Ok(())
}

#[tokio::test]
async fn a_library_release_the_lock_of_a_damaged_release_names_is_refused_and_forcing_reports_its_jobs()
-> Result<(), BoxError> {
    let held = with_library()?;
    std::fs::write(held.root.path().join("crm/1.2.0/sync/sync.rhai"), b"tampered")?;
    let home = FixedHome::new(LOCKED, vec![job_file("nightly", names("1.2.0"))])?;

    let error = held.remove(&home, ("text", "1.0.0"), false).await.err();

    let reasons = in_use_reasons(error.ok_or("the removal was not refused")?)?;
    assert!(
        reasons.iter().any(|reason| matches!(
            reason,
            KeepReason::Library(LibraryKeep::LockedBy { release, .. }) if release.to_string() == "crm 1.2.0"
        )),
        "{reasons:?}"
    );
    assert_eq!(held.versions("text"), ["1.0.0", "1.1.0"]);
    let removal = held.remove(&home, ("text", "1.0.0"), true).await?;
    assert_eq!(
        removal
            .affected_jobs
            .iter()
            .map(|job| job.job.as_str())
            .collect::<Vec<_>>(),
        ["nightly"]
    );
    Ok(())
}

#[tokio::test]
async fn a_job_file_written_while_the_removal_is_under_way_refuses_it_and_restores_the_release() -> Result<(), BoxError>
{
    let held = three()?;
    let home = ChangingHome::new(CURRENT_1_2, Vec::new(), Some(vec![job_file("late", names("1.0.0"))]))?;

    let error = held.remove(&home, ("crm", "1.0.0"), false).await.err();

    let reasons = in_use_reasons(error.ok_or("the removal was not refused")?)?;
    assert!(
        reasons.iter().any(|reason| matches!(
            reason,
            KeepReason::Workflow { reason: WorkflowKeep::NamedByJob { job, .. }, .. } if job == "late"
        )),
        "{reasons:?}"
    );
    assert_eq!(held.versions("crm"), ["1.0.0", "1.1.0", "1.2.0"]);
    assert_eq!(held.ledger_cause("crm", "1.0.0").await?, None);
    Ok(())
}

#[tokio::test]
async fn a_job_file_written_while_a_forced_removal_is_under_way_is_overridden_and_recorded_as_forced()
-> Result<(), BoxError> {
    let held = three()?;
    let home = ChangingHome::new(CURRENT_1_2, Vec::new(), Some(vec![job_file("late", names("1.0.0"))]))?;

    let removal = held.remove(&home, ("crm", "1.0.0"), true).await?;

    assert_eq!(held.versions("crm"), ["1.1.0", "1.2.0"]);
    assert_eq!(removal.cause, RecordedCause::Forced);
    assert_eq!(held.ledger_cause("crm", "1.0.0").await?, Some(RecordedCause::Forced));
    assert!(
        removal.overridden.iter().any(|reason| matches!(
            reason,
            KeepReason::Workflow { reason: WorkflowKeep::NamedByJob { job, .. }, .. } if job == "late"
        )),
        "{:?}",
        removal.overridden
    );
    Ok(())
}

#[tokio::test]
async fn job_files_that_cannot_be_read_again_restore_the_release_and_remove_nothing() -> Result<(), BoxError> {
    let held = three()?;
    let home = ChangingHome::new(CURRENT_1_2, Vec::new(), None)?;

    let error = held.remove(&home, ("crm", "1.0.0"), false).await.err();

    assert_eq!(error.map(|error| error.code()), Some("packages/state-unreadable"));
    assert_eq!(held.versions("crm"), ["1.0.0", "1.1.0", "1.2.0"]);
    assert_eq!(held.ledger_cause("crm", "1.0.0").await?, None);
    Ok(())
}
