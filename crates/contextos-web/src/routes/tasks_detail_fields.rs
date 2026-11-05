//! The Tasks detail/create screens' shared frontmatter field schema: which
//! fields exist, how each renders as an editable control
//! ([`FieldView`]/[`build_fields`]), and how a submitted form's own
//! flatly-named fields become a merge patch ([`raw_fields_to_patch`]).
//! Neither screen's own route handler duplicates this classification -
//! `routes::tasks_detail`'s `detail`/`patch_frontmatter`/`close_task` and
//! `routes::tasks_create`'s `new_task_form`/`create` all share this one
//! module so the two screens can never quietly disagree about which fields
//! are arrays, which are optional, or how a link field's raw id becomes a
//! wikilink.

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::entities::EntityEntry;
use crate::tasks_config::TasksConfig;
use crate::team::TeamMember;

use super::tasks_fields::{self, FieldWidget};

/// One frontmatter field this screen knows how to label and edit, in
/// display order. `id`, `type`, and `updated` are deliberately absent: `id`
/// is assigned once at creation and never edited, `type` is the fixed
/// literal `"task"`, and `updated` is stamped automatically by
/// `frontmatter_update` itself whenever a patch does not set it explicitly,
/// so this screen never submits it.
/// How a field's own current value becomes an editable control: plain text
/// (unchanged), a `date` input (every lifecycle field, already stored as
/// `YYYY-MM-DD`), or one of the closed-enum widgets `tasks_fields` builds -
/// either backed by real vault-configured data (`Entity`/`Assignee`/`State`)
/// or by the vocabularies `.contextos/tasks.toml` declares (`Priority`/`TaskType`).
#[derive(Clone, Copy)]
enum FieldKindSpec {
    Text,
    Date,
    Entity,
    Assignee,
    State,
    Priority,
    TaskType,
}

/// Which of the detail page's own visual groupings a field belongs to:
/// [`TaskFieldGroups`]'s main column (`Title`, the one field this screen's
/// own primary content carries, alongside the fixed `<h1>` and action row),
/// or one of the sidebar's three groups, in the order they render:
/// `Identity` (what this task is), `Date` (its lifecycle), and `Remaining`
/// (everything else this screen renders as a plain field, known or not).
#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldGroup {
    Title,
    Identity,
    Date,
    Remaining,
}

struct FieldSpec {
    key: &'static str,
    label: &'static str,
    array: bool,
    optional: bool,
    kind: FieldKindSpec,
    group: FieldGroup,
}

