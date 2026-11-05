//! A run in one process is seen, and asked to stop, from another.
//!
//! This is a test target of its own, and holds no other claim, because starting a process forks the whole test binary and
//! the child briefly shares every lock file the binary has open until it execs.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use contextos_web::run_claim::{ActiveRun, CancelError, ClaimSlot, ClaimState, RunClaim, watch_for_cancel};
use tempfile::tempdir;
use time::macros::datetime;
use tokio_util::sync::CancellationToken;

const HOLDER_ROOT: &str = "CONTEXTOS_RUN_HOLDER_ROOT";
const HOLDER_RUN: u64 = 5;

fn slot(root: &Path) -> Result<ClaimSlot, Box<dyn std::error::Error>> {
    Ok(ClaimSlot::new(root.join("sync"), "nightly")?)
}

/// Not a test of its own: the process the tests below start. It holds the claim as a running job does, says which run it
/// is, and ends when it is asked to stop, and does nothing at all unless a test started it.
#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "the child process tells its parent test what it has reached through standard output, flushed explicitly"
)]
fn run_holder_for_the_process_tests() -> Result<(), Box<dyn std::error::Error>> {
    let Some(root) = std::env::var_os(HOLDER_ROOT) else {
        return Ok(());
    };
    let slot = slot(&PathBuf::from(root))?;
    let claim = RunClaim::acquire(&slot)?;
    claim.publish(&ActiveRun {
        run_id: HOLDER_RUN,
        started: datetime!(2026-10-05 03:00:00 UTC),
    })?;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build()?;
    runtime.block_on(async {
        let token = CancellationToken::new();
        let _watch = watch_for_cancel(slot, HOLDER_RUN, token.clone(), Duration::from_millis(20));
        println!("active");
        std::io::stdout().flush()?;
        // A bound that only turns a request that never arrives into a failure instead of a hang.
        tokio::time::timeout(Duration::from_secs(60), token.cancelled()).await?;
        println!("asked to stop");
        std::io::stdout().flush()?;
        Ok::<(), Box<dyn std::error::Error>>(())
    })?;
    drop(claim);
    Ok(())
}

/// The holder process and what it prints.
type Holder = (Child, std::io::Lines<BufReader<std::process::ChildStdout>>);

fn start_holder(root: &Path) -> Result<Holder, Box<dyn std::error::Error>> {
    let mut holder = Command::new(std::env::current_exe()?)
        .args(["--exact", "run_holder_for_the_process_tests", "--nocapture"])
        .env(HOLDER_ROOT, root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;
    let stdout = holder.stdout.take().ok_or("the holder has no output")?;
    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next() {
            Some(line) => {
                if line? == "active" {
                    return Ok((holder, lines));
                }
            }
            None => return Err("the holder ended before its run was active".into()),
        }
    }
}

#[test]
fn another_process_sees_the_run_a_process_holds_and_asks_it_to_stop() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    let (mut holder, mut lines) = start_holder(root.path())?;

    match slot.state()? {
        ClaimState::Held(Some(active)) => assert_eq!(active.run_id, HOLDER_RUN),
        other => return Err(format!("the run was not seen: {other:?}").into()),
    }
    assert!(
        matches!(slot.request_cancel(HOLDER_RUN + 1), Err(CancelError::NotActive { .. })),
        "a request names the run it is for"
    );
    slot.request_cancel(HOLDER_RUN)?;

    let told = lines.next().ok_or("the holder said nothing")??;
    assert_eq!(told, "asked to stop");
    assert!(holder.wait()?.success());
    assert_eq!(
        slot.state()?,
        ClaimState::Idle,
        "the claim is released when the run ends"
    );
    assert!(
        !slot.cancel_path().exists() && !slot.run_path().exists(),
        "what the run published and what asked it to stop are gone"
    );
    Ok(())
}

#[test]
fn a_run_whose_process_is_killed_is_no_longer_active_and_cannot_be_asked_to_stop()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    let (mut holder, _lines) = start_holder(root.path())?;
    assert!(matches!(slot.state()?, ClaimState::Held(Some(_))));

    holder.kill()?;
    holder.wait()?;

    assert!(
        slot.run_path().exists(),
        "a process that was killed could not remove what it published"
    );
    assert_eq!(
        slot.state()?,
        ClaimState::Idle,
        "but its lock is gone, so it is not reported"
    );
    assert!(matches!(
        slot.request_cancel(HOLDER_RUN),
        Err(CancelError::NotActive { .. })
    ));
    Ok(())
}
