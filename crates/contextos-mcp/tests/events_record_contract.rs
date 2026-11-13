//! Clients record events of a kind the vault declares through `base_record_create` against `memory/events.base`: the
//! server validates the record, stamps `at` and the path, assigns the kind's key under the write lock, and refuses
//! everything else before a key is reserved or anything is written.
mod support;

use serde_json::{Value, json};
use support::events_vault::{event_notes, events_of_kind, field, scaffolded_harness};
use support::git_harness::{BoxError, Harness};

async fn record(harness: &Harness, frontmatter: Value) -> Result<Value, BoxError> {
    let prefix = if frontmatter.get("kind") == Some(&json!("session.run-started")) {
        json!("S")
    } else {
        Value::Null
    };
    let mut arguments = json!({"path": "memory/events.base", "frontmatter": frontmatter});
    if !prefix.is_null() {
        arguments["prefix"] = prefix;
    }
    record_with(harness, arguments).await
}

async fn record_with(harness: &Harness, arguments: Value) -> Result<Value, BoxError> {
    let result = harness.call("base_record_create", arguments).await?;
    Ok(result.structured_content.ok_or("no structured result")?)
}

fn started() -> Value {
    json!({"kind": "session.run-started", "members": ["ellis"], "scope": "team"})
}

fn sidecar(harness: &Harness) -> std::io::Result<String> {
    std::fs::read_to_string(harness.vault.path().join("memory/events.md"))
}

#[tokio::test]
async fn a_declared_kind_is_recorded_with_a_server_assigned_key_returned_as_key() -> Result<(), BoxError> {
    let harness = scaffolded_harness().await?;
    let first = record(&harness, started()).await?;

    assert_eq!(first.get("key"), Some(&json!("S-001")), "{first}");
    let path = first.get("path").and_then(Value::as_str).ok_or("no path")?;
    assert!(
        path.starts_with("memory/events/") && path.ends_with("-session-run-started.md"),
        "the file is not named from the key series: {path}"
    );
    let text = std::fs::read_to_string(harness.vault.path().join(path))?;
    assert!(
        text.starts_with("---\ntype: event\nkind: session.run-started\nseverity: info\nat: "),
        "{text}"
    );
    assert_eq!(field(&text, "entity").as_deref(), Some("vault"));
    assert_eq!(field(&text, "run_id").as_deref(), Some("S-001"));
    assert!(text.contains("\nscope: 'team'\n"), "{text}");
    for common in ["\ntitle:", "\ncreated:", "\nupdated:", "\ntags:", "\nbase:"] {
        assert!(!text.contains(common), "{common}: {text}");
    }
    assert!(sidecar(&harness)?.contains("S: 1"), "{}", sidecar(&harness)?);

    let second = record(&harness, started()).await?;
    assert_eq!(second.get("key"), Some(&json!("S-002")));
    Ok(())
}

#[tokio::test]
async fn two_sessions_recording_a_start_at_once_receive_different_keys() -> Result<(), BoxError> {
    let harness = scaffolded_harness().await?;
    let other =
        contextos_mcp::ContextOsServer::try_from(support::scoped_config(vec![harness.vault.path().to_path_buf()])?)?;
    let arguments = json!({"path": "memory/events.base", "prefix": "S", "frontmatter": started()});

    let (first, second) = tokio::join!(
        support::git_harness::call(&harness.server, "base_record_create", arguments.clone()),
        support::git_harness::call(&other, "base_record_create", arguments),
    );

    let key = |result: rmcp::model::CallToolResult| {
        result
            .structured_content
            .and_then(|content| content.get("key").and_then(Value::as_str).map(str::to_owned))
    };
    let (first, second) = (key(first?), key(second?));
    assert!(first.is_some() && second.is_some());
    assert_ne!(first, second);
    assert_eq!(events_of_kind(harness.vault.path(), "session.run-started").len(), 2);
    Ok(())
}

#[tokio::test]
async fn a_kind_with_no_key_field_returns_no_key_and_does_not_move_the_counter() -> Result<(), BoxError> {
    let harness = scaffolded_harness().await?;
    let before = sidecar(&harness)?;
    let result = record_with(
        &harness,
        json!({
            "path": "memory/events.base",
            "frontmatter": {"kind": "import.completed", "severity": "warning", "source": "bank", "rows": 12,
                            "entity": "zenith"},
            "content": "Twelve rows loaded."
        }),
    )
    .await?;

    assert_eq!(
        result.get("key"),
        None,
        "a kind with no key field leaves `key` out: {result}"
    );
    let path = result.get("path").and_then(Value::as_str).ok_or("no path")?;
    let text = std::fs::read_to_string(harness.vault.path().join(path))?;
    assert!(
        text.contains("\nseverity: warning\n") && text.contains("\nentity: 'zenith'\n"),
        "{text}"
    );
    assert!(text.contains("\nrows: 12\n"), "{text}");
    assert!(text.ends_with("# Import completed\n\nTwelve rows loaded.\n"), "{text}");
    assert_eq!(sidecar(&harness)?, before);
    Ok(())
}

