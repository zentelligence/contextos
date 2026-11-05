use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn lock_file(directory: &tempfile::TempDir) -> std::io::Result<(File, File)> {
    let path = directory.path().join("lock");
    let first = File::create(&path)?;
    let second = File::open(&path)?;
    Ok((first, second))
}

#[test]
fn an_unheld_lock_is_taken_exclusively() -> TestResult {
    let directory = tempfile::tempdir()?;
    let (file, _) = lock_file(&directory)?;

    assert!(try_lock_exclusive(&file)?);
    Ok(())
}

#[test]
fn an_exclusive_lock_held_by_another_handle_is_contention_not_an_error() -> TestResult {
    let directory = tempfile::tempdir()?;
    let (holder, contender) = lock_file(&directory)?;
    holder.lock()?;

    assert!(!try_lock_exclusive(&contender)?);
    assert!(!try_lock_shared(&contender)?);
    Ok(())
}

#[test]
fn shared_locks_admit_each_other_and_exclude_an_exclusive_one() -> TestResult {
    let directory = tempfile::tempdir()?;
    let (first, second) = lock_file(&directory)?;

    assert!(try_lock_shared(&first)?);
    assert!(try_lock_shared(&second)?);
    second.unlock()?;
    assert!(!try_lock_exclusive(&second)?);
    Ok(())
}

#[test]
fn a_released_lock_can_be_taken_again() -> TestResult {
    let directory = tempfile::tempdir()?;
    let (holder, contender) = lock_file(&directory)?;
    holder.lock()?;
    holder.unlock()?;

    assert!(try_lock_exclusive(&contender)?);
    Ok(())
}

#[test]
fn a_failed_attempt_is_an_error_rather_than_contention() {
    let failure = taken(Err(TryLockError::Error(std::io::Error::other(
        "the handle is unusable",
    ))));

    assert!(failure.is_err_and(|error| error.to_string() == "the handle is unusable"));
}
