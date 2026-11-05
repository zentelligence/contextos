use std::fs::OpenOptions;

use super::*;
use crate::doctor::DoctorStatus;

fn inspection(store: DerivedStore, health: Option<StoreHealth>) -> StoreInspection {
    StoreInspection {
        store,
        location: "state".into(),
        health,
        records: 12,
    }
}

fn corrupt(store: DerivedStore) -> StoreHealth {
    StoreHealth::Unreadable(StoreFailure::corrupt(store, "state", "malformed"))
}

fn no_holders() -> Result<(tempfile::TempDir, HolderRegistry), std::io::Error> {
    let state = tempfile::tempdir()?;
    let registry = HolderRegistry::new(state.path(), DerivedStore::Graph);
    Ok((state, registry))
}

#[test]
fn a_store_never_built_passes_as_not_built() -> Result<(), Box<dyn std::error::Error>> {
    let (_state, holders) = no_holders()?;
    let check = store_check(
        DoctorKind::LinkGraphStore,
        &inspection(DerivedStore::Graph, None),
        &holders,
    );
    assert_eq!(check.status, DoctorStatus::Pass);
    assert_eq!(check.message, "not built yet");
    Ok(())
}

#[test]
fn a_readable_store_passes_naming_what_it_holds() -> Result<(), Box<dyn std::error::Error>> {
    let (_state, holders) = no_holders()?;
    let check = store_check(
        DoctorKind::TextIndexStore,
        &inspection(DerivedStore::Text, Some(StoreHealth::Ready)),
        &holders,
    );
    assert_eq!(check.status, DoctorStatus::Pass);
    assert!(check.message.contains("12 documents"), "{}", check.message);
    Ok(())
}

#[test]
fn a_damaged_store_no_other_instance_holds_is_a_failure_doctor_resolve_recovers()
-> Result<(), Box<dyn std::error::Error>> {
    let (_state, holders) = no_holders()?;
    let check = store_check(
        DoctorKind::LinkGraphStore,
        &inspection(DerivedStore::Graph, Some(corrupt(DerivedStore::Graph))),
        &holders,
    );
    assert_eq!(check.status, DoctorStatus::Fail);
    assert!(check.auto_fixable);
    assert_eq!(check.remediation, Some(Remediation::RecoverStore(DerivedStore::Graph)));
    assert!(check.message.contains("index/corrupt"), "{}", check.message);
    Ok(())
}

#[test]
fn a_damaged_store_another_instance_holds_names_it_and_is_not_auto_fixable() -> Result<(), Box<dyn std::error::Error>> {
    let (state, holders) = no_holders()?;
    let directory = state.path().join("holders").join("graph");
    std::fs::create_dir_all(&directory)?;
    let other = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory.join(format!("{}-0.holder", std::process::id().wrapping_add(1))))?;
    other.lock()?;

    let check = store_check(
        DoctorKind::LinkGraphStore,
        &inspection(DerivedStore::Graph, Some(corrupt(DerivedStore::Graph))),
        &holders,
    );

    assert_eq!(check.status, DoctorStatus::Fail);
    assert!(!check.auto_fixable);
    assert!(
        check.message.contains("open in 1 other contextos process"),
        "{}",
        check.message
    );
    assert!(
        check
            .action
            .as_deref()
            .is_some_and(|action| action.contains("Stop the other contextos processes")),
        "{:?}",
        check.action
    );
    Ok(())
}

#[test]
fn a_busy_store_passes_as_not_checked() -> Result<(), Box<dyn std::error::Error>> {
    let (_state, holders) = no_holders()?;
    let busy = Some(StoreHealth::Unreadable(StoreFailure::busy(
        DerivedStore::Vectors,
        "state",
        "writing",
    )));
    let check = store_check(
        DoctorKind::VectorStore,
        &inspection(DerivedStore::Vectors, busy),
        &holders,
    );
    assert_eq!(check.status, DoctorStatus::Pass);
    assert!(check.message.starts_with("not checked"), "{}", check.message);
    Ok(())
}

#[test]
fn a_graph_never_rebuilt_passes_noting_how_to_complete_it() -> Result<(), Box<dyn std::error::Error>> {
    let (_state, holders) = no_holders()?;
    let check = store_check(
        DoctorKind::LinkGraphStore,
        &inspection(DerivedStore::Graph, Some(StoreHealth::Empty)),
        &holders,
    );
    assert_eq!(check.status, DoctorStatus::Pass);
    assert!(check.message.contains("query_index_rebuild"), "{}", check.message);
    Ok(())
}

#[test]
fn a_graph_written_by_another_version_is_a_failure_doctor_resolve_rebuilds() -> Result<(), Box<dyn std::error::Error>> {
    let (_state, holders) = no_holders()?;
    let check = store_check(
        DoctorKind::LinkGraphStore,
        &inspection(DerivedStore::Graph, Some(StoreHealth::Incompatible)),
        &holders,
    );
    assert_eq!(check.status, DoctorStatus::Fail);
    assert_eq!(check.remediation, Some(Remediation::RebuildStore(DerivedStore::Graph)));
    Ok(())
}

#[test]
fn a_store_that_cannot_be_read_from_disk_is_left_to_the_operator() -> Result<(), Box<dyn std::error::Error>> {
    let (_state, holders) = no_holders()?;
    let io = Some(StoreHealth::Unreadable(StoreFailure::io(
        DerivedStore::Text,
        "state",
        "permission denied",
    )));
    let check = store_check(
        DoctorKind::TextIndexStore,
        &inspection(DerivedStore::Text, io),
        &holders,
    );
    assert_eq!(check.status, DoctorStatus::Fail);
    assert!(!check.auto_fixable);
    Ok(())
}
