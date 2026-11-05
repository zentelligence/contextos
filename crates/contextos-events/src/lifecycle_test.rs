use super::*;

fn states(lifecycles: &TaskLifecycles, task_type: Option<&str>) -> Vec<String> {
    lifecycles.declared_states(task_type).to_vec()
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn the_shipped_lifecycles_cover_actions_decisions_and_questions() {
    let shipped = TaskLifecycles::default();
    let action = names(&["backlog", "next", "doing", "waiting", "review", "done", "dropped"]);
    assert_eq!(states(&shipped, Some("action")), action);
    assert_eq!(
        states(&shipped, Some("decision")),
        names(&["proposed", "adopted", "dropped"])
    );
    assert_eq!(
        states(&shipped, Some("question")),
        names(&["open", "answered", "dropped"])
    );
}

#[test]
fn a_task_with_no_type_or_an_undeclared_one_follows_the_shared_stages() {
    let shipped = TaskLifecycles::default();
    let shared = states(&shipped, Some("action"));
    assert_eq!(states(&shipped, None), shared);
    assert_eq!(states(&shipped, Some("epic")), shared);
}

#[test]
fn a_type_is_matched_without_regard_to_ascii_case() {
    let shipped = TaskLifecycles::default();
    assert_eq!(
        states(&shipped, Some("Decision")),
        names(&["proposed", "adopted", "dropped"])
    );
}

#[test]
fn a_configuration_replaces_the_shared_stages_and_the_declared_types() -> Result<(), EventsError> {
    let text = r#"
[[stage]]
state = "todo"
[[stage]]
state = "finished"
closes = "completed"

[[task_type]]
value = "chore"

[[task_type]]
value = "bug"
[[task_type.stage]]
state = "reported"
[[task_type.stage]]
state = "fixed"
"#;
    let lifecycles = TaskLifecycles::try_from(text)?;
    assert_eq!(states(&lifecycles, Some("chore")), names(&["todo", "finished"]));
    assert_eq!(states(&lifecycles, Some("bug")), names(&["reported", "fixed"]));
    assert_eq!(states(&lifecycles, Some("decision")), names(&["todo", "finished"]));
    Ok(())
}

#[test]
fn a_configuration_that_declares_no_types_keeps_the_shipped_ones() -> Result<(), EventsError> {
    let lifecycles = TaskLifecycles::try_from("[[stage]]\nstate = \"todo\"\n")?;
    assert_eq!(
        states(&lifecycles, Some("decision")),
        names(&["proposed", "adopted", "dropped"])
    );
    assert_eq!(states(&lifecycles, Some("action")), names(&["todo"]));
    Ok(())
}

#[test]
fn a_configuration_with_no_stages_keeps_the_shipped_shared_stages() -> Result<(), EventsError> {
    let lifecycles = TaskLifecycles::try_from("[hierarchy]\ndepth_labels = [\"Initiative\"]\n")?;
    assert_eq!(states(&lifecycles, None).len(), 7);
    Ok(())
}

#[test]
fn text_that_is_not_toml_or_has_the_wrong_shape_is_unreadable() {
    for text in ["= nonsense", "stage = 3", "[[stage]]\nlabel = \"no state\"\n"] {
        assert!(
            matches!(
                TaskLifecycles::try_from(text),
                Err(EventsError::TasksConfigUnreadable { .. })
            ),
            "{text}"
        );
    }
}
