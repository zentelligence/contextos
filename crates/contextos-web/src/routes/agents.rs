//! `GET /{{vault_name}}/agents/`: the Command Centre Agents screen, listing `.contextos/team.toml`'s `type: "agent"`
//! entries grouped by domain, with each agent's working/blocked/idle/awaiting-input state and an optional `?state=`
//! filter. `team.toml`'s human entries never appear here - this screen is agent-specific; they still appear on the
//! Tasks `assignee` field, which reads the same file unfiltered.
//!
//! This route only ever reads `.contextos/team.toml`; the `state` field is written exclusively by `contextos-mcp`'s
//! own `agent_state_change` tool, never by this crate.
//!
//! The entity switcher still renders (it is global chrome, present on every screen), but its selection has no
//! effect on this list: agents work across every entity, not scoped to one (confirmed by the operator directly),
//! so `?entity=` is threaded through only so the switcher itself renders correctly and keeps its selection across
//! navigation, never as a filter on `.contextos/team.toml`'s own rows.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::agents_liveness::MemberLiveness;
use crate::config;
use crate::mcp_client::McpCallError;
use crate::mcp_client::{McpClient, McpClientSet};
use crate::rendering::page;
use crate::rendering::shell::{self, ActiveScreen};
use crate::routes::vault::{html_response, not_found, unreachable};
use crate::team::{self, MemberState, TeamMember};
use crate::view_favourites;

#[derive(Clone)]
pub struct AgentsRoutesState {
    pub clients: Arc<McpClientSet>,
    pub web_config_path: Arc<PathBuf>,
    /// Where liveness reads "now", to tell a run that is overdue from one that is still working.
    pub clock: Arc<dyn contextos_workflows::Clock>,
}

impl From<super::CommonRoutesState> for AgentsRoutesState {
    fn from(value: super::CommonRoutesState) -> Self {
        Self {
            clients: value.clients,
            web_config_path: value.web_config_path,
            clock: value.clock,
        }
    }
}

