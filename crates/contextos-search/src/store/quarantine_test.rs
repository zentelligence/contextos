use time::macros::datetime;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const AT: OffsetDateTime = datetime!(2026-10-19 02:44:19 UTC);

#[test]
fn a_damaged_store_is_moved_into_its_own_quarantine_directory_and_kept() -> TestResult {
    let state = tempfile::tempdir()?;
    let graph = state.path().join("graph");
    std::fs::create_dir_all(&graph)?;
    std::fs::write(graph.join("graph.sqlite3"), b"damaged")?;

    let recovery = quarantine(state.path(), DerivedStore::Graph, std::slice::from_ref(&graph), AT)?;

    assert_eq!(
        recovery.moved_to,
        state.path().join("quarantine").join("graph-20261019T024419Z")
    );
    assert_eq!(
        std::fs::read(recovery.moved_to.join("graph").join("graph.sqlite3"))?,
        b"damaged"
    );
    assert!(!graph.exists(), "the store's own location must be free to recreate it");
    Ok(())
}

#[test]
fn every_member_that_exists_is_moved_and_a_missing_one_is_skipped() -> TestResult {
    let state = tempfile::tempdir()?;
    let members = ["vectors.db", "vectors.db-wal", "vectors.db-shm"].map(|name| state.path().join(name));
    std::fs::write(&members[0], b"db")?;
    std::fs::write(&members[1], b"wal")?;

    let recovery = quarantine(state.path(), DerivedStore::Vectors, &members, AT)?;

    assert!(recovery.moved_to.join("vectors.db").is_file());
    assert!(recovery.moved_to.join("vectors.db-wal").is_file());
    assert!(!recovery.moved_to.join("vectors.db-shm").exists());
    assert!(members.iter().all(|member| !member.exists()));
    Ok(())
}

#[test]
fn a_second_quarantine_in_the_same_second_gets_its_own_directory() -> TestResult {
    let state = tempfile::tempdir()?;
    let index = state.path().join("index");
    std::fs::create_dir_all(&index)?;
    let first = quarantine(state.path(), DerivedStore::Text, std::slice::from_ref(&index), AT)?;
    std::fs::create_dir_all(&index)?;
    let second = quarantine(state.path(), DerivedStore::Text, std::slice::from_ref(&index), AT)?;

    assert_ne!(first.moved_to, second.moved_to);
    assert!(first.moved_to.join("index").is_dir() && second.moved_to.join("index").is_dir());
    Ok(())
}

#[test]
fn a_move_that_fails_part_way_puts_back_what_it_moved() -> TestResult {
    let state = tempfile::tempdir()?;
    // The first member is a directory moved in as `item`; the second, a file also named `item`, cannot be moved onto
    // it, so the move fails part-way.
    let first = state.path().join("nested").join("item");
    std::fs::create_dir_all(&first)?;
    std::fs::write(first.join("inside"), b"first")?;
    let second = state.path().join("item");
    std::fs::write(&second, b"second")?;

    let failed = quarantine(
        state.path(),
        DerivedStore::Vectors,
        &[first.clone(), second.clone()],
        AT,
    );

    assert!(failed.is_err(), "the move must fail on the colliding name");
    assert_eq!(
        std::fs::read(first.join("inside"))?,
        b"first",
        "the member already moved must be put back"
    );
    assert_eq!(std::fs::read(&second)?, b"second");
    Ok(())
}

#[test]
fn the_times_a_store_was_moved_aside_are_counted_per_store() -> TestResult {
    let state = tempfile::tempdir()?;
    assert_eq!(quarantined_count(state.path(), DerivedStore::Graph), 0);
    quarantine(state.path(), DerivedStore::Graph, &[], AT)?;
    quarantine(state.path(), DerivedStore::Graph, &[], AT)?;
    quarantine(state.path(), DerivedStore::Text, &[], AT)?;
    assert_eq!(quarantined_count(state.path(), DerivedStore::Graph), 2);
    assert_eq!(quarantined_count(state.path(), DerivedStore::Text), 1);
    Ok(())
}
