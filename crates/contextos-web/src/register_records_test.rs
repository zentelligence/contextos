use serde_json::json;

use super::*;

fn object(value: &serde_json::Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

fn context<'a>(
    current: &'a Map<String, Value>,
    decisions: &'a [String],
    entities: &'a [String],
) -> ValidationContext<'a> {
    ValidationContext {
        current,
        decision_ids: decisions,
        entity_slugs: entities,
    }
}

#[test]
fn a_timestamp_shows_as_its_date_and_anything_else_as_written() {
    assert_eq!(display_date("2026-09-29 20:48:31.92 +00:00:00"), "2026-09-29");
    assert_eq!(display_date("2026-08-07"), "2026-08-07");
    assert_eq!(display_date("soon"), "soon");
    assert_eq!(display_date(""), "");
}

#[test]
fn a_heading_title_drops_a_repeated_id() {
    assert_eq!(
        heading_title("\n# VD-001: Decisions are one file per record\n", "VD-001"),
        Some("Decisions are one file per record".to_owned())
    );
    assert_eq!(
        heading_title("# prioritisation-methodology.md file missing\n", "VQ-001"),
        Some("prioritisation-methodology.md file missing".to_owned())
    );
    assert_eq!(heading_title("No heading here", "VD-002"), None);
}

#[test]
fn ids_order_by_their_number_not_their_text() {
    let mut ids = vec!["VD-10", "VD-9", "VD-100", "VD-1"];
    ids.sort_by(|left, right| compare_ids(left, right));
    assert_eq!(ids, ["VD-1", "VD-9", "VD-10", "VD-100"]);
}

#[test]
fn a_records_type_comes_from_its_type_field_and_otherwise_its_id_prefix() {
    let settings = RegisterSettings::default();
    assert_eq!(classify("question", "VD-004", &settings), Some(RecordType::Question));
    assert_eq!(classify("", "VQ-001", &settings), Some(RecordType::Question));
    assert_eq!(classify("", "vd-002", &settings), Some(RecordType::Decision));
    assert_eq!(classify("note", "VD-003", &settings), Some(RecordType::Decision));
    assert_eq!(
        classify("", "VDX-1", &settings),
        None,
        "a prefix only matches up to its own dash"
    );
    assert_eq!(classify("", "VD", &settings), None);
    assert_eq!(classify("", "", &settings), None);
    assert_eq!(
        classify("", "Vé-1", &settings),
        None,
        "a multi-byte id never splits a character"
    );
}

#[test]
fn a_row_outside_the_register_folder_is_not_a_record() -> Result<(), Box<dyn std::error::Error>> {
    let settings = RegisterSettings::default();
    let question = object(&json!({
        "file.path": "memory/decisions/vq-001.md", "id": "VQ-001", "status": "open",
        "updated": "2026-09-29 20:48:31 +00:00:00", "decision": "VD-001", "superseded_by": "VD-009"
    }));
    let row = record_row(&settings, &question).ok_or("expected a record")?;
    assert_eq!(row.stem, "vq-001");
    assert_eq!(row.record_type, Some(RecordType::Question));
    assert_eq!(row.related, "VD-001", "a question's onward link is its decision");
    assert_eq!(row.updated, "2026-09-29");
    let outside = object(&json!({"file.path": "memory/decisions-old/vd-001.md"}));
    assert!(record_row(&settings, &outside).is_none());
    Ok(())
}

#[test]
fn a_decision_form_patches_only_the_fields_that_change() -> Result<(), String> {
    let current = object(&json!({
        "id": "VD-004", "title": "Keep one file per record", "status": "adopted", "date": "2026-08-07",
        "applies_to": ["memory/decisions/"], "superseded_by": null
    }));
    let form = object(&json!({
        "title": " Keep one file per record ", "status": "superseded", "date": "2026-08-07",
        "superseded_by": "VD-005", "applies_to": "memory/decisions/\n\n memory/questions/ ", "description": ""
    }));
    let decisions = ["VD-005".to_owned()];
    let patch = validate_fields(Some(RecordType::Decision), &form, &context(&current, &decisions, &[]))?;
    assert_eq!(
        serde_json::Value::Object(patch),
        json!({
            "status": "superseded", "superseded_by": "VD-005",
            "applies_to": ["memory/decisions/", "memory/questions/"]
        })
    );
    Ok(())
}

