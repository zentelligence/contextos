use serde_json::{Map, Value, json};

use super::{FieldContext, build_fields, extract_body, field_value_string};
use crate::entities::EntityEntry;
use crate::tasks_config::TasksConfig;

fn empty_context() -> FieldContext<'static> {
    // A `TasksConfig::default` (leaked once, for `'static`) matches every
    // other "no vault-specific configuration present" test fixture in this
    // module: the default stage list is never empty, so `state` still
    // renders as a real select even with no other vault data configured.
    static DEFAULT_CONFIG: std::sync::OnceLock<TasksConfig> = std::sync::OnceLock::new();
    FieldContext {
        entities: &[],
        team: &[],
        tasks_config: DEFAULT_CONFIG.get_or_init(TasksConfig::default),
    }
}

#[test]
fn field_value_string_renders_null_and_absent_as_empty() {
    assert_eq!(field_value_string(None), "");
    assert_eq!(field_value_string(Some(&Value::Null)), "");
}

#[test]
fn field_value_string_joins_an_array_with_a_comma_and_space() {
    let value = json!(["alpha", "beta"]);
    assert_eq!(field_value_string(Some(&value)), "alpha, beta");
}

#[test]
fn field_value_string_renders_a_bare_string_and_number_verbatim() {
    assert_eq!(field_value_string(Some(&json!("piper"))), "piper");
    assert_eq!(field_value_string(Some(&json!(3))), "3");
}

