//! What a list of workflows, one workflow, and a job's card look like, for a vault's workflows and the host's shared
//! workflows alike.
//!
//! Both scopes are the same hierarchy of workflows, jobs, and runs, shown with the same templates. Each route module gathers
//! what its scope has and builds these views, so what an operator sees does not depend on where a job lives.

use askama::Template;
use contextos_workflows::{JobLifecycle, JobState};

use crate::job_operation::needs_attention;
use crate::job_runs::RunSummary;
use crate::release_views::ReleasesSection;

/// One job as a card shows it.
pub(crate) struct JobCard {
    /// The job's name, or its slug when its file does not register, so there is no name to show.
    pub(crate) name: String,
    pub(crate) href: String,
    pub(crate) registers: bool,
    /// Whether the card offers to run the job now.
    pub(crate) can_run: bool,
    pub(crate) lifecycle_label: &'static str,
    pub(crate) attention: bool,
    pub(crate) schedule_label: String,
    pub(crate) owner_name: String,
    pub(crate) owner_icon: &'static str,
    /// The latest run's state, for the status dot.
    pub(crate) state: &'static str,
    pub(crate) last_run: String,
    /// The star toggle's own rendered markup, or nothing for a job that has no favourites.
    pub(crate) favourite_toggle_html: String,
    pub(crate) accent: u8,
}

/// What a card is built from, whichever scope the job is in.
pub(crate) struct CardParts<'a> {
    /// The job's slug, which its accent colour is taken from.
    pub(crate) slug: &'a str,
    pub(crate) name: String,
    pub(crate) href: String,
    pub(crate) registers: bool,
    pub(crate) can_run: bool,
    pub(crate) stored: Option<&'a JobState>,
    pub(crate) owner: Option<&'a crate::team::TeamMember>,
    pub(crate) latest: Option<&'a RunSummary>,
    pub(crate) favourite_toggle_html: String,
}

impl From<CardParts<'_>> for JobCard {
    fn from(parts: CardParts<'_>) -> Self {
        let lifecycle = parts.stored.map_or_else(JobLifecycle::default, JobState::lifecycle);
        Self {
            name: parts.name,
            href: parts.href,
            registers: parts.registers,
            can_run: parts.can_run,
            lifecycle_label: lifecycle.as_str(),
            attention: needs_attention(lifecycle),
            schedule_label: parts
                .stored
                .and_then(|state| state.schedule().map(str::to_owned))
                .unwrap_or_else(|| "Manual trigger only".to_owned()),
            owner_name: parts
                .owner
                .map_or_else(|| "Unassigned".to_owned(), |owner| owner.name.clone()),
            owner_icon: parts.owner.map_or("user-round", |owner| match owner.kind {
                crate::team::TeamMemberKind::Agent => "bot",
                crate::team::TeamMemberKind::Human => "user-round",
            }),
            state: parts.latest.map_or("idle", RunSummary::state),
            last_run: parts
                .latest
                .map_or_else(|| "Never run".to_owned(), |run| run.started.clone()),
            favourite_toggle_html: parts.favourite_toggle_html,
            accent: parts.slug.bytes().fold(0u8, |sum, byte| (sum + byte % 6) % 6) + 1,
        }
    }
}

/// One labelled fact about a workflow.
pub(crate) struct Fact {
    pub(crate) label: &'static str,
    pub(crate) value: String,
}

/// Why a workflow cannot run its jobs, as a badge and as the sentence on its own page.
pub(crate) struct Problem {
    pub(crate) badge: &'static str,
    pub(crate) detail: String,
}

/// One workflow, with its jobs as cards.
pub(crate) struct WorkflowEntry {
    pub(crate) name: String,
    pub(crate) href: String,
    pub(crate) problem: Option<Problem>,
    pub(crate) facts: Vec<Fact>,
    pub(crate) jobs: Vec<JobCard>,
}

/// Which jobs a list shows, from the `state` query parameter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StateFilter {
    All,
    /// Drafts and suspended jobs, which need the operator.
    Attention,
    State(JobLifecycle),
}

impl TryFrom<Option<&str>> for StateFilter {
    type Error = ();