impl AgentsRoutesState {
    fn client(&self) -> Result<Arc<McpClient>, Box<Response>> {
        crate::routes::primary_session(&self.clients)
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct AgentsQuery {
    /// The global entity switcher's own selection: read only so
    /// [`shell::build_nav`] can render the switcher correctly and keep its
    /// selection across navigation, never to filter the agent list itself.
    #[serde(default)]
    pub entity: Option<String>,
    /// The attention-rail state filter (`working`/`blocked`/`idle`/
    /// `awaiting-input`); absent shows every configured agent.
    #[serde(default)]
    pub state: Option<String>,
    /// The sidebar's Business/Personal quick-access filter, matched against
    /// each agent's own `team` field case-insensitively; absent shows every
    /// configured agent regardless of team. Combinable with `state` (both
    /// present narrows by both).
    #[serde(default)]
    pub team: Option<String>,
}

struct AgentListEntry {
    name: String,
    chief_of_staff: bool,
    state_label: &'static str,
    detail_href: String,
    /// What the member's session events and claims say of whether its sessions are alive; none when no session event
    /// names it.
    liveness: Option<LivenessView>,
    /// Claims on the member's tasks that lapsed in the last week.
    lapsed: Vec<LapsedView>,
}

/// A member's liveness, ready to show.
struct LivenessView {
    last_finish: Option<LastFinishView>,
    claimed: usize,
    overdue: Vec<OverdueView>,
}

struct LastFinishView {
    iso: String,
    time: String,
    severity: &'static str,
    colour: &'static str,
}

struct OverdueView {
    run_id: String,
    time: String,
}

struct LapsedView {
    task_id: String,
    href: String,
    time: String,
}

impl LivenessView {
    fn new(found: &MemberLiveness) -> Option<Self> {
        found.shows_liveness().then(|| Self {
            last_finish: found.last_finish.map(|(at, severity)| LastFinishView {
                iso: at
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_default(),
                time: crate::signals_view::format_time(at),
                severity: severity.as_str(),
                colour: crate::signals_view::SeverityColour::from(severity).0,
            }),
            claimed: found.claimed,
            overdue: found
                .overdue
                .iter()
                .take(OVERDUE_SHOWN)
                .map(|run| OverdueView {
                    run_id: run.run_id.clone(),
                    time: crate::signals_view::format_time(run.started),
                })
                .collect(),
        })
    }
}

/// How many overdue runs a member's card names.
const OVERDUE_SHOWN: usize = 3;

struct DomainGroup {
    domain: String,
    agents: Vec<AgentListEntry>,
}

/// Which state-filter tab is active, as one flag apiece: an Askama
/// template comparing an `Option<&str>` field against `Some("literal")`
/// is unreliable syntax, so the comparison happens once here in Rust
/// instead, matching [`crate::rendering::page::NavData`]'s own
/// `active_*_screen` boolean-per-destination precedent, including its
/// identical justification for allowing `struct_excessive_bools`:
/// [`StateFilterTabs::for_selection`] is this struct's only constructor,
/// and it sets every flag from one `selected` comparison, so an invalid
/// combination cannot arise even though the type itself does not forbid it.
#[allow(clippy::struct_excessive_bools)]
struct StateFilterTabs {
    all: bool,
    working: bool,
    blocked: bool,
    awaiting_input: bool,
    idle: bool,
}

impl StateFilterTabs {
    fn for_selection(selected: Option<&str>) -> Self {
        Self {
            all: selected.is_none(),
            working: selected == Some(MemberState::Working.label()),
            blocked: selected == Some(MemberState::Blocked.label()),
            awaiting_input: selected == Some(MemberState::AwaitingInput.label()),
            idle: selected == Some(MemberState::Idle.label()),
        }
    }
}

#[derive(Template)]
#[template(path = "agents_list.html")]
struct AgentsListTemplate {
    /// Every URL the template links to within the vault.
    urls: crate::urls::VaultUrls,
    groups: Vec<DomainGroup>,
    state_filter: StateFilterTabs,
    /// This list's own address, which it reloads itself from when the live stream reports a session or claim event.
    refresh_href: String,
}

fn list_entry(vault_name: &str, agent: &TeamMember, liveness: &HashMap<String, MemberLiveness>) -> AgentListEntry {
    let found = liveness.get(&agent.slug);
    let urls = crate::urls::VaultUrls::from(vault_name);
    AgentListEntry {
        liveness: found.and_then(LivenessView::new),
        lapsed: found.map_or_else(Vec::new, |found| {
            found
                .lapsed
                .iter()
                .map(|lapse| LapsedView {
                    task_id: lapse.task_id.clone(),
                    href: urls.task(&lapse.task_id),
                    time: crate::signals_view::format_time(lapse.at),
                })
                .collect()
        }),
        name: agent.name.clone(),
        chief_of_staff: agent.chief_of_staff,
        state_label: agent.state.label(),
        // The definition's file: the generic route sends it on to the Vault Settings page that shows it.
        detail_href: crate::urls::VaultUrls::from(vault_name).file(&format!("registry/agents/{}.md", agent.slug)),
    }
}

/// Groups `agents` by their own `domain` (an agent with no `domain` set falls into a literal "Unassigned" group,
/// never silently dropped), preserving each group's first-seen order rather than sorting alphabetically:
/// `.contextos/team.toml`'s own row order is whatever the operator chose, the same "operator's own order wins"
/// precedent `entities::seed_from_index` already established.
fn group_by_domain(
    vault_name: &str,
    agents: &[TeamMember],
    liveness: &HashMap<String, MemberLiveness>,
) -> Vec<DomainGroup> {
    let mut groups: Vec<DomainGroup> = Vec::new();
    for agent in agents {
        let domain = agent.domain.clone().unwrap_or_else(|| "Unassigned".to_owned());
        let entry = list_entry(vault_name, agent, liveness);
        if let Some(group) = groups.iter_mut().find(|group| group.domain == domain) {
            group.agents.push(entry);
        } else {
            groups.push(DomainGroup {
                domain,
                agents: vec![entry],
            });
        }
    }
    groups
}

fn render_list(
    vault_name: &str,
    agents: &[TeamMember],
    query: &AgentsQuery,
    liveness: &HashMap<String, MemberLiveness>,
) -> String {
    let filtered: Vec<TeamMember> = team::matching_agents(agents, query.team.as_deref(), query.state.as_deref())
        .cloned()
        .collect();
    let template = AgentsListTemplate {
        urls: crate::urls::VaultUrls::from(vault_name),
        groups: group_by_domain(vault_name, &filtered, liveness),
        state_filter: StateFilterTabs::for_selection(query.state.as_deref()),
        refresh_href: crate::urls::VaultUrls::from(vault_name).agents_with(&refresh_query(query)),
    };
    template
        .render()
        .unwrap_or_else(|_| "<p>The agents list could not be rendered.</p>".to_owned())
}

/// The query string that keeps the list's filters when it reloads itself, already encoded.
fn refresh_query(query: &AgentsQuery) -> String {
    let mut pairs = Vec::new();
    for (name, value) in [
        ("entity", &query.entity),
        ("state", &query.state),
        ("team", &query.team),
    ] {
        if let Some(value) = value.as_deref().filter(|value| !value.is_empty()) {
            pairs.push(format!("{name}={}", crate::inbox_outbox::encode_query_value(value)));
        }
    }
    pairs.join("&")
}

async fn ensure_vault(client: &McpClient, vault_name: &str) -> Result<(), Box<Response>> {
    match crate::apps::vault_exists(client, vault_name).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(Box::new(not_found())),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => Err(Box::new(unreachable())),
    }
}

/// `GET /{{vault_name}}/agents/`.
pub async fn list(
    State(state): State<AgentsRoutesState>,
    Path(vault_name): Path<String>,
    Query(query): Query<AgentsQuery>,
    headers: HeaderMap,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let agents = team::load_team(&client, &vault_name).await;
    let overdue_after = {
        let path = Arc::clone(&state.web_config_path);
        tokio::task::spawn_blocking(move || crate::config::current_agents(&path).run_overdue())
            .await
            .unwrap_or_else(|_| crate::config::AgentsConfig::default().run_overdue())
    };
    let slugs: Vec<String> = agents.iter().map(|agent| agent.slug.clone()).collect();
    let liveness =
        crate::agents_load::load_liveness(&client, &vault_name, &slugs, state.clock.now(), overdue_after).await;
    let fragment = render_list(&vault_name, &agents, &query, &liveness);
    if headers.contains_key("hx-request") {
        html_response(fragment)
    } else {
        let web_config_path = Arc::clone(&state.web_config_path);
        let entity_base_path = crate::urls::VaultUrls::from(vault_name.as_str()).agents();
        let known_mcp_servers = state.clients.names();
        // The path alone cannot say which Agents view this is: the General, Business, Personal, and Blocked views are
        // filters in the query, so the title is chosen here, where the query is known.
        let view = view_favourites::agents_view(query.team.as_deref(), query.state.as_deref());
        let (mut nav, appearance, mcp_status, page_title) = tokio::join!(
            Box::pin(shell::build_nav(
                &client,
                ActiveScreen::Agents,
                Some(&vault_name),
                Some("Agents"),
                query.entity.as_deref(),
                Some(&entity_base_path),
                &known_mcp_servers,
            )),
            async move {
                tokio::task::spawn_blocking(move || config::current_appearance(&web_config_path))
                    .await
                    .unwrap_or_default()
            },
            shell::mcp_status(&state.clients),
            shell::page_title(&client, &vault_name, view),
        );
        nav.appearance = appearance;
        nav.sidebar_wide = crate::routes::sidebar_wide_from_headers(&headers);
        nav.mcp_status = mcp_status;
        nav.page_title = Some(page_title);
        html_response(page::render_page(&nav, "Agents", &fragment))
    }
    .into_response()
}

#[cfg(test)]
#[path = "agents_test.rs"]
mod tests;
