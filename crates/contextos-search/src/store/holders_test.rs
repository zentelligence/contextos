use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const WAIT: Duration = Duration::from_secs(5);

/// A holder registered by another process: a holder file named for a process id other than this one's, locked.
fn foreign_holder(registry: &HolderRegistry) -> Result<File, Box<dyn std::error::Error>> {
    std::fs::create_dir_all(&registry.directory)?;
    let other = std::process::id().wrapping_add(1);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(registry.directory.join(format!("{other}-0.holder")))?;
    file.lock()?;
    Ok(file)
}

#[test]
fn a_store_with_no_holders_counts_none() -> TestResult {
    let state = tempfile::tempdir()?;
    let registry = HolderRegistry::new(state.path(), DerivedStore::Graph);
    assert_eq!(registry.census(None, WAIT)?, HolderCensus::default());
    Ok(())
}

#[test]
fn holders_are_counted_by_process_and_the_callers_own_registration_is_left_out() -> TestResult {
    let state = tempfile::tempdir()?;
    let registry = HolderRegistry::new(state.path(), DerivedStore::Graph);
    let own = registry.register(WAIT)?;
    let _sibling = registry.register(WAIT)?;
    let _other = foreign_holder(&registry)?;

    let census = registry.census(Some(&own), WAIT)?;

    assert_eq!(
        census,
        HolderCensus {
            this_process: 1,
            other_processes: 1
        }
    );
    Ok(())
}

#[test]
fn a_dropped_registration_is_no_longer_a_holder() -> TestResult {
    let state = tempfile::tempdir()?;
    let registry = HolderRegistry::new(state.path(), DerivedStore::Vectors);
    drop(registry.register(WAIT)?);
    assert_eq!(registry.census(None, WAIT)?.total(), 0);
    Ok(())
}

#[test]
fn a_holder_whose_process_has_gone_is_removed_rather_than_counted() -> TestResult {
    let state = tempfile::tempdir()?;
    let registry = HolderRegistry::new(state.path(), DerivedStore::Text);
    let gone = foreign_holder(&registry)?;
    let path = registry
        .directory
        .join(format!("{}-0.holder", std::process::id().wrapping_add(1)));
    // The operating system releases a lock when its process exits; closing the file does the same here.
    drop(gone);

    assert_eq!(registry.census(None, WAIT)?.total(), 0);
    assert!(!path.exists(), "a gone holder's file must be removed");
    Ok(())
}

#[test]
fn work_that_needs_sole_use_runs_only_when_no_other_holder_is_live() -> TestResult {
    let state = tempfile::tempdir()?;
    let registry = HolderRegistry::new(state.path(), DerivedStore::Graph);
    let own = registry.register(WAIT)?;

    assert_eq!(registry.while_sole_holder(Some(&own), WAIT, || 7)?, Ok(7));

    let other = foreign_holder(&registry)?;
    let refused = registry.while_sole_holder(Some(&own), WAIT, || 7)?;
    assert_eq!(
        refused,
        Err(HolderCensus {
            this_process: 0,
            other_processes: 1
        })
    );
    drop(other);
    Ok(())
}

#[test]
fn only_holder_files_are_considered() -> TestResult {
    let state = tempfile::tempdir()?;
    let registry = HolderRegistry::new(state.path(), DerivedStore::Graph);
    std::fs::create_dir_all(&registry.directory)?;
    std::fs::write(registry.directory.join("notes.txt"), "not a holder")?;
    std::fs::write(registry.directory.join("12-x.holder"), "malformed name")?;

    assert_eq!(registry.census(None, WAIT)?.total(), 0);
    assert!(
        registry.directory.join("notes.txt").exists(),
        "a file that is not a holder must be left alone"
    );
    Ok(())
}

#[test]
fn observing_holders_counts_them_without_removing_a_gone_holders_file() -> TestResult {
    let state = tempfile::tempdir()?;
    let registry = HolderRegistry::new(state.path(), DerivedStore::Graph);
    let _own = registry.register(WAIT)?;
    let _other = foreign_holder(&registry)?;
    let gone_path = registry
        .directory
        .join(format!("{}-7.holder", std::process::id().wrapping_add(2)));
    std::fs::write(&gone_path, "")?;

    let census = registry.observe();

    assert_eq!(
        census,
        HolderCensus {
            this_process: 1,
            other_processes: 1
        }
    );
    assert!(gone_path.exists(), "observing must not remove anything");
    Ok(())
}

#[test]
fn observing_a_store_with_no_holder_directory_counts_none_and_creates_nothing() -> TestResult {
    let state = tempfile::tempdir()?;
    let registry = HolderRegistry::new(state.path(), DerivedStore::Text);
    assert_eq!(registry.observe(), HolderCensus::default());
    assert!(!state.path().join("holders").exists());
    Ok(())
}
