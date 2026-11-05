use std::path::PathBuf;
use std::time::Duration;

use contextos_core::VaultRootId;

use contextos_fs::HOST_WRITE_LOCK;

use super::{WriteCoordinator, WriteCoordinatorConfig};
use crate::tool_error::{ToolError, ToolFailure};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

fn coordinator(lock: PathBuf, host_wait: Duration) -> WriteCoordinator {
    WriteCoordinator::from(WriteCoordinatorConfig {
        host_locks: vec![lock],
        host_wait,
    })
}

#[tokio::test]
async fn concurrent_mutations_for_one_root_in_one_process_are_serialised() -> Result<(), BoxError> {
    let state = tempfile::tempdir()?;
    let coordinator = coordinator(state.path().join(HOST_WRITE_LOCK), Duration::from_secs(30));
    let root = VaultRootId::try_from(0_usize)?;
    let first = coordinator.lock_roots(&[root]).await?;
    let competitor = coordinator.clone();
    let waiting = tokio::spawn(async move { competitor.lock_roots(&[root]).await });

    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());

    drop(first);
    let second = waiting.await??;
    assert_eq!(second.len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_second_process_waits_for_the_host_lock_and_proceeds_once_it_is_released() -> Result<(), BoxError> {
    let state = tempfile::tempdir()?;
    let lock = state.path().join(HOST_WRITE_LOCK);
    let root = VaultRootId::try_from(0_usize)?;
    // Two coordinators share nothing in memory, exactly as two processes do: only the lock file joins them.
    let first_process = coordinator(lock.clone(), Duration::from_secs(30));
    let second_process = coordinator(lock, Duration::from_secs(30));
    let held = first_process.lock_roots(&[root]).await?;

    let waiting = tokio::spawn(async move { second_process.lock_roots(&[root]).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !waiting.is_finished(),
        "the second process took a lock the first still held"
    );

    drop(held);
    assert_eq!(waiting.await??.len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_host_lock_held_beyond_the_wait_is_refused_as_busy() -> Result<(), BoxError> {
    let state = tempfile::tempdir()?;
    let lock = state.path().join(HOST_WRITE_LOCK);
    let root = VaultRootId::try_from(0_usize)?;
    let first_process = coordinator(lock.clone(), Duration::from_secs(30));
    let impatient = coordinator(lock.clone(), Duration::from_millis(50));
    let _held = first_process.lock_roots(&[root]).await?;

    let refused = impatient.lock_roots(&[root]).await;
    let Err(error @ ToolError::VaultBusy { .. }) = refused else {
        return Err(format!("expected VaultBusy, got {refused:?}").into());
    };
    let failure = ToolFailure::from(error);
    assert_eq!(failure.code, "io/vault-busy");
    assert_eq!(failure.path, Some(lock));
    Ok(())
}

#[tokio::test]
async fn the_host_lock_file_and_its_directory_are_created_when_absent() -> Result<(), BoxError> {
    let state = tempfile::tempdir()?;
    let lock = state.path().join("not-yet").join(HOST_WRITE_LOCK);
    let root = VaultRootId::try_from(0_usize)?;
    let guards = coordinator(lock.clone(), Duration::from_secs(30))
        .lock_roots(&[root])
        .await?;
    assert_eq!(guards.len(), 1);
    assert!(lock.is_file());
    Ok(())
}

#[tokio::test]
async fn a_root_the_coordinator_was_not_configured_with_is_refused() -> Result<(), BoxError> {
    let state = tempfile::tempdir()?;
    let unknown = VaultRootId::try_from(3_usize)?;
    let result = coordinator(state.path().join(HOST_WRITE_LOCK), Duration::from_secs(30))
        .lock_roots(&[unknown])
        .await;
    assert!(
        matches!(result, Err(ToolError::RootLockMissing { root_index: 3 })),
        "{result:?}"
    );
    Ok(())
}
