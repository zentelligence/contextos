use contextos_core::{OperationService, OperationWarning};
use time::macros::datetime;

use super::*;
use crate::Severity;

fn warn(codes: &[&str]) -> Vec<OperationWarning> {
    codes
        .iter()
        .map(|code| OperationWarning {
            code: (*code).to_owned(),
            message: "the message text must never reach an event".to_owned(),
        })
        .collect()
}

const ALL: [OperationService; 5] = [
    OperationService::Index,
    OperationService::OperationLog,
    OperationService::Git,
    OperationService::Search,
    OperationService::Projection,
];

fn degraded(code: &str, service: &str) -> HealthChange {
    HealthChange::Degraded {
        code: code.to_owned(),
        service: service.to_owned(),
    }
}

fn recovered(code: &str, service: &str) -> HealthChange {
    HealthChange::Recovered {
        code: code.to_owned(),
        service: service.to_owned(),
    }
}

#[test]
fn the_first_failure_of_a_code_degrades_the_vault_once() {
    let mut current = CurrentFailures::default();

    assert_eq!(
        current.observe(&warn(&["git/commit"]), &ALL),
        [degraded("git/commit", "git")]
    );
    assert!(
        current.observe(&warn(&["git/commit"]), &ALL).is_empty(),
        "already current"
    );
    assert!(current.contains("git/commit"));
}

#[test]
fn a_notice_fires_nothing_and_is_never_current() {
    let mut current = CurrentFailures::default();

    let changes = current.observe(
        &warn(&["write/line-endings-converted", "projection/hand-edited-replaced"]),
        &ALL,
    );

    assert!(changes.is_empty() && current.is_empty());
}

#[test]
fn a_clean_write_from_the_service_that_failed_recovers_the_vault() {
    let mut current = CurrentFailures::default();
    let _ = current.observe(&warn(&["git/commit"]), &ALL);

    assert_eq!(current.observe(&[], &ALL), [recovered("git/commit", "git")]);
    assert!(current.is_empty());
    assert!(current.observe(&[], &ALL).is_empty(), "recovery fires once");
}

#[test]
fn a_service_the_write_did_not_reach_neither_recovers_nor_is_judged() {
    let mut current = CurrentFailures::default();
    let _ = current.observe(&warn(&["projection/write-failed", "git/stage"]), &ALL);

    let without_projection = [OperationService::Git, OperationService::Search];
    let changes = current.observe(&[], &without_projection);

    assert_eq!(changes, [recovered("git/stage", "git")]);
    assert!(current.contains("projection/write-failed"));
}

#[test]
fn the_events_service_always_counts_as_having_applied() {
    let mut current = CurrentFailures::default();
    let _ = current.observe(&warn(&["events/write-failed"]), &[]);

    assert_eq!(current.observe(&[], &[]), [recovered("events/write-failed", "events")]);
}

#[test]
fn a_code_still_failing_stays_current_and_other_codes_change_independently() {
    let mut current = CurrentFailures::default();
    let _ = current.observe(&warn(&["git/commit", "index/storage"]), &ALL);

    let changes = current.observe(&warn(&["index/storage", "vector/storage", "vector/storage"]), &ALL);

    assert_eq!(
        changes,
        [degraded("vector/storage", "vector"), recovered("git/commit", "git")]
    );
    assert!(current.contains("index/storage") && current.contains("vector/storage"));
}

#[test]
fn an_unclassified_code_is_treated_as_a_failure_so_it_is_never_hidden() {
    let mut current = CurrentFailures::default();

    assert_eq!(
        current.observe(&warn(&["git/brand-new"]), &ALL),
        [degraded("git/brand-new", "git")]
    );
}

#[test]
fn health_events_carry_the_code_and_service_but_never_the_message() {
    let at = datetime!(2026-10-28 03:15:42 UTC);

    let down = EventRecord::from((&degraded("git/commit", "git"), at));
    let up = EventRecord::from((&recovered("git/commit", "git"), at));

    assert_eq!(
        down.to_string(),
        "---\ntype: event\nkind: vault.degraded\nseverity: warning\nat: 2026-10-28T03:15:42Z\nentity: 'vault'\n\
         code: 'git/commit'\nservice: 'git'\n---\n\n# Vault degraded: git/commit\n"
    );
    assert_eq!((up.kind.as_str(), up.severity), ("vault.recovered", Severity::Info));
    assert_eq!(up.heading, "Vault recovered: git/commit");
    assert!(down.subject.is_none() && down.actor.is_none());
    assert!(down.relative_path(0).ends_with("-vault-degraded.md"));
}

#[test]
fn the_current_failures_survive_a_restart() -> Result<(), EventsError> {
    let directory = tempfile::tempdir().map_err(|error| EventsError::TableUnwritable {
        reason: error.to_string(),
    })?;
    let path = directory.path().join("health.json");
    let mut current = CurrentFailures::default();
    let _ = current.observe(&warn(&["git/commit"]), &ALL);

    current.store(&path)?;

    assert_eq!(CurrentFailures::load(&path)?, Some(current));
    assert_eq!(CurrentFailures::load(&directory.path().join("absent.json"))?, None);
    Ok(())
}

#[test]
fn an_unreadable_health_file_is_reported() {
    let Ok(directory) = tempfile::tempdir() else { return };
    let path = directory.path().join("health.json");
    assert!(std::fs::write(&path, "[1,2").is_ok());

    assert_eq!(
        CurrentFailures::load(&path).err().map(|error| error.code()),
        Some("events/table-unreadable")
    );
}
