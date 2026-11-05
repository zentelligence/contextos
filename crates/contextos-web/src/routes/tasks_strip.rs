//! The Tasks screen's stage strip, and the stage filter it and the Focus view link with. The strip is there to prompt
//! action, not to tally everything: one group per task type, with a tile for each of the type's open stages only (an
//! action's backlog to review, a decision's proposed, a question's open), and none for a closing stage. Each tile
//! names its stage as `type:state` (a [`StageKey`]), so a state two types share is counted and filtered per type. A
//! task whose type the vocabulary does not declare, or which has none, has no tile; it is still listed, counted, and
//! placed in Focus by the shared stages. The strip ends with one tile for momentum rather than action: Done today,
//! the tasks of every type closed as completed on the operator's today ([`completed_on`]).

use time::Date;

use crate::tasks_config::{Bucket, Closes, StageKey, TasksConfig};

use super::tasks::{CompletedOn, StageOverride, TaskRow, TasksQuery, ViewMode, tasks_href};

/// What tasks whose type is not declared, or which have none, are called where a stage of theirs is named.
pub(super) const OTHER_TYPES_LABEL: &str = "Other types";

/// The Done today tile's group, its own label within it, and its icon (the shipped Done stage's).
const TODAY_LABEL: &str = "Today";
const DONE_TODAY_LABEL: &str = "Done";
const DONE_TODAY_ICON: &str = "check";

pub(super) struct StageTile {
    pub(super) label: String,
    pub(super) icon: Option<String>,
    pub(super) count: usize,
    pub(super) active: bool,
    pub(super) href: String,
}

/// One task type's tiles in the stage strip, under the type's name.
pub(super) struct StageGroup {
    pub(super) label: String,
    pub(super) tiles: Vec<StageTile>,
}

/// Whether `row` is in one of the stages `filter` names: a comma-joined list of stage keys ([`StageKey`]), each
/// `type:state` (a stage tile's own, or several sharing one Focus bucket, `tasks_focus::bucket_href`), `:state` for a
/// task whose type is not declared, or a bare `state`, which matches that state in every type. Shared by the Table
/// view's own `?stage=` filter and the Focus view's own single-stage/bucket drill-down, so the two never interpret
/// this query value differently.
pub(super) fn stage_matches(config: &TasksConfig, row: &TaskRow, filter: &str) -> bool {
    filter
        .split(',')
        .map(StageKey::from)
        .any(|key| config.in_stage(&key, &row.task_type, &row.state))
}

