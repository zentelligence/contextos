//! The doctor's derived store checks: one per enabled store (the text index, the link graph store, and the vector
//! store), reporting whether it opens, is intact, and is readable, without changing it.
//!
//! Each store is inspected read-only from the vault's state directory, the same place every `contextos` instance on
//! the host keeps it. A damaged store is auto-fixable only while no other `contextos` process holds it open, since only
//! then can `doctor_resolve` move it aside and rebuild it; otherwise the finding names how many do. The failure codes
//! and remediations are the ones `query_index_status` and `vault_info` report for the same store, so every surface
//! agrees about it.

use contextos_core::VaultRoot;
use contextos_search::{
    DEFAULT_WRITE_WAIT, DerivedStore, HolderRegistry, StoreFailure, StoreFailureKind, StoreHealth, StoreInspection,
    inspect_graph_store, inspect_text_index, inspect_vector_store,
};

use super::{DoctorCheck, DoctorError, Remediation};
use crate::VaultConfig;
use crate::doctor_kind::DoctorKind;

/// One check per derived store of `vault`, each passing as `disabled` when its store is not enabled.
pub(super) fn store_checks(vault: &VaultConfig, root: &VaultRoot) -> Result<Vec<DoctorCheck>, DoctorError> {
    let state = crate::state_dir::resolve_state_directory(vault.state_directory.as_deref(), root.path())?;
    let check = |enabled: bool, kind: DoctorKind, store: DerivedStore, inspect: &dyn Fn() -> StoreInspection| {
        if !enabled {
            return DoctorCheck::pass(kind, "disabled");
        }
        let holders = HolderRegistry::new(&state, store);
        // Registered as one of the store's holders while inspecting it, when it exists, so no instance moves it aside
        // while it is open here: closing the inspection's connection would otherwise remove the write-ahead log of the
        // fresh store that replaced it.
        let registered = if state.join(location_of(store)).exists() {
            match holders.register(DEFAULT_WRITE_WAIT) {
                Ok(holder) => Some(holder),
                Err(failure) => return DoctorCheck::pass(kind, format!("not checked: {failure} ({})", failure.code())),
            }
        } else {
            None
        };
        let inspection = inspect();
        drop(registered);
        store_check(kind, &inspection, &holders)
    };
    let backend = vault.search.graph_backend.into();
    Ok(vec![
        check(
            vault.search.text,
            DoctorKind::TextIndexStore,
            DerivedStore::Text,
            &|| inspect_text_index(&state.join("index")),
        ),
        check(
            vault.search.graph,
            DoctorKind::LinkGraphStore,
            DerivedStore::Graph,
            &|| inspect_graph_store(&state.join("graph"), backend),
        ),
        check(
            vault.search.semantic,
            DoctorKind::VectorStore,
            DerivedStore::Vectors,
            &|| inspect_vector_store(&state.join("vectors.db")),
        ),
    ])
}

/// Where `store` lives in a vault's state directory.
const fn location_of(store: DerivedStore) -> &'static str {
    match store {
        DerivedStore::Text => "index",
        DerivedStore::Graph => "graph",
        DerivedStore::Vectors => "vectors.db",
    }
}

/// What to report for one inspected store.
fn store_check(kind: DoctorKind, inspection: &StoreInspection, holders: &HolderRegistry) -> DoctorCheck {
    let store = inspection.store;
    let others = holders.observe().other_processes;
    let held = match others {
        0 => String::new(),
        1 => "; open in 1 other contextos process".to_owned(),
        count => format!("; open in {count} other contextos processes"),
    };
    let Some(health) = &inspection.health else {
        return DoctorCheck::pass(kind, format!("not built yet{held}"));
    };
    match health {
        StoreHealth::Ready => DoctorCheck::pass(
            kind,
            format!(
                "readable and intact, holding {} {}{held}",
                inspection.records,
                records_of(store)
            ),
        ),
        // A graph not yet rebuilt is intact and readable; it may hold only the notes written since it was created,
        // which `query_index_status` reports as `needs_rebuild`, so it is noted rather than failed.
        StoreHealth::Empty => DoctorCheck::pass(
            kind,
            format!(
                "readable and intact, holding {} notes, but not rebuilt from the vault since it was created; \
                 query_index_rebuild with index graph makes it the vault's whole graph{held}",
                inspection.records
            ),
        ),
        StoreHealth::Incompatible => DoctorCheck::fail_auto_fixable(
            kind,
            format!("was written by a build whose store format this one does not read{held}"),
            "Call doctor_resolve, or query_index_rebuild with index graph, to rewrite it in this build's format, \
             then run the doctor again.",
            Remediation::RebuildStore(store),
        ),
        StoreHealth::Unreadable(failure) => unreadable_check(kind, failure, others, &held),
    }
}

/// What to report for a store that cannot be read.
fn unreadable_check(kind: DoctorKind, failure: &StoreFailure, others: usize, held: &str) -> DoctorCheck {
    let store = failure.store();
    let message = format!("{} ({}){held}", failure, failure.code());
    match failure.kind() {
        // Contention clears by itself: another instance was writing, and its own status reports the store.
        StoreFailureKind::Busy => DoctorCheck::pass(kind, format!("not checked: {message}")),
        StoreFailureKind::Corrupt if others == 0 => DoctorCheck::fail_auto_fixable(
            kind,
            message,
            "Call doctor_resolve to move the damaged files aside (they are kept for diagnosis) and rebuild the store \
             from the vault, then run the doctor again.",
            Remediation::RecoverStore(store),
        ),
        StoreFailureKind::Corrupt => DoctorCheck::fail(
            kind,
            message,
            "Stop the other contextos processes using this vault (or wait until each finds the damage and releases \
             the store), then call doctor_resolve to move the damaged files aside and rebuild the store.",
        ),
        StoreFailureKind::Undecodable => DoctorCheck::fail_auto_fixable(
            kind,
            message,
            "Call doctor_resolve to rewrite the store from the vault, then run the doctor again.",
            if store == DerivedStore::Vectors {
                Remediation::RecoverStore(store)
            } else {
                Remediation::RebuildStore(store)
            },
        ),
        StoreFailureKind::Io => DoctorCheck::fail(kind, message, failure.remediation()),
    }
}

/// What a store's records are called in a report.
const fn records_of(store: DerivedStore) -> &'static str {
    match store {
        DerivedStore::Text => "documents",
        DerivedStore::Graph => "notes",
        DerivedStore::Vectors => "chunks",
    }
}

#[cfg(test)]
#[path = "doctor_stores_test.rs"]
mod tests;
