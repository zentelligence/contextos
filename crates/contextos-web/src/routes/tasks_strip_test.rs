use super::*;

use super::super::tasks::CompletedOn;

/// The day the Done today tile counts against.
const TODAY: time::Date = time::macros::date!(2026 - 10 - 13);

/// A task of `task_type` in `state` whose `completed` date is `completed`.
fn closed_row(id: &str, task_type: &str, state: &str, completed: &str) -> TaskRow {
    TaskRow {
        completed: Some(completed.to_owned()),
        ..row(id, task_type, state)
    }
}

fn row(id: &str, task_type: &str, state: &str) -> TaskRow {
    TaskRow {
        path: format!("memory/tasks/{}.md", id.to_ascii_lowercase()),
        id: id.to_owned(),
        title: format!("Title for {id}"),
        state: state.to_owned(),
        task_type: task_type.to_owned(),
        entity: "zenith".to_owned(),
        priority: None,
        due: None,
        assignee: None,
        parent_id: None,
        completed: None,
        updated: None,
        attention: None,
    }
}

#[test]
fn the_strip_offers_only_each_types_open_stages_the_ones_that_still_ask_for_action() {
    let config = TasksConfig::default();
    let groups = stage_groups("vault", &config, &[], &TasksQuery::default(), TODAY);
    let tiles: Vec<(&str, Vec<&str>)> = groups
        .iter()
        .map(|group| {
            (
                group.label.as_str(),
                group.tiles.iter().map(|tile| tile.label.as_str()).collect(),
            )
        })
        .collect();
    assert_eq!(
        tiles,
        [
            ("Action", vec!["Backlog", "Next", "Doing", "Waiting", "Review"]),
            ("Decision", vec!["Proposed"]),
            ("Question", vec!["Open"]),
            ("Today", vec!["Done"]),
        ]
    );
}

#[test]
fn tasks_whose_type_is_not_declared_add_no_group_to_the_strip() {
    let config = TasksConfig::default();
    let rows = vec![
        row("x-1", "", "doing"),
        row("x-2", "bug", "doing"),
        row("a-1", "action", "doing"),
    ];
    let groups = stage_groups("vault", &config, &rows, &TasksQuery::default(), TODAY);
    let labels: Vec<&str> = groups.iter().map(|group| group.label.as_str()).collect();
    assert_eq!(labels, ["Action", "Decision", "Question", "Today"]);
    let doing = groups[0]
        .tiles
        .iter()
        .find(|tile| tile.label == "Doing")
        .map(|tile| tile.count);
    assert_eq!(doing, Some(1), "only the declared type's task is counted");
}

#[test]
fn a_tile_is_active_whatever_the_case_of_the_type_its_link_names() {
    let config = TasksConfig::default();
    let query = TasksQuery {
        stage: Some("Action:next".to_owned()),
        ..TasksQuery::default()
    };
    let groups = stage_groups("vault", &config, &[], &query, TODAY);
    let next = groups[0].tiles.iter().find(|tile| tile.label == "Next");
    assert!(next.is_some_and(|tile| tile.active));
}

#[test]
fn the_stage_filter_reaches_tasks_whose_type_is_not_declared() {
    let config = TasksConfig::default();
    assert!(stage_matches(&config, &row("x-1", "", "doing"), ":doing"));
    assert!(stage_matches(
        &config,
        &row("x-2", "bug", "doing"),
        "action:next,:doing"
    ));
    assert!(!stage_matches(&config, &row("a-1", "action", "doing"), ":doing"));
}

#[test]
fn a_task_whose_type_is_not_declared_is_done_by_the_shared_stages() {
    let config = TasksConfig::default();
    assert!(is_done_row(&config, &row("x-1", "", "done")));
    assert!(
        !is_done_row(&config, &row("d-1", "decision", "done")),
        "a decision is never done, only adopted"
    );
    let query = TasksQuery {
        stage: Some(":done".to_owned()),
        ..TasksQuery::default()
    };
    assert!(
        !effective_hide_done(&config, &query),
        "naming a done stage shows done tasks"
    );
}

/// The Done today tile: the strip's last group.
fn done_today(groups: &[StageGroup]) -> Option<&StageTile> {
    groups
        .last()
        .filter(|group| group.label == "Today")
        .and_then(|group| group.tiles.first())
}

