use super::*;

use crate::package_release_manifest::InterfaceKind;

fn field(name: &str, kind: InterfaceKind, required: bool) -> InterfaceField {
    InterfaceField {
        name: name.to_owned(),
        kind,
        required,
    }
}

fn interface(inputs: Vec<InterfaceField>, outputs: Vec<InterfaceField>) -> WorkflowInterface {
    WorkflowInterface {
        slug: "sync".to_owned(),
        inputs,
        outputs,
        tools: Vec::new(),
        server_allow: None,
        server_deny: Vec::new(),
        tool_allow: Vec::new(),
        tool_deny: Vec::new(),
    }
}

fn change(side: FieldSide, name: &str, change: FieldChange) -> InterfaceChange {
    InterfaceChange {
        side,
        name: name.to_owned(),
        change,
    }
}

#[test]
fn identical_interfaces_have_no_changes() {
    let same = interface(
        vec![field("since", InterfaceKind::Date, true)],
        vec![field("count", InterfaceKind::Number, false)],
    );
    assert_eq!(same.changes_since(&same.clone()), Vec::new());
}

#[test]
fn an_input_added_is_reported_with_whether_it_is_required() {
    let earlier = interface(Vec::new(), Vec::new());
    let now = interface(
        vec![
            field("since", InterfaceKind::Date, true),
            field("limit", InterfaceKind::Number, false),
        ],
        Vec::new(),
    );
    assert_eq!(
        now.changes_since(&earlier),
        vec![
            change(FieldSide::Input, "since", FieldChange::Added { required: true }),
            change(FieldSide::Input, "limit", FieldChange::Added { required: false }),
        ]
    );
}

#[test]
fn an_input_removed_is_reported() {
    let earlier = interface(vec![field("since", InterfaceKind::Date, true)], Vec::new());
    let now = interface(Vec::new(), Vec::new());
    assert_eq!(
        now.changes_since(&earlier),
        vec![change(FieldSide::Input, "since", FieldChange::Removed)]
    );
}

#[test]
fn an_input_that_changes_type_or_requirement_is_reported_for_what_changed() {
    let earlier = interface(
        vec![
            field("since", InterfaceKind::Date, false),
            field("limit", InterfaceKind::Number, true),
            field("note", InterfaceKind::String, true),
        ],
        Vec::new(),
    );
    let now = interface(
        vec![
            field("since", InterfaceKind::Date, true),
            field("limit", InterfaceKind::Number, false),
            field("note", InterfaceKind::Bool, true),
        ],
        Vec::new(),
    );
    assert_eq!(
        now.changes_since(&earlier),
        vec![
            change(FieldSide::Input, "since", FieldChange::BecameRequired),
            change(FieldSide::Input, "limit", FieldChange::BecameOptional),
            change(FieldSide::Input, "note", FieldChange::TypeChanged),
        ]
    );
}

#[test]
fn outputs_follow_inputs_and_are_told_apart_from_them() {
    let earlier = interface(
        vec![field("since", InterfaceKind::Date, true)],
        vec![field("count", InterfaceKind::Number, false)],
    );
    let now = interface(Vec::new(), vec![field("total", InterfaceKind::Number, false)]);
    assert_eq!(
        now.changes_since(&earlier),
        vec![
            change(FieldSide::Input, "since", FieldChange::Removed),
            change(FieldSide::Output, "count", FieldChange::Removed),
            change(FieldSide::Output, "total", FieldChange::Added { required: false }),
        ]
    );
}

#[test]
fn removed_and_changed_fields_come_before_added_ones() {
    let earlier = interface(vec![field("a", InterfaceKind::String, true)], Vec::new());
    let now = interface(
        vec![
            field("b", InterfaceKind::String, true),
            field("a", InterfaceKind::Number, true),
        ],
        Vec::new(),
    );
    assert_eq!(
        now.changes_since(&earlier),
        vec![
            change(FieldSide::Input, "a", FieldChange::TypeChanged),
            change(FieldSide::Input, "b", FieldChange::Added { required: true }),
        ]
    );
}

#[test]
fn a_change_reads_as_a_line_naming_the_field() {
    let lines: Vec<String> = [
        change(FieldSide::Input, "since", FieldChange::Added { required: true }),
        change(FieldSide::Input, "limit", FieldChange::Added { required: false }),
        change(FieldSide::Output, "count", FieldChange::Removed),
        change(FieldSide::Input, "note", FieldChange::TypeChanged),
        change(FieldSide::Input, "a", FieldChange::BecameRequired),
        change(FieldSide::Input, "b", FieldChange::BecameOptional),
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    assert_eq!(
        lines,
        [
            "input since added, and required",
            "input limit added, and optional",
            "output count removed",
            "input note has another type",
            "input a is now required",
            "input b is now optional",
        ]
    );
}
