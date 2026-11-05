//! A claim held by a process that is killed is taken by the next run.
//!
//! This is a test target of its own, and holds no other claim, because starting a process forks the whole test binary
//! and the child briefly shares every lock file the binary has open until it execs. A claim another test released a
//! moment earlier could then still look held, which is a property of the operating system's locks and not of the claim.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use contextos_web::shared_claim::{SharedClaim, SharedClaimError, SharedJobKey};
use tempfile::tempdir;

fn job() -> SharedJobKey {
    SharedJobKey {
        package: "acme".to_owned(),
        workflow: "sync".to_owned(),
        job: "nightly".to_owned(),
    }
}

/// Not a test of its own: the process the crash test below starts and then kills. It holds a claim until its input
/// closes, and does nothing at all unless that test started it.
#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "the child process tells its parent test it holds the claim through standard output, flushed explicitly"
)]
fn claim_holder_for_the_crash_test() -> Result<(), Box<dyn std::error::Error>> {
    let Some(root) = std::env::var_os("CONTEXTOS_CLAIM_HOLDER_ROOT") else {
        return Ok(());
    };
    let _claim = SharedClaim::acquire(std::path::Path::new(&root), &job())?;
    println!("held");
    std::io::stdout().flush()?;
    let mut ignored = String::new();
    std::io::stdin().read_line(&mut ignored)?;
    Ok(())
}

#[test]
fn a_claim_held_by_a_process_that_is_killed_is_taken_by_the_next_run() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let mut holder = Command::new(std::env::current_exe()?)
        .args(["--exact", "claim_holder_for_the_crash_test", "--nocapture"])
        .env("CONTEXTOS_CLAIM_HOLDER_ROOT", root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let stdout = holder.stdout.take().ok_or("the holder has no output")?;
    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next() {
            Some(line) => {
                if line? == "held" {
                    break;
                }
            }
            None => return Err("the holder ended before it held the claim".into()),
        }
    }

    assert!(
        matches!(
            SharedClaim::acquire(root.path(), &job()),
            Err(SharedClaimError::Busy { .. })
        ),
        "a live holder keeps the claim"
    );

    holder.kill()?;
    holder.wait()?;

    assert!(
        SharedClaim::acquire(root.path(), &job()).is_ok(),
        "a holder that died without releasing no longer holds the claim"
    );
    Ok(())
}
