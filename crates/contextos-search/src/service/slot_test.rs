use std::fs::OpenOptions;
use std::sync::atomic::{AtomicBool, Ordering};

use contextos_core::SystemClock;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const WAIT: Duration = Duration::from_secs(5);

/// A slot over a store whose one member is the file `store.db`: opening fails, as damaged, while `damaged` is set, and
/// otherwise reads the file's content (or nothing, for a fresh store).
fn slot(state: &tempfile::TempDir, damaged: Arc<AtomicBool>) -> Result<Slot<String>, SearchError> {
    failing_slot(state, damaged, StoreFailureKind::Corrupt)
}

/// As [`slot`], but opening fails with a failure of `kind` while `failing` is set.
fn failing_slot(
    state: &tempfile::TempDir,
    failing: Arc<AtomicBool>,
    kind: StoreFailureKind,
) -> Result<Slot<String>, SearchError> {
    let member = state.path().join("store.db");
    let opened = member.clone();
    Slot::try_open(SlotConfig {
        store: DerivedStore::Vectors,
        state_directory: state.path().to_path_buf(),
        members: vec![member],
        wait: WAIT,
        open: Box::new(move || {
            if failing.load(Ordering::SeqCst) {
                return Err(StoreFailure::of(DerivedStore::Vectors, kind, "store.db", "cannot open").into());
            }
            Ok(std::fs::read_to_string(&opened).unwrap_or_default())
        }),
    })
}

fn census(state: &tempfile::TempDir) -> Result<usize, Box<dyn std::error::Error>> {
    Ok(HolderRegistry::new(state.path(), DerivedStore::Vectors)
        .census(None, WAIT)?
        .total())
}

#[test]
fn a_store_that_cannot_open_starts_closed_naming_why_and_opens_once_it_can() -> TestResult {
    let state = tempfile::tempdir()?;
    let failing = Arc::new(AtomicBool::new(true));
    let slot = failing_slot(&state, Arc::clone(&failing), StoreFailureKind::Io)?;

    let closed_by = slot.closed_by().ok_or("a store that cannot open must start closed")?;
    assert_eq!(closed_by.code(), "index/storage");
    assert_eq!(census(&state)?, 0, "a closed store must not count as held");

    failing.store(false, Ordering::SeqCst);
    slot.get()?;
    assert!(slot.closed_by().is_none());
    assert_eq!(census(&state)?, 1, "an open store is held");
    Ok(())
}

#[test]
fn a_store_found_damaged_when_it_opens_stays_closed_until_recovered() -> TestResult {
    let state = tempfile::tempdir()?;
    let damaged = Arc::new(AtomicBool::new(true));
    let slot = slot(&state, Arc::clone(&damaged))?;
    damaged.store(false, Ordering::SeqCst);

    assert_eq!(slot.get().err().map(|error| error.code()), Some("index/corrupt"));
    assert!(slot.needs_recovery());
    Ok(())
}

#[test]
fn a_store_found_damaged_is_released_and_no_longer_held() -> TestResult {
    let state = tempfile::tempdir()?;
    let slot = slot(&state, Arc::new(AtomicBool::new(false)))?;
    assert_eq!(census(&state)?, 1);

    slot.note(&StoreFailure::busy(DerivedStore::Vectors, "store.db", "contended").into());
    assert!(slot.closed_by().is_none(), "a busy store is not released");

    slot.note(&StoreFailure::corrupt(DerivedStore::Vectors, "store.db", "malformed").into());
    assert_eq!(slot.closed_by().map(|failure| failure.code()), Some("index/corrupt"));
    assert!(slot.needs_recovery());
    assert_eq!(census(&state)?, 0, "a released store must not count as held");
    Ok(())
}

#[test]
fn recovering_moves_the_damaged_store_aside_and_opens_a_fresh_one() -> TestResult {
    let state = tempfile::tempdir()?;
    std::fs::write(state.path().join("store.db"), "damaged content")?;
    let damaged = Arc::new(AtomicBool::new(true));
    let slot = slot(&state, Arc::clone(&damaged))?;
    // The damage is in the files moved aside: the fresh store opens.
    damaged.store(false, Ordering::SeqCst);

    let Recovered::MovedAside(recovery) = slot.recover(&SystemClock)? else {
        return Err("the damaged store must be moved aside".into());
    };

    assert_eq!(
        std::fs::read_to_string(recovery.moved_to.join("store.db"))?,
        "damaged content"
    );
    assert_eq!(slot.get()?.as_str(), "", "the fresh store holds nothing yet");
    assert_eq!(slot.last_recovery(), Some(recovery));
    assert_eq!(census(&state)?, 1, "the recovered store is held again");
    Ok(())
}