#[tokio::test]
async fn every_refusal_carries_its_code_and_reserves_no_key_and_writes_nothing() -> Result<(), BoxError> {
    let harness = scaffolded_harness().await?;
    let before = sidecar(&harness)?;
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "no kind",
            json!({"members": ["a"], "scope": "team"}),
            "events/unknown-kind",
        ),
        (
            "undeclared kind",
            json!({"kind": "no.such-kind"}),
            "events/unknown-kind",
        ),
        (
            "server-fired kind",
            json!({"kind": "task.created", "members": ["a"], "scope": "team"}),
            "events/server-fired-kind",
        ),
        (
            "severity not allowed",
            json!({"kind": "session.run-started", "severity": "error", "members": ["a"], "scope": "team"}),
            "events/invalid-severity",
        ),
        (
            "required field missing",
            json!({"kind": "session.run-started", "members": ["a"]}),
            "events/invalid-fields",
        ),
        (
            "undeclared field",
            json!({"kind": "session.run-started", "members": ["a"], "scope": "team", "colour": "red"}),
            "events/invalid-fields",
        ),
        (
            "common note field",
            json!({"kind": "session.run-started", "members": ["a"], "scope": "team", "title": "x"}),
            "events/invalid-fields",
        ),
        (
            "subject for a kind with none",
            json!({"kind": "session.run-started", "members": ["a"], "scope": "team", "subject": "[[x]]"}),
            "events/invalid-subject",
        ),
        (
            "at supplied",
            json!({"kind": "session.run-started", "members": ["a"], "scope": "team", "at": "2026-01-01T00:00:00Z"}),
            "events/server-stamped",
        ),
        (
            "key field supplied",
            json!({"kind": "session.run-started", "members": ["a"], "scope": "team", "run_id": "S-99"}),
            "events/server-stamped",
        ),
    ];
    for (name, frontmatter, code) in cases {
        let result = record(&harness, frontmatter).await?;
        assert_eq!(result.get("code"), Some(&json!(code)), "{name}: {result}");
    }
    for (name, arguments, code) in series_refusals() {
        let result = record_with(&harness, arguments).await?;
        assert_eq!(result.get("code"), Some(&json!(code)), "{name}: {result}");
    }
    for (name, extra) in [
        ("filename", json!({"filename": "x.md"})),
        ("folder", json!({"folder": "/elsewhere"})),
    ] {
        let mut arguments = json!({"path": "memory/events.base", "frontmatter": started()});
        arguments
            .as_object_mut()
            .ok_or("object")?
            .extend(extra.as_object().cloned().ok_or("object")?);
        let result = record_with(&harness, arguments).await?;
        assert_eq!(
            result.get("code"),
            Some(&json!("events/server-stamped")),
            "{name}: {result}"
        );
    }

    assert_eq!(event_notes(harness.vault.path()), Vec::new());
    assert_eq!(
        sidecar(&harness)?,
        before,
        "no key may be reserved for a refused record"
    );
    Ok(())
}

fn series_refusals() -> Vec<(&'static str, Value, &'static str)> {
    let base = |frontmatter: Value, prefix: Option<&str>| {
        let mut arguments = json!({"path": "memory/events.base", "frontmatter": frontmatter});
        if let Some(prefix) = prefix {
            arguments["prefix"] = json!(prefix);
        }
        arguments
    };
    vec![
        (
            "prefix missing for a keyed kind",
            base(started(), None),
            "events/invalid-series",
        ),
        (
            "series not declared by the sidecar",
            base(started(), Some("T")),
            "events/invalid-series",
        ),
        (
            "prefix for a kind without a key field",
            base(
                json!({
                    "kind": "session.run-finished",
                    "run_id": "S-1",
                    "members": ["a"],
                    "dispatched": 1,
                    "review": 0,
                    "waiting": 0
                }),
                Some("S"),
            ),
            "events/invalid-series",
        ),
    ]
}

