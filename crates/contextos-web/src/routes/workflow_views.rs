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
    pub(crate) lifecycle: JobLifecycle,
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
    /// The workflow the job is under, on a list of jobs from several.
    pub(crate) workflow: Option<String>,
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
            lifecycle,
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
            workflow: None,
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

/// Every job's state in a list, whatever the list's filter shows, so the strip always gives the whole picture.
pub(crate) struct StateCounts(Vec<JobLifecycle>);

impl FromIterator<JobLifecycle> for StateCounts {
    fn from_iter<T: IntoIterator<Item = JobLifecycle>>(states: T) -> Self {
        Self(states.into_iter().collect())
    }
}

impl StateCounts {
    fn of(&self, filter: StateFilter) -> usize {
        self.0.iter().filter(|state| filter.admits(**state)).count()
    }
}

/// One tile of the state strip: a filter, with how many jobs it admits.
pub(crate) struct StateTile {
    pub(crate) label: &'static str,
    pub(crate) href: String,
    pub(crate) active: bool,
    pub(crate) count: usize,
    /// Whether this tile is the one that counts the jobs waiting on the operator.
    pub(crate) attention: bool,
}

/// The tiles a list offers, each a request for the same list with another filter.
pub(crate) fn state_tiles(list_path: &str, active: StateFilter, counts: &StateCounts) -> Vec<StateTile> {
    let tile = |label, href, filter| StateTile {
        label,
        href,
        active: active == filter,
        count: counts.of(filter),
        attention: filter == StateFilter::Attention,
    };
    let mut tiles = vec![
        tile("All", list_path.to_owned(), StateFilter::All),
        tile(
            "Needs attention",
            format!("{list_path}?state=attention"),
            StateFilter::Attention,
        ),
    ];
    for state in [
        JobLifecycle::Draft,
        JobLifecycle::Validated,
        JobLifecycle::Registered,
        JobLifecycle::Enabled,
        JobLifecycle::Suspended,
        JobLifecycle::Retired,
    ] {
        tiles.push(tile(
            state.as_str(),
            format!("{list_path}?state={}", state.as_str()),
            StateFilter::State(state),
        ));
    }
    tiles
}

/// Which of the workflow screens a page is, and so which tab it marks and where the others lead.
#[derive(Clone, Copy)]
pub(crate) enum Scope<'a> {
    VaultWorkflows(&'a str),
    VaultLibraries(&'a str),
    SharedWorkflows,
    SharedJobs,
    SharedLibraries,
}

/// One tab of the row that moves between a vault's workflows, the shared workflows, and the library releases.
pub(crate) struct ScopeTab {
    pub(crate) label: String,
    pub(crate) href: String,
    pub(crate) active: bool,
}

impl Scope<'_> {
    /// The tabs of this screen. The shared screens have no vault in their address, so `vaults` is every configured
    /// vault, each with a tab of its own that leads back to it; a vault's own screens need no other vault's.
    pub(crate) fn tabs(self, vaults: &[String]) -> Vec<ScopeTab> {
        let tab = |label: String, href: String, active: bool| ScopeTab { label, href, active };
        let shared = |active| {
            tab(
                "Shared Workflows".to_owned(),
                crate::urls::SERVER_WORKFLOWS.to_owned(),
                active,
            )
        };
        let libraries = |href, active| tab("Library Releases".to_owned(), href, active);
        match self {
            Self::VaultWorkflows(vault) | Self::VaultLibraries(vault) => {
                let urls = crate::urls::VaultUrls::from(vault);
                vec![
                    tab(
                        "Workflows".to_owned(),
                        urls.workflows(),
                        matches!(self, Self::VaultWorkflows(_)),
                    ),
                    shared(false),
                    libraries(urls.packages(), matches!(self, Self::VaultLibraries(_))),
                ]
            }
            Self::SharedWorkflows | Self::SharedJobs | Self::SharedLibraries => {
                let mut tabs: Vec<ScopeTab> = vaults
                    .iter()
                    .map(|vault| {
                        tab(
                            format!("{vault} workflows"),
                            crate::urls::VaultUrls::from(vault.as_str()).workflows(),
                            false,
                        )
                    })
                    .collect();
                tabs.push(shared(matches!(self, Self::SharedWorkflows)));
                tabs.push(tab(
                    "Shared Jobs".to_owned(),
                    crate::urls::SERVER_JOBS.to_owned(),
                    matches!(self, Self::SharedJobs),
                ));
                tabs.push(libraries(
                    crate::urls::SERVER_PACKAGES.to_owned(),
                    matches!(self, Self::SharedLibraries),
                ));
                tabs
            }
        }
    }
}

/// A link back up from a page to the list it belongs to.
pub(crate) struct Link {
    pub(crate) href: String,
    pub(crate) label: &'static str,
}

#[derive(Template)]
#[template(path = "workflows_list.html")]
pub(crate) struct WorkflowsList {
    /// The other places workflows are kept, and the library releases beside them.
    pub(crate) scope: Vec<ScopeTab>,
    pub(crate) filter_value: &'static str,
    pub(crate) tiles: Vec<StateTile>,
    pub(crate) workflows: Vec<WorkflowEntry>,
    /// What the page says about the vault before its workflows, such as a team that cannot be used.
    pub(crate) notices: Vec<String>,
}

/// Every job of the server, whichever workflow it is under.
#[derive(Template)]
#[template(path = "server_jobs.html")]
pub(crate) struct JobsList {
    pub(crate) scope: Vec<ScopeTab>,
    pub(crate) filter_value: &'static str,
    pub(crate) tiles: Vec<StateTile>,
    pub(crate) jobs: Vec<JobCard>,
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

/// The server's jobs as a fragment, or a sentence saying they could not be drawn.
pub(crate) fn render_jobs(view: &JobsList) -> String {
    view.render()
        .unwrap_or_else(|_| "<p>The jobs could not be rendered.</p>".to_owned())
}

/// One workflow as a fragment, or a sentence saying it could not be drawn.
pub(crate) fn render_detail(view: &WorkflowDetail) -> String {
    view.render()
        .unwrap_or_else(|_| "<p>The workflow could not be rendered.</p>".to_owned())
}