#[test]
fn the_done_today_tile_counts_tasks_of_every_type_completed_today_and_nothing_else() {
    let config = TasksConfig::default();
    let rows = vec![
        closed_row("a-1", "action", "done", "2026-10-13"),
        closed_row("d-1", "decision", "adopted", "2026-10-13"),
        closed_row("q-1", "question", "answered", "2026-10-13T09:15:00+11:00"),
        closed_row("a-2", "action", "done", "2026-10-12"),
        closed_row("a-3", "action", "dropped", "2026-10-13"),
        closed_row("a-4", "action", "doing", "2026-10-13"),
        closed_row("x-1", "bug", "done", "2026-10-13"),
        row("a-5", "action", "done"),
    ];
    let groups = stage_groups("vault", &config, &rows, &TasksQuery::default(), TODAY);
    let tile = done_today(&groups);
    assert_eq!(
        tile.map(|tile| (tile.count, tile.icon.as_deref())),
        Some((4, Some("check"))),
        "a task closed as completed today is counted whatever its type, one of an undeclared type by the shared \
         stages; not one dated yesterday, dropped, reopened, or with no date"
    );
}

#[test]
fn completed_today_needs_a_completing_stage_and_todays_date() {
    let config = TasksConfig::default();
    assert!(completed_on(
        &config,
        &closed_row("a-1", "action", "done", "2026-10-13"),
        TODAY
    ));
    assert!(completed_on(
        &config,
        &closed_row("a-1", "Action", "done", " 2026-10-13 08:00 "),
        TODAY
    ));
    assert!(!completed_on(
        &config,
        &closed_row("a-2", "action", "done", "2026-10-12"),
        TODAY
    ));
    assert!(!completed_on(
        &config,
        &closed_row("a-3", "action", "dropped", "2026-10-13"),
        TODAY
    ));
    assert!(!completed_on(
        &config,
        &closed_row("a-4", "action", "done", "13/10/2026"),
        TODAY
    ));
}

#[test]
fn the_done_today_tile_links_to_all_tasks_filtered_to_today_and_back_out() {
    let config = TasksConfig::default();
    let query = TasksQuery {
        stage: Some("action:next".to_owned()),
        ..TasksQuery::default()
    };
    let groups = stage_groups("vault", &config, &[], &query, TODAY);
    let tile = done_today(&groups);
    assert!(tile.is_some_and(|tile| !tile.active));
    let href = tile.map(|tile| tile.href.as_str()).unwrap_or_default();
    assert!(href.contains("view=table"), "{href}");
    assert!(href.contains("completed=today"), "{href}");
    assert!(!href.contains("stage="), "the tile replaces a stage selection: {href}");

    let selected = TasksQuery {
        view: Some("table".to_owned()),
        completed: Some(CompletedOn::Today),
        ..TasksQuery::default()
    };
    let groups = stage_groups("vault", &config, &[], &selected, TODAY);
    let tile = done_today(&groups);
    assert!(tile.is_some_and(|tile| tile.active));
    let href = tile.map(|tile| tile.href.as_str()).unwrap_or_default();
    assert!(!href.contains("completed="), "clicking it again clears it: {href}");
    let next = groups[0].tiles.iter().find(|tile| tile.label == "Next");
    let next_href = next.map(|tile| tile.href.as_str()).unwrap_or_default();
    assert!(next_href.contains("stage=action:next"), "{next_href}");
    assert!(
        !next_href.contains("completed="),
        "a stage tile replaces the Done today selection: {next_href}"
    );
}

#[test]
fn filtering_to_tasks_completed_today_shows_done_tasks_unless_hidden_explicitly() {
    let config = TasksConfig::default();
    let query = TasksQuery {
        completed: Some(CompletedOn::Today),
        ..TasksQuery::default()
    };
    assert!(!effective_hide_done(&config, &query));
    let hidden = TasksQuery {
        done: Some("hide".to_owned()),
        ..query
    };
    assert!(effective_hide_done(&config, &hidden));
}

#[test]
fn the_done_today_tile_shows_done_tasks_even_where_they_were_hidden() {
    let config = TasksConfig::default();
    let query = TasksQuery {
        view: Some("table".to_owned()),
        done: Some("hide".to_owned()),
        ..TasksQuery::default()
    };
    let groups = stage_groups("vault", &config, &[], &query, TODAY);
    let href = done_today(&groups).map(|tile| tile.href.as_str()).unwrap_or_default();
    assert!(href.contains("completed=today"), "{href}");
    assert!(
        !href.contains("done="),
        "every task the tile counts is done, so it never links to them hidden: {href}"
    );
}

#[test]
fn a_stage_a_vault_configures_to_close_as_completed_counts_toward_done_today() -> Result<(), toml::de::Error> {
    let config: TasksConfig = toml::from_str(
        "[[stage]]\nstate = \"building\"\n\n[[stage]]\nstate = \"shipped\"\ncloses = \"completed\"\n\n\
         [[stage]]\nstate = \"done\"\ncloses = \"cancelled\"\n",
    )?;
    assert!(completed_on(
        &config,
        &closed_row("x-1", "", "shipped", "2026-10-13"),
        TODAY
    ));
    assert!(
        !completed_on(&config, &closed_row("x-2", "", "done", "2026-10-13"), TODAY),
        "a stage named done that the vault closes as cancelled is not completed"
    );
    Ok(())
}