const FIELD_ORDER: &[FieldSpec] = &[
    FieldSpec {
        key: "title",
        label: "Title",
        array: false,
        optional: false,
        kind: FieldKindSpec::Text,
        group: FieldGroup::Title,
    },
    FieldSpec {
        key: "state",
        label: "State",
        array: false,
        optional: false,
        kind: FieldKindSpec::State,
        group: FieldGroup::Identity,
    },
    FieldSpec {
        key: "task_type",
        label: "Type",
        array: false,
        optional: false,
        kind: FieldKindSpec::TaskType,
        group: FieldGroup::Identity,
    },
    FieldSpec {
        key: "entity",
        label: "Entity",
        array: false,
        optional: false,
        kind: FieldKindSpec::Entity,
        group: FieldGroup::Identity,
    },
    FieldSpec {
        key: "priority",
        label: "Priority",
        array: false,
        optional: true,
        kind: FieldKindSpec::Priority,
        group: FieldGroup::Identity,
    },
    FieldSpec {
        key: "assignee",
        label: "Assignee",
        array: false,
        optional: false,
        kind: FieldKindSpec::Assignee,
        group: FieldGroup::Identity,
    },
    FieldSpec {
        key: "created",
        label: "Created",
        array: false,
        optional: true,
        kind: FieldKindSpec::Date,
        group: FieldGroup::Date,
    },
    FieldSpec {
        key: "completed",
        label: "Completed",
        array: false,
        optional: true,
        kind: FieldKindSpec::Date,
        group: FieldGroup::Date,
    },
    FieldSpec {
        key: "cancelled",
        label: "Cancelled",
        array: false,
        optional: true,
        kind: FieldKindSpec::Date,
        group: FieldGroup::Date,
    },
    FieldSpec {
        key: "start_after",
        label: "Start after",
        array: false,
        optional: true,
        kind: FieldKindSpec::Date,
        group: FieldGroup::Date,
    },
    FieldSpec {
        key: "finish_by",
        label: "Finish by",
        array: false,
        optional: true,
        kind: FieldKindSpec::Date,
        group: FieldGroup::Date,
    },
    FieldSpec {
        key: "scheduled",
        label: "Scheduled",
        array: false,
        optional: true,
        kind: FieldKindSpec::Date,
        group: FieldGroup::Date,
    },
    FieldSpec {
        key: "due",
        label: "Due",
        array: false,
        optional: true,
        kind: FieldKindSpec::Date,
        group: FieldGroup::Date,
    },
    FieldSpec {
        key: "recurrence",
        label: "Recurrence",
        array: false,
        optional: true,
        kind: FieldKindSpec::Text,
        group: FieldGroup::Remaining,
    },
    FieldSpec {
        key: "waiting_on",
        label: "Waiting on",
        array: false,
        optional: true,
        kind: FieldKindSpec::Text,
        group: FieldGroup::Remaining,
    },
    FieldSpec {
        key: "on_completion",
        label: "On completion",
        array: false,
        optional: true,
        kind: FieldKindSpec::Text,
        group: FieldGroup::Remaining,
    },
    // `parent`, `depends_on`, `blocks`, and `related` are deliberately
    // absent here: all four are genuine task-to-task link fields, not plain
    // text, and render through `routes::tasks_links`'s own search-and-pick
    // chip widget instead (`build_link_fields`, wired into
    // `TaskDetailTemplate`/`TaskCreateTemplate` in `routes::tasks_detail`/
    // `routes::tasks_create`). `waiting_on` looks similar but is free text
    // (who or what is being waited on), so it stays in this list unchanged.
    FieldSpec {
        key: "tags",
        label: "Tags",
        array: true,
        optional: true,
        kind: FieldKindSpec::Text,
        group: FieldGroup::Remaining,
    },
    FieldSpec {
        key: "aliases",
        label: "Aliases",
        array: true,
        optional: true,
        kind: FieldKindSpec::Text,
        group: FieldGroup::Remaining,
    },
];

/// Keys this screen never renders as an editable field, whether or not they
/// appear in [`FIELD_ORDER`]: assigned once (`id`), fixed (`type`), or
/// tool-stamped (`updated`).
const NON_EDITABLE_KEYS: &[&str] = &["id", "type", "updated"];

pub(super) struct FieldView {
    pub(super) key: String,
    pub(super) label: String,
    pub(super) value: String,
    pub(super) widget: FieldWidget,
}

/// The real, vault-loaded data every closed-enum field's own widget is
/// built against, fetched once per detail-page render and threaded through
/// [`build_fields`] rather than each field re-fetching its own source.
pub(super) struct FieldContext<'a> {
    pub(super) entities: &'a [EntityEntry],
    pub(super) team: &'a [TeamMember],
    pub(super) tasks_config: &'a TasksConfig,
}

fn field_widget(kind: FieldKindSpec, value: &str, task_type: &str, context: &FieldContext<'_>) -> FieldWidget {
    match kind {
        FieldKindSpec::Text => FieldWidget::Text,
        FieldKindSpec::Date => FieldWidget::Date,
        FieldKindSpec::Entity => tasks_fields::entity_widget(context.entities, value),
        FieldKindSpec::Assignee => tasks_fields::assignee_widget(context.team, value),
        FieldKindSpec::State => tasks_fields::state_widget(context.tasks_config, task_type, value),
        FieldKindSpec::Priority => tasks_fields::priority_widget(&context.tasks_config.priority, value),
        FieldKindSpec::TaskType => tasks_fields::task_type_widget(&context.tasks_config.task_type, value),
    }
}

