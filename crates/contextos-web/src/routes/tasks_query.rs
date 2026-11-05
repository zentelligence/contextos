//! The Tasks screen's query string: what `GET /{vault}/tasks/` reads from it ([`TasksQuery`]) and the modes it
//! selects, and the one builder ([`tasks_href`]) every link the screen renders goes through, so no two links spell a
//! filter differently.

use serde::Deserialize;

#[derive(Debug, Default, Deserialize, Clone)]
pub struct TasksQuery {
    /// The global entity switcher's own selection; filters the row set to
    /// this `entity`, case-insensitively, matching `memory/tasks.base`'s
    /// own `entity` property.
    #[serde(default)]
    pub entity: Option<String>,
    /// The stage-strip's own selected tile: one configured `state`, or
    /// several comma-joined when a Focus bucket's own "browse all"/
    /// overflow link spans more than one state
    /// (`tasks_focus::bucket_href`). Applies to the Table and Focus views;
    /// the Hierarchy view always renders its full tree, since filtering a
    /// tree to one state would silently orphan a matching node's own
    /// ancestors and children.
    #[serde(default)]
    pub stage: Option<String>,
    /// `None`/`"focus"` (the default), `"table"`, or `"hierarchy"`.
    #[serde(default)]
    pub view: Option<String>,
    /// Free-text search, matched case-insensitively against a row's own
    /// `title` or `id` substring. Applies to the Table and Focus views.
    #[serde(default)]
    pub q: Option<String>,
    /// One exact `assignee` value (case-insensitive), or absent/empty for
    /// every assignee. Applies to the Table and Focus views.
    #[serde(default)]
    pub assignee: Option<String>,
    /// One exact `task_type` value (`"action"`/`"decision"`/`"question"`, case-insensitive), or absent/empty for every
    /// type.
    /// Applies to the Table and Focus views, the same as `assignee`.
    #[serde(default)]
    pub task_type: Option<String>,
    /// One exact `priority` value (case-insensitive), or absent/empty for every priority. Applies to the Table and
    /// Focus views, the same as `assignee`.
    #[serde(default)]
    pub priority: Option<String>,
    /// The Table view's own row order: `None`/`""`/`"id"` (by id, the default), `"title"`, `"due"`, `"priority"`, or
    /// `"assignee"` ([`SortKey::from_query`]). Ignored by the Focus view (its own sections have a fixed, curated
    /// order) and the Hierarchy view (a `parent`-tree's own pre-order is what makes it a tree at all).
    #[serde(default)]
    pub sort: Option<String>,
    /// The Table/Hierarchy views' own row layout: `None`/`"table"` (the existing plain table, the default) or
    /// `"cards"` (the same card grid the Focus view already uses, [`DisplayMode::from_query`]). The Focus view
    /// ignores this: its own sectioned layout already mixes cards and compact rows by design, not a single
    /// homogeneous list this toggle could sensibly switch as a whole.
    #[serde(default)]
    pub display: Option<String>,
    /// The Table/Hierarchy views' own done-state visibility: `None` (the default) hides every row bucketed to
    /// [`Bucket::Done`](crate::tasks_config::Bucket::Done) unless the Done today tile is selected
    /// ([`TasksQuery::completed`]) or the current `?stage=` filter explicitly names one of those states itself;
    /// `"show"` always shows them; any other value (the toggle's own explicit "hide" link, [`done_href`]) always
    /// hides them, including against either selection.
    /// Ignored by the Focus view, whose own "Done" panel is already a separate, deliberately capped section rather
    /// than noise mixed into a flat list.
    #[serde(default)]
    pub done: Option<String>,
    /// The stage strip's Done today tile: lists only the tasks closed as completed on the day it names, done tasks
    /// shown unless `?done=` hides them explicitly. Applies to every view, unlike `stage`: the tree it leaves keeps
    /// each listed task, a task whose parent is not listed shown as a root. Replaces a `stage` selection when its
    /// tile is clicked, as a stage tile replaces it. Never changes what the strip counts.
    #[serde(default)]
    pub completed: Option<CompletedOn>,
}