    fn try_from(value: Option<&str>) -> Result<Self, ()> {
        match value {
            None | Some("" | "all") => Ok(Self::All),
            Some("attention") => Ok(Self::Attention),
            Some(name) => JobLifecycle::try_from(name).map(Self::State).map_err(|_| ()),
        }
    }
}

impl StateFilter {
    pub(crate) fn admits(self, lifecycle: JobLifecycle) -> bool {
        match self {
            Self::All => true,
            Self::Attention => needs_attention(lifecycle),
            Self::State(wanted) => lifecycle == wanted,
        }
    }

    pub(crate) fn value(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Attention => "attention",
            Self::State(state) => state.as_str(),
        }
    }
}

/// One filter chip.
pub(crate) struct FilterChip {
    pub(crate) label: &'static str,
    pub(crate) href: String,
    pub(crate) active: bool,
}

/// The chips a list offers, each a request for the same list with another filter.
pub(crate) fn chips(list_path: &str, active: StateFilter) -> Vec<FilterChip> {
    let mut chips = vec![
        FilterChip {
            label: "All",
            href: list_path.to_owned(),
            active: active == StateFilter::All,
        },
        FilterChip {
            label: "Needs attention",
            href: format!("{list_path}?state=attention"),
            active: active == StateFilter::Attention,
        },
    ];
    for state in [
        JobLifecycle::Draft,
        JobLifecycle::Validated,
        JobLifecycle::Registered,
        JobLifecycle::Enabled,
        JobLifecycle::Suspended,
        JobLifecycle::Retired,
    ] {
        chips.push(FilterChip {
            label: state.as_str(),
            href: format!("{list_path}?state={}", state.as_str()),
            active: active == StateFilter::State(state),
        });
    }
    chips
}

/// A link to the other scope's workflows, which a list offers.
pub(crate) struct Link {
    pub(crate) href: String,
    pub(crate) label: &'static str,
}

#[derive(Template)]
#[template(path = "workflows_list.html")]
pub(crate) struct WorkflowsList {
    /// Links to the other places workflows are kept and to the packages they come from.
    pub(crate) links: Vec<Link>,
    pub(crate) filter_value: &'static str,
    pub(crate) chips: Vec<FilterChip>,
    pub(crate) workflows: Vec<WorkflowEntry>,
    pub(crate) shown: usize,
    /// What the page says about the vault before its workflows, such as a team that cannot be used.
    pub(crate) notices: Vec<String>,
}

#[derive(Template)]
#[template(path = "workflow_detail.html")]
pub(crate) struct WorkflowDetail {
    pub(crate) back: Link,
    pub(crate) workflow: WorkflowEntry,
    /// The workflow's releases, with the jobs on each.
    pub(crate) releases: ReleasesSection,
}

#[derive(Template)]
#[template(path = "job_detail.html")]
pub(crate) struct JobDetail {
    pub(crate) workflow_href: String,
    pub(crate) job: JobCard,
    /// The status region, already rendered, which an action replaces.
    pub(crate) status_html: String,
    /// The release region, already rendered: the release, its lock, the jobs either side, and what is wrong with the job.
    pub(crate) release_html: String,
    pub(crate) registers: bool,
    pub(crate) facts: Vec<Fact>,
    /// The runs region, already rendered: what the job's runs are doing, the controls, and the log.
    pub(crate) runs_html: String,
    /// What the page says about the job before its facts, such as an owner who is not on the team.
    pub(crate) notices: Vec<String>,
}

/// One job as a fragment, or a sentence saying it could not be drawn.
pub(crate) fn render_job(view: &JobDetail) -> String {
    view.render()
        .unwrap_or_else(|_| "<p>The job could not be rendered.</p>".to_owned())
}

/// The list of workflows as a fragment, or a sentence saying it could not be drawn.
pub(crate) fn render_list(view: &WorkflowsList) -> String {
    view.render()
        .unwrap_or_else(|_| "<p>The workflows could not be rendered.</p>".to_owned())
}

/// One workflow as a fragment, or a sentence saying it could not be drawn.
pub(crate) fn render_detail(view: &WorkflowDetail) -> String {
    view.render()
        .unwrap_or_else(|_| "<p>The workflow could not be rendered.</p>".to_owned())
}