/// Renders one frontmatter value as the text an editable input round-trips:
/// `null`/absent as an empty string, an array as its own elements joined by
/// `", "` (split back apart on save by the array-aware patch-building
/// script), everything else via its own plain text form.
fn field_value_string(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Bool(flag)) => flag.to_string(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", "),
        Some(other) => other.to_string(),
    }
}

/// Converts one submission's raw, flatly-named fields (exactly what a
/// plain HTML form plus `hx-ext="json-enc"` serialises - no `hx-vals`
/// computation anywhere on either the create or the edit screen) into the
/// `frontmatter_update`/`base_record_create` merge-patch value each key
/// needs: a known [`FIELD_ORDER`] array field (`tags`/`aliases`) splits its
/// one comma-joined string; a known optional field submitted blank clears
/// (`Value::Null`); a known link field (`parent`/`depends_on`/`blocks`/
/// `related`) wraps each raw submitted id into its own `[[id|ID]]` wikilink,
/// cleared only when its own `{key}__present` sentinel arrived with no
/// accompanying value - the one way this screen tells "every chip was
/// removed" from "this field was never on the form", since a hidden input
/// with no chips left is simply absent from the submission, not blank.
///
/// `current_frontmatter` is `Some` for an edit (the note's own frontmatter
/// as last read, consulted only to classify a vault-specific field
/// [`FIELD_ORDER`] does not know about by the same array-ness its current
/// value already carries) and `None` for a creation, which never renders
/// such a field in the first place.
pub(super) fn raw_fields_to_patch(
    mut raw: Map<String, Value>,
    current_frontmatter: Option<&Map<String, Value>>,
) -> Map<String, Value> {
    fn as_text(value: Value) -> String {
        match value {
            Value::String(text) => text,
            other => field_value_string(Some(&other)),
        }
    }
    fn split_array(text: &str) -> Vec<Value> {
        text.split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(|item| Value::String(item.to_owned()))
            .collect()
    }

    let mut patch = Map::new();

    for spec in FIELD_ORDER {
        let Some(value) = raw.remove(spec.key) else {
            continue;
        };
        let text = as_text(value);
        let text = text.trim();
        if spec.array {
            patch.insert(spec.key.to_owned(), Value::Array(split_array(text)));
        } else if text.is_empty() && spec.optional {
            patch.insert(spec.key.to_owned(), Value::Null);
        } else {
            patch.insert(spec.key.to_owned(), Value::String(text.to_owned()));
        }
    }

    for (key, single) in super::tasks_links::link_field_specs() {
        let present = raw.remove(&format!("{key}__present")).is_some();
        if !present {
            continue;
        }
        let ids: Vec<String> = match raw.remove(key) {
            Some(Value::String(id)) => vec![id],
            Some(Value::Array(items)) => items
                .into_iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        };
        let wikilinks: Vec<Value> = ids
            .iter()
            .map(|id| Value::String(super::tasks::wikilink_from_target(id)))
            .collect();
        let patch_value = if single {
            wikilinks.into_iter().next().unwrap_or(Value::Null)
        } else {
            Value::Array(wikilinks)
        };
        patch.insert(key.to_owned(), patch_value);
    }

    if let Some(current) = current_frontmatter {
        for (key, value) in raw {
            // A key the note's own current frontmatter never had is not a
            // genuine vault-specific field the operator edited through the
            // generic Remaining-fields UI; it is stray form noise (a
            // same-page widget's own input sharing this form, such as a
            // link field's live search box) that must never be invented as
            // a new frontmatter key.
            let Some(current_value) = current.get(&key) else {
                continue;
            };
            let is_array = matches!(current_value, Value::Array(_));
            let text = as_text(value);
            let text = text.trim();
            if is_array {
                patch.insert(key, Value::Array(split_array(text)));
            } else if text.is_empty() {
                patch.insert(key, Value::Null);
            } else {
                patch.insert(key, Value::String(text.to_owned()));
            }
        }
    }

    patch
}

