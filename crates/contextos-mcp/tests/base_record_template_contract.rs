//! `base_record_create` stamps the fixed fields of the template a base's sidecar declares for the record's series,
//! refuses a supplied value that differs, and refuses a template it cannot use, always before a key is reserved.

mod support;
use std::collections::BTreeSet;

use contextos_mcp::ContextOsServer;
use serde_json::{Value, json};
use support::templated_vault::{
    BoxError, DECISION_TEMPLATE, STANDARD_DECLARATION, TASK_TEMPLATE, call_tool, create_record, created_path,
    failure_text, frontmatter_of, notes_under, sidecar, standard_files, structured, vault_with,
};

const TASKS_BASE_TEXT: &str = "filters: file.inFolder(\"tasks\")\nviews:\n- type: table\n  name: All\n";

type TestResult = Result<(), BoxError>;
/// A reason a template cannot be used, the link the sidecar names, extra vault files, and a read limit in MiB.
type UnusableTemplate<'a> = (&'a str, &'a str, Vec<(&'a str, String)>, Option<u64>);

fn assert_rejected_with_code(result: &rmcp::model::CallToolResult, code: &str) -> TestResult {
    assert_eq!(result.is_error, Some(true), "{result:?}");
    assert_eq!(structured(result)?.get("code"), Some(&json!(code)), "{result:?}");
    Ok(())
}

