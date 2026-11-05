use std::collections::HashMap;

use crate::tasks_config::TasksConfig;

use super::StageOverride;
use super::{CompletedOn, render_list};
use super::{
    DisplayMode, RowContext, SortKey, TaskRow, TasksQuery, ViewMode, display_href, done_href, effective_hide_done,
    fixed_filter_options, flat_rows, hierarchy_rows, matches_entity, matches_priority, matches_task_type, stage_groups,
    task_reference, tasks_href, wikilink_from_target, wikilink_target, wikilink_written_target,
};

/// The day the strip's Done today tile counts against; no test in this file closes a task on it.
const TODAY: time::Date = time::macros::date!(2026 - 10 - 13);

/// No test in this file cares about entity accents or state labels, so every call site shares one empty accent map
/// rather than each constructing its own unused `HashMap`.
fn context<'a>(config: &'a TasksConfig, accents: &'a HashMap<String, u8>) -> RowContext<'a> {
    RowContext { config, accents }
}

fn row(id: &str, state: &str, entity: &str, parent: Option<&str>) -> TaskRow {
    TaskRow {
        path: format!("memory/tasks/{}.md", id.to_ascii_lowercase()),
        id: id.to_owned(),
        title: format!("Title for {id}"),
        state: state.to_owned(),
        task_type: "action".to_owned(),
        entity: entity.to_owned(),
        priority: None,
        due: None,
        assignee: None,
        parent_id: parent.map(str::to_ascii_lowercase),
        completed: None,
        updated: None,
        attention: None,
    }
}

#[test]
fn wikilink_target_extracts_a_bare_link() {
    assert_eq!(wikilink_target("[[some-id]]").as_deref(), Some("some-id"));
}

#[test]
fn wikilink_target_extracts_the_target_before_a_display_alias() {
    assert_eq!(wikilink_target("[[some-id|Some Display]]").as_deref(), Some("some-id"));
}

#[test]
fn wikilink_target_lowercases_and_strips_a_trailing_extension() {
    assert_eq!(wikilink_target("[[SOME-ID.md]]").as_deref(), Some("some-id"));
}

#[test]
fn wikilink_target_is_none_for_a_non_wikilink_value() {
    assert_eq!(wikilink_target("plain text"), None);
    assert_eq!(wikilink_target(""), None);
}

#[test]
fn matches_entity_is_permissive_when_no_entity_is_selected() {
    assert!(matches_entity(&row("t-001", "next", "zenith", None), None));
}

#[test]
fn matches_entity_matches_case_insensitively() {
    let entry = row("t-001", "next", "Zenith", None);
    assert!(matches_entity(&entry, Some("zenith")));
    assert!(!matches_entity(&entry, Some("family")));
}

fn typed_row(id: &str, task_type: &str, state: &str) -> TaskRow {
    TaskRow {
        task_type: task_type.to_owned(),
        ..row(id, state, "zenith", None)
    }
}

#[test]
fn the_stage_strip_groups_each_types_open_stages_and_counts_its_rows() {
    let config = TasksConfig::default();
    let rows = vec![
        row("t-001", "next", "zenith", None),
        row("t-002", "next", "zenith", None),
        row("t-003", "doing", "zenith", None),
        typed_row("t-004", "action", "dropped"),
        typed_row("d-001", "decision", "proposed"),
        typed_row("d-002", "decision", "dropped"),
        typed_row("d-003", "decision", "dropped"),
    ];
    let groups = stage_groups("vault", &config, &rows, &TasksQuery::default(), TODAY);
    let labels: Vec<&str> = groups.iter().map(|group| group.label.as_str()).collect();
    assert_eq!(labels, ["Action", "Decision", "Question", "Today"]);
    let counts = |index: usize| -> Vec<(&str, usize)> {
        groups[index]
            .tiles
            .iter()
            .map(|tile| (tile.label.as_str(), tile.count))
            .collect()
    };
    assert_eq!(
        counts(0),
        [("Backlog", 0), ("Next", 2), ("Doing", 1), ("Waiting", 0), ("Review", 0)]
    );
    assert_eq!(counts(1), [("Proposed", 1)]);
    assert_eq!(counts(2), [("Open", 0)]);
}

