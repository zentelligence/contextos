use std::sync::{Arc, Barrier};
use std::thread;

use contextos_workflows::{Actor, JobFingerprint, Transition};
use time::macros::datetime;

use super::*;

const NOW: time::OffsetDateTime = datetime!(2026-10-04 03:00:00 UTC);

fn version() -> JobFingerprint {
    JobFingerprint::from(&contextos_workflows::ReleaseFingerprintInput {
        store: contextos_workflows::StoreKind::Host,
        package: "acme",
        release: "1.0.0",
        digest: "sha256:0",
        job_toml: "",
        release_manifest: "",
        libraries: &[],
    })
}

#[test]
fn a_store_with_no_file_reads_as_an_empty_state() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;

    let state = SharedStateStore::new(root.path().join("state")).read()?;

    assert_eq!(state, JobsState::default());
    Ok(())
}

#[test]
fn a_change_is_kept_and_read_back_by_another_handle() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let store = SharedStateStore::new(root.path().join("state"));

    store.update(|state| {
        state.transition(
            "acme",
            "sync",
            "nightly",
            Actor::Validator,
            &Transition::Validated(version()),
            NOW,
        )
    })??;

    let read = SharedStateStore::new(root.path().join("state")).read()?;
    assert_eq!(read.lifecycle_of("acme", "sync", "nightly").as_str(), "validated");
    Ok(())
}

#[test]
fn an_update_that_changes_nothing_writes_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let store = SharedStateStore::new(root.path().join("state"));

    let refused = store.update(|state| {
        state.transition(
            "acme",
            "sync",
            "nightly",
            Actor::Operator,
            &Transition::Resume(version()),
            NOW,
        )
    })?;

    assert!(refused.is_err());
    assert!(!store.state_path().exists(), "a refused change leaves no file");
    Ok(())
}

#[test]
fn a_malformed_state_file_is_refused_and_left_as_it_was() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let store = SharedStateStore::new(root.path().join("state"));
    std::fs::create_dir_all(root.path().join("state"))?;
    std::fs::write(store.state_path(), "packages = 7\n")?;

    assert!(matches!(store.read(), Err(SharedStateError::Malformed { .. })));
    let updated = store.update(|state| state.record_fire("acme", "sync", "nightly", NOW));
    assert!(matches!(updated, Err(SharedStateError::Malformed { .. })));
    assert_eq!(std::fs::read_to_string(store.state_path())?, "packages = 7\n");
    Ok(())
}

#[test]
fn simultaneous_updates_of_different_jobs_each_keep_their_own_change() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let store = Arc::new(SharedStateStore::new(root.path().join("state")));
    let writers = 12;
    let start = Arc::new(Barrier::new(writers));
    let handles: Vec<_> = (0..writers)
        .map(|index| {
            let store = Arc::clone(&store);
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                store.update(|state| state.record_fire("acme", "sync", &format!("job-{index}"), NOW))
            })
        })
        .collect();
    for handle in handles {
        handle.join().map_err(|_| "a writer panicked")??;
    }

    let state = store.read()?;
    for index in 0..writers {
        assert_eq!(
            state.last_scheduled_fire("acme", "sync", &format!("job-{index}")),
            Some(NOW),
            "job-{index} lost its change"
        );
    }
    Ok(())
}

#[test]
fn the_lock_is_a_file_beside_the_state_and_is_kept() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let store = SharedStateStore::new(root.path().join("state"));
    store.update(|state| state.record_fire("acme", "sync", "nightly", NOW))?;

    assert!(store.lock_path().exists());
    assert_eq!(store.lock_path().parent(), store.state_path().parent());
    Ok(())
}

#[test]
fn an_invalid_state_file_is_named_relative_to_the_app_data_folder() -> Result<(), Box<dyn std::error::Error>> {
    let app_data = tempfile::tempdir()?;
    let store = SharedStateStore::new(app_data.path().join("state"));
    std::fs::create_dir_all(app_data.path().join("state"))?;
    std::fs::write(store.state_path(), "packages = 7\n")?;

    let message = store
        .read()
        .err()
        .map(|error| error.to_string())
        .ok_or("a malformed state file was read")?;

    assert!(
        message.contains("state/shared-jobs.toml"),
        "the file is named relative to the app-data folder: {message}"
    );
    assert!(
        !message.contains(&*app_data.path().to_string_lossy()),
        "no absolute host path is printed: {message}"
    );
    Ok(())
}
