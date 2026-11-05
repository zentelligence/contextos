//! Showing, replacing, and listing shared jobs, and the wait for the state lock, apart from the creation behaviours in
//! `shared_job_authoring_test.rs`.

use std::fs;
use std::sync::mpsc;
use std::thread;

use contextos_workflows::{Actor, JobSchedule, ReleaseKind, Transition, TransitionError};

use super::tests::{Host, NOW, TestResult, VALID, hash_of, key, key_of, refusal, version};
use super::*;

#[test]
fn show_returns_the_content_and_the_hash_contextos_mcp_would_report_for_it() -> TestResult {
    let host = Host::new()?;
    let bytes = b"# kept\r\nname = \"Nightly\"\r\ntimeout_secs = 120\r\nversion = \"1.0.0\"\r\n";
    host.place("nightly", bytes)?;

    let shown = host.authoring().show(&key())?;

    assert_eq!(shown.content.as_bytes(), bytes);
    assert_eq!(shown.hash, hash_of(bytes));
    assert_eq!(<&str>::from(&shown.hash).len(), 64);
    Ok(())
}

#[test]
fn show_finds_a_job_by_the_slug_its_file_names() -> TestResult {
    let host = Host::new()?;
    host.place(
        "renamed-file",
        b"name = \"N\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nslug = \"wanted\"\n",
    )?;

    let shown = host.authoring().show(&key_of("acme", "sync", "wanted"))?;

    assert_eq!(
        shown.content,
        "name = \"N\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nslug = \"wanted\"\n"
    );
    Ok(())
}

#[test]
fn show_of_a_key_with_no_file_is_job_not_found_and_names_the_key_not_the_path() -> TestResult {
    let host = Host::new()?;

    let error = refusal(host.authoring().show(&key()))?;

    assert_eq!(error.code(), "workflow/job-not-found");
    let message = error.to_string();
    assert!(message.contains("nightly"), "{message}");
    assert!(
        !message.contains(&host.root.path().to_string_lossy().into_owned()),
        "{message}"
    );
    Ok(())
}

#[test]
fn replace_with_the_current_hash_stores_the_new_bytes() -> TestResult {
    let host = Host::new()?;
    host.place("nightly", VALID)?;
    let replacement = b"# edited\nname = \"Nightly 2\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n";

    let replaced = host.authoring().replace(&key(), replacement, &hash_of(VALID))?;

    assert_eq!(replaced.outcome, AuthoredOutcome::Replaced);
    assert_eq!(replaced.hash, hash_of(replacement));
    assert_eq!(fs::read(host.job_file("nightly"))?, replacement);
    Ok(())
}

#[test]
fn replace_with_a_stale_hash_is_refused_with_job_changed_giving_the_current_hash_and_writes_nothing() -> TestResult {
    let host = Host::new()?;
    host.place("nightly", VALID)?;

    let error = refusal(host.authoring().replace(
        &key(),
        b"name = \"Other\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n",
        &hash_of(b"name = \"Stale\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n"),
    ))?;

    assert_eq!(error.code(), "workflow/job-changed");
    assert!(error.to_string().contains(<&str>::from(&hash_of(VALID))), "{error}");
    assert_eq!(fs::read(host.job_file("nightly"))?, VALID);
    Ok(())
}

#[test]
fn replace_with_the_same_bytes_reports_unchanged() -> TestResult {
    let host = Host::new()?;
    host.place("nightly", VALID)?;

    let replaced = host.authoring().replace(&key(), VALID, &hash_of(VALID))?;

    assert_eq!(replaced.outcome, AuthoredOutcome::Unchanged);
    assert_eq!(fs::read(host.job_file("nightly"))?, VALID);
    Ok(())
}

#[test]
fn replace_of_a_key_with_no_file_is_job_not_found() -> TestResult {
    let host = Host::new()?;

    let error = refusal(host.authoring().replace(&key(), VALID, &hash_of(VALID)))?;

    assert_eq!(error.code(), "workflow/job-not-found");
    assert!(!host.jobs_root().exists());
    Ok(())
}

#[test]
fn replace_writes_the_file_the_job_is_tracked_under_when_its_name_differs() -> TestResult {
    let host = Host::new()?;
    let original = b"name = \"N\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nslug = \"wanted\"\n";
    host.place("renamed-file", original)?;
    let replacement = b"name = \"N2\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nslug = \"wanted\"\n";

    let replaced = host
        .authoring()
        .replace(&key_of("acme", "sync", "wanted"), replacement, &hash_of(original))?;

    assert_eq!(replaced.outcome, AuthoredOutcome::Replaced);
    assert_eq!(fs::read(host.job_file("renamed-file"))?, replacement);
    assert!(!host.job_file("wanted").exists());
    Ok(())
}

