use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::*;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const PATIENT: Duration = Duration::from_secs(30);

/// The queue directory every build that queues for `vault-write.lock` agrees on; spelt out here because other
/// processes, including other builds, depend on it.
fn queue_directory(state: &Path) -> PathBuf {
    state.join("vault-write.lock.queue")
}

/// The ticket files in the queue directory, sorted, or none when the directory does not exist.
fn tickets(directory: &Path) -> Result<Vec<String>, BoxError> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut names = Vec::new();
    for entry in entries {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.ends_with(".ticket") {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

/// Waits, within a generous bound, until the queue holds `count` tickets, so a test knows a waiter has joined it.
fn queue_reaches(directory: &Path, count: usize) -> Result<(), BoxError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if tickets(directory)?.len() == count {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("the queue never held {count} tickets: {:?}", tickets(directory)?).into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn a_second_holder_waits_for_the_host_lock_and_proceeds_once_it_is_released() -> Result<(), BoxError> {
    let state = tempfile::tempdir()?;
    let lock = state.path().join(HOST_WRITE_LOCK);
    let held = lock_host(&lock, PATIENT)?;
    let finished = Arc::new(AtomicBool::new(false));

    let waiting = {
        let lock = lock.clone();
        let finished = Arc::clone(&finished);
        std::thread::spawn(move || {
            let taken = lock_host(&lock, PATIENT);
            finished.store(true, Ordering::SeqCst);
            taken.map(drop)
        })
    };
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !finished.load(Ordering::SeqCst),
        "the second holder took a lock the first still held"
    );

    drop(held);
    waiting.join().map_err(|_| "the waiter panicked")??;
    Ok(())
}

/// A child process started while the lock is held, between its `fork` and its `exec`, holds a duplicate of the lock
/// file's descriptor, which shares the lock. Dropping the holder's own handle must still release the lock at once.
#[test]
fn a_lock_is_released_when_its_holder_drops_it_even_while_a_duplicate_of_the_handle_is_alive() -> Result<(), BoxError> {
    let state = tempfile::tempdir()?;
    let held = lock_vault_writes(state.path(), Duration::ZERO)?;
    let inherited = held.try_clone()?;

    drop(held);

    let retaken = lock_vault_writes(state.path(), Duration::ZERO);
    assert!(
        retaken.is_ok(),
        "the lock stayed held by a duplicate of the dropped handle"
    );
    drop(inherited);
    Ok(())
}

#[test]
fn a_host_lock_held_beyond_the_wait_is_refused_as_busy() -> Result<(), BoxError> {
    let state = tempfile::tempdir()?;
    let lock = state.path().join(HOST_WRITE_LOCK);
    let _held = lock_host(&lock, PATIENT)?;

    let refused = lock_host(&lock, Duration::from_millis(50));
    let Err(error @ FsError::HostLockBusy { .. }) = refused else {
        return Err(format!("expected HostLockBusy, got {refused:?}").into());
    };
    assert_eq!(error.code(), "io/vault-busy");
    assert_eq!(error.path(), Some(lock.as_path()));
    Ok(())
}

#[test]
fn the_host_lock_file_and_its_directory_are_created_when_absent() -> Result<(), BoxError> {
    let state = tempfile::tempdir()?;
    let lock = state.path().join("not-yet").join(HOST_WRITE_LOCK);
    let _held = lock_host(&lock, PATIENT)?;
    assert!(lock.is_file());
    Ok(())
}

#[test]
fn a_lock_path_that_names_no_file_is_a_write_lock_failure() {
    let refused = lock_host(Path::new("/"), Duration::from_millis(50));
    assert!(
        matches!(&refused, Err(error @ FsError::HostLock { .. }) if error.code() == "io/write-lock"),
        "{refused:?}"
    );
}

#[test]
fn a_vault_write_lock_is_the_host_lock_file_in_its_state_directory() -> Result<(), BoxError> {
    let state = tempfile::tempdir()?;
    let held = lock_vault_writes(state.path(), PATIENT)?;
    assert!(state.path().join(HOST_WRITE_LOCK).is_file());

    let refused = lock_host(&state.path().join(HOST_WRITE_LOCK), Duration::from_millis(50));
    assert!(matches!(refused, Err(FsError::HostLockBusy { .. })), "{refused:?}");
    drop(held);
    Ok(())
}

#[test]
fn a_process_retaking_the_lock_in_a_tight_loop_cannot_starve_another_past_its_wait() -> Result<(), BoxError> {
    let state = tempfile::tempdir()?;
    let lock = state.path().join(HOST_WRITE_LOCK);
    let stop = Arc::new(AtomicBool::new(false));
    let writes = Arc::new(AtomicUsize::new(0));

    // The busy writer holds the lock for 250 ms at a time and asks again the moment it lets go, so the lock is free
    // only for the instant between one hold and the next: a caller that merely polls sees it free only by luck. A
    // queued caller is served at the first release.
    let busy_loop = {
        let lock = lock.clone();
        let stop = Arc::clone(&stop);
        let writes = Arc::clone(&writes);
        std::thread::spawn(move || {
            let started = Instant::now();
            while !stop.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(5) {
                let held = lock_host(&lock, PATIENT)?;
                writes.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(250));
                drop(held);
            }
            Ok::<(), FsError>(())
        })
    };
    while writes.load(Ordering::SeqCst) < 1 {
        std::thread::sleep(Duration::from_millis(1));
    }

    let asked = Instant::now();
    let served = lock_host(&lock, Duration::from_secs(2));
    let waited = asked.elapsed();
    stop.store(true, Ordering::SeqCst);
    drop(served?);
    busy_loop.join().map_err(|_| "the busy writer panicked")??;
    assert!(
        waited < Duration::from_secs(1),
        "the waiter was served only after {waited:?}, behind a writer that kept retaking the lock"
    );
    Ok(())
}

#[test]
fn waiting_processes_are_served_in_the_order_they_asked() -> Result<(), BoxError> {
    let state = tempfile::tempdir()?;
    let lock = state.path().join(HOST_WRITE_LOCK);
    let queue = queue_directory(state.path());
    let held = lock_host(&lock, PATIENT)?;
    let served = Arc::new(Mutex::new(Vec::new()));

    let mut waiters = Vec::new();
    for arrival in 0..4_usize {
        let lock = lock.clone();
        let served = Arc::clone(&served);
        waiters.push(std::thread::spawn(move || {
            let guard = lock_host(&lock, Duration::from_secs(10))?;
            if let Ok(mut order) = served.lock() {
                order.push(arrival);
            }
            std::thread::sleep(Duration::from_millis(5));
            drop(guard);
            Ok::<(), FsError>(())
        }));
        queue_reaches(&queue, arrival + 1)?;
    }

    drop(held);
    for waiter in waiters {
        waiter.join().map_err(|_| "a waiter panicked")??;
    }
    let order = served.lock().map_err(|error| error.to_string())?.clone();
    assert_eq!(order, vec![0, 1, 2, 3]);
    assert_eq!(tickets(&queue)?, Vec::<String>::new());
    Ok(())
}

#[test]
fn a_ticket_left_by_a_process_that_died_waiting_is_reaped_and_does_not_block_the_queue() -> Result<(), BoxError> {
    let state = tempfile::tempdir()?;
    let lock = state.path().join(HOST_WRITE_LOCK);
    let queue = queue_directory(state.path());
    std::fs::create_dir_all(&queue)?;
    // Nothing holds this ticket's lock, which is exactly what a process that crashed while waiting leaves behind.
    let abandoned = queue.join("00000000000000000007.ticket");
    std::fs::write(&abandoned, b"")?;

    let _held = lock_host(&lock, Duration::from_secs(2))?;
    assert!(!abandoned.exists(), "the abandoned ticket was left in the queue");
    assert_eq!(tickets(&queue)?, Vec::<String>::new());
    Ok(())
}

#[test]
fn a_waiter_that_gives_up_leaves_the_queue_and_is_refused_as_busy() -> Result<(), BoxError> {
    let state = tempfile::tempdir()?;
    let lock = state.path().join(HOST_WRITE_LOCK);
    let queue = queue_directory(state.path());
    let _held = lock_host(&lock, PATIENT)?;

    let impatient = {
        let lock = lock.clone();
        std::thread::spawn(move || lock_host(&lock, Duration::from_millis(300)).map(drop))
    };
    queue_reaches(&queue, 1)?;
    let refused = impatient.join().map_err(|_| "the waiter panicked")?;

    let Err(error @ FsError::HostLockBusy { .. }) = refused else {
        return Err(format!("expected HostLockBusy, got {refused:?}").into());
    };
    assert_eq!(error.code(), "io/vault-busy");
    assert_eq!(error.path(), Some(lock.as_path()));
    assert_eq!(tickets(&queue)?, Vec::<String>::new());
    Ok(())
}
