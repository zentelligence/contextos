//! Non-blocking advisory file locks.
//!
//! `std::fs::File::try_lock` and `try_lock_shared` report a lock another handle holds as
//! `TryLockError::WouldBlock` and a failed attempt as `TryLockError::Error`. Every caller here retries or reports
//! contention and propagates a failure, so these answer whether the lock was taken, keeping the two apart.

use std::fs::{File, TryLockError};
use std::io;

/// Tries to take an exclusive lock on `file` without waiting.
///
/// Returns `false` when another handle holds a lock on the file, shared or exclusive.
///
/// # Errors
///
/// Returns the I/O error when the attempt itself failed.
pub fn try_lock_exclusive(file: &File) -> io::Result<bool> {
    taken(file.try_lock())
}

/// Tries to take a shared lock on `file` without waiting.
///
/// Returns `false` when another handle holds an exclusive lock on the file.
///
/// # Errors
///
/// Returns the I/O error when the attempt itself failed.
pub fn try_lock_shared(file: &File) -> io::Result<bool> {
    taken(file.try_lock_shared())
}

fn taken(attempt: Result<(), TryLockError>) -> io::Result<bool> {
    match attempt {
        Ok(()) => Ok(true),
        Err(TryLockError::WouldBlock) => Ok(false),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

#[cfg(test)]
#[path = "file_lock_test.rs"]
mod tests;
