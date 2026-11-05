use std::fs::OpenOptions;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use contextos_core::{OpKind, Origin, VaultRootId};
use time::OffsetDateTime;

use super::*;
use crate::write_coordinator::{WriteCoordinator, WriteCoordinatorConfig};
use contextos_fs::HOST_WRITE_LOCK;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

fn event() -> OperationEvent {
    OperationEvent {
        kind: OpKind::Modify,
        paths: Vec::new(),
        origin: Origin::Tool("fs_write_file".to_owned()),
        summary: "test".to_owned(),
        at: OffsetDateTime::UNIX_EPOCH,
    }
}

/// Counts calls and records whether the projection lock was held while it ran.
#[derive(Clone, Debug)]
struct Probe {
    lock: PathBuf,
    calls: Arc<AtomicUsize>,
    held_during_call: Arc<AtomicUsize>,
}

impl Probe {
    fn new(lock: &Path) -> Self {
        Self {
            lock: lock.to_path_buf(),
            calls: Arc::default(),
            held_during_call: Arc::default(),
        }
    }
}

impl ProjectsDefinitions for Probe {
    fn project(&self, _event: &OperationEvent) -> Vec<OperationWarning> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let held = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.lock)
            .ok()
            .and_then(|file| contextos_fs::try_lock_exclusive(&file).ok())
            .is_some_and(|acquired| !acquired);
        if held {
            self.held_during_call.fetch_add(1, Ordering::SeqCst);
        }
        Vec::new()
    }
}

fn taken<T>(outcome: Result<T, OperationWarning>) -> Result<T, BoxError> {
    outcome.map_err(|warning| format!("{}: {}", warning.code, warning.message).into())
}

fn locked(probe: Probe, lock: PathBuf, wait: Duration) -> LockedProjection<Probe> {
    LockedProjection::from(LockedProjectionConfig {
        inner: probe,
        lock,
        wait,
    })
}

#[test]
fn the_lock_is_taken_beneath_the_state_root_and_held_while_the_projection_runs() -> Result<(), BoxError> {
    let state = tempfile::tempdir()?;
    let state_root = state.path().join("not/yet/created");
    let lock = state_root.join(PROJECTION_LOCK);
    let probe = Probe::new(&lock);
    let projection = LockedProjection::from(LockedProjectionConfig::beneath(probe.clone(), &state_root));

    let warnings = projection.project(&event());

    assert_eq!(warnings, Vec::new());
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        probe.held_during_call.load(Ordering::SeqCst),
        1,
        "the projection ran without holding the lock"
    );
    assert!(lock.is_file(), "the lock file was not created in the state root");
    Ok(())
}

#[test]
fn the_lock_is_released_after_each_projection() -> Result<(), BoxError> {
    let state = tempfile::tempdir()?;
    let lock = state.path().join(PROJECTION_LOCK);
    let projection = locked(Probe::new(&lock), lock.clone(), Duration::from_secs(5));

    let _ = projection.project(&event());

    let file = OpenOptions::new().read(true).write(true).open(&lock)?;
    assert!(
        contextos_fs::try_lock_exclusive(&file)?,
        "the lock was still held after the call"
    );
    Ok(())
}

#[test]
fn a_wait_beyond_the_bound_reports_one_busy_warning_and_changes_nothing() -> Result<(), BoxError> {
    let state = tempfile::tempdir()?;
    let lock = state.path().join(PROJECTION_LOCK);
    let other_process = locked(Probe::new(&lock), lock.clone(), Duration::from_secs(5));
    let probe = Probe::new(&lock);
    let this_process = locked(probe.clone(), lock, Duration::from_millis(50));

    let warnings = taken(other_process.with_lock(|_| this_process.project(&event())))?;

    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].code, "projection/busy");
    assert_ne!(warnings[0].message, "");
    assert_eq!(
        probe.calls.load(Ordering::SeqCst),
        0,
        "the projection ran without the lock"
    );
    Ok(())
}