#[test]
fn stage_tile_href_toggles_off_when_the_same_stage_is_clicked_again() {
    let config = TasksConfig::default();
    let rows = vec![row("t-001", "next", "zenith", None)];
    let inactive = stage_groups("vault", &config, &rows, &TasksQuery::default(), TODAY)
        .remove(0)
        .tiles;
    assert_eq!(inactive[1].href, "/vault/tasks/?stage=action:next");
    let active_query = TasksQuery {
        stage: Some("action:next".to_owned()),
        ..TasksQuery::default()
    };
    let active = stage_groups("vault", &config, &rows, &active_query, TODAY)
        .remove(0)
        .tiles;
    assert!(active[1].active);
    assert_eq!(active[1].href, "/vault/tasks/");
}

#[test]
fn stage_tile_href_preserves_the_selected_entity() {
    let config = TasksConfig::default();
    let rows = vec![row("t-001", "next", "zenith", None)];
    let query = TasksQuery {
        entity: Some("zenith".to_owned()),
        ..TasksQuery::default()
    };
    let tiles = stage_groups("vault", &config, &rows, &query, TODAY).remove(0).tiles;
    assert_eq!(tiles[1].href, "/vault/tasks/?stage=action:next&entity=zenith");
}

#[test]
fn stage_tile_href_preserves_the_current_view_mode() {
    let config = TasksConfig::default();
    let rows = vec![row("t-001", "next", "zenith", None)];
    let query = TasksQuery {
        view: Some("table".to_owned()),
        ..TasksQuery::default()
    };
    let tiles = stage_groups("vault", &config, &rows, &query, TODAY).remove(0).tiles;
    assert_eq!(tiles[1].href, "/vault/tasks/?view=table&stage=action:next");
}

#[test]
fn stage_tile_href_preserves_search_and_assignee_filters() {
    let config = TasksConfig::default();
    let rows = vec![row("t-001", "next", "zenith", None)];
    let query = TasksQuery {
        q: Some("ship".to_owned()),
        assignee: Some("piper".to_owned()),
        ..TasksQuery::default()
    };
    let tiles = stage_groups("vault", &config, &rows, &query, TODAY).remove(0).tiles;
    assert_eq!(tiles[1].href, "/vault/tasks/?stage=action:next&q=ship&assignee=piper");
}

#[test]
fn flat_rows_filters_by_stage_when_one_is_selected() {
    let rows = vec![
        row("t-001", "next", "zenith", None),
        row("t-002", "doing", "zenith", None),
    ];
    let config = TasksConfig::default();
    let accents = HashMap::new();
    let context = context(&config, &accents);
    let all = flat_rows("vault", &rows, None, false, SortKey::Id, &context);
    assert_eq!(all.len(), 2);
    let filtered = flat_rows("vault", &rows, Some("doing"), false, SortKey::Id, &context);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].id, "t-002");
}

#[test]
fn flat_rows_filters_by_any_state_in_a_comma_joined_stage_filter() {
    let rows = vec![
        row("t-001", "backlog", "zenith", None),
        row("t-002", "next", "zenith", None),
        row("t-003", "doing", "zenith", None),
    ];
    let config = TasksConfig::default();
    let accents = HashMap::new();
    let filtered = flat_rows(
        "vault",
        &rows,
        Some("backlog,next"),
        false,
        SortKey::Id,
        &context(&config, &accents),
    );
    let ids: Vec<&str> = filtered.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["t-001", "t-002"]);
}

#[test]
fn flat_rows_sorts_by_title_when_requested() {
    let rows = vec![
        row("t-002", "next", "zenith", None),
        row("t-001", "next", "zenith", None),
    ];
    let config = TasksConfig::default();
    let accents = HashMap::new();
    let sorted = flat_rows("vault", &rows, None, false, SortKey::Title, &context(&config, &accents));
    let ids: Vec<&str> = sorted.iter().map(|r| r.id.as_str()).collect();
    // `row()` titles each note "Title for {id}", so title order matches id order here; this asserts the title sort
    // ran on rows fetched out of order.
    assert_eq!(ids, vec!["t-001", "t-002"]);
}