/// Every field this screen renders, grouped for the detail/create
/// templates' own two-column layout: the one `title` field (the main
/// column's own primary content, alongside the fixed `<h1>` and action
/// row), then the sidebar's three groups in render order - `identity`
/// (what this task is: `state`, `task_type`, `entity`, `priority`,
/// `assignee`), `dates` (its lifecycle), and `remaining` (every other known
/// field, plus any frontmatter key this note has that [`FIELD_ORDER`] does
/// not know about, rendered generically rather than silently dropped - a
/// vault-specific extra field is exactly the kind of content a generic
/// `ContextOS` deployment must not lose just because this screen's own
/// known-field list did not anticipate it). The four `routes::tasks_links`
/// link fields are excluded from every group: they render through their
/// own widget, never as a second, competing plain-text field.
pub(super) struct TaskFieldGroups {
    pub(super) title: FieldView,
    pub(super) identity: Vec<FieldView>,
    pub(super) dates: Vec<FieldView>,
    pub(super) remaining: Vec<FieldView>,
}

impl TaskFieldGroups {
    /// Every field across every group, title first: for callers (tests, and
    /// any future whole-note field sweep) that need "every field this
    /// screen renders" without caring which visual group it landed in.
    #[cfg(test)]
    fn all(&self) -> impl Iterator<Item = &FieldView> {
        std::iter::once(&self.title)
            .chain(self.identity.iter())
            .chain(self.dates.iter())
            .chain(self.remaining.iter())
    }
}

pub(super) fn build_fields(frontmatter: &Map<String, Value>, context: &FieldContext<'_>) -> TaskFieldGroups {
    let mut title = None;
    let mut identity = Vec::new();
    let mut dates = Vec::new();
    let mut remaining = Vec::new();
    let task_type = frontmatter.get("task_type").and_then(Value::as_str).unwrap_or_default();
    for spec in FIELD_ORDER {
        let value = field_value_string(frontmatter.get(spec.key));
        let widget = field_widget(spec.kind, &value, task_type, context);
        let view = FieldView {
            key: spec.key.to_owned(),
            label: spec.label.to_owned(),
            value,
            widget,
        };
        match spec.group {
            FieldGroup::Title => title = Some(view),
            FieldGroup::Identity => identity.push(view),
            FieldGroup::Date => dates.push(view),
            FieldGroup::Remaining => remaining.push(view),
        }
    }

    let known: HashSet<&str> = FIELD_ORDER
        .iter()
        .map(|spec| spec.key)
        .chain(super::tasks_links::link_field_keys())
        .collect();
    for (key, value) in frontmatter {
        if known.contains(key.as_str()) || NON_EDITABLE_KEYS.contains(&key.as_str()) {
            continue;
        }
        remaining.push(FieldView {
            key: key.clone(),
            label: key.clone(),
            value: field_value_string(Some(value)),
            widget: FieldWidget::Text,
        });
    }

    // `FIELD_ORDER` always carries exactly one `FieldGroup::Title` entry
    // (`title` itself); this fallback exists only so a future edit that
    // dropped that entry by mistake degrades to a blank, still-editable
    // title field rather than a panic.
    let title = title.unwrap_or_else(|| FieldView {
        key: "title".to_owned(),
        label: "Title".to_owned(),
        value: field_value_string(frontmatter.get("title")),
        widget: FieldWidget::Text,
    });

    TaskFieldGroups {
        title,
        identity,
        dates,
        remaining,
    }
}

/// The note's body text: everything from `body_start_line` onward.
/// `body_start_line` is `frontmatter_read`'s own 1-indexed line number of
/// the body's first line (confirmed against a real note: it names the
/// blank line immediately after the closing `---`, not the heading after
/// it), so the 0-indexed line to start collecting from is one less.
pub(super) fn extract_body(full_text: &str, body_start_line: u64) -> String {
    let skip = usize::try_from(body_start_line.saturating_sub(1)).unwrap_or(usize::MAX);
    full_text.split('\n').skip(skip).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
#[path = "tasks_detail_fields_test.rs"]
mod tests;