fn frontmatter(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

#[test]
fn build_fields_puts_title_in_its_own_group_and_state_in_identity() -> Result<(), Box<dyn std::error::Error>> {
    let groups = build_fields(&frontmatter(&[("title", json!("Only title set"))]), &empty_context());
    assert_eq!(groups.title.key, "title");
    let state = groups
        .identity
        .iter()
        .find(|f| f.key == "state")
        .ok_or("state field present in identity group")?;
    assert_eq!(state.value, "");
    Ok(())
}

#[test]
fn build_fields_renders_a_documented_array_field_as_its_comma_joined_value() -> Result<(), Box<dyn std::error::Error>> {
    let groups = build_fields(&frontmatter(&[("tags", json!(["ops", "urgent"]))]), &empty_context());
    let tags = groups
        .remaining
        .iter()
        .find(|f| f.key == "tags")
        .ok_or("field present")?;
    assert_eq!(tags.value, "ops, urgent");
    Ok(())
}

#[test]
fn raw_fields_to_patch_splits_a_documented_array_field_back_into_a_list() {
    let raw = frontmatter(&[("tags", json!("ops, urgent"))]);
    let patch = super::raw_fields_to_patch(raw, None);
    assert_eq!(patch.get("tags"), Some(&json!(["ops", "urgent"])));
}

#[test]
fn raw_fields_to_patch_clears_a_blank_optional_field() {
    let raw = frontmatter(&[("priority", json!(""))]);
    let patch = super::raw_fields_to_patch(raw, None);
    assert_eq!(patch.get("priority"), Some(&Value::Null));
}

#[test]
fn raw_fields_to_patch_keeps_a_blank_required_field_as_an_empty_string() {
    let raw = frontmatter(&[("title", json!(""))]);
    let patch = super::raw_fields_to_patch(raw, None);
    assert_eq!(patch.get("title"), Some(&json!("")));
}

#[test]
fn raw_fields_to_patch_wraps_a_link_field_id_into_a_wikilink() {
    let mut raw = frontmatter(&[("parent", json!("t-002"))]);
    raw.insert("parent__present".to_owned(), json!("1"));
    let patch = super::raw_fields_to_patch(raw, None);
    assert_eq!(patch.get("parent"), Some(&json!("[[t-002|T-002]]")));
}

#[test]
fn raw_fields_to_patch_clears_a_link_field_whose_every_chip_was_removed() {
    let mut raw = Map::new();
    raw.insert("parent__present".to_owned(), json!("1"));
    let patch = super::raw_fields_to_patch(raw, None);
    assert_eq!(patch.get("parent"), Some(&Value::Null));
}

#[test]
fn raw_fields_to_patch_leaves_a_link_field_untouched_when_its_form_never_rendered_it() {
    let patch = super::raw_fields_to_patch(Map::new(), None);
    assert_eq!(patch.get("parent"), None);
}

#[test]
fn raw_fields_to_patch_classifies_an_unknown_field_by_its_current_array_ness() {
    let mut raw = Map::new();
    raw.insert("custom_list".to_owned(), json!("a, b"));
    let mut current = Map::new();
    current.insert("custom_list".to_owned(), json!(["existing"]));
    let patch = super::raw_fields_to_patch(raw, Some(&current));
    assert_eq!(patch.get("custom_list"), Some(&json!(["a", "b"])));
}

/// Reproduces the corrupted `q: ", , ,"` frontmatter observed in real task
/// notes: the four link-field search boxes in `task_link_fields.html` all
/// share `name="q"`, so htmx's `json-enc` extension collapses their four
/// (typically blank) values into one `"q": ["", "", "", ""]` array on every
/// "Save fields" submission. A raw key the note's own current frontmatter
/// never had is stray client-side widget noise, not a genuine vault-specific
/// field the operator edited through the generic Remaining-fields UI, so it
/// must never be swept into the patch at all - unlike
/// `raw_fields_to_patch_classifies_an_unknown_field_by_its_current_array_ness`,
/// where the key already exists in `current_frontmatter`.
#[test]
fn raw_fields_to_patch_ignores_a_raw_field_the_note_never_had() {
    let mut raw = Map::new();
    raw.insert("q".to_owned(), json!(["", "", "", ""]));
    let current = Map::new();
    let patch = super::raw_fields_to_patch(raw, Some(&current));
    assert_eq!(patch.get("q"), None);
}

#[test]
fn build_fields_never_renders_the_four_task_link_fields_generically() {
    let groups = build_fields(
        &frontmatter(&[
            ("parent", json!("[[t-001|T-001]]")),
            ("depends_on", json!(["[[t-002|T-002]]"])),
            ("blocks", json!(["[[t-003|T-003]]"])),
            ("related", json!(["[[t-004|T-004]]"])),
        ]),
        &empty_context(),
    );
    for key in ["parent", "depends_on", "blocks", "related"] {
        assert!(
            !groups.all().any(|f| f.key == key),
            "{key} is a link field and must render only through the link widget, not the generic field grid"
        );
    }
}

#[test]
fn build_fields_still_renders_waiting_on_as_a_plain_text_field() -> Result<(), Box<dyn std::error::Error>> {
    let groups = build_fields(
        &frontmatter(&[("waiting_on", json!("Legal counsel"))]),
        &empty_context(),
    );
    let waiting_on = groups
        .remaining
        .iter()
        .find(|f| f.key == "waiting_on")
        .ok_or("field present")?;
    assert_eq!(waiting_on.value, "Legal counsel");
    assert!(matches!(waiting_on.widget, super::FieldWidget::Text));
    Ok(())
}

#[test]
fn build_fields_never_renders_id_type_or_updated() {
    let groups = build_fields(
        &frontmatter(&[
            ("id", json!("T-001")),
            ("type", json!("task")),
            ("updated", json!("2026-09-13")),
        ]),
        &empty_context(),
    );
    assert!(
        !groups
            .all()
            .any(|f| f.key == "id" || f.key == "type" || f.key == "updated")
    );
}

#[test]
fn build_fields_appends_an_unknown_frontmatter_key_to_the_remaining_group() -> Result<(), Box<dyn std::error::Error>> {
    let groups = build_fields(&frontmatter(&[("custom_field", json!("kept"))]), &empty_context());
    let custom = groups
        .remaining
        .iter()
        .find(|f| f.key == "custom_field")
        .ok_or("unknown field kept")?;
    assert_eq!(custom.value, "kept");
    Ok(())
}

#[test]
fn build_fields_renders_a_date_input_for_every_lifecycle_field() -> Result<(), Box<dyn std::error::Error>> {
    let groups = build_fields(&frontmatter(&[("due", json!("2026-09-20"))]), &empty_context());
    let due = groups
        .dates
        .iter()
        .find(|f| f.key == "due")
        .ok_or("due field present")?;
    assert!(matches!(due.widget, super::FieldWidget::Date));
    assert_eq!(due.value, "2026-09-20");
    Ok(())
}

#[test]
fn build_fields_falls_back_entity_to_text_when_the_vault_has_no_configured_entities()
-> Result<(), Box<dyn std::error::Error>> {
    let groups = build_fields(&frontmatter(&[("entity", json!("personal"))]), &empty_context());
    let entity = groups
        .identity
        .iter()
        .find(|f| f.key == "entity")
        .ok_or("entity field present")?;
    assert!(matches!(entity.widget, super::FieldWidget::Text));
    Ok(())
}

#[test]
fn build_fields_renders_entity_as_a_select_backed_by_the_configured_entity_list()
-> Result<(), Box<dyn std::error::Error>> {
    let entities = [EntityEntry {
        slug: "personal".to_owned(),
        name: "Personal".to_owned(),
        accent: 1,
    }];
    let context = FieldContext {
        entities: &entities,
        team: &[],
        tasks_config: &TasksConfig::default(),
    };
    let groups = build_fields(&frontmatter(&[("entity", json!("personal"))]), &context);
    let entity = groups
        .identity
        .iter()
        .find(|f| f.key == "entity")
        .ok_or("entity field present")?;
    let super::FieldWidget::Select(options) = &entity.widget else {
        return Err("expected entity to render as a select".into());
    };
    assert_eq!(options.len(), 1);
    assert_eq!(options[0].value, "personal");
    assert_eq!(options[0].label, "Personal");
    assert!(options[0].selected);
    Ok(())
}

#[test]
fn extract_body_starts_one_line_before_the_reported_body_start_line() {
    let full_text = "---\ntitle: x\n---\n\n# Heading\nBody text.\n";
    // Line 1 "---", line 2 "title: x", line 3 "---", line 4 "" (blank),
    // line 5 "# Heading": `frontmatter_read`'s own 1-indexed convention
    // names line 4 (the blank line right after the closing delimiter) as
    // `body_start_line`, confirmed against a real note.
    let body = extract_body(full_text, 4);
    assert_eq!(body, "\n# Heading\nBody text.\n");
}

#[test]
fn extract_body_returns_empty_for_a_body_start_line_past_the_end_of_the_file() {
    assert_eq!(extract_body("---\ntitle: x\n---\n", 100), "");
}