#[test]
fn replace_validates_the_new_bytes_as_create_does_and_keeps_the_old_file_on_refusal() -> TestResult {
    let host = Host::new()?;
    host.place("nightly", VALID)?;
    let current = hash_of(VALID);

    let invalid = refusal(host.authoring().replace(
        &key(),
        b"name = \"N\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nowner = \"x\"\n",
        &current,
    ))?;
    let scheduled = refusal(host.authoring().replace(
        &key(),
        b"name = \"N\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nschedule = \"0 3 * * *\"\n",
        &current,
    ))?;
    let mismatch = refusal(host.authoring().replace(
        &key(),
        b"name = \"N\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nslug = \"other\"\n",
        &current,
    ))?;

    assert_eq!(invalid.code(), "workflow/job-file-invalid");
    assert_eq!(scheduled.cause(), Some("validation/artefact-invalid"));
    assert_eq!(mismatch.code(), "workflow/job-key-mismatch");
    assert_eq!(fs::read(host.job_file("nightly"))?, VALID);
    Ok(())
}

#[test]
fn list_shows_every_file_and_every_state_entry_once_with_state_schedule_hash_and_problems() -> TestResult {
    let host = Host::new()?;
    let good = b"name = \"Good\"\ntimeout_secs = 120\nready = true\nversion = \"1.2.0\"\n";
    host.place("good", good)?;
    host.place(
        "broken",
        b"name = \"B\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nowner = \"x\"\n",
    )?;
    host.place(
        "twin-a",
        b"name = \"A\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nslug = \"twin\"\n",
    )?;
    host.place(
        "twin-b",
        b"name = \"B\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nslug = \"twin\"\n",
    )?;
    host.validated("ghost")?;
    let schedule = JobSchedule::try_from("0 3 * * *")?;
    host.authoring().state.update(|jobs| -> Result<(), TransitionError> {
        let v = version();
        jobs.transition(
            "acme",
            "sync",
            "good",
            Actor::Validator,
            &Transition::Validated(v.clone()),
            NOW,
        )?;
        jobs.transition(
            "acme",
            "sync",
            "good",
            Actor::DryRun,
            &Transition::DryRunCompleted(v.clone()),
            NOW,
        )?;
        jobs.transition(
            "acme",
            "sync",
            "good",
            Actor::Operator,
            &Transition::Enable(schedule, v),
            NOW,
        )?;
        Ok(())
    })??;
    let state_before = fs::read(host.authoring().state.state_path())?;

    let rows = host.authoring().list()?;

    assert_eq!(rows.len(), 5, "{rows:#?}");
    let find = |job: &str, file: Option<&str>| {
        rows.iter()
            .filter(|row| row.key.job == job && row.file.as_deref() == file)
            .collect::<Vec<_>>()
    };
    let good_rows = find("good", None);
    assert_eq!(good_rows.len(), 1, "{rows:#?}");
    let good_row = good_rows[0];
    assert_eq!(good_row.name.as_deref(), Some("Good"));
    assert_eq!(good_row.lifecycle.as_str(), "enabled");
    assert_eq!(good_row.schedule.as_deref(), Some("0 3 * * *"));
    assert_eq!(good_row.ready, Some(true));
    assert_eq!(good_row.version, Some(Version::parse("1.2.0")?));
    assert_eq!(
        good_row.release_kind,
        Some(ReleaseKind::Future),
        "a workflow with no current release has only future ones"
    );
    assert_eq!(good_row.predecessor, None);
    assert_eq!(good_row.successor, None);
    assert_eq!(good_row.hash, Some(hash_of(good)));
    assert!(good_row.problems.is_empty(), "{:?}", good_row.problems);

    let broken = find("broken", None);
    assert_eq!(broken.len(), 1, "{rows:#?}");
    assert_eq!(broken[0].name, None);
    assert_eq!(broken[0].lifecycle.as_str(), "draft");
    assert_eq!(broken[0].problems.len(), 1);
    assert_eq!(broken[0].problems[0].code, "validation/artefact-invalid");

    for stem in ["twin-a", "twin-b"] {
        let twin = find("twin", Some(stem));
        assert_eq!(twin.len(), 1, "{stem}: {rows:#?}");
        assert_eq!(twin[0].problems.len(), 1, "{stem}");
        assert_eq!(twin[0].problems[0].code, "workflow/job-key-taken");
    }

    let ghost = find("ghost", None);
    assert_eq!(ghost.len(), 1, "{rows:#?}");
    assert_eq!(ghost[0].hash, None);
    assert_eq!(ghost[0].lifecycle.as_str(), "validated");
    assert_eq!(ghost[0].problems.len(), 1);
    assert_eq!(ghost[0].problems[0].code, "workflow/job-not-found");

    assert_eq!(
        fs::read(host.authoring().state.state_path())?,
        state_before,
        "listing writes nothing"
    );
    Ok(())
}

