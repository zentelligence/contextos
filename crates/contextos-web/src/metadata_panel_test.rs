use super::*;

#[test]
fn metadata_field_views_shows_a_string_field_as_editable_verbatim() {
    let mut frontmatter = Map::new();
    frontmatter.insert("status".to_owned(), Value::String("draft".to_owned()));
    let fields = metadata_field_views(&frontmatter);
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].key, "status");
    assert_eq!(fields[0].value, "draft");
    assert!(fields[0].editable);
}

#[test]
fn metadata_field_views_shows_a_string_array_field_as_editable_comma_joined() {
    let mut frontmatter = Map::new();
    frontmatter.insert(
        "tags".to_owned(),
        Value::Array(vec![
            Value::String("note".to_owned()),
            Value::String("draft".to_owned()),
        ]),
    );
    let fields = metadata_field_views(&frontmatter);
    assert_eq!(fields[0].value, "note, draft");
    assert!(fields[0].editable);
}

#[test]
fn metadata_field_views_shows_created_and_updated_as_read_only_even_though_they_are_strings() {
    let mut frontmatter = Map::new();
    frontmatter.insert(
        "created".to_owned(),
        Value::String("2026-10-02T06:05:00+00:00".to_owned()),
    );
    frontmatter.insert(
        "updated".to_owned(),
        Value::String("2026-10-02T06:05:00+00:00".to_owned()),
    );
    let fields = metadata_field_views(&frontmatter);
    assert_eq!(fields[0].value, "2026-10-02T06:05:00+00:00");
    assert!(!fields[0].editable, "created should be read-only");
    assert_eq!(fields[1].value, "2026-10-02T06:05:00+00:00");
    assert!(!fields[1].editable, "updated should be read-only");
}

#[test]
fn metadata_field_views_shows_any_other_json_shape_as_read_only() {
    let mut frontmatter = Map::new();
    frontmatter.insert("priority".to_owned(), Value::Number(3.into()));
    let fields = metadata_field_views(&frontmatter);
    assert_eq!(fields[0].value, "3");
    assert!(!fields[0].editable);
}

#[test]
fn metadata_field_views_shows_a_boolean_field_as_read_only_json_text() {
    let mut frontmatter = Map::new();
    frontmatter.insert("archived".to_owned(), Value::Bool(true));
    let fields = metadata_field_views(&frontmatter);
    assert_eq!(fields[0].value, "true");
    assert!(!fields[0].editable);
}

#[test]
fn metadata_field_views_shows_a_mixed_array_field_as_read_only_json_text_rather_than_a_comma_joined_string() {
    let mut frontmatter = Map::new();
    frontmatter.insert(
        "mixed".to_owned(),
        Value::Array(vec![Value::String("a".to_owned()), Value::Number(1.into())]),
    );
    let fields = metadata_field_views(&frontmatter);
    assert_eq!(fields[0].value, "[\"a\",1]");
    assert!(!fields[0].editable);
}

#[test]
fn metadata_field_views_preserves_the_frontmatter_map_own_insertion_order() {
    let mut frontmatter = Map::new();
    frontmatter.insert("zeta".to_owned(), Value::String("z".to_owned()));
    frontmatter.insert("alpha".to_owned(), Value::String("a".to_owned()));
    let fields = metadata_field_views(&frontmatter);
    assert_eq!(fields[0].key, "zeta");
    assert_eq!(fields[1].key, "alpha");
}

#[test]
fn metadata_patch_splits_a_field_currently_an_array_back_into_one_and_leaves_a_plain_string_alone() {
    let mut current = Map::new();
    current.insert("tags".to_owned(), Value::Array(vec![Value::String("note".to_owned())]));
    current.insert("status".to_owned(), Value::String("draft".to_owned()));

    let mut submitted = std::collections::BTreeMap::new();
    submitted.insert("tags".to_owned(), "note, final , ".to_owned());
    submitted.insert("status".to_owned(), "final".to_owned());

    let patch = metadata_patch(&current, submitted);
    assert_eq!(
        patch.get("tags"),
        Some(&Value::Array(vec![
            Value::String("note".to_owned()),
            Value::String("final".to_owned()),
        ]))
    );
    assert_eq!(patch.get("status"), Some(&Value::String("final".to_owned())));
}

