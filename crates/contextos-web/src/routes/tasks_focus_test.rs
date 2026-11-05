use crate::entities::EntityEntry;
use crate::tasks_config::TasksConfig;

use super::*;

fn row(id: &str, state: &str, entity: &str, priority: Option<&str>, assignee: Option<&str>) -> TaskRow {
    TaskRow {
        path: format!("memory/tasks/{}.md", id.to_ascii_lowercase()),
        id: id.to_owned(),
        title: format!("Title for {id}"),
        state: state.to_owned(),
        task_type: "action".to_owned(),
        entity: entity.to_owned(),
        priority: priority.map(str::to_owned),
        due: None,
        assignee: assignee.map(str::to_owned),
        parent_id: None,
        completed: None,
        updated: None,
        attention: None,
    }
}

fn entity(slug: &str, accent: u8) -> EntityEntry {
    EntityEntry {
        slug: slug.to_owned(),
        name: slug.to_owned(),
        accent,
    }
}

#[test]
fn the_now_grid_caps_at_the_wip_limit_and_reports_the_overflow() -> Result<(), Box<dyn std::error::Error>> {
    let config = TasksConfig::default();
    let rows: Vec<TaskRow> = (0..8)
        .map(|n| row(&format!("t-{n:03}"), "doing", "zenith", None, None))
        .collect();
    let data = build_focus_data("vault", &config, &rows, &[], &TasksQuery::default());
    assert_eq!(data.now_cards.len(), WIP_LIMIT);
    assert_eq!(data.now_total, 8);
    let overflow = data.now_overflow.ok_or("overflow present past the WIP limit")?;
    assert_eq!(overflow.count, 2);
    assert_eq!(overflow.href, "/vault/tasks/?view=table&stage=action:doing");
    Ok(())
}

#[test]
fn the_now_grid_has_no_overflow_link_at_or_under_the_wip_limit() {
    let config = TasksConfig::default();
    let rows = vec![row("t-001", "doing", "zenith", None, None)];
    let data = build_focus_data("vault", &config, &rows, &[], &TasksQuery::default());
    assert!(data.now_overflow.is_none());
}

#[test]
fn the_backlog_browse_link_joins_every_stage_sharing_the_backlog_bucket() {
    let config = TasksConfig::default();
    let rows = vec![row("t-001", "backlog", "zenith", None, None)];
    let data = build_focus_data("vault", &config, &rows, &[], &TasksQuery::default());
    assert_eq!(
        data.backlog_browse_href,
        "/vault/tasks/?view=table&stage=action:backlog,action:next"
    );
}

#[test]
fn the_backlog_and_done_browse_links_target_the_full_table_view_not_a_card_only_drill() {
    // `bucket_href`'s own doc covers why: a "Browse all"/"View all" link exists precisely because the panel's own
    // capped preview is not the whole picture, so it should land somewhere offering the list/card toggle and the
    // Hierarchy view too, not the single card-grid-only drill an ordinary stage-tile click still uses.
    let config = TasksConfig::default();
    let rows = vec![
        row("t-001", "backlog", "zenith", None, None),
        row("t-002", "done", "zenith", None, None),
    ];
    let data = build_focus_data("vault", &config, &rows, &[], &TasksQuery::default());
    assert_eq!(
        data.backlog_browse_href,
        "/vault/tasks/?view=table&stage=action:backlog,action:next"
    );
    assert_eq!(
        data.done_browse_href,
        "/vault/tasks/?view=table&stage=action:done,decision:adopted,question:answered"
    );
}

#[test]
fn backlog_rows_sort_most_urgent_first_by_the_declared_priorities_with_absent_priority_last() {
    // "normal" spells after "highest" but ranks below it: the declared vocabulary decides, not the spelling.
    let config = TasksConfig::default();
    let rows = vec![
        row("t-001", "backlog", "zenith", None, None),
        row("t-002", "backlog", "zenith", Some("normal"), None),
        row("t-003", "backlog", "zenith", Some("highest"), None),
    ];
    let data = build_focus_data("vault", &config, &rows, &[], &TasksQuery::default());
    let titles: Vec<&str> = data.backlog_rows.iter().map(|r| r.title.as_str()).collect();
    assert_eq!(titles, vec!["Title for t-003", "Title for t-002", "Title for t-001"]);
}

#[test]
fn backlog_rows_cap_at_the_panel_limit_but_the_total_reflects_every_row() {
    let config = TasksConfig::default();
    let rows: Vec<TaskRow> = (0..5)
        .map(|n| row(&format!("t-{n:03}"), "backlog", "zenith", None, None))
        .collect();
    let data = build_focus_data("vault", &config, &rows, &[], &TasksQuery::default());
    assert_eq!(data.backlog_rows.len(), PANEL_LIMIT);
    assert_eq!(data.backlog_total, 5);
}

#[test]
fn pull_next_targets_the_top_sorted_backlog_row() {
    let config = TasksConfig::default();
    let rows = vec![
        row("t-001", "backlog", "zenith", Some("low"), None),
        row("t-002", "backlog", "zenith", Some("high"), None),
    ];
    let data = build_focus_data("vault", &config, &rows, &[], &TasksQuery::default());
    assert_eq!(data.pull_next_href.as_deref(), Some("/vault/tasks/t-002"));
}

#[test]
fn pull_next_is_absent_when_the_backlog_is_empty() {
    let config = TasksConfig::default();
    let data = build_focus_data("vault", &config, &[], &[], &TasksQuery::default());
    assert!(data.pull_next_href.is_none());
}

