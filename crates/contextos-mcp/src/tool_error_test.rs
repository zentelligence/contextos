use super::*;

#[test]
fn a_reserved_key_whose_note_was_not_written_has_its_own_stable_code_and_names_the_key() {
    let failure = ToolFailure::from(ToolError::KeyReservedNoteNotWritten {
        key: "REQ-007".to_owned(),
        source: Box::new(FsError::NotFound {
            path: std::path::PathBuf::from("requirements/req-007.md"),
        }),
    });

    assert_eq!(failure.code, "io/key-reserved-note-not-written");
    assert!(failure.message.contains("REQ-007"), "{}", failure.message);
    assert!(failure.remediation.contains("gap"), "{}", failure.remediation);
    assert_eq!(
        failure.path.as_deref(),
        Some(std::path::Path::new("requirements/req-007.md"))
    );
}

#[test]
fn a_failure_without_details_serialises_exactly_its_code_message_and_remediation() -> Result<(), serde_json::Error> {
    let failure = ToolFailure::from(ToolError::Invalid("bad"));

    assert_eq!(
        serde_json::to_value(&failure)?,
        serde_json::json!({
            "code": "io/invalid-argument",
            "message": "invalid tool arguments: bad",
            "remediation": "Correct the tool arguments using the advertised input schema.",
        })
    );
    Ok(())
}

#[test]
fn a_fixed_field_conflict_carries_its_fields_series_and_template_beside_the_standard_members()
-> Result<(), serde_json::Error> {
    let failure = ToolFailure::from(ToolError::FixedFieldConflict {
        fields: vec!["type".to_owned(), "task_type".to_owned()],
        series: "A".to_owned(),
        template: "templates/task.md".to_owned(),
    });

    let body = serde_json::to_value(&failure)?;
    assert_eq!(body["code"], "io/fixed-field-conflict");
    assert_eq!(body["fields"], serde_json::json!(["type", "task_type"]));
    assert_eq!(body["series"], "A");
    assert_eq!(body["template"], "templates/task.md");
    assert!(
        body["remediation"].as_str().is_some_and(|text| text.contains("Omit")),
        "{body}"
    );
    assert_eq!(
        body["message"],
        "type, task_type are fixed by the template of series A (templates/task.md), and the supplied value differs; omit it to have it stamped"
    );
    Ok(())
}

#[test]
fn an_unusable_series_template_carries_a_remediation_naming_what_to_fix() {
    let failure = ToolFailure::from(ToolError::SeriesTemplate {
        series: "series A".to_owned(),
        link: "[[templates/gone]]".to_owned(),
        problem: crate::tools::base_record_template::TemplateProblem::NotFound,
    });

    assert_eq!(failure.code, "format/series-template");
    assert!(failure.message.contains("[[templates/gone]]"), "{}", failure.message);
    assert!(
        failure.remediation.contains("Create the template note"),
        "{}",
        failure.remediation
    );
    assert!(failure.details.is_none());
}