#[test]
fn a_busy_lock_is_reported_by_with_lock_without_running_the_body() -> Result<(), BoxError> {
    let state = tempfile::tempdir()?;
    let lock = state.path().join(PROJECTION_LOCK);
    let first = locked(Probe::new(&lock), lock.clone(), Duration::from_secs(5));
    let second = locked(Probe::new(&lock), lock, Duration::from_millis(50));
    let mut ran = false;

    let outcome = taken(first.with_lock(|_| {
        second.with_lock(|_| {
            ran = true;
        })
    }))?;

    assert_eq!(
        outcome.err().map(|warning| warning.code),
        Some("projection/busy".to_owned())
    );
    assert!(!ran);
    Ok(())
}

#[test]
fn a_second_holder_waits_for_the_first_and_then_proceeds() -> Result<(), BoxError> {
    let state = tempfile::tempdir()?;
    let lock = state.path().join(PROJECTION_LOCK);
    let first = locked(Probe::new(&lock), lock.clone(), Duration::from_secs(5));
    let probe = Probe::new(&lock);
    let second = locked(probe.clone(), lock, Duration::from_secs(30));

    std::thread::scope(|scope| {
        let waiter = first.with_lock(|_| {
            let handle = scope.spawn(|| second.project(&event()));
            std::thread::sleep(Duration::from_millis(100));
            let finished_early = handle.is_finished();
            (handle, finished_early)
        });
        let (handle, finished_early) = taken(waiter)?;
        assert!(!finished_early, "the second holder ran while the first held the lock");
        let warnings = handle.join().map_err(|_| "the waiting thread panicked")?;
        assert!(warnings.is_empty(), "{warnings:?}");
        Ok::<(), BoxError>(())
    })?;

    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[test]
fn a_lock_file_that_cannot_be_created_is_reported_as_a_warning_not_a_panic() -> Result<(), BoxError> {
    let state = tempfile::tempdir()?;
    let blocker = state.path().join("blocker");
    std::fs::write(&blocker, b"a file where a folder must be")?;
    let lock = blocker.join(PROJECTION_LOCK);
    let probe = Probe::new(&lock);
    let projection = locked(probe.clone(), lock, Duration::from_millis(50));

    let warnings = projection.project(&event());

    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].code, "projection/lock-failed");
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn projection_can_be_taken_while_vault_locks_are_held_because_vault_locks_come_first() -> Result<(), BoxError> {
    let state = tempfile::tempdir()?;
    let vault_lock = state.path().join("vault").join(HOST_WRITE_LOCK);
    let lock = state.path().join(PROJECTION_LOCK);
    let writes = WriteCoordinator::from(WriteCoordinatorConfig {
        host_locks: vec![vault_lock],
        host_wait: Duration::from_secs(5),
    });
    let probe = Probe::new(&lock);
    let projection = locked(probe.clone(), lock, Duration::from_secs(5));

    // A tool write holds its vault locks while the router calls projection; projection takes only its own lock.
    let vault_guards = writes.lock_roots(&[VaultRootId::try_from(0_usize)?]).await?;
    let warnings = tokio::task::spawn_blocking(move || projection.project(&event())).await?;

    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    drop(vault_guards);
    Ok(())
}

#[test]
fn a_write_time_projection_waits_five_seconds_for_the_lock() {
    let projection = LockedProjection::from(LockedProjectionConfig::beneath(
        (),
        std::path::Path::new("/nonexistent-state-root"),
    ));

    assert_eq!(projection.event_wait(), Duration::from_secs(5));
}

#[test]
fn start_up_refresh_doctor_and_resolution_keep_the_longer_wait() {
    let projection = LockedProjection::from(LockedProjectionConfig::beneath(
        (),
        std::path::Path::new("/nonexistent-state-root"),
    ));

    assert_eq!(projection.maintenance_wait(), DEFAULT_HOST_WAIT);
    assert!(projection.event_wait() < projection.maintenance_wait());
}
