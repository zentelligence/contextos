use std::collections::BTreeSet;
use std::sync::Arc;

use tempfile::tempdir;
use time::macros::datetime;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn slot(root: &std::path::Path) -> Result<ClaimSlot, crate::run_claim::RunClaimError> {
    ClaimSlot::new(root.join("vaults").join("work").join("sync"), "nightly")
}

#[test]
fn the_first_run_number_follows_the_highest_record_the_vault_holds() -> TestResult {
    let root = tempdir()?;
    assert_eq!(slot(root.path())?.take_run_number(0)?, 1);

    let other = tempdir()?;
    assert_eq!(slot(other.path())?.take_run_number(41)?, 42);
    Ok(())
}

#[test]
fn run_numbers_advance_by_one_each_time() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;

    let taken = [
        slot.take_run_number(0)?,
        slot.take_run_number(0)?,
        slot.take_run_number(0)?,
    ];

    assert_eq!(taken, [1, 2, 3]);
    Ok(())
}

#[test]
fn a_counter_behind_the_records_is_lifted_past_them() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    slot.take_run_number(0)?;

    assert_eq!(slot.take_run_number(9)?, 10);
    assert_eq!(slot.take_run_number(0)?, 11);
    Ok(())
}

#[test]
fn a_counter_ahead_of_the_records_is_kept() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    for _ in 0..5 {
        slot.take_run_number(0)?;
    }

    assert_eq!(
        slot.take_run_number(2)?,
        6,
        "a number a crashed run took is never reused"
    );
    Ok(())
}

#[test]
fn a_counter_that_is_not_a_number_is_refused_and_left_alone() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    slot.take_run_number(0)?;
    std::fs::write(slot.sibling("next-run-id"), "forty")?;

    let refused = slot.take_run_number(0);

    assert!(matches!(refused, Err(BookkeepingError::Counter { .. })), "{refused:?}");
    assert_eq!(std::fs::read_to_string(slot.sibling("next-run-id"))?, "forty");
    Ok(())
}

#[test]
fn concurrent_runs_never_take_the_same_number() -> TestResult {
    let root = tempdir()?;
    let slot = Arc::new(slot(root.path())?);

    let handles: Vec<_> = (0..16)
        .map(|_| {
            let slot = Arc::clone(&slot);
            std::thread::spawn(move || slot.take_run_number(0))
        })
        .collect();
    let mut taken = BTreeSet::new();
    for handle in handles {
        let number = handle.join().map_err(|_| "a thread panicked")??;
        assert!(taken.insert(number), "{number} was taken twice");
    }

    assert_eq!(taken.len(), 16);
    Ok(())
}

#[test]
fn no_tick_has_fired_for_a_job_that_never_ran() -> TestResult {
    let root = tempdir()?;
    assert_eq!(slot(root.path())?.last_fire()?, None);
    Ok(())
}

#[test]
fn a_recorded_tick_is_read_back() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    let tick = datetime!(2026-10-05 03:00:00 UTC);

    assert!(slot.record_fire(tick)?);

    assert_eq!(slot.last_fire()?, Some(tick));
    Ok(())
}

#[test]
fn the_last_fire_only_moves_forward() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    let later = datetime!(2026-10-05 03:00:00 UTC);
    slot.record_fire(later)?;

    assert!(!slot.record_fire(later)?, "the same tick does not fire twice");
    assert!(
        !slot.record_fire(datetime!(2026-10-04 03:00:00 UTC))?,
        "an older tick is not recorded"
    );

    assert_eq!(slot.last_fire()?, Some(later));
    assert!(slot.record_fire(datetime!(2026-10-06 03:00:00 UTC))?);
    Ok(())
}

#[test]
fn only_one_of_two_processes_firing_the_same_tick_wins() -> TestResult {
    let root = tempdir()?;
    let slot = Arc::new(slot(root.path())?);
    let tick = datetime!(2026-10-05 03:00:00 UTC);

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let slot = Arc::clone(&slot);
            std::thread::spawn(move || slot.record_fire(tick))
        })
        .collect();
    let mut winners = 0;
    for handle in handles {
        if handle.join().map_err(|_| "a thread panicked")?? {
            winners += 1;
        }
    }

    assert_eq!(winners, 1);
    Ok(())
}

#[test]
fn a_last_fire_that_is_not_a_time_is_refused_and_left_alone() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    slot.record_fire(datetime!(2026-10-05 03:00:00 UTC))?;
    std::fs::write(slot.sibling("last-fire"), "yesterday")?;

    assert!(matches!(slot.last_fire(), Err(BookkeepingError::LastFire { .. })));
    assert!(matches!(
        slot.record_fire(datetime!(2026-10-06 03:00:00 UTC)),
        Err(BookkeepingError::LastFire { .. })
    ));
    assert_eq!(std::fs::read_to_string(slot.sibling("last-fire"))?, "yesterday");
    Ok(())
}

#[test]
fn bookkeeping_for_one_job_is_independent_of_another() -> TestResult {
    let root = tempdir()?;
    let first = slot(root.path())?;
    let second = ClaimSlot::new(root.path().join("vaults").join("work").join("sync"), "weekly")?;
    first.take_run_number(0)?;
    first.record_fire(datetime!(2026-10-05 03:00:00 UTC))?;

    assert_eq!(second.take_run_number(0)?, 1);
    assert_eq!(second.last_fire()?, None);
    Ok(())
}

