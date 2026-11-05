use serde_json::json;

use super::*;

fn scope(filters: Value) -> TaskScope {
    TaskScope { filters: Some(filters) }
}

fn tasks_scope() -> TaskScope {
    scope(json!("file.inFolder(\"memory/tasks\") && file.basename != \"index\""))
}

fn frontmatter(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

#[test]
fn only_markdown_notes_inside_the_bases_folder_could_be_tasks() {
    let scope = tasks_scope();

    assert!(scope.holds("memory/tasks/a-001.md"));
    assert!(scope.holds("memory/tasks/acme/a-002.md"));
    assert!(!scope.holds("memory/tasks"));
    assert!(!scope.holds("memory/tasks/a-001.png"));
    assert!(!scope.holds("memory/tasks-archive/a-001.md"));
    assert!(!scope.holds("notes/a-001.md"));
}

#[test]
fn a_base_that_names_no_folder_could_hold_a_task_anywhere() {
    let scope = scope(json!("note.type == \"task\""));

    assert!(scope.holds("anywhere/at/all.md"));
    assert!(!scope.holds("anywhere/at/all.txt"));
}

#[test]
fn the_full_filter_decides_which_notes_the_base_selects() {
    let scope = tasks_scope();
    let task = frontmatter(json!({"type": "task"}));

    assert!(scope.selects("memory/tasks/a-001.md", &task, 10));
    assert!(
        !scope.selects("memory/tasks/index.md", &task, 10),
        "the filter leaves out index notes"
    );
}

#[test]
fn a_filter_outside_the_grammar_selects_nothing() {
    let scope = scope(json!("formula.anything == 1"));

    assert!(!scope.selects("memory/tasks/a-001.md", &frontmatter(json!({})), 10));
}

fn parse(text: &str) -> Option<TaskScope> {
    TaskScope::try_from(text).ok()
}

#[test]
fn the_cache_parses_the_base_once_until_its_text_changes() {
    let cache = ScopeCache::default();
    let parses = std::cell::Cell::new(0);
    let counted = |text: &str| {
        parses.set(parses.get() + 1);
        parse(text)
    };
    let first = "filters: file.inFolder(\"memory/tasks\")\n";

    assert!(cache.get(Some(first), counted).is_some());
    assert!(cache.get(Some(first), counted).is_some());
    assert_eq!(parses.get(), 1);

    let second = "filters: file.inFolder(\"memory/tasks\") && file.basename != \"index\"\n";
    assert!(cache.get(Some(second), counted).is_some());
    assert_eq!(parses.get(), 2, "a changed base is parsed again");
}

#[test]
fn an_edit_that_keeps_the_length_is_still_noticed() {
    let cache = ScopeCache::default();
    let before = "filters: file.inFolder(\"memory/tasks\")\n";
    let after = "filters: file.inFolder(\"memory/other\")\n";
    assert_eq!(before.len(), after.len(), "the premise: same length");

    let first = cache.get(Some(before), parse);
    let second = cache.get(Some(after), parse);

    assert!(first.is_some_and(|scope| scope.holds("memory/tasks/a.md")));
    assert!(second.is_some_and(|scope| scope.holds("memory/other/a.md") && !scope.holds("memory/tasks/a.md")));
}

#[test]
fn a_missing_base_means_no_scope_and_nothing_is_parsed() {
    let cache = ScopeCache::default();
    let parsed = std::cell::Cell::new(false);

    let found = cache.get(None, |_| {
        parsed.set(true);
        None
    });

    assert!(found.is_none());
    assert!(!parsed.get(), "a missing base must not be parsed");
}

#[test]
fn a_folder_may_reach_tasks_when_it_is_the_bases_folder_or_lies_on_either_side_of_it() {
    let scope = tasks_scope();

    for folder in [
        "memory",
        "memory/tasks",
        "memory/tasks/2026.Q1",
        "memory/tasks/acme/archive",
    ] {
        assert!(scope.may_reach(folder), "{folder}");
    }
    for folder in ["notes", "memory/tasks-archive", "memory/events", "memory/tasks2"] {
        assert!(!scope.may_reach(folder), "{folder}");
    }
    assert!(
        super::TaskScope {
            filters: Some(json!("note.type == \"task\""))
        }
        .may_reach("anywhere"),
        "a base that names no folder could hold a task anywhere"
    );
}
