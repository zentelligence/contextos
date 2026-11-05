use serde_json::json;

use super::*;

fn fields(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

#[test]
fn the_shipped_configuration_round_trips_through_the_draft() -> Result<(), TasksConfigError> {
    let config = TasksConfig::default();
    assert_eq!(TasksConfig::try_from(&TasksDraft::from(&config))?, config);
    Ok(())
}

#[test]
fn rows_are_read_in_index_order_whatever_order_the_fields_arrive_in() -> Result<(), Malformed> {
    let submission = parse_submission(&fields(&json!({
        "stage.10.state": "done", "stage.2.state": "next", "stage.2.bucket": "backlog",
        "depth_label.1": "Action", "depth_label.0": "Initiative", "expected_hash": "h"
    })))?;
    let states: Vec<&str> = submission
        .draft
        .stages
        .iter()
        .map(|stage| stage.state.as_str())
        .collect();
    assert_eq!(states, ["next", "done"]);
    assert_eq!(submission.draft.depth_labels, ["Initiative", "Action"]);
    assert_eq!(submission.expected_hash, "h");
    Ok(())
}

#[test]
fn blank_optional_fields_are_absent_and_blank_depth_labels_are_dropped() -> Result<(), Box<dyn std::error::Error>> {
    let submission = parse_submission(&fields(&json!({
        "stage.0.state": "next", "stage.0.label": "  ", "stage.0.icon": "", "stage.0.bucket": "",
        "depth_label.0": "", "task_type.0.value": "action", "task_type.0.prefix": "", "priority.0.value": "high"
    })))?;
    let config = TasksConfig::try_from(&submission.draft)?;
    assert_eq!(config.stage[0].label, None);
    assert_eq!(config.stage[0].bucket, None);
    assert_eq!(config.hierarchy.depth_labels, Vec::<String>::new());
    assert_eq!(config.task_type[0].prefix, None);
    Ok(())
}

#[test]
fn a_types_own_stages_and_starting_state_are_read_from_its_nested_fields() -> Result<(), Box<dyn std::error::Error>> {
    let submission = parse_submission(&fields(&json!({
        "stage.0.state": "next", "task_type.0.value": "action",
        "task_type.1.value": "decision", "task_type.1.initial": "proposed",
        "task_type.1.stage.1.state": "adopted", "task_type.1.stage.1.closes": "completed",
        "task_type.1.stage.0.state": "proposed", "task_type.1.stage.0.bucket": "needs_call",
        "task_type.1.stage.0.closes": ""
    })))?;
    let config = TasksConfig::try_from(&submission.draft)?;
    let states: Vec<&str> = config
        .lifecycle("decision")
        .iter()
        .map(|stage| stage.state.as_str())
        .collect();
    assert_eq!(states, ["proposed", "adopted"]);
    assert_eq!(config.task_type[1].initial.as_deref(), Some("proposed"));
    assert_eq!(config.task_type[1].stage[0].closes, None);
    assert_eq!(config.task_type[1].stage[1].closes, Some(Closes::Completed));
    assert!(
        config.task_type[0].stage.is_empty(),
        "a type with no stage rows uses the shared stages"
    );
    Ok(())
}

#[test]
fn a_closes_value_the_form_never_offers_is_refused() -> Result<(), Malformed> {
    let submission = parse_submission(&fields(&json!({
        "stage.0.state": "next", "stage.0.closes": "finished", "task_type.0.value": "action"
    })))?;
    assert_eq!(
        TasksConfig::try_from(&submission.draft),
        Err(TasksConfigError::UnknownCloses("finished".to_owned()))
    );
    Ok(())
}

#[test]
fn edit_actions_reach_a_types_own_stages_and_give_or_take_them_away() -> Result<(), Malformed> {
    let mut draft = parse_submission(&fields(&json!({
        "stage.0.state": "backlog", "stage.1.state": "done",
        "task_type.0.value": "action",
        "task_type.1.value": "question", "task_type.1.stage.0.state": "open", "task_type.1.stage.1.state": "answered"
    })))?
    .draft;
    let own = |draft: &TasksDraft, index: usize| -> Vec<String> {
        draft.task_types[index]
            .stages
            .iter()
            .map(|stage| stage.state.clone())
            .collect()
    };
    draft.apply(EditAction::try_from("down:type_stage:1:0")?);
    assert_eq!(own(&draft, 1), ["answered", "open"]);
    draft.apply(EditAction::try_from("add:type_stage:1")?);
    assert_eq!(own(&draft, 1), ["answered", "open", ""]);
    draft.apply(EditAction::try_from("remove:type_stage:1:2")?);
    draft.apply(EditAction::try_from("up:type_stage:9:0")?);
    assert_eq!(own(&draft, 1), ["answered", "open"]);

    draft.apply(EditAction::try_from("own:task_type:0")?);
    assert_eq!(
        own(&draft, 0),
        ["backlog", "done"],
        "a type given its own stages starts from the shared ones"
    );
    draft.apply(EditAction::try_from("share:task_type:1")?);
    assert!(
        own(&draft, 1).is_empty(),
        "a type returned to the shared stages keeps none of its own"
    );
    draft.apply(EditAction::try_from("remove:type_stage:0:1")?);
    draft.apply(EditAction::try_from("remove:type_stage:0:0")?);
    assert!(
        own(&draft, 0).is_empty(),
        "removing a type's last own stage returns it to the shared stages"
    );
    Ok(())
}

#[test]
fn a_bucket_the_form_never_offers_is_refused() -> Result<(), Malformed> {
    let submission = parse_submission(&fields(&json!({
        "stage.0.state": "next", "stage.0.bucket": "someday", "task_type.0.value": "action"
    })))?;
    assert_eq!(
        TasksConfig::try_from(&submission.draft),
        Err(TasksConfigError::InvalidValue("someday".to_owned()))
    );
    Ok(())
}

#[test]
fn edit_actions_add_remove_and_move_rows_and_ignore_impossible_moves() -> Result<(), Malformed> {
    let mut draft = parse_submission(&fields(&json!({
        "priority.0.value": "high", "priority.1.value": "low"
    })))?
    .draft;
    draft.apply(EditAction::try_from("up:priority:0")?);
    draft.apply(EditAction::try_from("down:priority:1")?);
    draft.apply(EditAction::try_from("remove:priority:9")?);
    assert_eq!(
        draft
            .priorities
            .iter()
            .map(|row| row.value.as_str())
            .collect::<Vec<_>>(),
        ["high", "low"]
    );
    draft.apply(EditAction::try_from("down:priority:0")?);
    assert_eq!(
        draft
            .priorities
            .iter()
            .map(|row| row.value.as_str())
            .collect::<Vec<_>>(),
        ["low", "high"]
    );
    draft.apply(EditAction::try_from("add:depth_label")?);
    draft.apply(EditAction::try_from("remove:priority:1")?);
    assert_eq!(draft.depth_labels, [""]);
    assert_eq!(draft.priorities.len(), 1);
    Ok(())
}

#[test]
fn anything_the_form_could_not_have_submitted_is_malformed() {
    for bad in [
        json!({"stage.0.shade": "red"}),
        json!({"stage.x.state": "next"}),
        json!({"stage.0.state": ["next"]}),
        json!({"edit_action": "explode:stage"}),
        json!({"edit_action": "remove:stage"}),
        json!({"edit_action": "add:type_stage"}),
        json!({"edit_action": "own:task_type"}),
        json!({"task_type.0.stage.x.state": "open"}),
        json!({"task_type.0.stage.0.shade": "red"}),
        json!({"task_type.0.holding_state": "pending"}),
        json!({"task_type.0.review": "weekly"}),
        json!({"unexpected": "x"}),
    ] {
        assert!(parse_submission(&fields(&bad)).is_err(), "{bad}");
    }
}

#[test]
fn a_rows_position_marks_the_ends_of_its_list() {
    let first = position(0, 3);
    let last = position(2, 3);
    assert!(first.first && !first.last);
    assert!(last.last && !last.first);
    assert!(position(0, 1).first && position(0, 1).last);
}

#[test]
fn a_stage_colour_chosen_in_the_editor_is_kept_and_a_blank_one_falls_back_to_the_shipped_colour()
-> Result<(), Box<dyn std::error::Error>> {
    let submission = parse_submission(&fields(&json!({
        "stage.0.state": "review", "stage.0.colour": "red",
        "stage.1.state": "triage", "stage.1.colour": "",
        "task_type.0.value": "action", "priority.0.value": "high"
    })))?;
    let config = TasksConfig::try_from(&submission.draft)?;
    assert_eq!(config.stage[0].colour, Some(StageColour::Red));
    assert_eq!(config.stage[1].colour, None);
    assert_eq!(config.stage_colour("action", "triage"), StageColour::Grey);
    Ok(())
}

#[test]
fn a_colour_the_form_never_offers_is_refused() -> Result<(), Malformed> {
    let submission = parse_submission(&fields(&json!({
        "stage.0.state": "review", "stage.0.colour": "chartreuse", "task_type.0.value": "action"
    })))?;
    assert_eq!(
        TasksConfig::try_from(&submission.draft),
        Err(TasksConfigError::InvalidValue("chartreuse".to_owned()))
    );
    Ok(())
}