/// The day `?completed=` names. Only today has a tile; any other value is refused rather than read as no filter.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompletedOn {
    Today,
}

impl CompletedOn {
    /// The value `?completed=` carries.
    pub(super) const fn as_param(self) -> &'static str {
        match self {
            Self::Today => "today",
        }
    }
}

/// Which of the Tasks screen's three view modes is current.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ViewMode {
    Focus,
    Table,
    Hierarchy,
}

impl ViewMode {
    pub(super) fn from_query(query: &TasksQuery) -> Self {
        match query.view.as_deref() {
            Some("table") => Self::Table,
            Some("hierarchy") => Self::Hierarchy,
            _ => Self::Focus,
        }
    }

    /// The `?view=` value this mode round-trips as, or `None` for Focus:
    /// the default mode is never spelled out in a generated link, matching
    /// every other query parameter this screen omits when it is already
    /// the implied default.
    pub(super) fn as_param(self) -> Option<&'static str> {
        match self {
            Self::Focus => None,
            Self::Table => Some("table"),
            Self::Hierarchy => Some("hierarchy"),
        }
    }
}

/// Which row layout the Table/Hierarchy views render: the existing plain table, or the Focus view's own card grid
/// reused here. Never consulted by the Focus view itself (`ViewMode::Focus`'s own doc covers why).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DisplayMode {
    Table,
    Cards,
}

impl DisplayMode {
    pub(super) fn from_query(query: &TasksQuery) -> Self {
        match query.display.as_deref() {
            Some("cards") => Self::Cards,
            _ => Self::Table,
        }
    }

    /// The `?display=` value this mode round-trips as, or `None` for Table: the default is never spelled out in a
    /// generated link, matching [`ViewMode::as_param`]'s identical convention.
    pub(super) fn as_param(self) -> Option<&'static str> {
        match self {
            Self::Table => None,
            Self::Cards => Some("cards"),
        }
    }
}

/// Whether the Table/Hierarchy views are currently excluding done-state rows
/// ([`effective_hide_done`](super::tasks_strip::effective_hide_done)'s own doc covers the precedence rule this
/// reflects): an enum, not a bool, matching [`DisplayMode`]'s own established "stay under this workspace's own
/// `clippy::struct_excessive_bools` bound" precedent for the Tasks screen's list template.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DoneVisibility {
    Hidden,
    Shown,
}

impl From<bool> for DoneVisibility {
    fn from(hide_done: bool) -> Self {
        if hide_done { Self::Hidden } else { Self::Shown }
    }
}

/// The Table view's own row order ([`TasksQuery::sort`]'s own doc covers the vocabulary). Never consulted by the
/// Focus or Hierarchy views.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SortKey {
    Id,
    Title,
    Due,
    Priority,
    Assignee,
}

impl SortKey {
    pub(super) fn from_query(query: &TasksQuery) -> Self {
        match query.sort.as_deref() {
            Some("title") => Self::Title,
            Some("due") => Self::Due,
            Some("priority") => Self::Priority,
            Some("assignee") => Self::Assignee,
            _ => Self::Id,
        }
    }

    pub(super) fn as_param(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Title => "title",
            Self::Due => "due",
            Self::Priority => "priority",
            Self::Assignee => "assignee",
        }
    }
}

/// Percent-encodes `value` for safe inclusion in a URL query-string value.
/// Only `q` and `assignee` ever need this: every other query value this
/// screen builds is always a known slug, state, or id with no character
/// that would corrupt a query string.
fn encode_query_value(value: &str) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => encoded.push(byte as char),
            // `write!` to a `String` never fails.
            _ => drop(write!(encoded, "%{byte:02X}")),
        }
    }
    encoded
}

/// How [`tasks_href`] should treat the stage strip's current selection: a `?stage=` filter, or the Done today tile's
/// `?completed=`, never both from a tile click. Unclear as a plain `Option<Option<&str>>` (`clippy::option_option`'s
/// own reason for flagging that shape), so a small named enum instead.
#[derive(Clone, Copy)]
pub(super) enum StageOverride<'a> {
    /// Carry `query`'s own current selection forward unchanged.
    Keep,
    /// Drop the selection entirely.
    Clear,
    /// Replace it with one state, or several comma-joined.
    Set(&'a str),
    /// Replace it with the Done today tile's selection, [`TasksQuery::completed`].
    CompletedToday,
}