/// Whether two stage keys name the same stage: types compared without regard to ASCII case, as rows are matched.
fn same_stage(left: &StageKey, right: &StageKey) -> bool {
    left.state == right.state
        && match (&left.task_type, &right.task_type) {
            (None, None) => true,
            (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
            _ => false,
        }
}

/// Builds the stage strip: one group per declared task type, with one tile per open stage of the type's own lifecycle,
/// in order; a type with no open stage has no group. Then the Done today group, always present, counting the tasks
/// closed as completed on `today` and linking to All tasks filtered to them. Counted against `filtered_rows` (already
/// entity-, search-, and assignee-scoped, matching `TasksScreen.jsx`'s own `CcmStageStrip` receiving `base`, the
/// identically-filtered set, rather than every row).
pub(super) fn stage_groups(
    vault_name: &str,
    config: &TasksConfig,
    filtered_rows: &[TaskRow],
    query: &TasksQuery,
    today: Date,
) -> Vec<StageGroup> {
    let view = ViewMode::from_query(query);
    let selected = query.stage.as_deref().map(StageKey::from);
    let tile = |key: StageKey, label: &str, icon: Option<&String>| {
        let token = key.to_string();
        let active = selected.as_ref().is_some_and(|selected| same_stage(selected, &key));
        StageTile {
            label: label.to_owned(),
            icon: icon.cloned(),
            count: filtered_rows
                .iter()
                .filter(|row| config.in_stage(&key, &row.task_type, &row.state))
                .count(),
            active,
            href: tasks_href(
                vault_name,
                view,
                query,
                if active {
                    StageOverride::Clear
                } else {
                    StageOverride::Set(token.as_str())
                },
            ),
        }
    };
    let done_today_active = query.completed == Some(CompletedOn::Today);
    let done_today = StageGroup {
        label: TODAY_LABEL.to_owned(),
        tiles: vec![StageTile {
            label: DONE_TODAY_LABEL.to_owned(),
            icon: Some(DONE_TODAY_ICON.to_owned()),
            count: filtered_rows
                .iter()
                .filter(|row| completed_on(config, row, today))
                .count(),
            active: done_today_active,
            href: if done_today_active {
                tasks_href(vault_name, view, query, StageOverride::Clear)
            } else {
                // Every task the tile counts is done, so it never links to them under an explicit hide.
                let shown = TasksQuery {
                    done: None,
                    ..query.clone()
                };
                tasks_href(vault_name, ViewMode::Table, &shown, StageOverride::CompletedToday)
            },
        }],
    };
    config
        .lifecycles()
        .into_iter()
        .map(|(entry, stages)| StageGroup {
            label: entry.display_label().to_owned(),
            tiles: stages
                .iter()
                .filter(|stage| stage.closing() == Closes::Never)
                .map(|stage| {
                    tile(
                        StageKey::of(&entry.value, &stage.state),
                        stage.display_label(),
                        stage.icon.as_ref(),
                    )
                })
                .collect(),
        })
        .filter(|group| !group.tiles.is_empty())
        .chain(std::iter::once(done_today))
        .collect()
}

/// Whether `row` was closed as completed on `day`: its own type's lifecycle closes it as completed in its current
/// state ([`Closes::Completed`]), so a task dropped or reopened since is not counted, and its `completed` date, or the
/// date a `completed` timestamp begins with, is `day`.
pub(super) fn completed_on(config: &TasksConfig, row: &TaskRow, day: Date) -> bool {
    config.closing(&row.task_type, &row.state) == Some(Closes::Completed)
        && row.completed.as_deref().is_some_and(|completed| {
            crate::register_records::display_date(completed) == crate::journal::format_date(day)
        })
}

/// Whether `row`'s stage, in its own type's lifecycle, is bucketed to [`Bucket::Done`] per `config`.
pub(super) fn is_done_row(config: &TasksConfig, row: &TaskRow) -> bool {
    config.bucket_of(&row.task_type, &row.state) == Some(Bucket::Done)
}

/// Whether the stage `key` names is bucketed to [`Bucket::Done`]: in its own type (the shared stages for `:state`), or
/// for a bare state, in any type.
fn names_a_done_stage(config: &TasksConfig, key: &StageKey) -> bool {
    match &key.task_type {
        Some(task_type) => config.bucket_of(task_type, &key.state) == Some(Bucket::Done),
        None => config
            .task_type
            .iter()
            .any(|entry| config.bucket_of(&entry.value, &key.state) == Some(Bucket::Done)),
    }
}

/// Whether the Table/Hierarchy views should exclude every [`Bucket::Done`] row for the current `query`
/// ([`TasksQuery::done`]'s own doc covers the precedence this implements): defaults to hiding them, but never when
/// the operator has explicitly clicked through to the Done today tile or to a `?stage=` selection that itself names
/// a done state (`?done=` unset), and always exactly as the explicit `?done=show`/other toggle link asks regardless
/// of either.
pub(super) fn effective_hide_done(config: &TasksConfig, query: &TasksQuery) -> bool {
    match query.done.as_deref() {
        Some("show") => false,
        Some(_) => true,
        None if query.completed.is_some() => false,
        None => !query.stage.as_deref().is_some_and(|stage| {
            stage
                .split(',')
                .any(|key| names_a_done_stage(config, &StageKey::from(key)))
        }),
    }
}

#[cfg(test)]
#[path = "tasks_strip_test.rs"]
mod tests;
