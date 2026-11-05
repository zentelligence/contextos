use super::*;
use crate::tasks_config::TasksConfig;

type BoxError = Box<dyn std::error::Error>;

fn entity(slug: &str, name: &str) -> EntityEntry {
    EntityEntry {
        slug: slug.to_owned(),
        name: name.to_owned(),
        accent: 1,
    }
}

fn agent(slug: &str, name: &str) -> TeamMember {
    TeamMember {
        slug: slug.to_owned(),
        name: name.to_owned(),
        kind: crate::team::TeamMemberKind::default(),
        domain: None,
        team: None,
        chief_of_staff: false,
        state: crate::team::MemberState::default(),
    }
}

fn human(slug: &str, name: &str) -> TeamMember {
    TeamMember {
        kind: crate::team::TeamMemberKind::Human,
        ..agent(slug, name)
    }
}

fn select_options(widget: FieldWidget) -> Result<Vec<SelectOption>, BoxError> {
    match widget {
        FieldWidget::Select(options) => Ok(options),
        FieldWidget::Text | FieldWidget::Date => Err("expected a Select widget".into()),
    }
}

#[test]
fn entity_widget_falls_back_to_text_when_the_vault_has_no_configured_entities() {
    assert!(matches!(entity_widget(&[], ""), FieldWidget::Text));
}

#[test]
fn entity_widget_renders_slug_as_value_and_name_as_label() -> Result<(), BoxError> {
    let entities = vec![entity("personal", "Personal"), entity("family", "Family")];
    let options = select_options(entity_widget(&entities, "family"))?;
    assert_eq!(options.len(), 2);
    assert_eq!(options[0].value, "personal");
    assert_eq!(options[0].label, "Personal");
    assert!(!options[0].selected);
    assert_eq!(options[1].value, "family");
    assert!(options[1].selected);
    Ok(())
}

#[test]
fn assignee_widget_falls_back_to_text_when_the_vault_has_no_configured_team_members() {
    assert!(matches!(assignee_widget(&[], ""), FieldWidget::Text));
}

#[test]
fn assignee_widget_lists_every_configured_team_member_agent_and_human_alike() -> Result<(), BoxError> {
    let team = vec![agent("sam", "Sam"), human("piper", "Piper")];
    let options = select_options(assignee_widget(&team, "piper"))?;
    assert_eq!(options.len(), 2);
    assert_eq!(options[0].value, "sam");
    assert_eq!(options[1].value, "piper");
    assert_eq!(options[1].label, "Piper");
    assert!(options[1].selected);
    assert!(!options[0].selected);
    Ok(())
}

#[test]
fn assignee_widget_labels_each_member_with_their_domain_so_the_operator_sees_what_each_does() -> Result<(), BoxError> {
    let team = vec![
        TeamMember {
            domain: Some("Finance".to_owned()),
            ..agent("ledger", "Ledger")
        },
        TeamMember {
            domain: Some("  ".to_owned()),
            ..agent("blank", "Blank")
        },
        human("piper", "Piper"),
    ];

    let options = select_options(assignee_widget(&team, "ledger"))?;

    let labels: Vec<&str> = options.iter().map(|option| option.label.as_str()).collect();
    assert_eq!(labels, ["Ledger (Finance)", "Blank", "Piper"]);
    assert_eq!(
        options[0].value, "ledger",
        "the value stays the slug, so what is saved is unchanged"
    );
    assert!(options[0].selected);
    Ok(())
}

#[test]
fn assignee_widget_carries_no_hardcoded_literal_identity() -> Result<(), BoxError> {
    let team = vec![agent("sam", "Sam")];
    let options = select_options(assignee_widget(&team, ""))?;
    assert!(
        options.iter().all(|option| option.value != "piper"),
        "no option should exist that the operator did not configure"
    );
    Ok(())
}

#[test]
fn state_widget_falls_back_to_text_when_the_vault_has_no_configured_stages() {
    let mut config = TasksConfig::default();
    config.stage.clear();
    for entry in &mut config.task_type {
        entry.stage.clear();
    }
    assert!(matches!(state_widget(&config, "action", "doing"), FieldWidget::Text));
}

