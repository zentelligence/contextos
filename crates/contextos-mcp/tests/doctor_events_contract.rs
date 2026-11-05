//! `doctor` reports an event kind definition it cannot use, a kind defined twice, and an event note whose time it
//! cannot read, each naming the file, never auto-fixable, and reports nothing for a vault that declares no events.

mod support;
use serde_json::{Map, Value, json};
use support::events_vault::{EVENTS_BASE, EVENTS_SIDECAR, IMPORT_COMPLETED, run_finished, run_started};
use support::templated_vault::{BoxError, call_tool, structured, vault_with};

fn files(extra: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut files = vec![
        ("memory/events.base".to_owned(), EVENTS_BASE.to_owned()),
        ("memory/events.md".to_owned(), EVENTS_SIDECAR.to_owned()),
        ("registry/events/session.run-started.md".to_owned(), run_started()),
        ("registry/events/session.run-finished.md".to_owned(), run_finished()),
    ];
    files.extend(
        extra
            .iter()
            .map(|(path, text)| ((*path).to_owned(), (*text).to_owned())),
    );
    files
}

fn check_of<'a>(report: &'a Map<String, Value>, kind: &str) -> Option<&'a Map<String, Value>> {
    report
        .get("checks")?
        .as_array()?
        .iter()
        .filter_map(Value::as_object)
        .find(|check| check.get("kind") == Some(&json!(kind)))
}

async fn report(files: &[(String, String)]) -> Result<(tempfile::TempDir, Map<String, Value>), BoxError> {
    let (vault, server) = vault_with(files, None)?;
    let result = call_tool(&server, "doctor", json!({})).await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    Ok((vault, structured(&result)?.clone()))
}

fn event(at: &str) -> String {
    format!("---\ntype: event\nkind: task.created\nseverity: info\nat: {at}\nentity: vault\n---\n\n# Created\n")
}

#[tokio::test]
async fn usable_definitions_and_readable_events_pass() -> Result<(), BoxError> {
    let (_vault, report) = report(&files(&[(
        "memory/events/2026/10/2026-10-28-000000-a.md",
        &event("2026-10-28T00:00:00Z"),
    )]))
    .await?;

    for kind in ["event_kind_invalid", "event_kind_duplicate", "event_unreadable"] {
        let check = check_of(&report, kind).ok_or(kind)?;
        assert_eq!(check.get("status"), Some(&json!("pass")), "{kind}: {check:?}");
    }
    Ok(())
}

#[tokio::test]
async fn a_vault_with_no_events_gets_no_event_checks() -> Result<(), BoxError> {
    let (_vault, report) = report(&[("notes/a.md".to_owned(), "# A\n".to_owned())]).await?;

    for kind in ["event_kind_invalid", "event_kind_duplicate", "event_unreadable"] {
        assert!(check_of(&report, kind).is_none(), "{kind}");
    }
    Ok(())
}

#[tokio::test]
async fn an_unusable_definition_is_named_and_not_auto_fixable() -> Result<(), BoxError> {
    let broken = "---\ntype: event-kind\nkind: other.name\nlabel: B\ndescription: d\nseverities: [info]\ndefault_severity: info\nsubject: none\n---\n";
    let server_fired = "---\ntype: event-kind\nkind: task.created\nlabel: T\ndescription: d\nseverities: [info]\ndefault_severity: info\nsubject: none\n---\n";
    let (_vault, report) = report(&files(&[
        ("registry/events/mismatch.md", broken),
        ("registry/events/task.created.md", server_fired),
    ]))
    .await?;

    let check = check_of(&report, "event_kind_invalid").ok_or("no check")?;
    assert_eq!(check.get("status"), Some(&json!("fail")));
    assert_eq!(check.get("auto_fixable"), Some(&json!(false)));
    assert_eq!(check.get("remediation_tool"), None);
    let message = check.get("message").and_then(Value::as_str).unwrap_or_default();
    assert!(message.contains("registry/events/mismatch.md"), "{message}");
    assert!(message.contains("registry/events/task.created.md"), "{message}");
    Ok(())
}

#[tokio::test]
async fn the_folder_index_notes_in_the_kind_registry_are_not_read_as_definitions() -> Result<(), BoxError> {
    let index = "---\ntype: index\n---\n\n# Events\n";
    let (_vault, report) = report(&files(&[
        ("registry/events/index.md", index),
        ("registry/events/_index.md", index),
        ("registry/events/more/index.md", index),
    ]))
    .await?;

    let check = check_of(&report, "event_kind_invalid").ok_or("no check")?;
    assert_eq!(check.get("status"), Some(&json!("pass")), "{check:?}");
    Ok(())
}