#[test]
fn list_shows_a_successors_predecessor_and_the_predecessors_successor_until_the_successor_is_retired() -> TestResult {
    let host = Host::new()?;
    host.place("old", b"name = \"Old\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n")?;
    host.place(
        "new",
        b"name = \"New\"\ntimeout_secs = 120\nversion = \"2.0.0\"\npredecessor = \"old\"\n",
    )?;
    let row_of = |rows: &[JobListing], job: &str| rows.iter().find(|row| row.key.job == job).cloned();

    let rows = host.authoring().list()?;

    let old = row_of(&rows, "old").ok_or("no row for old")?;
    let new = row_of(&rows, "new").ok_or("no row for new")?;
    assert_eq!(old.successor.as_deref(), Some("new"));
    assert_eq!(old.predecessor, None);
    assert_eq!(new.predecessor.as_deref(), Some("old"));
    assert_eq!(new.successor, None);

    host.enabled("new")?;
    host.settle_at("retired", "new")?;
    let rows = host.authoring().list()?;
    let old = row_of(&rows, "old").ok_or("no row for old")?;
    assert_eq!(old.successor, None, "a retired successor is not a standing one");
    Ok(())
}

#[test]
fn list_of_a_host_with_no_jobs_folder_is_empty_and_creates_nothing() -> TestResult {
    let host = Host::new()?;

    let rows = host.authoring().list()?;

    assert!(rows.is_empty(), "{rows:?}");
    assert!(!host.jobs_root().exists());
    assert!(!host.state_directory().exists());
    Ok(())
}

/// Waits, without a clock, until another thread of this process is queued behind the held lock on `lock_path`, which
/// the kernel lists in `/proc/locks` as a waiter (`->`) on the lock file's inode. Elsewhere there is nothing to ask, so
/// the test skips the wait and proves less.
#[cfg(target_os = "linux")]
fn wait_until_blocked_on(lock_path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::MetadataExt;

    let inode = fs::metadata(lock_path)?.ino().to_string();
    for _ in 0..50_000_000_u32 {
        let locks = fs::read_to_string("/proc/locks")?;
        let waiting = locks.lines().any(|line| {
            line.contains("->")
                && line
                    .split_whitespace()
                    .any(|field| field.rsplit(':').next() == Some(inode.as_str()) && field.matches(':').count() == 2)
        });
        if waiting {
            return Ok(());
        }
        thread::yield_now();
    }
    Err("the creator never queued behind the lock".into())
}

#[test]
fn an_authoring_change_waits_for_the_state_lock_and_checks_the_state_as_it_is_then() -> TestResult {
    let host = Host::new()?;
    let authoring = host.authoring();
    let state = authoring.state.clone();
    #[cfg(target_os = "linux")]
    let lock_path = state.lock_path();
    let (held_tx, held_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holder = thread::spawn(move || {
        state.update(|jobs| {
            let _ = held_tx.send(());
            let _ = release_rx.recv();
            jobs.transition(
                "acme",
                "sync",
                "nightly",
                Actor::Validator,
                &Transition::Validated(version()),
                NOW,
            )
        })
    });
    held_rx.recv()?;
    let creator = thread::spawn(move || authoring.create(&key(), VALID));

    // The creator is queued behind the held lock before the lock is released, so what it checks is the state as the
    // holder leaves it, and nothing was written while it waited.
    #[cfg(target_os = "linux")]
    wait_until_blocked_on(&lock_path)?;
    assert!(
        !host.job_file("nightly").exists(),
        "nothing is written before the lock is free"
    );
    release_tx.send(())?;
    holder
        .join()
        .map_err(|_| "the lock holder panicked")??
        .map_err(|e| e.to_string())?;
    let result = creator.join().map_err(|_| "the creator panicked")?;

    let error = refusal(result)?;
    assert_eq!(error.code(), "workflow/job-state-remains");
    assert!(!host.job_file("nightly").exists());
    Ok(())
}