#[test]
fn recovering_is_refused_while_another_instance_holds_the_store_and_changes_nothing() -> TestResult {
    let state = tempfile::tempdir()?;
    std::fs::write(state.path().join("store.db"), "damaged content")?;
    let slot = slot(&state, Arc::new(AtomicBool::new(true)))?;
    let holders = state.path().join("holders").join("vectors");
    std::fs::create_dir_all(&holders)?;
    let other = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(holders.join(format!("{}-0.holder", std::process::id().wrapping_add(1))))?;
    other.lock()?;

    let refused = slot
        .recover(&SystemClock)
        .err()
        .ok_or("recovery must wait for the other holder")?;

    assert_eq!(refused.code(), "index/in-use", "{refused}");
    assert_eq!(
        std::fs::read_to_string(state.path().join("store.db"))?,
        "damaged content"
    );
    assert!(
        !state.path().join("quarantine").exists()
            || std::fs::read_dir(state.path().join("quarantine"))?.next().is_none()
    );
    assert!(slot.last_recovery().is_none());
    Ok(())
}

#[test]
fn a_text_index_failure_tantivy_reports_as_damage_is_classified_as_corrupt() {
    let error = SearchError::from(tantivy::TantivyError::DataCorruption(
        tantivy::error::DataCorruption::comment_only("bad footer"),
    ));
    let failure = classify(DerivedStore::Text, "index", &error);
    assert_eq!(failure.map(|failure| failure.code()), Some("index/corrupt"));
    assert!(classify(DerivedStore::Graph, "graph", &error).is_none());
}

#[test]
fn a_store_with_a_handle_still_in_use_in_this_process_is_not_moved_aside() -> TestResult {
    let state = tempfile::tempdir()?;
    std::fs::write(state.path().join("store.db"), "content")?;
    let slot = slot(&state, Arc::new(AtomicBool::new(false)))?;
    let in_flight = slot.get()?;
    slot.note(&StoreFailure::corrupt(DerivedStore::Vectors, "store.db", "malformed").into());

    let refused = slot
        .recover(&SystemClock)
        .err()
        .ok_or("a store this process is still using must not be moved")?;

    assert_eq!(refused.code(), "index/in-use", "{refused}");
    assert_eq!(std::fs::read_to_string(state.path().join("store.db"))?, "content");
    drop(in_flight);
    assert!(
        slot.recover(&SystemClock).is_ok(),
        "once the handle is released the store can be moved"
    );
    Ok(())
}

#[test]
fn a_store_closed_as_damaged_is_not_reopened_until_it_has_been_recovered() -> TestResult {
    let state = tempfile::tempdir()?;
    let slot = slot(&state, Arc::new(AtomicBool::new(false)))?;
    slot.note(&StoreFailure::corrupt(DerivedStore::Vectors, "store.db", "malformed").into());

    let refused = slot.get().err().ok_or("a damaged store must stay closed")?;
    assert_eq!(refused.code(), "index/corrupt", "{refused}");
    assert!(
        slot.needs_recovery(),
        "reading it again must not erase that it needs recovering"
    );
    Ok(())
}

#[test]
fn a_store_another_instance_has_recovered_is_reopened_rather_than_recovered_again() -> TestResult {
    let state = tempfile::tempdir()?;
    std::fs::write(state.path().join("store.db"), "damaged")?;
    let first = slot(&state, Arc::new(AtomicBool::new(false)))?;
    let second = slot(&state, Arc::new(AtomicBool::new(false)))?;
    for instance in [&first, &second] {
        instance.note(&StoreFailure::corrupt(DerivedStore::Vectors, "store.db", "malformed").into());
    }

    let moved = first.recover(&SystemClock)?;
    assert!(matches!(moved, Recovered::MovedAside(_)), "{moved:?}");
    let reopened = second.recover(&SystemClock)?;

    assert!(matches!(reopened, Recovered::AlreadyReplaced), "{reopened:?}");
    assert!(second.get().is_ok());
    assert_eq!(
        std::fs::read_dir(state.path().join("quarantine"))?.count(),
        1,
        "moved aside once"
    );
    Ok(())
}

#[test]
fn a_move_whose_fresh_store_then_fails_to_open_is_still_recorded() -> TestResult {
    let state = tempfile::tempdir()?;
    std::fs::write(state.path().join("store.db"), "damaged")?;
    let damaged = Arc::new(AtomicBool::new(true));
    let slot = slot(&state, Arc::clone(&damaged))?;

    let failed = slot.recover(&SystemClock);

    assert!(failed.is_err(), "the fresh store still fails to open here");
    let recorded = slot.last_recovery().ok_or("the move must be recorded")?;
    assert!(recorded.moved_to.join("store.db").is_file());
    Ok(())
}
