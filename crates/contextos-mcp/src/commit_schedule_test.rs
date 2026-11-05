use std::sync::{Arc, Mutex};
use std::time::Duration;

use contextos_core::{
    ConfiguredTimeZone, OpKind, OperationEvent, OperatorTimeZone, Origin, VaultPath, VaultPathInput, VaultRoot,
    VaultRootInput, VaultSet, VersionsVault,
};
use contextos_fs::{
    Filesystem, FilesystemConfig, FilesystemService, FilesystemServiceConfig, FsLimits, default_hidden_patterns,
};
use contextos_git::{Git2Vault, Git2VaultConfig};

use super::*;
use crate::server::VaultClock;

type BoxError = Box<dyn std::error::Error>;

type Job = Box<dyn FnOnce() + Send>;

/// Holds every scheduled job until the test runs it, so a debounce window passes exactly when the test says.
#[derive(Clone, Default)]
struct ManualScheduler {
    queued: Arc<Mutex<Vec<(Duration, Job)>>>,
    without_runtime: bool,
}

impl std::fmt::Debug for ManualScheduler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ManualScheduler")
    }
}

impl SchedulesCommits for ManualScheduler {
    fn after(&self, delay: Duration, job: Job) -> Result<(), Unschedulable> {
        if self.without_runtime {
            return Err(Unschedulable);
        }
        self.queued.lock().map_err(|_| Unschedulable)?.push((delay, job));
        Ok(())
    }
}

impl ManualScheduler {
    fn queued_delays(&self) -> Vec<Duration> {
        self.queued
            .lock()
            .map(|queued| queued.iter().map(|(delay, _)| *delay).collect())
            .unwrap_or_default()
    }

    /// Runs the oldest queued job, returning whether there was one.
    fn run_next(&self) -> bool {
        let next = self.queued.lock().ok().and_then(|mut queued| {
            if queued.is_empty() {
                None
            } else {
                Some(queued.remove(0))
            }
        });
        next.map(|(_, job)| job()).is_some()
    }
}

struct Fixture {
    vault: tempfile::TempDir,
    roots: VaultSet,
    git: ManagedGit,
    scheduler: ManualScheduler,
    versions: VersionCollection,
}

fn fixture(scheduler: ManualScheduler) -> Result<Fixture, BoxError> {
    fixture_with_window(scheduler, 30)
}

fn fixture_with_window(scheduler: ManualScheduler, window_seconds: u64) -> Result<Fixture, BoxError> {
    let vault = tempfile::tempdir()?;
    std::fs::write(vault.path().join("note.md"), "initial\n")?;
    std::fs::write(vault.path().join("other.md"), "initial\n")?;
    let root = VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?;
    let roots = VaultSet::try_from(vec![root.clone()])?;
    // The vault clock the server composes, in a fixed zone so the test never depends on the host's.
    let clock = VaultClock::from(contextos_core::LocalClockInput {
        clock: crate::ServerClock::default(),
        zone: OperatorTimeZone::from(ConfiguredTimeZone::try_from("UTC")?),
    });
    let git = Git2Vault::try_from(Git2VaultConfig {
        root: root.clone(),
        roots: roots.clone(),
        clock: clock.clone(),
        author_name: "Context OS MCP".to_owned(),
        author_email: "mcp@contextos.local".to_owned(),
        allow_destructive_restore: false,
        protected_restore_paths: Vec::new(),
    })?;
    let filesystem = Filesystem::try_from(FilesystemConfig {
        roots: roots.clone(),
        limits: vec![FsLimits {
            max_read_bytes: 1024 * 1024,
            max_batch_files: 50,
        }],
        hidden: vec![default_hidden_patterns()],
        atomic_write_guard: None,
    })?;
    git.initialise(&FilesystemService::from(FilesystemServiceConfig { filesystem, clock }))?;
    let versions = VersionCollection::new(
        Arc::new(vec![Some(git.clone())]),
        Arc::new(vec![window_seconds]),
        Arc::new(scheduler.clone()),
    );
    Ok(Fixture {
        vault,
        roots,
        git,
        scheduler,
        versions,
    })
}