fn message_of(result: &rmcp::model::CallToolResult) -> Result<String, BoxError> {
    Ok(structured(result)?
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}

fn files_with(replacing: &[(&str, &str)], declaration: &str) -> Vec<(String, String)> {
    let mut files = standard_files(declaration);
    for (path, content) in replacing {
        files.retain(|(existing, _)| existing != path);
        files.push(((*path).to_owned(), (*content).to_owned()));
    }
    files
}

fn assert_nothing_written_or_reserved(vault: &tempfile::TempDir, sidecar_before: &str) -> TestResult {
    assert_eq!(
        notes_under(vault, "tasks")?,
        Vec::<String>::new(),
        "no record was written"
    );
    assert_eq!(
        std::fs::read_to_string(vault.path().join("tasks.md"))?,
        sidecar_before,
        "no key was reserved"
    );
    Ok(())
}

#[tokio::test]
async fn a_record_created_without_a_type_carries_its_series_templates_fixed_fields_and_not_type_note() -> TestResult {
    let (vault, server) = vault_with(&standard_files(STANDARD_DECLARATION), None)?;

    let result = create_record(&server, None, json!({})).await?;

    let path = created_path(&result)?;
    let record = frontmatter_of(&vault, &path)?;
    assert_eq!(record.get("type"), Some(&json!("task")), "{record:?}");
    assert_eq!(record.get("task_type"), Some(&json!("action")), "{record:?}");
    assert_eq!(
        record.get("level"),
        Some(&json!(1)),
        "the template's YAML type is kept: {record:?}"
    );
    assert_eq!(record.get("id"), Some(&json!("A-001")));
    assert!(
        !std::fs::read_to_string(vault.path().join(&path))?.contains("type: note"),
        "no type: note"
    );
    Ok(())
}

#[tokio::test]
async fn each_series_is_stamped_from_its_own_template() -> TestResult {
    let (vault, server) = vault_with(&standard_files(STANDARD_DECLARATION), None)?;

    for (prefix, task_type) in [("A", "action"), ("D", "decision"), ("Q", "action")] {
        let result = create_record(&server, Some(prefix), json!({})).await?;
        let record = frontmatter_of(&vault, &created_path(&result)?)?;
        assert_eq!(record.get("type"), Some(&json!("task")), "{prefix}: {record:?}");
        assert_eq!(record.get("task_type"), Some(&json!(task_type)), "{prefix}: {record:?}");
    }
    Ok(())
}

#[tokio::test]
async fn a_supplied_value_equal_to_the_templates_is_kept() -> TestResult {
    let (vault, server) = vault_with(&standard_files(STANDARD_DECLARATION), None)?;

    let result = create_record(
        &server,
        None,
        json!({"type": "task", "task_type": "action", "level": 1, "title": "Renew"}),
    )
    .await?;

    let record = frontmatter_of(&vault, &created_path(&result)?)?;
    assert_eq!(record.get("type"), Some(&json!("task")));
    assert_eq!(record.get("task_type"), Some(&json!("action")));
    assert_eq!(record.get("level"), Some(&json!(1)));
    assert_eq!(record.get("title"), Some(&json!("Renew")));
    Ok(())
}

#[tokio::test]
async fn a_differing_null_or_differently_typed_supplied_value_is_refused_with_fixed_field_conflict_and_nothing_is_written_or_reserved()
-> TestResult {
    let (vault, server) = vault_with(&standard_files(STANDARD_DECLARATION), None)?;
    let sidecar_before = std::fs::read_to_string(vault.path().join("tasks.md"))?;

    for supplied in [
        json!({"type": "note"}),
        json!({"task_type": "decision"}),
        json!({"task_type": null}),
        json!({"task_type": "Action"}),
        json!({"level": "1"}),
        json!({"level": 2}),
        json!({"type": null}),
    ] {
        let result = create_record(&server, None, supplied.clone()).await?;

        assert_rejected_with_code(&result, "io/fixed-field-conflict")
            .map_err(|error| format!("{supplied}: {error}"))?;
        assert_nothing_written_or_reserved(&vault, &sidecar_before).map_err(|error| format!("{supplied}: {error}"))?;
    }
    Ok(())
}

#[tokio::test]
async fn the_conflict_error_names_every_field_the_series_and_the_template_and_echoes_no_value() -> TestResult {
    let (_vault, server) = vault_with(&standard_files(STANDARD_DECLARATION), None)?;

    let result = create_record(
        &server,
        None,
        json!({"type": "ZZ-SECRET-TYPE", "task_type": "ZZ-SECRET-KIND"}),
    )
    .await?;

    assert_rejected_with_code(&result, "io/fixed-field-conflict")?;
    let message = message_of(&result)?;
    for needle in ["type", "task_type", "series A", "templates/task.md"] {
        assert!(message.contains(needle), "the message should name {needle}: {message}");
    }
    let body = structured(&result)?;
    let fields = body
        .get("fields")
        .and_then(Value::as_array)
        .ok_or("the error should carry fields as a structured list")?
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(fields, BTreeSet::from(["type", "task_type"]));
    assert_eq!(body.get("series"), Some(&json!("A")), "{body:?}");
    assert_eq!(body.get("template"), Some(&json!("templates/task.md")), "{body:?}");
    let everything = failure_text(&result)?;
    assert!(
        !everything.contains("ZZ-SECRET"),
        "no supplied value is echoed: {everything}"
    );
    assert!(
        body.get("remediation")
            .and_then(Value::as_str)
            .is_some_and(|text| text.contains("Omit")),
        "{body:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_template_default_such_as_state_or_priority_is_never_stamped() -> TestResult {
    let (vault, server) = vault_with(&standard_files(STANDARD_DECLARATION), None)?;

    let result = create_record(&server, None, json!({})).await?;

    let record = frontmatter_of(&vault, &created_path(&result)?)?;
    assert_eq!(
        record.get("type"),
        Some(&json!("task")),
        "the fixed fields are stamped: {record:?}"
    );
    assert_eq!(record.get("state"), None, "{record:?}");
    assert_eq!(record.get("priority"), None, "{record:?}");
    assert_ne!(record.get("entity"), Some(&json!("{{entity}}")), "{record:?}");
    assert_eq!(
        record.get("status"),
        Some(&json!("new")),
        "every other standard default is unchanged: {record:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_missing_ambiguous_non_markdown_oversized_or_malformed_template_is_refused_with_series_template_and_no_key_is_reserved()
-> TestResult {
    let oversized = format!(
        "---\ntype: task\ntask_type: action\n---\n{}",
        "x".repeat(2 * 1024 * 1024)
    );
    let cases: Vec<UnusableTemplate> = vec![
        ("missing", "[[templates/gone]]", vec![], None),
        (
            "ambiguous",
            "[[dup]]",
            vec![
                ("alpha/dup.md", TASK_TEMPLATE.to_owned()),
                ("beta/dup.md", TASK_TEMPLATE.to_owned()),
            ],
            None,
        ),
        (
            "not markdown",
            "[[templates/odd.base]]",
            vec![("templates/odd.base", "views:\n- type: table\n  name: All\n".to_owned())],
            None,
        ),
        (
            "oversized",
            "[[templates/big]]",
            vec![("templates/big.md", oversized)],
            Some(1),
        ),
        (
            "malformed frontmatter",
            "[[templates/bad]]",
            vec![("templates/bad.md", "---\ntype: task: broken\n---\nBody\n".to_owned())],
            None,
        ),
        (
            "type is a placeholder",
            "[[templates/placeholder]]",
            vec![(
                "templates/placeholder.md",
                "---\ntype: \"{{type}}\"\n---\nBody\n".to_owned(),
            )],
            None,
        ),
        (
            "type is absent",
            "[[templates/typeless]]",
            vec![(
                "templates/typeless.md",
                "---\ntask_type: action\n---\nBody\n".to_owned(),
            )],
            None,
        ),
        (
            "a fixed field is a menu",
            "[[templates/menu]]",
            vec![(
                "templates/menu.md",
                "---\ntype: task\ntask_type: action|decision\nlevel: 1\n---\nBody\n".to_owned(),
            )],
            None,
        ),
    ];
    for (reason, link, extra, limit) in cases {
        let declaration = format!("template: '{link}'\nfixed_fields: [task_type, level]\n");
        let extra_files = extra
            .iter()
            .map(|(path, content)| (*path, content.as_str()))
            .collect::<Vec<_>>();
        let (vault, server) = vault_with(&files_with(&extra_files, &declaration), limit)?;
        let sidecar_before = std::fs::read_to_string(vault.path().join("tasks.md"))?;

        let result = create_record(&server, None, json!({})).await?;

        assert_rejected_with_code(&result, "format/series-template").map_err(|error| format!("{reason}: {error}"))?;
        let message = message_of(&result)?;
        let link_name = link.trim_start_matches("[[").trim_end_matches("]]");
        assert!(
            message.contains(link_name),
            "{reason}: the message names the link: {message}"
        );
        assert!(
            message.contains('A'),
            "{reason}: the message names the series: {message}"
        );
        assert_nothing_written_or_reserved(&vault, &sidecar_before).map_err(|error| format!("{reason}: {error}"))?;
    }
    Ok(())
}

#[tokio::test]
async fn a_template_link_resolving_to_a_base_sidecar_is_refused_with_series_template_and_no_key_is_reserved()
-> TestResult {
    let other_sidecar = "---\ntype: task\n---\n![[other.base|other]]\n";
    let cases: Vec<UnusableTemplate> = vec![
        ("the base's own sidecar", "[[tasks]]", vec![], None),
        (
            "another base's sidecar",
            "[[other]]",
            vec![
                ("other.base", TASKS_BASE_TEXT.to_owned()),
                ("other.md", other_sidecar.to_owned()),
            ],
            None,
        ),
    ];
    for (reason, link, extra, limit) in cases {
        let declaration = format!("template: '{link}'\n");
        let extra_files = extra
            .iter()
            .map(|(path, content)| (*path, content.as_str()))
            .collect::<Vec<_>>();
        let (vault, server) = vault_with(&files_with(&extra_files, &declaration), limit)?;
        let sidecar_before = std::fs::read_to_string(vault.path().join("tasks.md"))?;

        let result = create_record(&server, None, json!({})).await?;

        assert_rejected_with_code(&result, "format/series-template").map_err(|error| format!("{reason}: {error}"))?;
        let message = message_of(&result)?;
        assert!(
            message.contains("resolves to a base sidecar"),
            "{reason}: the message names the reason: {message}"
        );
        assert_nothing_written_or_reserved(&vault, &sidecar_before).map_err(|error| format!("{reason}: {error}"))?;
    }
    Ok(())
}

#[tokio::test]
async fn a_template_that_cannot_be_used_is_never_replaced_by_type_note() -> TestResult {
    let declaration = "template: '[[templates/gone]]'\n";
    let (vault, server) = vault_with(&standard_files(declaration), None)?;

    let result = create_record(&server, None, json!({"type": "note"})).await?;

    assert_rejected_with_code(&result, "format/series-template")?;
    assert_eq!(notes_under(&vault, "tasks")?, Vec::<String>::new());
    Ok(())
}

#[tokio::test]
async fn a_bare_template_link_resolves_nearest_first_from_the_sidecar() -> TestResult {
    let nearest = "---\ntype: task\ntask_type: nearest\n---\nBody\n";
    let decoy = "---\ntype: decoy\ntask_type: decoy\n---\nBody\n";
    let memory_base = "filters: file.inFolder(\"memory/tasks\")\nviews:\n- type: table\n  name: All\n";
    let declaration = "template: '[[task]]'\nfixed_fields: [task_type]\n";
    let memory_sidecar = sidecar(declaration).replace("tasks.base|tasks", "memory/tasks.base|tasks");
    let files = |templates: Vec<(&str, &str)>| -> Vec<(String, String)> {
        let mut files = vec![
            ("memory/tasks.base".to_owned(), memory_base.to_owned()),
            ("memory/tasks.md".to_owned(), memory_sidecar.clone()),
            ("memory/tasks/.keep".to_owned(), String::new()),
        ];
        files.extend(
            templates
                .into_iter()
                .map(|(path, content)| (path.to_owned(), content.to_owned())),
        );
        files
    };

    // The note beside the sidecar wins over a same-named note elsewhere in the vault.
    let (vault, server) = vault_with(
        &files(vec![("memory/task.md", nearest), ("templates/task.md", decoy)]),
        None,
    )?;
    let result = call_tool(
        &server,
        "base_record_create",
        json!({"path": "memory/tasks.base", "content": "Body.\n"}),
    )
    .await?;
    let record = frontmatter_of(&vault, &created_path(&result)?)?;
    assert_eq!(record.get("type"), Some(&json!("task")), "{record:?}");
    assert_eq!(record.get("task_type"), Some(&json!("nearest")), "{record:?}");

    // With none nearer, the one note of that name in the vault is the template.
    let (vault, server) = vault_with(&files(vec![("templates/task.md", TASK_TEMPLATE)]), None)?;
    let result = call_tool(
        &server,
        "base_record_create",
        json!({"path": "memory/tasks.base", "content": "Body.\n"}),
    )
    .await?;
    let record = frontmatter_of(&vault, &created_path(&result)?)?;
    assert_eq!(record.get("task_type"), Some(&json!("action")), "{record:?}");
    Ok(())
}

#[tokio::test]
async fn a_base_without_a_template_still_writes_type_note() -> TestResult {
    // Guard: a base that declares no template, or a template list of several links with no entry for the series,
    // behaves exactly as before.
    for declaration in [
        "",
        "template:\n- '[[templates/task]]'\n- '[[templates/task-decision]]'\n",
    ] {
        let (vault, server) = vault_with(&standard_files(declaration), None)?;

        let result = create_record(&server, None, json!({})).await?;

        let record = frontmatter_of(&vault, &created_path(&result)?)?;
        assert_eq!(record.get("type"), Some(&json!("note")), "{declaration}: {record:?}");
        assert_eq!(record.get("task_type"), None, "{declaration}: {record:?}");
    }
    Ok(())
}

#[tokio::test]
async fn a_filepath_base_with_one_template_stamps_every_record() -> TestResult {
    let sidecar = "---\nkey_format: FILEPATH\nlast_id: 0\nlast_create: t0\nlast_update: t0\ntemplate: '[[templates/task]]'\nfixed_fields: [task_type]\n---\n![[tasks.base|tasks]]\n";
    let (vault, server) = vault_with(&files_with(&[("tasks.md", sidecar)], ""), None)?;

    let result = call_tool(
        &server,
        "base_record_create",
        json!({"path": "tasks.base", "filename": "one.md", "content": "Body.\n"}),
    )
    .await?;

    let record = frontmatter_of(&vault, &created_path(&result)?)?;
    assert_eq!(record.get("type"), Some(&json!("task")), "{record:?}");
    assert_eq!(record.get("task_type"), Some(&json!("action")), "{record:?}");
    Ok(())
}

#[tokio::test]
async fn a_template_edited_between_two_creations_stamps_the_second_from_its_new_values() -> TestResult {
    let (vault, server) = vault_with(&standard_files(STANDARD_DECLARATION), None)?;
    let first = create_record(&server, None, json!({})).await?;
    let first = frontmatter_of(&vault, &created_path(&first)?)?;
    assert_eq!(first.get("task_type"), Some(&json!("action")), "{first:?}");

    std::fs::write(
        vault.path().join("templates/task.md"),
        DECISION_TEMPLATE.replace("decision", "review"),
    )?;
    let second = create_record(&server, None, json!({})).await?;

    let second = frontmatter_of(&vault, &created_path(&second)?)?;
    assert_eq!(second.get("task_type"), Some(&json!("review")), "{second:?}");
    Ok(())
}

#[tokio::test]
async fn a_malformed_template_declaration_refuses_frontmatter_link_with_base_sidecar() -> TestResult {
    let declaration = "template: '[[templates/task]]'\nseries_templates:\n  Z: '[[templates/task]]'\nlink_fields:\n  related:\n    inverse: related\n";
    let mut files = standard_files(declaration);
    files.push((
        "tasks/alpha.md".to_owned(),
        "---\nid: A-001\nbase: '[[tasks.base]]'\n---\nBody\n".to_owned(),
    ));
    files.push((
        "tasks/beta.md".to_owned(),
        "---\nid: A-002\nbase: '[[tasks.base]]'\n---\nBody\n".to_owned(),
    ));
    let (vault, server) = vault_with(&files, None)?;
    let before = std::fs::read_to_string(vault.path().join("tasks/alpha.md"))?;

    let result = call_tool(
        &server,
        "frontmatter_link",
        json!({"path": "tasks/alpha.md", "operations": [{"op": "add", "field": "related", "target": "A-002"}]}),
    )
    .await?;

    assert_rejected_with_code(&result, "format/base-sidecar")?;
    assert_eq!(std::fs::read_to_string(vault.path().join("tasks/alpha.md"))?, before);
    Ok(())
}

#[test]
fn the_advertised_base_record_create_description_names_stamping_and_both_codes() -> TestResult {
    let tools = ContextOsServer::catalogue().list_all();
    let tool = tools
        .iter()
        .find(|tool| tool.name == "base_record_create")
        .ok_or("base_record_create is not advertised")?;
    let description = tool.description.as_deref().unwrap_or_default();

    for needle in ["template", "io/fixed-field-conflict", "format/series-template"] {
        assert!(
            description.contains(needle),
            "the description should name {needle}: {description}"
        );
    }
    Ok(())
}