/// Builds a `/{vault_name}/tasks/` href carrying `view` and every filter
/// `query` already has, except the strip's selection (`stage` and
/// `completed`), which `stage_override` controls.
/// Every link this screen renders (stage tiles, the view-mode toggle, the
/// Focus view's own drill-down/browse-all/overflow links, and the search/
/// assignee filter form's own hidden fields) goes through this one
/// builder, so no two links can disagree about how a filter is spelled.
pub(super) fn tasks_href(
    vault_name: &str,
    view: ViewMode,
    query: &TasksQuery,
    stage_override: StageOverride<'_>,
) -> String {
    let (stage, completed) = match stage_override {
        StageOverride::Keep => (query.stage.as_deref(), query.completed),
        StageOverride::Clear => (None, None),
        StageOverride::Set(state) => (Some(state), None),
        StageOverride::CompletedToday => (None, Some(CompletedOn::Today)),
    };
    let mut params = Vec::new();
    if let Some(view_param) = view.as_param() {
        params.push(format!("view={view_param}"));
    }
    if let Some(stage) = stage {
        params.push(format!("stage={stage}"));
    }
    if let Some(completed) = completed {
        params.push(format!("completed={}", completed.as_param()));
    }
    if let Some(entity) = query.entity.as_deref() {
        params.push(format!("entity={entity}"));
    }
    if let Some(q) = query.q.as_deref().filter(|value| !value.is_empty()) {
        params.push(format!("q={}", encode_query_value(q)));
    }
    if let Some(assignee) = query.assignee.as_deref().filter(|value| !value.is_empty()) {
        params.push(format!("assignee={}", encode_query_value(assignee)));
    }
    if let Some(task_type) = query.task_type.as_deref().filter(|value| !value.is_empty()) {
        params.push(format!("task_type={}", encode_query_value(task_type)));
    }
    if let Some(priority) = query.priority.as_deref().filter(|value| !value.is_empty()) {
        params.push(format!("priority={}", encode_query_value(priority)));
    }
    if let Some(sort) = query.sort.as_deref().filter(|value| !value.is_empty()) {
        params.push(format!("sort={}", encode_query_value(sort)));
    }
    if let Some(display) = query.display.as_deref().filter(|value| !value.is_empty()) {
        params.push(format!("display={}", encode_query_value(display)));
    }
    if let Some(done) = query.done.as_deref().filter(|value| !value.is_empty()) {
        params.push(format!("done={}", encode_query_value(done)));
    }
    let urls = crate::urls::VaultUrls::from(vault_name);
    if params.is_empty() {
        urls.tasks()
    } else {
        urls.tasks_with(&params.join("&"))
    }
}

/// A `?display=` toggle link, carrying every other current filter/sort/view param forward unchanged (built by
/// cloning `query` and overwriting only `display`, then routing through [`tasks_href`] like every other link this
/// screen builds, so it can never quietly disagree with them about how a filter is spelled).
pub(super) fn display_href(vault_name: &str, view: ViewMode, query: &TasksQuery, display: DisplayMode) -> String {
    let mut overridden = query.clone();
    overridden.display = display.as_param().map(str::to_owned);
    tasks_href(vault_name, view, &overridden, StageOverride::Keep)
}

/// A `?done=` toggle link, matching [`display_href`]'s identical "clone `query`, override one field, route through
/// [`tasks_href`]" pattern. `hide` is the *new* state the link should switch to, not the current one.
pub(super) fn done_href(vault_name: &str, view: ViewMode, query: &TasksQuery, hide: bool) -> String {
    let mut overridden = query.clone();
    overridden.done = Some(if hide { "hide" } else { "show" }.to_owned());
    tasks_href(vault_name, view, &overridden, StageOverride::Keep)
}