#[test]
fn a_stage_query_selects_the_single_drill_down_grid_instead_of_the_sections() {
    let config = TasksConfig::default();
    let rows = vec![
        row("t-001", "doing", "zenith", None, None),
        row("t-002", "backlog", "zenith", None, None),
    ];
    let query = TasksQuery {
        stage: Some("doing".to_owned()),
        ..TasksQuery::default()
    };
    let data = build_focus_data("vault", &config, &rows, &[], &query);
    assert_eq!(data.drill_label.as_deref(), Some("Doing"));
    assert_eq!(data.drill_cards.len(), 1);
    assert_eq!(data.drill_cards[0].id, "t-001");
    assert!(data.now_cards.is_empty(), "sectioned view must not also render");
}

#[test]
fn a_comma_joined_stage_query_drills_into_every_named_stage() {
    let config = TasksConfig::default();
    let rows = vec![
        row("t-001", "waiting", "zenith", None, None),
        row("t-002", "review", "zenith", None, None),
        row("t-003", "doing", "zenith", None, None),
    ];
    let query = TasksQuery {
        stage: Some("waiting,review".to_owned()),
        ..TasksQuery::default()
    };
    let data = build_focus_data("vault", &config, &rows, &[], &query);
    assert_eq!(data.drill_cards.len(), 2);
    assert_eq!(data.drill_label.as_deref(), Some("Waiting + Review"));
}

#[test]
fn clear_filter_href_drops_only_the_stage_parameter() {
    let config = TasksConfig::default();
    let query = TasksQuery {
        stage: Some("doing".to_owned()),
        entity: Some("zenith".to_owned()),
        ..TasksQuery::default()
    };
    let data = build_focus_data("vault", &config, &[], &[], &query);
    assert_eq!(data.clear_filter_href, "/vault/tasks/?entity=zenith");
}

#[test]
fn a_task_with_no_bucketed_stage_appears_in_no_focus_section() {
    let config = TasksConfig::default();
    let rows = vec![row("t-001", "dropped", "zenith", None, None)];
    let data = build_focus_data("vault", &config, &rows, &[], &TasksQuery::default());
    assert!(data.now_cards.is_empty());
    assert!(data.needs_call_cards.is_empty());
    assert!(data.backlog_rows.is_empty());
    assert!(data.done_rows.is_empty());
}

#[test]
fn entity_accent_uses_the_configured_entity_when_one_exists() {
    let config = TasksConfig::default();
    let rows = vec![row("t-001", "doing", "family", None, None)];
    let configured = vec![entity("zenith", 1), entity("family", 4)];
    let data = build_focus_data("vault", &config, &rows, &configured, &TasksQuery::default());
    assert_eq!(data.now_cards[0].entity_accent, 4);
}

#[test]
fn entity_accent_falls_back_to_position_when_the_entity_is_not_configured() {
    let config = TasksConfig::default();
    let rows = vec![
        row("t-001", "doing", "unlisted-one", None, None),
        row("t-002", "doing", "unlisted-two", None, None),
    ];
    let data = build_focus_data("vault", &config, &rows, &[], &TasksQuery::default());
    assert_eq!(data.now_cards[0].entity_accent, crate::rendering::shell::entity_for(0));
    assert_eq!(data.now_cards[1].entity_accent, crate::rendering::shell::entity_for(1));
}

#[test]
fn card_view_carries_the_configured_display_label_for_its_state() {
    let config = TasksConfig::default();
    let rows = vec![row("t-001", "doing", "zenith", None, None)];
    let query = TasksQuery {
        stage: Some("doing".to_owned()),
        ..TasksQuery::default()
    };
    let data = build_focus_data("vault", &config, &rows, &[], &query);
    assert_eq!(data.drill_cards[0].state_label, "Doing");
}

#[test]
fn truncated_for_button_leaves_a_short_title_untouched() {
    assert_eq!(truncated_for_button("Ship the thing"), "Ship the thing");
}

#[test]
fn truncated_for_button_shortens_a_long_title_with_an_ellipsis() {
    let long = "a".repeat(60);
    let truncated = truncated_for_button(&long);
    assert_eq!(truncated.chars().count(), BUTTON_TITLE_LIMIT + 1);
    assert!(truncated.ends_with('…'));
}

#[test]
fn a_stage_drill_names_its_type_and_matches_only_that_types_tasks() {
    let config = TasksConfig::default();
    let decision = TaskRow {
        task_type: "decision".to_owned(),
        ..row("d-001", "dropped", "zenith", None, None)
    };
    let rows = vec![row("t-001", "dropped", "zenith", None, None), decision];
    let query = TasksQuery {
        stage: Some("decision:dropped".to_owned()),
        ..TasksQuery::default()
    };
    let data = build_focus_data("vault", &config, &rows, &[], &query);
    assert_eq!(data.drill_cards.len(), 1);
    assert_eq!(data.drill_cards[0].id, "d-001");
    assert_eq!(data.drill_label.as_deref(), Some("Decision: Dropped"));
}

#[test]
fn proposed_decisions_and_open_questions_need_the_operators_call() {
    let config = TasksConfig::default();
    let typed = |id: &str, task_type: &str, state: &str| TaskRow {
        task_type: task_type.to_owned(),
        ..row(id, state, "zenith", None, None)
    };
    let rows = vec![
        typed("d-001", "decision", "proposed"),
        typed("q-001", "question", "open"),
        typed("d-002", "decision", "adopted"),
    ];
    let data = build_focus_data("vault", &config, &rows, &[], &TasksQuery::default());
    assert_eq!(data.needs_call_total, 2);
    assert_eq!(data.done_total, 1);
}
