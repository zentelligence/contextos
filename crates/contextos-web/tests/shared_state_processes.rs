//! The host state file changed by separate operating-system processes at once, as the Web scheduler and a command-line
//! process can: the advisory lock is between processes and not only between threads, so no process loses another's change.
//!
//! The test runs this same test binary as its own children, so it needs no helper binary: a child is this binary started
//! with the directory to write in its environment, and does nothing when that is not set.

use std::process::Command;

use contextos_web::shared_state::SharedStateStore;
use time::macros::datetime;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const DIRECTORY: &str = "SHARED_STATE_PROCESS_DIRECTORY";
const WRITER: &str = "SHARED_STATE_PROCESS_WRITER";
const WRITES: usize = 25;
const PROCESSES: usize = 4;

/// What each child does: records `WRITES` ticks for jobs of its own. Without the environment it does nothing, so it is
/// harmless in an ordinary run of the suite.
#[test]
fn a_child_process_writes_its_own_jobs() -> Result<(), BoxError> {
    let (Ok(directory), Ok(writer)) = (std::env::var(DIRECTORY), std::env::var(WRITER)) else {
        return Ok(());
    };
    let store = SharedStateStore::new(directory.into());
    for write in 0..WRITES {
        let job = format!("job-{writer}-{write}");
        store.update(|state| state.record_fire("acme", "sync", &job, datetime!(2026-10-05 03:00:00 UTC)))?;
    }
    Ok(())
}

#[test]
fn separate_processes_changing_the_state_at_once_each_keep_every_change() -> Result<(), BoxError> {
    let directory = tempfile::tempdir()?;
    let executable = std::env::current_exe()?;
    let mut children = Vec::new();
    for writer in 0..PROCESSES {
        children.push(
            Command::new(&executable)
                .args(["--exact", "a_child_process_writes_its_own_jobs", "--test-threads=1"])
                .env(DIRECTORY, directory.path())
                .env(WRITER, writer.to_string())
                .spawn()?,
        );
    }
    for mut child in children {
        assert!(child.wait()?.success());
    }

    let state = SharedStateStore::new(directory.path().to_path_buf()).read()?;
    for writer in 0..PROCESSES {
        for write in 0..WRITES {
            assert!(
                state
                    .last_scheduled_fire("acme", "sync", &format!("job-{writer}-{write}"))
                    .is_some(),
                "process {writer} lost its change {write}"
            );
        }
    }
    Ok(())
}