#[tokio::test]
async fn a_definition_doctor_would_reject_leaves_its_kind_unrecordable() -> Result<(), BoxError> {
    let harness = support::events_vault::events_harness_with(
        |_| {},
        |root| {
            support::events_vault::seed_events_scaffold(root)?;
            // Not valid: its severities are empty.
            std::fs::write(
                root.join("registry/events/broken.kind.md"),
                "---\ntype: event-kind\nkind: broken.kind\nlabel: B\ndescription: d\nseverities: []\ndefault_severity: info\nsubject: none\n---\n",
            )?;
            // Two definitions of one kind, one in a sub-folder.
            std::fs::create_dir_all(root.join("registry/events/more"))?;
            std::fs::write(root.join("registry/events/more/import.completed.md"), support::events_vault::IMPORT_COMPLETED)
        },
    )
    .await?;

    for kind in ["broken.kind", "import.completed"] {
        let result = record(&harness, json!({"kind": kind, "source": "x"})).await?;
        assert_eq!(
            result.get("code"),
            Some(&json!("events/unknown-kind")),
            "{kind}: {result}"
        );
    }
    let still = record(&harness, started()).await?;
    assert_eq!(still.get("key"), Some(&json!("S-001")), "other kinds are unaffected");
    Ok(())
}

#[tokio::test]
async fn unique_fields_let_a_recorder_that_runs_again_find_the_event_it_already_recorded() -> Result<(), BoxError> {
    let harness = scaffolded_harness().await?;
    let arguments = json!({
        "path": "memory/events.base",
        "frontmatter": {
            "kind": "session.run-finished",
            "run_id": "S-7",
            "members": ["ellis"],
            "dispatched": 2,
            "review": 1,
            "waiting": 0
        },
        "unique_fields": ["run_id"]
    });
    let first = record_with(&harness, arguments.clone()).await?;
    assert!(first.get("path").is_some(), "{first}");

    let again = record_with(&harness, arguments).await?;
    assert_eq!(again.get("code"), Some(&json!("io/duplicate-record")), "{again}");
    assert_eq!(events_of_kind(harness.vault.path(), "session.run-finished").len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_recorded_event_is_staged_in_git_and_raises_no_task_event() -> Result<(), BoxError> {
    let harness = scaffolded_harness().await?;
    let result = record(&harness, started()).await?;
    let path = result.get("path").and_then(Value::as_str).ok_or("no path")?;

    let pending = harness.pending_paths().await?;
    assert!(pending.contains(&path.to_owned()), "{pending:?}");
    assert_eq!(event_notes(harness.vault.path()).len(), 1);
    Ok(())
}

#[tokio::test]
async fn every_other_base_behaves_as_before() -> Result<(), BoxError> {
    let harness = scaffolded_harness().await?;
    let result = harness
        .call(
            "base_record_create",
            json!({"path": "memory/tasks.base", "frontmatter": {"type": "task", "state": "next"}, "filename": "a-002.md"}),
        )
        .await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert!(harness.vault.path().join("memory/tasks/a-002.md").exists());
    Ok(())
}

#[tokio::test]
async fn only_the_vaults_events_base_records_events_whatever_another_sidecar_declares() -> Result<(), BoxError> {
    let harness = scaffolded_harness().await?;
    let root = harness.vault.path();
    std::fs::create_dir_all(root.join("projects"))?;
    std::fs::write(
        root.join("projects.base"),
        "filters:\n  and:\n    - file.inFolder(\"projects\")\nviews:\n  - type: table\n    name: All\n",
    )?;
    std::fs::write(
        root.join("projects.md"),
        concat!(
            "---\nbase: '[[projects.base|Projects]]'\nlast_create: 2026-10-28T00:00:00Z\nlast_update: 2026-10-28T00:00:00Z\nkey_prefixes:\n  P: 0\nkey_format: '{key_prefix}-{key_id}'\n",
            "event_kinds: registry/events\nseries_kinds:\n  P: session.run-started\n---\n\n# Projects\n"
        ),
    )?;
    let before = event_notes(root).len();

    let result = record_with(
        &harness,
        json!({
            "path": "projects.base",
            "prefix": "P",
            "folder": "projects",
            "frontmatter": {"title": "Alpha"}
        }),
    )
    .await?;

    assert_eq!(
        event_notes(root).len(),
        before,
        "no event note may be written: {result}"
    );
    assert!(
        result
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| path.starts_with("projects/")),
        "an ordinary keyed record is created beside its base: {result}"
    );
    Ok(())
}

#[tokio::test]
async fn doctor_finds_the_events_the_server_fires_and_the_ones_clients_record_conformant() -> Result<(), BoxError> {
    let harness = scaffolded_harness().await?;
    // One event the server fires itself, and one a client records.
    harness
        .structured(
            "fs_write_file",
            json!({"path": "memory/tasks/a-002.md", "content": support::events_vault::task_note("A-002", "backlog", "")}),
        )
        .await?;
    let recorded = record(&harness, started()).await?;
    assert!(recorded.get("path").is_some(), "{recorded}");
    assert_eq!(events_of_kind(harness.vault.path(), "task.created").len(), 1);

    let report = harness.structured("doctor", json!({})).await?;

    let checks = report.get("checks").and_then(Value::as_array).ok_or("checks")?;
    let conformance = checks
        .iter()
        .find(|check| check.get("kind") == Some(&json!("record_conformance")))
        .ok_or("no record_conformance check")?;
    assert_eq!(conformance.get("status"), Some(&json!("pass")), "{conformance}");
    Ok(())
}

#[tokio::test]
async fn a_recorded_subject_naming_a_task_by_its_key_is_stamped_with_its_key_path_and_base() -> Result<(), BoxError> {
    let harness = scaffolded_harness().await?;
    let result = record(
        &harness,
        json!({"kind": "import.completed", "source": "bank", "subject": "[[a-001|A-001]]"}),
    )
    .await?;

    let path = result.get("path").and_then(Value::as_str).ok_or("no path")?;
    let text = std::fs::read_to_string(harness.vault.path().join(path))?;
    assert_eq!(field(&text, "subject").as_deref(), Some("[[a-001|A-001]]"));
    assert_eq!(field(&text, "subject_id").as_deref(), Some("A-001"));
    assert_eq!(field(&text, "subject_path").as_deref(), Some("memory/tasks/a-001.md"));
    assert_eq!(field(&text, "subject_base").as_deref(), Some("memory/tasks.base"));
    Ok(())
}

#[tokio::test]
async fn a_recorded_subject_that_is_no_record_is_stamped_with_its_path_only() -> Result<(), BoxError> {
    let harness = scaffolded_harness().await?;
    std::fs::create_dir_all(harness.vault.path().join("notes"))?;
    std::fs::write(harness.vault.path().join("notes/plain.md"), "# Plain\n")?;

    let result = record(
        &harness,
        json!({"kind": "import.completed", "source": "bank", "subject": "[[notes/plain|plain]]"}),
    )
    .await?;

    let path = result.get("path").and_then(Value::as_str).ok_or("no path")?;
    let text = std::fs::read_to_string(harness.vault.path().join(path))?;
    assert_eq!(field(&text, "subject_path").as_deref(), Some("notes/plain.md"));
    assert_eq!(field(&text, "subject_id"), None);
    assert_eq!(field(&text, "subject_base"), None);
    Ok(())
}

#[tokio::test]
async fn a_recorded_subject_is_refused_when_it_names_a_task_by_path_or_names_several_notes_or_is_stamped()
-> Result<(), BoxError> {
    let harness = scaffolded_harness().await?;
    for folder in ["notes/a", "notes/b"] {
        std::fs::create_dir_all(harness.vault.path().join(folder))?;
        std::fs::write(harness.vault.path().join(folder).join("dup.md"), "# Dup\n")?;
    }
    let cases = [
        (
            json!({"kind": "import.completed", "source": "bank", "subject": "[[memory/tasks/a-001|A-001]]"}),
            "events/invalid-subject",
        ),
        (
            json!({"kind": "import.completed", "source": "bank", "subject": "[[dup]]"}),
            "events/invalid-subject",
        ),
        (
            json!({"kind": "import.completed", "source": "bank", "subject": "memory/tasks/a-001"}),
            "events/invalid-subject",
        ),
        (
            json!({"kind": "import.completed", "source": "bank", "subject": "![[memory/tasks/a-001]]"}),
            "events/invalid-subject",
        ),
        (
            json!({"kind": "import.completed", "source": "bank", "subject": "see [[memory/tasks/a-001]]"}),
            "events/invalid-subject",
        ),
        (
            json!({"kind": "import.completed", "source": "bank", "subject": "[[dup]]", "subject_id": "A-001"}),
            "events/server-stamped",
        ),
        (
            json!({"kind": "import.completed", "source": "bank", "subject": "[[a-001|A-001]]", "subject_id": "A-001"}),
            "events/server-stamped",
        ),
        (
            json!({"kind": "import.completed", "source": "bank", "subject": "[[a-001|A-001]]", "subject_base": "x.base"}),
            "events/server-stamped",
        ),
    ];
    for (frontmatter, code) in cases {
        let result = record(&harness, frontmatter.clone()).await?;
        assert_eq!(result.get("code"), Some(&json!(code)), "{frontmatter}: {result}");
    }
    assert_eq!(event_notes(harness.vault.path()), Vec::new(), "nothing is written");
    Ok(())
}