async fn invalid_message_with_sidecar(sidecar: String) -> Result<String, BoxError> {
    let mut files = files(&[]);
    files.retain(|(path, _)| path != "memory/events.md");
    files.push(("memory/events.md".to_owned(), sidecar));
    let (_vault, report) = report(&files).await?;
    let check = check_of(&report, "event_kind_invalid").ok_or("no check")?;
    assert_eq!(check.get("status"), Some(&json!("fail")), "{check:?}");
    Ok(check
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}

#[tokio::test]
async fn a_key_field_that_no_series_maps_to_is_an_invalid_definition() -> Result<(), BoxError> {
    let message =
        invalid_message_with_sidecar(EVENTS_SIDECAR.replace("series_kinds:\n  S: session.run-started\n", "")).await?;

    assert!(message.contains("registry/events/session.run-started.md"), "{message}");
    assert!(message.contains("series_kinds"), "{message}");
    Ok(())
}

#[tokio::test]
async fn a_kind_that_more_than_one_series_maps_to_is_an_invalid_definition() -> Result<(), BoxError> {
    let message = invalid_message_with_sidecar(EVENTS_SIDECAR.replace("  S: 0\n", "  S: 0\n  T: 0\n").replace(
        "  S: session.run-started\n",
        "  S: session.run-started\n  T: session.run-started\n",
    ))
    .await?;

    assert!(message.contains("registry/events/session.run-started.md"), "{message}");
    assert!(message.contains("more than one series"), "{message}");
    Ok(())
}

#[tokio::test]
async fn a_kind_defined_twice_is_reported_under_its_own_code_naming_both_files() -> Result<(), BoxError> {
    let (_vault, report) = report(&files(&[
        ("registry/events/import.completed.md", IMPORT_COMPLETED),
        ("registry/events/more/import.completed.md", IMPORT_COMPLETED),
    ]))
    .await?;

    let check = check_of(&report, "event_kind_duplicate").ok_or("no check")?;
    assert_eq!(check.get("status"), Some(&json!("fail")));
    assert_eq!(check.get("auto_fixable"), Some(&json!(false)));
    let message = check.get("message").and_then(Value::as_str).unwrap_or_default();
    assert!(message.contains("registry/events/import.completed.md"), "{message}");
    assert!(
        message.contains("registry/events/more/import.completed.md"),
        "{message}"
    );
    assert_eq!(
        check_of(&report, "event_kind_invalid").and_then(|check| check.get("status")),
        Some(&json!("pass")),
        "a duplicate is not also reported as invalid"
    );
    Ok(())
}

#[tokio::test]
async fn an_event_whose_time_cannot_be_read_is_reported_and_left_alone() -> Result<(), BoxError> {
    let odd = "memory/events/2026/10/2026-10-28-000000-odd.md";
    let (vault, report) = report(&files(&[(odd, &event("someday"))])).await?;

    let check = check_of(&report, "event_unreadable").ok_or("no check")?;
    assert_eq!(check.get("status"), Some(&json!("fail")));
    assert_eq!(check.get("auto_fixable"), Some(&json!(false)));
    assert!(
        check
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains(odd)
    );
    assert!(vault.path().join(odd).exists());
    Ok(())
}

#[tokio::test]
async fn a_note_under_the_events_folder_that_is_not_an_event_is_reported_and_never_deleted() -> Result<(), BoxError> {
    let stray = "memory/events/2026/10/2026-10-28-000000-stray.md";
    let stray_text = event("2026-10-28T00:00:00Z").replace("type: event", "type: note");
    let (vault, report) = report(&files(&[(stray, &stray_text)])).await?;

    let check = check_of(&report, "event_unreadable").ok_or("no check")?;
    assert_eq!(check.get("status"), Some(&json!("fail")), "{check:?}");
    assert_eq!(check.get("auto_fixable"), Some(&json!(false)));
    let message = check.get("message").and_then(Value::as_str).unwrap_or_default();
    assert!(message.contains(stray), "{message}");
    assert!(vault.path().join(stray).exists());
    Ok(())
}

const OTHER_BASE: &str =
    "filters:\n  and:\n    - file.inFolder(\"projects\")\nviews:\n  - type: table\n    name: All\n";
const OTHER_SIDECAR: &str = "---\nbase: '[[projects.base|Projects]]'\nkey_prefixes:\n  P: 0\nkey_format: '{key_prefix}-{key_id}'\nevent_kinds: registry/events\n---\n\n# Projects\n";

#[tokio::test]
async fn an_event_registry_declared_on_any_other_base_is_reported_and_not_auto_fixable() -> Result<(), BoxError> {
    let (_vault, report) = report(&files(&[("projects.base", OTHER_BASE), ("projects.md", OTHER_SIDECAR)])).await?;

    let check = check_of(&report, "event_kind_misplaced").ok_or("no check")?;
    assert_eq!(check.get("status"), Some(&json!("fail")), "{check:?}");
    assert_eq!(check.get("auto_fixable"), Some(&json!(false)));
    let message = check.get("message").and_then(Value::as_str).unwrap_or_default();
    assert!(message.contains("projects.md"), "{message}");
    Ok(())
}

#[tokio::test]
async fn a_vault_whose_only_event_registry_is_on_the_events_base_has_no_misplaced_finding() -> Result<(), BoxError> {
    let (_vault, report) = report(&files(&[])).await?;

    let check = check_of(&report, "event_kind_misplaced").ok_or("no check")?;
    assert_eq!(check.get("status"), Some(&json!("pass")), "{check:?}");
    Ok(())
}

#[tokio::test]
async fn a_registry_folder_with_more_definitions_than_are_read_is_reported() -> Result<(), BoxError> {
    let mut extra = Vec::new();
    for number in 0..1001 {
        extra.push((
            format!("registry/events/pad/filler-{number:04}.md"),
            "# Not a definition\n".to_owned(),
        ));
    }
    let mut all = files(&[]);
    all.extend(extra);
    let (_vault, report) = report(&all).await?;

    let check = check_of(&report, "event_kind_truncated").ok_or("no check")?;
    assert_eq!(check.get("status"), Some(&json!("fail")), "{check:?}");
    assert_eq!(check.get("auto_fixable"), Some(&json!(false)));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_symbolic_link_in_the_events_folder_is_reported_and_never_followed() -> Result<(), BoxError> {
    let (vault, server) = vault_with(&files(&[("notes/precious.md", &event("2026-10-28T00:00:00Z"))]), None)?;
    let month = vault.path().join("memory/events/2026/10");
    std::fs::create_dir_all(&month)?;
    std::os::unix::fs::symlink(
        vault.path().join("notes/precious.md"),
        month.join("2026-10-28-000000-link.md"),
    )?;
    let outside = tempfile::tempdir()?;
    std::fs::write(outside.path().join("out.md"), "# Out\n")?;
    std::os::unix::fs::symlink(outside.path().join("out.md"), month.join("2026-10-28-000001-escape.md"))?;

    let result = call_tool(&server, "doctor", json!({})).await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let report = structured(&result)?.clone();
    let check = check_of(&report, "event_unreadable").ok_or("no check")?;
    assert_eq!(check.get("status"), Some(&json!("fail")), "{check:?}");
    let message = check.get("message").and_then(Value::as_str).unwrap_or_default();
    assert!(message.contains("2026-10-28-000000-link.md"), "{message}");
    assert!(message.contains("2026-10-28-000001-escape.md"), "{message}");
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn an_events_base_that_cannot_be_addressed_is_a_finding_and_the_report_completes() -> Result<(), BoxError> {
    let (vault, server) = vault_with(&files(&[]), None)?;
    let outside = tempfile::tempdir()?;
    std::fs::write(outside.path().join("elsewhere.base"), EVENTS_BASE)?;
    std::fs::remove_file(vault.path().join("memory/events.base"))?;
    std::os::unix::fs::symlink(
        outside.path().join("elsewhere.base"),
        vault.path().join("memory/events.base"),
    )?;

    let result = call_tool(&server, "doctor", json!({})).await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let report = structured(&result)?.clone();
    let check = check_of(&report, "event_kind_invalid").ok_or("no check")?;
    assert_eq!(check.get("status"), Some(&json!("fail")), "{check:?}");
    assert_eq!(check.get("auto_fixable"), Some(&json!(false)));
    let message = check.get("message").and_then(Value::as_str).unwrap_or_default();
    assert!(message.contains("memory/events.base"), "{message}");
    Ok(())
}