impl Fixture {
    fn change(&self, relative: &str) -> Result<OperationEvent, BoxError> {
        std::fs::write(self.vault.path().join(relative), "changed\n")?;
        Ok(OperationEvent {
            kind: OpKind::Modify,
            paths: vec![VaultPath::try_from(VaultPathInput {
                roots: &self.roots,
                raw: relative,
            })?],
            origin: Origin::Tool("fs_write_file".to_owned()),
            summary: format!("Changed {relative}"),
            at: time::OffsetDateTime::now_utc(),
        })
    }

    fn lock_branch(&self) -> Result<(), BoxError> {
        std::fs::write(self.vault.path().join(".git/refs/heads/main.lock"), "")?;
        Ok(())
    }

    fn unlock_branch(&self) -> Result<(), BoxError> {
        std::fs::remove_file(self.vault.path().join(".git/refs/heads/main.lock"))?;
        Ok(())
    }

    fn pending(&self) -> Result<Vec<String>, BoxError> {
        Ok(self.git.status()?.pending_paths)
    }
}

#[test]
fn staging_arms_one_timer_for_the_configured_window_and_running_it_commits() -> Result<(), BoxError> {
    let fixture = fixture(ManualScheduler::default())?;

    fixture
        .versions
        .stage(&fixture.change("note.md")?)
        .map_err(|warning| warning.message)?;

    assert_eq!(fixture.scheduler.queued_delays(), [Duration::from_secs(30)]);
    assert_eq!(fixture.pending()?, ["note.md"]);
    assert!(fixture.scheduler.run_next());
    assert!(
        fixture.pending()?.is_empty(),
        "`fixture.pending()?` should be empty, got {:?}",
        fixture.pending()?
    );
    assert!(fixture.git.status()?.commit_failure.is_none());
    Ok(())
}

#[test]
fn a_later_stage_supersedes_the_earlier_timer_so_the_batch_is_one_commit() -> Result<(), BoxError> {
    let fixture = fixture(ManualScheduler::default())?;
    fixture
        .versions
        .stage(&fixture.change("note.md")?)
        .map_err(|warning| warning.message)?;
    fixture
        .versions
        .stage(&fixture.change("other.md")?)
        .map_err(|warning| warning.message)?;
    assert_eq!(fixture.scheduler.queued_delays().len(), 2);

    assert!(fixture.scheduler.run_next());
    assert_eq!(
        fixture.pending()?.len(),
        2,
        "the first timer is stale and commits nothing"
    );
    assert!(fixture.scheduler.run_next());

    assert!(
        fixture.pending()?.is_empty(),
        "`fixture.pending()?` should be empty, got {:?}",
        fixture.pending()?
    );
    assert_eq!(
        fixture
            .git
            .log(&contextos_git::GitLogRequest { path: None, limit: 10 })?
            .len(),
        2
    );
    Ok(())
}

#[test]
fn a_failed_commit_is_retried_until_it_succeeds() -> Result<(), BoxError> {
    let fixture = fixture(ManualScheduler::default())?;
    fixture
        .versions
        .stage(&fixture.change("note.md")?)
        .map_err(|warning| warning.message)?;
    fixture.lock_branch()?;

    assert!(fixture.scheduler.run_next());
    assert_eq!(
        fixture.pending()?,
        ["note.md"],
        "a failed commit leaves the path pending"
    );
    assert_eq!(
        fixture.git.status()?.commit_failure.map(|failure| failure.attempts),
        Some(1)
    );
    assert_eq!(fixture.scheduler.queued_delays().len(), 1, "a retry is scheduled");

    fixture.unlock_branch()?;
    assert!(fixture.scheduler.run_next());

    assert!(
        fixture.pending()?.is_empty(),
        "`fixture.pending()?` should be empty, got {:?}",
        fixture.pending()?
    );
    assert!(fixture.git.status()?.commit_failure.is_none());
    Ok(())
}

#[test]
fn retries_are_bounded_and_the_failure_stays_reported() -> Result<(), BoxError> {
    let fixture = fixture(ManualScheduler::default())?;
    fixture
        .versions
        .stage(&fixture.change("note.md")?)
        .map_err(|warning| warning.message)?;
    fixture.lock_branch()?;

    let mut runs = 0;
    while fixture.scheduler.run_next() {
        runs += 1;
        assert!(runs <= 20, "retries must be bounded");
    }

    assert_eq!(runs, usize::try_from(COMMIT_ATTEMPTS)?);
    assert_eq!(fixture.pending()?, ["note.md"]);
    assert_eq!(
        fixture.git.status()?.commit_failure.map(|failure| failure.attempts),
        Some(COMMIT_ATTEMPTS)
    );
    Ok(())
}

