use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn a_write_lock_held_past_the_wait_fails_as_busy() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("store.lock");
    let _held = StoreWriteLock::acquire(DerivedStore::Vectors, &path, Duration::from_secs(5))?;

    let refused = StoreWriteLock::acquire(DerivedStore::Vectors, &path, Duration::from_millis(50))
        .err()
        .ok_or("a second holder must not take a lock that is still held")?;

    assert_eq!(refused.code(), "index/busy", "{refused}");
    assert_eq!(refused.store(), DerivedStore::Vectors);
    Ok(())
}

#[test]
fn a_waiting_writer_takes_the_lock_once_it_is_released() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("store.lock");
    let held = StoreWriteLock::acquire(DerivedStore::Graph, &path, Duration::from_secs(5))?;
    let waiter_path = path.clone();
    let waiter =
        std::thread::spawn(move || StoreWriteLock::acquire(DerivedStore::Graph, &waiter_path, Duration::from_secs(5)));

    std::thread::sleep(Duration::from_millis(50));
    assert!(!waiter.is_finished(), "the waiter took a lock that was still held");
    drop(held);

    let taken = waiter.join().map_err(|_| "the waiting thread panicked")?;
    assert!(taken.is_ok(), "{taken:?}");
    Ok(())
}

#[test]
fn the_lock_file_and_its_directory_are_created_when_absent() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("not-yet").join("store.lock");
    let _held = StoreWriteLock::acquire(DerivedStore::Text, &path, Duration::from_secs(1))?;
    assert!(path.is_file());
    Ok(())
}

#[test]
fn the_lock_beside_a_store_file_appends_lock_to_its_name() {
    assert_eq!(
        lock_path_beside(Path::new("/state/vectors.db")),
        Path::new("/state/vectors.db.lock")
    );
}