#[test]
fn metadata_patch_treats_a_currently_string_field_as_a_plain_string_even_when_the_submitted_text_has_commas() {
    let mut current = Map::new();
    current.insert("title".to_owned(), Value::String("Old title".to_owned()));

    let mut submitted = std::collections::BTreeMap::new();
    submitted.insert("title".to_owned(), "New, still one title".to_owned());

    let patch = metadata_patch(&current, submitted);
    assert_eq!(
        patch.get("title"),
        Some(&Value::String("New, still one title".to_owned()))
    );
}

#[test]
fn metadata_patch_writes_a_key_the_current_frontmatter_does_not_have_as_a_plain_string() {
    let current = Map::new();

    let mut submitted = std::collections::BTreeMap::new();
    submitted.insert("brand_new_key".to_owned(), "a, b, c".to_owned());

    let patch = metadata_patch(&current, submitted);
    assert_eq!(patch.get("brand_new_key"), Some(&Value::String("a, b, c".to_owned())));
}

// ---------------------------------------------------------------------------
// Links in a field
// ---------------------------------------------------------------------------

#[test]
fn a_string_that_is_one_wikilink_is_a_link_field() {
    let links = wikilink_values(&serde_json::json!("[[mcp/rec-007|REC-007]]"));

    assert_eq!(
        links.map(|links| links.into_iter().map(|link| link.target).collect::<Vec<_>>()),
        Some(vec!["mcp/rec-007".to_owned()])
    );
}

#[test]
fn an_array_of_wikilinks_is_a_link_field_in_order() {
    let links = wikilink_values(&serde_json::json!(["[[a]]", "[[b|B]]"]));

    assert_eq!(
        links.map(|links| links.into_iter().map(|link| link.target).collect::<Vec<_>>()),
        Some(vec!["a".to_owned(), "b".to_owned()])
    );
}

#[test]
fn a_field_is_a_link_field_only_when_every_item_is_a_wikilink() {
    for value in [
        serde_json::json!("plain"),
        serde_json::json!(["[[a]]", "plain"]),
        serde_json::json!([]),
        serde_json::json!(3),
        serde_json::json!(["[[a]]", 3]),
        serde_json::json!({"k": "[[a]]"}),
    ] {
        assert!(wikilink_values(&value).is_none(), "{value}");
    }
}

// ---------------------------------------------------------------------------
// Saving a link field whose entries contain commas
// ---------------------------------------------------------------------------

#[test]
fn a_link_list_with_commas_inside_its_links_round_trips_through_the_panel_unchanged() {
    let stored = serde_json::json!(["[[a/b|Fix, then ship]]", "[[c, d/e]]"]);
    let mut current = Map::new();
    current.insert("related".to_owned(), stored.clone());
    let shown = metadata_field_views(&current);
    let mut submitted = std::collections::BTreeMap::new();
    submitted.insert("related".to_owned(), shown[0].value.clone());

    let patch = metadata_patch(&current, submitted);

    assert_eq!(
        patch.get("related"),
        Some(&serde_json::json!(["[[a/b|Fix, then ship]]", "[[c, d/e]]"]))
    );
}

#[test]
fn an_edited_link_list_is_split_on_commas_between_links_only() {
    let mut current = Map::new();
    current.insert("related".to_owned(), serde_json::json!(["[[a]]"]));
    let mut submitted = std::collections::BTreeMap::new();
    submitted.insert("related".to_owned(), "[[a]],[[b|B, C]] ,  [[d]]".to_owned());

    let patch = metadata_patch(&current, submitted);

    assert_eq!(
        patch.get("related"),
        Some(&serde_json::json!(["[[a]]", "[[b|B, C]]", "[[d]]"]))
    );
}