#[test]
fn flat_rows_excludes_done_bucket_states_when_hide_done_is_set() {
    let rows = vec![
        row("t-001", "next", "zenith", None),
        row("t-002", "done", "zenith", None),
    ];
    let config = TasksConfig::default();
    let accents = HashMap::new();
    let context = context(&config, &accents);
    let shown = flat_rows("vault", &rows, None, false, SortKey::Id, &context);
    assert_eq!(shown.len(), 2);
    let hidden = flat_rows("vault", &rows, None, true, SortKey::Id, &context);
    let ids: Vec<&str> = hidden.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["t-001"]);
}

#[test]
fn effective_hide_done_defaults_to_hiding_with_no_query_at_all() {
    let config = TasksConfig::default();
    assert!(effective_hide_done(&config, &TasksQuery::default()));
}

#[test]
fn effective_hide_done_respects_an_explicit_show_or_hide_override() {
    let config = TasksConfig::default();
    let show = TasksQuery {
        done: Some("show".to_owned()),
        ..TasksQuery::default()
    };
    assert!(!effective_hide_done(&config, &show));
    let hide = TasksQuery {
        done: Some("hide".to_owned()),
        ..TasksQuery::default()
    };
    assert!(effective_hide_done(&config, &hide));
}

#[test]
fn effective_hide_done_auto_reveals_when_the_stage_filter_itself_targets_a_done_state() {
    let config = TasksConfig::default();
    let done_stage = TasksQuery {
        stage: Some("done".to_owned()),
        ..TasksQuery::default()
    };
    assert!(!effective_hide_done(&config, &done_stage));
    // An explicit `?done=hide` still wins even against a `?stage=done` selection.
    let forced_hide = TasksQuery {
        stage: Some("done".to_owned()),
        done: Some("hide".to_owned()),
        ..TasksQuery::default()
    };
    assert!(effective_hide_done(&config, &forced_hide));
    // A stage filter that does not itself target a done state does not auto-reveal.
    let other_stage = TasksQuery {
        stage: Some("doing".to_owned()),
        ..TasksQuery::default()
    };
    assert!(effective_hide_done(&config, &other_stage));
}

#[test]
fn done_href_sets_the_opposite_state_from_what_is_passed() {
    let query = TasksQuery::default();
    let hide = done_href("vault", ViewMode::Table, &query, true);
    assert_eq!(hide, "/vault/tasks/?view=table&done=hide");
    let show = done_href("vault", ViewMode::Table, &query, false);
    assert_eq!(show, "/vault/tasks/?view=table&done=show");
}

#[test]
fn hierarchy_rows_orders_a_parent_immediately_before_its_children() {
    let rows = vec![
        row("t-002", "next", "zenith", Some("t-001")),
        row("t-001", "next", "zenith", None),
        row("t-003", "next", "zenith", Some("t-001")),
    ];
    let config = TasksConfig::default();
    let accents = HashMap::new();
    let tree = hierarchy_rows("vault", &rows, &context(&config, &accents));
    let ids: Vec<&str> = tree.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["t-001", "t-002", "t-003"]);
    assert_eq!(tree[0].depth, 0);
    assert_eq!(tree[1].depth, 1);
    assert_eq!(tree[2].depth, 1);
}

#[test]
fn hierarchy_rows_nests_a_child_whose_parent_link_is_a_path_within_the_base_or_a_full_vault_path() {
    let parent = TaskRow {
        path: "memory/tasks/zenith/mcp/t-001.md".to_owned(),
        ..row("t-001", "next", "zenith", None)
    };
    let scoped_child = row("t-002", "next", "zenith", Some("zenith/mcp/t-001"));
    let full_path_child = row("t-003", "next", "zenith", Some("memory/tasks/zenith/mcp/t-001"));
    let rows = vec![scoped_child, full_path_child, parent];
    let config = TasksConfig::default();
    let accents = HashMap::new();

    let tree = hierarchy_rows("vault", &rows, &context(&config, &accents));

    let shape: Vec<(&str, usize)> = tree.iter().map(|r| (r.id.as_str(), r.depth)).collect();
    assert_eq!(shape, vec![("t-001", 0), ("t-002", 1), ("t-003", 1)]);
}

#[test]
fn hierarchy_rows_treats_a_dangling_parent_reference_as_a_root() {
    let rows = vec![row("t-001", "next", "zenith", Some("does-not-exist"))];
    let config = TasksConfig::default();
    let accents = HashMap::new();
    let tree = hierarchy_rows("vault", &rows, &context(&config, &accents));
    assert_eq!(tree.len(), 1);
    assert_eq!(tree[0].depth, 0);
}