#[test]
fn an_unreadable_counter_or_last_tick_names_the_job_file_and_never_the_host_folder_it_is_in() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    slot.take_run_number(0)?;
    slot.record_fire(datetime!(2026-10-05 03:00:00 UTC))?;
    std::fs::write(slot.sibling("next-run-id"), "forty")?;
    std::fs::write(slot.sibling("last-fire"), "yesterday")?;
    let folder = root.path().display().to_string();

    let counter = slot
        .take_run_number(0)
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    let last_fire = slot
        .last_fire()
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();

    for message in [&counter, &last_fire] {
        assert!(!message.is_empty(), "the corrupt file was not refused");
        assert!(!message.contains(&folder), "{message}");
    }
    assert!(counter.contains("sync/nightly.next-run-id"), "{counter}");
    assert!(last_fire.contains("sync/nightly.last-fire"), "{last_fire}");
    Ok(())
}

const MARK_FILE: &str = "run-failed-examined";

#[test]
fn no_run_has_been_examined_for_a_failure_task_in_a_job_that_never_ran() -> TestResult {
    let root = tempdir()?;
    assert_eq!(slot(root.path())?.run_failure_mark()?, None);
    Ok(())
}

#[test]
fn an_advanced_run_failure_mark_is_read_back() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;

    assert!(slot.advance_run_failure_mark(4)?);

    assert_eq!(slot.run_failure_mark()?, Some(4));
    assert_eq!(std::fs::read_to_string(slot.sibling(MARK_FILE))?.trim(), "4");
    Ok(())
}

#[test]
fn the_run_failure_mark_only_moves_forward() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    slot.advance_run_failure_mark(10)?;

    assert!(
        !slot.advance_run_failure_mark(10)?,
        "the same run is not examined twice"
    );
    assert!(
        !slot.advance_run_failure_mark(3)?,
        "a lower number does not move it back"
    );

    assert_eq!(slot.run_failure_mark()?, Some(10));
    assert!(slot.advance_run_failure_mark(11)?);
    assert_eq!(slot.run_failure_mark()?, Some(11));
    Ok(())
}

#[test]
fn concurrent_advances_of_the_run_failure_mark_leave_the_highest_whole() -> TestResult {
    let root = tempdir()?;
    let slot = Arc::new(slot(root.path())?);

    let handles: Vec<_> = (1..=16_u64)
        .map(|number| {
            let slot = Arc::clone(&slot);
            std::thread::spawn(move || slot.advance_run_failure_mark(number))
        })
        .collect();
    for handle in handles {
        handle.join().map_err(|_| "a thread panicked")??;
    }

    assert_eq!(slot.run_failure_mark()?, Some(16));
    assert_eq!(std::fs::read_to_string(slot.sibling(MARK_FILE))?, "16");
    Ok(())
}

#[test]
fn the_run_failure_mark_is_written_atomically_and_leaves_only_its_own_files_beside_the_claim() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    slot.advance_run_failure_mark(2)?;
    slot.advance_run_failure_mark(7)?;

    let mut names: Vec<String> = std::fs::read_dir(slot.directory())?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_, _>>()?;
    names.sort();

    assert_eq!(
        names,
        vec![
            "nightly.bookkeeping.lock".to_owned(),
            "nightly.run-failed-examined".to_owned()
        ],
        "no temporary file is left behind"
    );
    Ok(())
}

#[test]
fn a_run_failure_mark_that_is_not_a_number_is_refused_and_left_alone() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    slot.advance_run_failure_mark(2)?;
    std::fs::write(slot.sibling(MARK_FILE), "R-2")?;

    assert!(
        matches!(slot.run_failure_mark(), Err(BookkeepingError::RunFailureMark { .. })),
        "reading"
    );
    assert!(
        matches!(
            slot.advance_run_failure_mark(3),
            Err(BookkeepingError::RunFailureMark { .. })
        ),
        "advancing"
    );
    assert_eq!(std::fs::read_to_string(slot.sibling(MARK_FILE))?, "R-2");
    Ok(())
}

#[test]
fn an_unreadable_run_failure_mark_names_the_job_file_never_the_host_folder_and_echoes_at_most_32_bytes() -> TestResult {
    let root = tempdir()?;
    let slot = slot(root.path())?;
    slot.advance_run_failure_mark(2)?;
    std::fs::write(slot.sibling(MARK_FILE), "x".repeat(100))?;
    let folder = root.path().display().to_string();

    let message = slot
        .run_failure_mark()
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();

    assert!(!message.is_empty(), "the corrupt file was not refused");
    assert!(!message.contains(&folder), "{message}");
    assert!(message.contains("sync/nightly.run-failed-examined"), "{message}");
    assert!(message.contains(&"x".repeat(32)), "{message}");
    assert!(!message.contains(&"x".repeat(33)), "{message}");
    Ok(())
}

#[test]
fn the_run_failure_mark_of_one_job_is_independent_of_another_and_of_the_run_counter() -> TestResult {
    let root = tempdir()?;
    let first = slot(root.path())?;
    let second = ClaimSlot::new(root.path().join("vaults").join("work").join("sync"), "weekly")?;
    first.advance_run_failure_mark(5)?;

    assert_eq!(second.run_failure_mark()?, None);
    assert_eq!(first.peek_run_number()?, 1, "examining a run takes no run number");
    Ok(())
}
