use serde_json::json;

use super::*;

/// A fixture is always a JSON object literal; anything else yields an empty map, which the calling test's own
/// assertions then fail on.
fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn template() -> Result<ResolvedTemplate, SeriesTemplateError> {
    let frontmatter = object(json!({"type": "task", "task_type": "action", "level": 1}));
    let fixed_fields = ["task_type".to_owned(), "level".to_owned()];
    Ok(ResolvedTemplate {
        path: "templates/task.md".to_owned(),
        fixed: FixedFields::try_from(SeriesTemplateInput {
            template: &frontmatter,
            fixed_fields: &fixed_fields,
        })?,
    })
}

#[test]
fn only_a_supplied_value_that_differs_by_value_or_type_conflicts() -> Result<(), SeriesTemplateError> {
    let template = template()?;

    assert_eq!(template.conflicting_fields(&object(json!({}))), Vec::<String>::new());
    assert_eq!(
        template.conflicting_fields(&object(
            json!({"type": "task", "task_type": "action", "level": 1, "title": "x"})
        )),
        Vec::<String>::new()
    );
    assert_eq!(
        template.conflicting_fields(&object(json!({"level": "1", "task_type": null, "type": "Task"}))),
        ["type", "task_type", "level"]
    );
    Ok(())
}

#[test]
fn stamping_adds_only_the_missing_fixed_fields_after_the_suppliers_own() -> Result<(), SeriesTemplateError> {
    let stamped = template()?.stamped(&object(json!({"title": "Renew", "level": 1})));

    assert_eq!(
        stamped.keys().map(String::as_str).collect::<Vec<_>>(),
        ["title", "level", "type", "task_type"]
    );
    assert_eq!(stamped.get("type"), Some(&json!("task")));
    assert_eq!(stamped.get("task_type"), Some(&json!("action")));
    Ok(())
}

#[test]
fn a_problem_reads_as_its_reason_and_its_remediation_names_the_field_to_fix() {
    let unusable = TemplateProblem::Unusable {
        path: "templates/requirement.md".to_owned(),
        error: SeriesTemplateError::NotLiteral {
            field: "requirement_type".to_owned(),
        },
    };

    assert_eq!(TemplateProblem::NotFound.to_string(), "not found");
    assert!(unusable.to_string().contains("requirement_type"), "{unusable}");
    assert_eq!(
        unusable.remediation(),
        "Give templates/requirement.md a single literal requirement_type, or remove requirement_type from the \
         sidecar's fixed_fields, and retry."
    );
}
