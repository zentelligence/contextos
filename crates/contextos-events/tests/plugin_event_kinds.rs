//! Keeps the kind definitions the `ContextOS` plugin ships in a new vault's `registry/events/` in step with the rules this
//! server checks a vault's definitions against, so a shipped kind can never be one the server would refuse to record.
//!
//! The checking logic is exercised here against temporary directories, so its failure paths are proven on every run. The
//! definitions live in a separate repository, so the test that reads them is ignored by default and never runs as part
//! of a plain `cargo test`. `.claude/scripts/check-plugin-skill.sh` locates the plugin checkout, points
//! `CONTEXTOS_PLUGIN_EVENT_KINDS_DIR` at its `workflows/vault/templates/shipped/registry/events` directory, and runs
//! it, reporting a skip when no checkout is available.

use std::collections::BTreeMap;
use std::error::Error;
use std::path::Path;

use contextos_events::{KindDefinition, KindDefinitionSource, KindRegistry, SubjectRule};

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

/// Names the directory that holds the shipped definitions.
const KINDS_DIRECTORY_VARIABLE: &str = "CONTEXTOS_PLUGIN_EVENT_KINDS_DIR";

/// The kinds a new vault ships, and no others.
const SHIPPED: [&str; 7] = [
    "agent.delegated",
    "job.failed",
    "job.recovered",
    "job.retired",
    "job.suspended",
    "session.run-finished",
    "session.run-started",
];

/// Every definition note in `directory`, read with the server's own checks. The error names the file and the reason.
fn definitions_in(directory: &Path) -> Result<BTreeMap<String, KindDefinition>, String> {
    let mut found = BTreeMap::new();
    let mut registry = KindRegistry::default();
    let entries = std::fs::read_dir(directory).map_err(|error| format!("{}: {error}", directory.display()))?;
    for entry in entries {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| format!("{}: not a usable file name", path.display()))?
            .to_owned();
        let text = std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let definition = KindDefinition::try_from(KindDefinitionSource {
            stem: &stem,
            text: &text,
        })
        .map_err(|error| format!("{}: {error}", path.display()))?;
        registry
            .insert(definition.clone())
            .map_err(|error| format!("{}: {error}", path.display()))?;
        found.insert(stem, definition);
    }
    Ok(found)
}

fn names(fields: &[contextos_events::FieldName]) -> Vec<&str> {
    fields.iter().map(contextos_events::FieldName::as_str).collect()
}

/// The definition note for `kind` with `required` fields and `severities`, in the shape a vault holds.
fn note(kind: &str, severities: &str, required: &str) -> String {
    format!(
        "---\ntype: event-kind\nkind: {kind}\nlabel: Label\ndescription: A test kind.\nseverities: [{severities}]\n\
         default_severity: info\nfields:\n  required: [{required}]\n  optional: []\nsubject: none\n---\n\n# {kind}\n"
    )
}

#[test]
fn a_directory_of_usable_definitions_passes() -> TestResult {
    let directory = tempfile::tempdir()?;
    std::fs::write(
        directory.path().join("import.completed.md"),
        note("import.completed", "info", "source"),
    )?;

    let found = definitions_in(directory.path())?;

    assert_eq!(
        found.keys().map(String::as_str).collect::<Vec<_>>(),
        ["import.completed"]
    );
    Ok(())
}

#[test]
fn a_definition_the_server_would_refuse_fails_naming_the_file() -> TestResult {
    let directory = tempfile::tempdir()?;
    std::fs::write(
        directory.path().join("broken.kind.md"),
        note("broken.kind", "", "source"),
    )?;

    let error = definitions_in(directory.path())
        .err()
        .ok_or("a broken definition must fail")?;

    assert!(error.contains("broken.kind.md"), "{error}");
    Ok(())
}

#[test]
fn a_definition_of_a_kind_the_server_fires_itself_fails() -> TestResult {
    let directory = tempfile::tempdir()?;
    std::fs::write(
        directory.path().join("task.created.md"),
        note("task.created", "info", "source"),
    )?;

    assert!(definitions_in(directory.path()).is_err());
    Ok(())
}

#[test]
fn a_definition_whose_kind_differs_from_its_file_name_fails() -> TestResult {
    let directory = tempfile::tempdir()?;
    std::fs::write(
        directory.path().join("other.name.md"),
        note("import.completed", "info", "source"),
    )?;

    assert!(definitions_in(directory.path()).is_err());
    Ok(())
}

#[test]
#[ignore = "reads the plugin checkout; run through .claude/scripts/check-plugin-skill.sh"]
fn the_kinds_the_plugin_ships_pass_the_servers_checks_and_declare_the_fields_the_requirement_names() -> TestResult {
    let directory = std::env::var(KINDS_DIRECTORY_VARIABLE)
        .map_err(|_| format!("{KINDS_DIRECTORY_VARIABLE} must name the shipped registry/events directory"))?;

    let found = definitions_in(Path::new(&directory))?;

    assert_eq!(
        found.keys().map(String::as_str).collect::<Vec<_>>(),
        SHIPPED,
        "the plugin ships exactly the product's recordable kinds"
    );
    let expect = |kind: &str, required: &[&str], subject: SubjectRule, id_field: Option<&str>| {
        let definition = &found[kind];
        assert_eq!(names(&definition.required), required, "{kind}: required fields");
        assert!(definition.optional.is_empty(), "{kind}: optional fields");
        assert_eq!(definition.subject, subject, "{kind}: subject");
        assert_eq!(
            definition.id_field.as_ref().map(contextos_events::FieldName::as_str),
            id_field,
            "{kind}: key"
        );
    };
    expect(
        "job.failed",
        &["package", "workflow", "job", "run", "code"],
        SubjectRule::Required,
        None,
    );
    expect(
        "job.suspended",
        &["package", "workflow", "job", "code", "suspended_at"],
        SubjectRule::Required,
        None,
    );
    expect(
        "job.retired",
        &["package", "workflow", "job", "replaced_by", "retired_at"],
        SubjectRule::Required,
        None,
    );
    expect(
        "job.recovered",
        &["package", "workflow", "job", "run"],
        SubjectRule::Required,
        None,
    );
    expect(
        "session.run-started",
        &["members", "scope"],
        SubjectRule::Absent,
        Some("run_id"),
    );
    expect(
        "session.run-finished",
        &["run_id", "members", "dispatched", "review", "waiting"],
        SubjectRule::Absent,
        None,
    );
    expect("agent.delegated", &["agent", "caller"], SubjectRule::Optional, None);
    Ok(())
}