#[test]
fn an_unchanged_form_patches_nothing() -> Result<(), String> {
    let current = object(&json!({"title": "T", "status": "open", "blocks": "nothing"}));
    let form = object(&json!({
        "title": "T", "status": "open", "blocks": "nothing", "description": "", "resolved": ""
    }));
    let patch = validate_fields(Some(RecordType::Question), &form, &context(&current, &[], &[]))?;
    assert_eq!(patch, Map::new());
    Ok(())
}

#[test]
fn a_stored_timestamp_in_a_date_field_survives_an_edit_that_leaves_it_alone() -> Result<(), String> {
    let current = object(&json!({"title": "T", "status": "open", "resolved": "2026-10-01T03:12:00Z"}));
    let form = object(&json!({"title": "T", "status": "resolved", "resolved": "2026-10-01T03:12:00Z"}));
    let patch = validate_fields(Some(RecordType::Question), &form, &context(&current, &[], &[]))?;
    assert_eq!(serde_json::Value::Object(patch), json!({"status": "resolved"}));
    Ok(())
}

#[test]
fn the_server_managed_fields_can_never_be_submitted() {
    for key in ["id", "type", "created", "updated"] {
        let form = object(&json!({"title": "T", "status": "open", key: "x"}));
        assert_eq!(
            validate_fields(Some(RecordType::Question), &form, &context(&Map::new(), &[], &[])),
            Err(format!("`{key}` is not a field of this record."))
        );
    }
}

#[test]
fn a_required_field_cannot_be_cleared() {
    let form = object(&json!({"title": "", "status": "open"}));
    assert_eq!(
        validate_fields(Some(RecordType::Question), &form, &context(&Map::new(), &[], &[])),
        Err("Title is required.".to_owned())
    );
}

#[test]
fn a_status_outside_the_vocabulary_is_refused_unless_the_record_already_held_it() {
    let form = object(&json!({"title": "T", "status": "deferred"}));
    assert!(validate_fields(Some(RecordType::Decision), &form, &context(&Map::new(), &[], &[])).is_err());
    let current = object(&json!({"status": "deferred"}));
    assert!(validate_fields(Some(RecordType::Decision), &form, &context(&current, &[], &[])).is_ok());
}

#[test]
fn an_unclassified_record_takes_any_status_text() {
    let form = object(&json!({"title": "T", "status": "parked"}));
    assert!(validate_fields(None, &form, &context(&Map::new(), &[], &[])).is_ok());
}

#[test]
fn a_link_must_name_a_real_decision() {
    let form = object(&json!({"title": "T", "status": "resolved", "decision": "VD-999"}));
    let decisions = ["VD-001".to_owned()];
    assert_eq!(
        validate_fields(
            Some(RecordType::Question),
            &form,
            &context(&Map::new(), &decisions, &[])
        ),
        Err("Decision must name a decision in the register.".to_owned())
    );
}

#[test]
fn a_new_date_must_be_a_real_calendar_date() {
    for bad in ["2026-13-01", "2026-02-30", "07/08/2026", "2026-8-7"] {
        let form = object(&json!({"title": "T", "status": "open", "resolved": bad}));
        assert!(
            validate_fields(Some(RecordType::Question), &form, &context(&Map::new(), &[], &[])).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn an_entity_must_be_configured_when_the_vault_configures_any() {
    let form = object(&json!({"title": "T", "status": "open", "entity": "elsewhere"}));
    let entities = ["zenith".to_owned()];
    assert!(validate_fields(Some(RecordType::Question), &form, &context(&Map::new(), &[], &entities)).is_err());
    assert!(validate_fields(Some(RecordType::Question), &form, &context(&Map::new(), &[], &[])).is_ok());
}

#[test]
fn settled_records_read_as_done_open_questions_as_needing_attention() {
    assert_eq!(status_tone("adopted"), "done");
    assert_eq!(status_tone("resolved"), "done");
    assert_eq!(status_tone("open"), "attention");
    assert_eq!(status_tone("superseded"), "neutral");
    assert_eq!(status_tone("unrecognised"), "neutral");
}