#[test]
fn hierarchy_rows_never_loops_on_a_parent_cycle() {
    let rows = vec![
        row("t-001", "next", "zenith", Some("t-002")),
        row("t-002", "next", "zenith", Some("t-001")),
    ];
    let config = TasksConfig::default();
    let accents = HashMap::new();
    // A genuine cycle: neither row is reachable as a root via `parent`, so
    // the walk never starts and both rows are silently absent from the
    // rendered tree, rather than looping. This is a degraded-but-safe
    // outcome for corrupted data, not the common case.
    let tree = hierarchy_rows("vault", &rows, &context(&config, &accents));
    assert!(tree.len() <= 2);
}

#[test]
fn hierarchy_rows_attaches_the_configured_depth_label() {
    let rows = vec![row("t-001", "next", "zenith", None)];
    let config = TasksConfig {
        hierarchy: crate::tasks_config::HierarchyConfig {
            depth_labels: vec!["Initiative".to_owned()],
        },
        ..TasksConfig::default()
    };
    let accents = HashMap::new();
    let tree = hierarchy_rows("vault", &rows, &context(&config, &accents));
    assert_eq!(tree[0].depth_label.as_deref(), Some("Initiative"));
}

#[test]
fn tasks_query_defaults_to_the_focus_view() {
    let query = TasksQuery::default();
    assert_eq!(ViewMode::from_query(&query), ViewMode::Focus);
}

#[test]
fn view_mode_reads_table_and_hierarchy_from_the_view_parameter() {
    let table = TasksQuery {
        view: Some("table".to_owned()),
        ..TasksQuery::default()
    };
    assert_eq!(ViewMode::from_query(&table), ViewMode::Table);
    let hierarchy = TasksQuery {
        view: Some("hierarchy".to_owned()),
        ..TasksQuery::default()
    };
    assert_eq!(ViewMode::from_query(&hierarchy), ViewMode::Hierarchy);
}

#[test]
fn an_unrecognised_view_parameter_falls_back_to_focus_rather_than_erroring() {
    let query = TasksQuery {
        view: Some("kanban".to_owned()),
        ..TasksQuery::default()
    };
    assert_eq!(ViewMode::from_query(&query), ViewMode::Focus);
}

#[test]
fn matches_task_type_is_permissive_when_no_type_is_selected() {
    assert!(matches_task_type(&row("t-001", "next", "zenith", None), None));
    assert!(matches_task_type(&row("t-001", "next", "zenith", None), Some("")));
}

#[test]
fn matches_task_type_matches_case_insensitively() {
    let entry = row("t-001", "next", "zenith", None);
    assert!(matches_task_type(&entry, Some("Action")));
    assert!(!matches_task_type(&entry, Some("decision")));
}

#[test]
fn matches_priority_is_permissive_when_no_priority_is_selected() {
    assert!(matches_priority(&row("t-001", "next", "zenith", None), None));
}

#[test]
fn matches_priority_matches_case_insensitively_and_excludes_a_blank_row() {
    let mut with_priority = row("t-001", "next", "zenith", None);
    with_priority.priority = Some("High".to_owned());
    assert!(matches_priority(&with_priority, Some("high")));
    assert!(!matches_priority(&with_priority, Some("low")));
    let without_priority = row("t-002", "next", "zenith", None);
    assert!(!matches_priority(&without_priority, Some("high")));
}

#[test]
fn sort_rows_orders_by_due_date_blank_last() {
    let config = TasksConfig::default();
    let accents = HashMap::new();
    let mut rows = vec![
        row("t-001", "next", "zenith", None),
        row("t-002", "next", "zenith", None),
        row("t-003", "next", "zenith", None),
    ];
    rows[0].due = Some("2026-05-01".to_owned());
    rows[2].due = Some("2026-01-01".to_owned());
    let sorted = flat_rows("vault", &rows, None, false, SortKey::Due, &context(&config, &accents));
    let ids: Vec<&str> = sorted.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["t-003", "t-001", "t-002"]);
}