#[test]
fn a_new_stage_after_exhausted_retries_arms_a_fresh_window() -> Result<(), BoxError> {
    let fixture = fixture(ManualScheduler::default())?;
    fixture
        .versions
        .stage(&fixture.change("note.md")?)
        .map_err(|warning| warning.message)?;
    fixture.lock_branch()?;
    while fixture.scheduler.run_next() {}
    fixture.unlock_branch()?;

    fixture
        .versions
        .stage(&fixture.change("other.md")?)
        .map_err(|warning| warning.message)?;
    assert!(fixture.scheduler.run_next());

    assert!(
        fixture.pending()?.is_empty(),
        "the fresh window commits the earlier path too"
    );
    Ok(())
}

#[test]
fn staging_without_a_runtime_says_so_and_keeps_the_path_pending() -> Result<(), BoxError> {
    let fixture = fixture(ManualScheduler {
        without_runtime: true,
        ..ManualScheduler::default()
    })?;

    let warning = fixture
        .versions
        .stage(&fixture.change("note.md")?)
        .err()
        .ok_or("staging should warn that no commit could be scheduled")?;

    assert_eq!(warning.code, "git/schedule");
    assert_eq!(fixture.pending()?, ["note.md"]);
    Ok(())
}

#[test]
fn a_vault_without_a_repository_stages_nothing_and_schedules_nothing() -> Result<(), BoxError> {
    let fixture = fixture(ManualScheduler::default())?;
    std::fs::remove_dir_all(fixture.vault.path().join(".git"))?;

    fixture
        .versions
        .stage(&fixture.change("note.md")?)
        .map_err(|warning| warning.message)?;

    assert!(
        fixture.scheduler.queued_delays().is_empty(),
        "`fixture.scheduler.queued_delays()` should be empty, got {:?}",
        fixture.scheduler.queued_delays()
    );
    Ok(())
}

#[test]
fn the_tokio_scheduler_reports_when_there_is_no_runtime_to_run_a_job_later() {
    let scheduled = TokioScheduler.after(Duration::ZERO, Box::new(|| {}));

    assert_eq!(scheduled, Err(Unschedulable));
}

#[tokio::test]
async fn the_tokio_scheduler_runs_a_job_off_the_thread_that_scheduled_it() -> Result<(), BoxError> {
    let scheduling_thread = std::thread::current().id();
    let (done, finished) = tokio::sync::oneshot::channel();

    TokioScheduler
        .after(
            Duration::ZERO,
            Box::new(move || {
                let _ignored = done.send(std::thread::current().id());
            }),
        )
        .map_err(|Unschedulable| "a runtime is available")?;

    // Committing is blocking Git work: run inline on the async task it would share this thread.
    assert_ne!(finished.await?, scheduling_thread);
    Ok(())
}

#[test]
fn a_batch_that_has_outlived_the_cap_commits_at_once_even_as_changes_keep_arriving() -> Result<(), BoxError> {
    // A zero window makes the cap zero, so every batch is already past it.
    let fixture = fixture_with_window(ManualScheduler::default(), 0)?;
    fixture
        .versions
        .stage(&fixture.change("note.md")?)
        .map_err(|warning| warning.message)?;
    fixture
        .versions
        .stage(&fixture.change("other.md")?)
        .map_err(|warning| warning.message)?;
    assert_eq!(fixture.scheduler.queued_delays(), [Duration::ZERO, Duration::ZERO]);

    assert!(fixture.scheduler.run_next());

    assert!(
        fixture.pending()?.is_empty(),
        "an overdue batch commits whatever is pending, not only when no newer change has arrived"
    );
    Ok(())
}

#[test]
fn a_retry_waits_at_least_the_minimum_even_when_the_window_is_zero() -> Result<(), BoxError> {
    let fixture = fixture_with_window(ManualScheduler::default(), 0)?;
    fixture
        .versions
        .stage(&fixture.change("note.md")?)
        .map_err(|warning| warning.message)?;
    fixture.lock_branch()?;

    assert!(fixture.scheduler.run_next());

    assert_eq!(fixture.scheduler.queued_delays(), [MINIMUM_RETRY]);
    Ok(())
}
