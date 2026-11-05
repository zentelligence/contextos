use serde_json::{Value, json};
use time::macros::datetime;

use super::*;

fn frontmatter(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn occurrence(path: &str, kind: &str, severity: Severity, at: OffsetDateTime) -> Occurrence {
    Occurrence {
        path: path.to_owned(),
        kind: kind.to_owned(),
        severity,
        at,
        subject: None,
        code: None,
        workflow: None,
        job: None,
    }
}

fn note(path: &str) -> SubjectIdentity {
    SubjectIdentity::Note { path: path.to_owned() }
}

fn record(base: &str, key: &str) -> SubjectIdentity {
    SubjectIdentity::Record {
        base: base.to_owned(),
        key: key.to_owned(),
    }
}

#[test]
fn a_later_event_about_the_same_record_clears_a_warning_wherever_the_record_was_filed() {
    let mut warning = occurrence(
        "a",
        "task.claim-lapsed",
        Severity::Warning,
        datetime!(2026-10-01 00:00:00 UTC),
    );
    warning.subject = Some(record("memory/tasks.base", "A-024"));
    let mut later = occurrence(
        "b",
        "task.state-changed",
        Severity::Info,
        datetime!(2026-10-02 00:00:00 UTC),
    );
    later.subject = Some(record("memory/tasks.base", "A-024"));

    assert!(!warning.is_open(&[later]));
}

#[test]
fn the_same_key_in_another_base_is_another_subject() {
    let mut warning = occurrence(
        "a",
        "task.claim-lapsed",
        Severity::Warning,
        datetime!(2026-10-01 00:00:00 UTC),
    );
    warning.subject = Some(record("memory/tasks.base", "D-021"));
    let mut later = occurrence(
        "b",
        "decision.reviewed",
        Severity::Info,
        datetime!(2026-10-02 00:00:00 UTC),
    );
    later.subject = Some(record("registry/decisions.base", "D-021"));

    assert!(warning.is_open(&[later]));
}

#[test]
fn moving_a_task_does_not_clear_its_warning() {
    let mut warning = occurrence(
        "a",
        "task.claim-lapsed",
        Severity::Warning,
        datetime!(2026-10-01 00:00:00 UTC),
    );
    warning.subject = Some(record("memory/tasks.base", "A-024"));
    let mut moved = occurrence("b", "task.moved", Severity::Info, datetime!(2026-10-02 00:00:00 UTC));
    moved.subject = warning.subject.clone();

    assert!(warning.is_open(&[moved]));
}

#[test]
fn a_record_is_identified_by_its_base_and_key_without_regard_to_case() {
    let map = frontmatter(json!({
        "kind": "task.claim-lapsed", "severity": "warning", "at": "2026-10-01T00:00:00Z",
        "subject": "[[a-024|A-024]]", "subject_id": "a-024",
        "subject_path": "memory/tasks/clients/a-024.md", "subject_base": "memory/tasks.base"
    }));

    let Ok(read) = Occurrence::try_from(("memory/events/e.md", &map)) else {
        unreachable!("the frontmatter is judgeable")
    };

    assert_eq!(read.subject, Some(record("memory/tasks.base", "A-024")));
}

#[test]
fn a_note_is_identified_by_its_recorded_path_and_else_by_its_link() {
    let with_path = frontmatter(json!({
        "kind": "member.state-changed", "severity": "warning", "at": "2026-10-01T00:00:00Z",
        "subject": "[[registry/agents/ellis|ellis]]", "subject_path": "registry/agents/ellis.md"
    }));
    let link_only = frontmatter(json!({
        "kind": "x.y", "severity": "warning", "at": "2026-10-01T00:00:00Z", "subject": "[[registry/agents/ellis|ellis]]"
    }));

    for map in [with_path, link_only] {
        let Ok(read) = Occurrence::try_from(("memory/events/e.md", &map)) else {
            unreachable!("the frontmatter is judgeable")
        };
        assert_eq!(read.subject, Some(note("registry/agents/ellis")));
    }
}

#[test]
fn a_later_event_about_the_same_subject_clears_a_warning() {
    let mut warning = occurrence("a", "task.stuck", Severity::Warning, datetime!(2026-10-01 00:00:00 UTC));
    warning.subject = Some(note("memory/tasks/t"));
    let mut later = occurrence("b", "task.done", Severity::Info, datetime!(2026-10-02 00:00:00 UTC));
    later.subject = Some(note("memory/tasks/t"));
    let mut earlier = later.clone();
    earlier.at = warning.at - time::Duration::days(1);

    assert!(!warning.is_open(&[later]));
    assert!(warning.is_open(&[earlier]));
}

#[test]
fn an_event_without_a_subject_is_never_cleared_by_another_without_one() {
    let warning = occurrence("a", "x.y", Severity::Error, datetime!(2026-10-01 00:00:00 UTC));
    let later = occurrence("b", "x.z", Severity::Info, datetime!(2026-10-02 00:00:00 UTC));

    assert!(warning.is_open(&[later]));
}

#[test]
fn a_recovery_clears_a_degraded_vault_only_for_the_same_code() {
    let mut degraded = occurrence(
        "a",
        "vault.degraded",
        Severity::Warning,
        datetime!(2026-10-01 00:00:00 UTC),
    );
    degraded.code = Some("git/unavailable".to_owned());
    let mut same = occurrence(
        "b",
        "vault.recovered",
        Severity::Info,
        datetime!(2026-10-02 00:00:00 UTC),
    );
    same.code = Some("git/unavailable".to_owned());
    let mut other = same.clone();
    other.code = Some("index/stale".to_owned());

    assert!(!degraded.is_open(&[same]));
    assert!(degraded.is_open(&[other]));
}

#[test]
fn a_job_recovery_clears_a_failure_only_for_the_same_workflow_and_job() {
    let mut failed = occurrence("a", "job.failed", Severity::Error, datetime!(2026-10-01 00:00:00 UTC));
    failed.workflow = Some("w".to_owned());
    failed.job = Some("j".to_owned());
    let mut recovered = occurrence("b", "job.recovered", Severity::Info, datetime!(2026-10-02 00:00:00 UTC));
    recovered.workflow = Some("w".to_owned());
    recovered.job = Some("j".to_owned());
    let mut other_workflow = recovered.clone();
    other_workflow.workflow = Some("other".to_owned());

    assert!(!failed.is_open(&[recovered]));
    assert!(failed.is_open(&[other_workflow]));
}

#[test]
fn an_occurrence_is_read_from_frontmatter_and_its_subject_link_is_resolved() {
    let map = frontmatter(json!({
        "kind": "job.failed", "severity": "error", "at": "2026-10-01T10:00:00+10:00",
        "subject": "[[memory/jobs/runs/r-1.md|run]]", "workflow": "w", "job": "j", "code": " c "
    }));

    let Ok(read) = Occurrence::try_from(("memory/events/e.md", &map)) else {
        unreachable!("the frontmatter is judgeable")
    };

    assert_eq!(read.subject, Some(note("memory/jobs/runs/r-1")));
    assert_eq!(read.code.as_deref(), Some("c"));
    assert_eq!(read.at, datetime!(2026-10-01 00:00:00 UTC));
}

#[test]
fn frontmatter_without_a_kind_a_known_severity_or_a_time_is_not_judgeable() {
    for bad in [
        json!({"severity": "error", "at": "2026-10-01T00:00:00Z"}),
        json!({"kind": "k", "severity": "dire", "at": "2026-10-01T00:00:00Z"}),
        json!({"kind": "k", "severity": "error", "at": "yesterday"}),
    ] {
        assert!(Occurrence::try_from(("p", &frontmatter(bad))).is_err());
    }
}

fn job_event(path: &str, kind: &str, severity: Severity, at: OffsetDateTime) -> Occurrence {
    let mut event = occurrence(path, kind, severity, at);
    event.workflow = Some("w".to_owned());
    event.job = Some("j".to_owned());
    event
}

#[test]
fn a_job_failure_that_names_no_workflow_or_job_is_not_cleared_by_a_recovery_that_names_none() {
    let at = datetime!(2026-10-01 00:00:00 UTC);
    let later = at + time::Duration::hours(1);
    let bare_failure = occurrence("a", "job.failed", Severity::Error, at);
    let bare_recovery = occurrence("b", "job.recovered", Severity::Info, later);
    let mut recovery_with_job = job_event("c", "job.recovered", Severity::Info, later);
    let mut failure_with_job = job_event("d", "job.failed", Severity::Error, at);

    assert!(
        bare_failure.is_open(std::slice::from_ref(&bare_recovery)),
        "neither names a job"
    );
    assert!(
        failure_with_job.is_open(&[bare_recovery]),
        "a recovery that names no job clears nothing"
    );
    assert!(
        bare_failure.is_open(&[recovery_with_job.clone()]),
        "a failure that names no job is cleared by nothing"
    );
    failure_with_job.workflow = None;
    recovery_with_job.workflow = None;
    assert!(
        failure_with_job.is_open(&[recovery_with_job]),
        "both must name a workflow"
    );
}

#[test]
fn a_job_failure_that_names_a_workflow_but_no_job_is_not_cleared_by_a_recovery_that_does_the_same() {
    let at = datetime!(2026-10-01 00:00:00 UTC);
    let mut failure = job_event("a", "job.failed", Severity::Error, at);
    let mut recovery = job_event("b", "job.recovered", Severity::Info, at + time::Duration::hours(1));
    failure.job = None;
    recovery.job = None;

    assert!(failure.is_open(&[recovery]));
}

#[test]
fn an_information_event_is_never_open_however_little_has_followed_it() {
    let info = occurrence("a", "x.y", Severity::Info, datetime!(2026-10-01 00:00:00 UTC));

    assert!(!info.is_open(&[]));
    assert!(!info.needs_attention());
}

#[test]
fn a_recovery_in_the_same_second_as_the_failure_it_clears_clears_it_whatever_their_paths() {
    let at = datetime!(2026-10-01 00:00:00 UTC);
    let failure = job_event("z-failure", "job.failed", Severity::Error, at);
    let recovery = job_event("a-recovery", "job.recovered", Severity::Info, at);
    let mut degraded = occurrence("z-degraded", "vault.degraded", Severity::Warning, at);
    degraded.code = Some("git/unavailable".to_owned());
    let mut recovered = occurrence("a-recovered", "vault.recovered", Severity::Info, at);
    recovered.code = degraded.code.clone();

    assert!(!failure.is_open(&[recovery]));
    assert!(!degraded.is_open(&[recovered]));
}

#[test]
fn two_warnings_about_one_subject_in_the_same_second_do_not_clear_each_other() {
    let at = datetime!(2026-10-01 00:00:00 UTC);
    let mut first = occurrence("a", "x.y", Severity::Warning, at);
    let mut second = occurrence("b", "x.y", Severity::Warning, at);
    first.subject = Some(note("memory/tasks/t"));
    second.subject = first.subject.clone();

    assert!(
        !first.is_open(&[second.clone()]),
        "the later path clears the earlier only"
    );
    assert!(!first.is_cleared_by(&first), "an event never clears itself");
    assert!(second.is_open(&[first]), "the earlier path does not clear the later");
}

#[test]
fn a_failure_is_not_cleared_by_a_recovery_before_it() {
    let at = datetime!(2026-10-01 00:00:10 UTC);
    let failure = job_event("a", "job.failed", Severity::Error, at);
    let before = job_event("b", "job.recovered", Severity::Info, at - time::Duration::seconds(1));

    assert!(failure.is_open(&[before]));
}