#[test]
fn sort_rows_orders_by_id_ascending() {
    let config = TasksConfig::default();
    let accents = HashMap::new();
    let rows = vec![
        row("t-003", "next", "zenith", None),
        row("t-001", "next", "zenith", None),
        row("t-002", "next", "zenith", None),
    ];
    let sorted = flat_rows("vault", &rows, None, false, SortKey::Id, &context(&config, &accents));
    let ids: Vec<&str> = sorted.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["t-001", "t-002", "t-003"]);
}

#[test]
fn sort_rows_orders_by_the_declared_priority_vocabulary_most_urgent_first() {
    // The shipped vocabulary is highest, high, medium, normal, low: ranked by that declared order, not by spelling,
    // so "highest" leads and "normal" no longer sorts above it. An undeclared value follows every declared one,
    // and a row with no priority comes last.
    let config = TasksConfig::default();
    let accents = HashMap::new();
    let mut rows = vec![
        row("t-001", "next", "zenith", None),
        row("t-002", "next", "zenith", None),
        row("t-003", "next", "zenith", None),
        row("t-004", "next", "zenith", None),
        row("t-005", "next", "zenith", None),
    ];
    rows[0].priority = Some("normal".to_owned());
    rows[1].priority = Some("someday".to_owned());
    rows[2].priority = Some("highest".to_owned());
    rows[4].priority = Some("low".to_owned());
    let sorted = flat_rows(
        "vault",
        &rows,
        None,
        false,
        SortKey::Priority,
        &context(&config, &accents),
    );
    let ids: Vec<&str> = sorted.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["t-003", "t-001", "t-005", "t-002", "t-004"]);
}

#[test]
fn fixed_filter_options_marks_the_leading_blank_option_selected_when_current_is_absent_or_empty() {
    let values: &[(&str, &str)] = &[("a", "A"), ("b", "B")];
    let none = fixed_filter_options("All", values.iter().copied(), None);
    assert!(none[0].selected);
    let empty = fixed_filter_options("All", values.iter().copied(), Some(""));
    assert!(empty[0].selected);
}

#[test]
fn fixed_filter_options_marks_the_matching_value_selected() {
    let values: &[(&str, &str)] = &[("a", "A"), ("b", "B")];
    let options = fixed_filter_options("All", values.iter().copied(), Some("b"));
    assert!(!options[0].selected);
    assert!(options[2].selected);
    assert_eq!(options[2].value, "b");
}

#[test]
fn tasks_href_carries_the_type_priority_sort_and_display_filters_forward() {
    let query = TasksQuery {
        task_type: Some("action".to_owned()),
        priority: Some("high".to_owned()),
        sort: Some("due".to_owned()),
        display: Some("cards".to_owned()),
        ..TasksQuery::default()
    };
    let href = tasks_href("vault", ViewMode::Table, &query, StageOverride::Keep);
    assert_eq!(
        href,
        "/vault/tasks/?view=table&task_type=action&priority=high&sort=due&display=cards"
    );
}

#[test]
fn display_mode_defaults_to_table_and_reads_cards_from_the_display_parameter() {
    assert_eq!(DisplayMode::from_query(&TasksQuery::default()), DisplayMode::Table);
    let cards = TasksQuery {
        display: Some("cards".to_owned()),
        ..TasksQuery::default()
    };
    assert_eq!(DisplayMode::from_query(&cards), DisplayMode::Cards);
}

#[test]
fn display_href_toggles_the_display_parameter_while_keeping_every_other_filter() {
    let query = TasksQuery {
        assignee: Some("piper".to_owned()),
        ..TasksQuery::default()
    };
    let cards_href = display_href("vault", ViewMode::Table, &query, DisplayMode::Cards);
    assert_eq!(cards_href, "/vault/tasks/?view=table&assignee=piper&display=cards");
    // Table is the default, so its own toggle link omits `display=` entirely, matching every other
    // omit-when-default query parameter this screen builds.
    let table_href = display_href("vault", ViewMode::Table, &query, DisplayMode::Table);
    assert_eq!(table_href, "/vault/tasks/?view=table&assignee=piper");
}

#[test]
fn wikilink_written_target_keeps_the_case_it_was_written_in_and_drops_the_alias_and_extension() {
    assert_eq!(
        wikilink_written_target("[[Requirements/REQ-001.md|REQ-001]]").as_deref(),
        Some("Requirements/REQ-001")
    );
    assert_eq!(wikilink_written_target("plain"), None);
}