#[test]
fn state_widget_offers_the_tasks_own_states_first_then_every_other_types_labelled_with_its_type() -> Result<(), BoxError>
{
    let options = select_options(state_widget(&TasksConfig::default(), "decision", "adopted"))?;
    let listed: Vec<(&str, &str)> = options
        .iter()
        .map(|option| (option.value.as_str(), option.label.as_str()))
        .collect();
    assert_eq!(
        listed,
        [
            ("proposed", "Proposed"),
            ("adopted", "Adopted"),
            ("dropped", "Dropped"),
            ("backlog", "Backlog (Action)"),
            ("next", "Next (Action)"),
            ("doing", "Doing (Action)"),
            ("waiting", "Waiting (Action)"),
            ("review", "Review (Action)"),
            ("done", "Done (Action)"),
            ("open", "Open (Question)"),
            ("answered", "Answered (Question)"),
        ]
    );
    let selected: Vec<&str> = options
        .iter()
        .filter(|option| option.selected)
        .map(|option| option.value.as_str())
        .collect();
    assert_eq!(selected, ["adopted"]);
    Ok(())
}

#[test]
fn state_widget_keeps_a_state_no_type_declares_as_written() -> Result<(), BoxError> {
    let options = select_options(state_widget(&TasksConfig::default(), "decision", "pending"))?;
    let last = options.last().ok_or("an option")?;
    assert_eq!(
        (last.value.as_str(), last.label.as_str(), last.selected),
        ("pending", "pending", true)
    );
    Ok(())
}

#[test]
fn priority_widget_offers_a_blank_none_option_selected_when_priority_is_unset() -> Result<(), BoxError> {
    let options = select_options(priority_widget(&TasksConfig::default().priority, ""))?;
    assert_eq!(options[0].value, "");
    assert_eq!(options[0].label, "(none)");
    assert!(options[0].selected);
    Ok(())
}

#[test]
fn priority_widget_selects_the_matching_fixed_value() -> Result<(), BoxError> {
    let options = select_options(priority_widget(&TasksConfig::default().priority, "highest"))?;
    let highest = options.iter().find(|o| o.value == "highest").ok_or("highest present")?;
    assert!(highest.selected);
    assert_eq!(options.iter().filter(|o| o.selected).count(), 1);
    Ok(())
}

#[test]
fn task_type_widget_offers_exactly_action_decision_and_question_in_order() -> Result<(), BoxError> {
    let options = select_options(task_type_widget(&TasksConfig::default().task_type, "decision"))?;
    let values: Vec<&str> = options.iter().map(|option| option.value.as_str()).collect();
    assert_eq!(values, vec!["action", "decision", "question"]);
    assert!(options[1].selected);
    assert_eq!(options.iter().filter(|o| o.selected).count(), 1);
    Ok(())
}

#[test]
fn the_widgets_offer_a_vault_declared_vocabulary_rather_than_a_fixed_one() -> Result<(), BoxError> {
    let config: TasksConfig =
        toml::from_str("[[task_type]]\nvalue = \"chore\"\n\n[[priority]]\nvalue = \"urgent\"\nlabel = \"Urgent\"\n")?;
    let types = select_options(task_type_widget(&config.task_type, "chore"))?;
    assert_eq!(
        types.iter().map(|o| o.value.as_str()).collect::<Vec<_>>(),
        vec!["chore"]
    );
    let priorities = select_options(priority_widget(&config.priority, ""))?;
    assert_eq!(
        priorities.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(),
        vec!["(none)", "Urgent"]
    );
    Ok(())
}

#[test]
fn mark_selected_preserves_an_unrecognised_current_value_as_its_own_selected_option() -> Result<(), BoxError> {
    let options = select_options(task_type_widget(&TasksConfig::default().task_type, "project"))?;
    assert_eq!(options.len(), 4, "the legacy value must not be silently dropped");
    let legacy = options
        .iter()
        .find(|o| o.value == "project")
        .ok_or("legacy value kept")?;
    assert!(legacy.selected);
    assert_eq!(options.iter().filter(|o| o.selected).count(), 1);
    Ok(())
}

#[test]
fn state_widget_lists_each_shared_state_once_for_a_task_on_the_shared_stages() -> Result<(), BoxError> {
    let options = select_options(state_widget(&TasksConfig::default(), "bug", "doing"))?;
    let values: Vec<&str> = options.iter().map(|option| option.value.as_str()).collect();
    assert_eq!(
        values.iter().filter(|value| **value == "doing").count(),
        1,
        "{values:?}"
    );
    assert_eq!(
        values.first(),
        Some(&"backlog"),
        "its own, shared, states lead: {values:?}"
    );
    assert!(
        values.contains(&"proposed") && values.contains(&"answered"),
        "{values:?}"
    );
    Ok(())
}
