//! Field-type-aware widgets for the Tasks screen's own frontmatter fields.
//! Several fields are closed enums or dates, not free text, and this module
//! is the one place that decides how each renders: a `<select>` backed by
//! real, vault-configured data (`entity`, `assignee`, and, from
//! `.contextos/tasks.toml`, `state`, `priority`, and `task_type`), a `date`
//! input (every lifecycle field), or plain text (everything else, unchanged).
//!
//! Shared by `routes::tasks_detail`'s own detail/edit form and
//! `routes::tasks`'s create form: both need the identical real entity and
//! assignee option lists, built from the identical vault-configured sources,
//! so the two forms can never quietly disagree about what a valid `entity`
//! or `assignee` value is.

use crate::entities::EntityEntry;
use crate::tasks_config::{PriorityEntry, TaskTypeEntry, TasksConfig};
use crate::team::TeamMember;

/// One `<option>` a select-backed field offers, with whether it is the
/// field's own current value already marked in, so the template never has
/// to compare a field's raw string value against an option's own value
/// itself.
pub(super) struct SelectOption {
    pub(super) value: String,
    pub(super) label: String,
    pub(super) selected: bool,
}

/// How one frontmatter field's own current value renders as an editable
/// control. `Select`'s options are never empty for a field this module
/// classifies as `Select`: a source with zero configured entries (an empty
/// `.contextos/entities.toml`, or an empty `.contextos/tasks.toml` stage
/// list) is classified `Text` instead by this module's own widget builders,
/// never rendered as an empty, unusable dropdown.
pub(super) enum FieldWidget {
    Text,
    Date,
    Select(Vec<SelectOption>),
}

/// Marks whichever option's own `value` equals `current` as selected. When
/// `current` is non-empty and matches no offered option (a since-renamed or
/// since-removed entity, agent, or stage, or a legacy value predating a
/// schema change such as `task_type: project`), that value is appended as
/// its own extra, selected option rather than silently dropped: saving the
/// form without touching this field must never quietly rewrite a value the
/// operator did not choose to change.
fn mark_selected(mut options: Vec<SelectOption>, current: &str) -> Vec<SelectOption> {
    let mut matched = false;
    for option in &mut options {
        option.selected = option.value == current;
        matched |= option.selected;
    }
    if !matched && !current.is_empty() {
        options.push(SelectOption {
            value: current.to_owned(),
            label: current.to_owned(),
            selected: true,
        });
    }
    options
}

/// The `entity` field's widget: a real select over `entities` (`slug` as
/// value, `name` as label) when the vault has at least one configured, or a
/// plain text fallback when it has none, since `entity` is mandatory on
/// every task note and an empty `<select>` would make the field impossible
/// to fill in at all.
pub(super) fn entity_widget(entities: &[EntityEntry], current: &str) -> FieldWidget {
    if entities.is_empty() {
        return FieldWidget::Text;
    }
    let options = entities
        .iter()
        .map(|entity| SelectOption {
            value: entity.slug.clone(),
            label: entity.name.clone(),
            selected: false,
        })
        .collect();
    FieldWidget::Select(mark_selected(options, current))
}

/// The `assignee` field's widget: a real select over every configured `.contextos/team.toml` entry's own
/// `slug`/`name`, agent and human alike - no literal identity is ever compiled into this binary as a default or a
/// guaranteed option. Falls back to text on the same "never render an empty select" principle as `entity_widget`,
/// for the same reason: a vault with no configured team members is possible, and a hardcoded one-option select
/// would misrepresent who is actually available to assign a task to. A member with a `domain` is labelled
/// `Name (domain)`, so whoever assigns the task can see what each member does.
pub(super) fn assignee_widget(team: &[TeamMember], current: &str) -> FieldWidget {
    if team.is_empty() {
        return FieldWidget::Text;
    }
    let options = team
        .iter()
        .map(|member| SelectOption {
            value: member.slug.clone(),
            label: match member.domain.as_deref().map(str::trim) {
                Some(domain) if !domain.is_empty() => format!("{} ({domain})", member.name),
                _ => member.name.clone(),
            },
            selected: false,
        })
        .collect();
    FieldWidget::Select(mark_selected(options, current))
}

/// The `state` field's widget: a select over the states of the task's own type (`.contextos/tasks.toml`'s lifecycle
/// for `task_type`, the same configuration the stage strip reads), then every other declared type's states not
/// already listed, each labelled with its type. Offering the others lets a task's type and state change together in
/// one save without script; the save refuses a state its type does not declare. Falls back to text on the same "never
/// render an empty select" principle as `entity`, for a configuration that declares no stages at all.
pub(super) fn state_widget(config: &TasksConfig, task_type: &str, current: &str) -> FieldWidget {
    let own = config.lifecycle(task_type);
    // The other types' lists are those not owned by the task's own: its type's when it has stages of its own, else
    // every type on the shared stages.
    let own_is_shared = config.task_type(task_type).is_none_or(|entry| entry.stage.is_empty());
    let mut options: Vec<SelectOption> = own
        .iter()
        .map(|stage| SelectOption {
            value: stage.state.clone(),
            label: stage.display_label().to_owned(),
            selected: false,
        })
        .collect();
    for (entry, stages) in config.lifecycles() {
        let same_list = if own_is_shared {
            entry.stage.is_empty()
        } else {
            entry.value.eq_ignore_ascii_case(task_type)
        };
        if same_list {
            continue;
        }
        for stage in stages {
            if options.iter().any(|option| option.value == stage.state) {
                continue;
            }
            options.push(SelectOption {
                value: stage.state.clone(),
                label: format!("{} ({})", stage.display_label(), entry.display_label()),
                selected: false,
            });
        }
    }
    if options.is_empty() {
        return FieldWidget::Text;
    }
    FieldWidget::Select(mark_selected(options, current))
}

/// The `priority` field's widget: a leading blank option, then `.contextos/tasks.toml`'s declared priorities, most
/// urgent first. The blank option is deliberate, not a placeholder: `priority` is optional, and an absent value is
/// distinct from any declared one.
pub(super) fn priority_widget(priorities: &[PriorityEntry], current: &str) -> FieldWidget {
    let options = std::iter::once(SelectOption {
        value: String::new(),
        label: "(none)".to_owned(),
        selected: false,
    })
    .chain(priorities.iter().map(|entry| SelectOption {
        value: entry.value.clone(),
        label: entry.display_label().to_owned(),
        selected: false,
    }))
    .collect();
    FieldWidget::Select(mark_selected(options, current))
}

/// The `task_type` field's widget: every task type `.contextos/tasks.toml` declares, in its order. The create form
/// offers only the subset whose id prefix the tasks sidecar declares (`routes::tasks_key_prefixes`).
pub(super) fn task_type_widget(types: &[TaskTypeEntry], current: &str) -> FieldWidget {
    let options = types
        .iter()
        .map(|entry| SelectOption {
            value: entry.value.clone(),
            label: entry.display_label().to_owned(),
            selected: false,
        })
        .collect();
    FieldWidget::Select(mark_selected(options, current))
}

#[cfg(test)]
#[path = "tasks_fields_test.rs"]
mod tests;