#[test]
fn wikilink_from_target_keeps_a_path_as_written_with_its_last_segment_as_the_display_text() {
    assert_eq!(
        wikilink_from_target("Requirements/REQ-001"),
        "[[Requirements/REQ-001|REQ-001]]"
    );
    assert_eq!(wikilink_from_target("T-060"), "[[t-060|T-060]]");
}

#[test]
fn the_table_is_ordered_by_id_unless_another_order_is_chosen() {
    assert_eq!(SortKey::from_query(&TasksQuery::default()), SortKey::Id);
    let blank = TasksQuery {
        sort: Some(String::new()),
        ..TasksQuery::default()
    };
    assert_eq!(SortKey::from_query(&blank), SortKey::Id);
}

#[test]
fn ids_sort_by_their_series_then_by_number_with_blank_ids_last() {
    let config = TasksConfig::default();
    let accents = HashMap::new();
    let context = context(&config, &accents);
    let rows: Vec<TaskRow> = ["Q-3", "A-10", "", "D-1", "A-2", "a-9"]
        .into_iter()
        .map(|id| TaskRow {
            id: id.to_owned(),
            ..row(id, "next", "zenith", None)
        })
        .collect();
    let ids: Vec<String> = flat_rows("vault", &rows, None, false, SortKey::Id, &context)
        .into_iter()
        .map(|view| view.id)
        .collect();
    assert_eq!(ids, ["A-2", "a-9", "A-10", "D-1", "Q-3", ""]);
}

/// Whether `html`'s stage strip shows `count` on the tile labelled `label`.
fn tile_shows(html: &str, label: &str, count: usize) -> bool {
    html.find(&format!(">{label}<")).is_some_and(|at| {
        html[..at]
            .rfind("task-stage-count")
            .is_some_and(|count_at| html[count_at..at].contains(&format!(">{count}<")))
    })
}

#[test]
fn the_done_today_selection_narrows_every_view_but_never_the_strip_counts() {
    let config = TasksConfig::default();
    let rows = vec![
        row("A-1", "doing", "zenith", None),
        TaskRow {
            completed: Some("2026-10-13".to_owned()),
            attention: None,
            ..row("A-2", "done", "zenith", None)
        },
        TaskRow {
            completed: Some("2026-10-01".to_owned()),
            attention: None,
            ..row("A-3", "done", "zenith", None)
        },
    ];
    for view in [None, Some("table"), Some("hierarchy")] {
        let query = TasksQuery {
            view: view.map(str::to_owned),
            completed: Some(CompletedOn::Today),
            ..TasksQuery::default()
        };
        let html = render_list("vault", &config, &rows, &[], &query, TODAY);
        assert!(html.contains("Title for A-2"), "{view:?}: completed today is listed");
        assert!(!html.contains("Title for A-1"), "{view:?}: an open task is not listed");
        assert!(
            !html.contains("Title for A-3"),
            "{view:?}: one completed another day is not listed"
        );
        assert!(
            tile_shows(&html, "Doing", 1),
            "{view:?}: the strip still counts every task"
        );
        assert!(
            tile_shows(&html, "Done", 1),
            "{view:?}: the Done today tile counts today's"
        );
    }
}

#[test]
fn a_task_is_referenced_by_its_key_wherever_its_note_is_filed() {
    assert_eq!(
        task_reference("memory/tasks/a-001.md", Some("A-001")),
        "[[a-001|A-001]]"
    );
    assert_eq!(
        task_reference("memory/tasks/clients/acme/a-001.md", Some("A-001")),
        "[[a-001|A-001]]"
    );
}

#[test]
fn a_task_whose_file_is_not_named_for_its_key_is_referenced_by_its_file_name_never_its_folder() {
    assert_eq!(
        task_reference("memory/tasks/clients/ship-it.md", Some("A-001")),
        "[[ship-it|A-001]]"
    );
    assert_eq!(task_reference("memory/tasks/clients/ship-it.md", None), "[[ship-it]]");
    assert_eq!(
        task_reference("memory/tasks/clients/ship-it.md", Some(" ")),
        "[[ship-it]]"
    );
}
