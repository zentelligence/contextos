use serde_json::json;

use super::*;

/// A fixture is always a JSON object literal; anything else yields an empty map, which the
/// calling test's own assertions then fail on.
fn frontmatter(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn values(choices: &[TaskTypeChoice]) -> Vec<&str> {
    choices.iter().map(|choice| choice.value.as_str()).collect()
}

fn types() -> Vec<TaskTypeEntry> {
    crate::tasks_config::TasksConfig::default().task_type
}

#[test]
fn declared_prefixes_are_the_keys_of_the_key_prefixes_map_in_order() {
    let sidecar = frontmatter(json!({"key_prefix": "A", "key_prefixes": {"A": 4, "D": 1, "Q": 0}}));

    assert_eq!(declared_prefixes(&sidecar), vec!["A", "D", "Q"]);
}

#[test]
fn a_single_series_sidecar_declares_just_its_one_key_prefix() {
    let sidecar = frontmatter(json!({"key_prefix": "A", "last_id": 7}));

    assert_eq!(declared_prefixes(&sidecar), vec!["A"]);
}

#[test]
fn a_sidecar_declaring_nothing_declares_no_prefixes() {
    assert!(
        declared_prefixes(&frontmatter(json!({"key_format": "FILEPATH"}))).is_empty(),
        "`declared_prefixes(&frontmatter(json!({{\"key_format\": \"FILEPATH\"}})))` should be empty, got {:?}",
        declared_prefixes(&frontmatter(json!({"key_format": "FILEPATH"})))
    );
    assert!(
        declared_prefixes(&frontmatter(json!({"key_prefixes": "A"}))).is_empty(),
        "`declared_prefixes(&frontmatter(json!({{\"key_prefixes\": \"A\"}})))` should be empty, got {:?}",
        declared_prefixes(&frontmatter(json!({"key_prefixes": "A"})))
    );
}

#[test]
fn only_the_types_whose_prefix_is_declared_are_available_in_vocabulary_order() {
    let declared = vec!["Q".to_owned(), "A".to_owned()];

    let available = available_task_types(&types(), &declared);

    assert_eq!(values(&available), vec!["action", "question"]);
    assert_eq!(available.first().and_then(|choice| choice.prefix.as_deref()), Some("A"));
    assert_eq!(available.get(1).and_then(|choice| choice.prefix.as_deref()), Some("Q"));
}

#[test]
fn prefixes_are_matched_exactly_not_case_insensitively() {
    let declared = vec!["a".to_owned(), "d".to_owned()];

    let available = available_task_types(&types(), &declared);

    assert_eq!(values(&available), vec!["action"]);
    assert_eq!(available.first().and_then(|choice| choice.prefix.as_deref()), None);
}

#[test]
fn a_sidecar_declaring_none_of_the_known_prefixes_falls_back_to_a_plain_action() {
    for declared in [vec![], vec!["TASK".to_owned()]] {
        let available = available_task_types(&types(), &declared);

        assert_eq!(values(&available), vec!["action"]);
        assert_eq!(available.first().and_then(|choice| choice.prefix.as_deref()), None);
    }
}

#[test]
fn the_default_type_is_action_when_available_and_otherwise_the_first() {
    let with_action = available_task_types(&types(), &["D".to_owned(), "A".to_owned()]);
    let without_action = available_task_types(&types(), &["Q".to_owned(), "D".to_owned()]);

    assert_eq!(
        default_task_type(&with_action).map(|choice| choice.value.as_str()),
        Some("action")
    );
    assert_eq!(
        default_task_type(&without_action).map(|choice| choice.value.as_str()),
        Some("decision")
    );
}

#[test]
fn a_configured_vocabulary_decides_which_types_and_prefixes_are_offered() {
    let types = vec![
        TaskTypeEntry {
            value: "chore".to_owned(),
            label: Some("Chore".to_owned()),
            prefix: Some("C".to_owned()),
            initial: None,
            stage: Vec::new(),
        },
        TaskTypeEntry {
            value: "action".to_owned(),
            label: None,
            prefix: Some("A".to_owned()),
            initial: None,
            stage: Vec::new(),
        },
    ];
    let available = available_task_types(&types, &["A".to_owned(), "C".to_owned()]);
    assert_eq!(values(&available), vec!["chore", "action"]);
    assert_eq!(available[0].label, "Chore");
    assert_eq!(
        default_task_type(&available).map(|choice| choice.value.as_str()),
        Some("chore")
    );
}
