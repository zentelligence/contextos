//! The vault Overview's panels as regions: `GET /{{vault_name}}/overview/{{bridge,status,inputs}}`, each a fragment
//! the Overview loads after the page itself, so one slow or unreadable source never holds up the dashboard, and each
//! reloading itself when the live stream says something may have changed. The Overview's other panels reuse regions
//! that already exist: Needs a call from you is the Signals attention region and the Alert rail is the alerts list.

use std::path::PathBuf;
use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use time::Duration;

use crate::mcp_client::{McpClient, McpClientSet};
use crate::overview_panels::{
    BridgeView, ChiefCard, ChiefFacts, EventTally, InputRow, InputsView, RecordedSource, StatusView, chief_of_staff,
    newest_per_kind,
};
use crate::routes::vault::html_response;
use crate::signals_query::EventsApi;
use crate::urls::VaultUrls;
use crate::vault_catalogue::ManagementCategory;

/// How many of the last day's events System status counts before it says there were more.
const TALLY_CEILING: usize = 1000;

/// How many of the newest recorded events Inputs reads to find each source's latest.
const RECORDED_LIMIT: usize = 200;

#[derive(Clone)]
pub struct OverviewRoutesState {
    pub clients: Arc<McpClientSet>,
    pub web_config_path: Arc<PathBuf>,
    pub clock: Arc<dyn contextos_workflows::Clock>,
}

impl From<super::CommonRoutesState> for OverviewRoutesState {
    fn from(value: super::CommonRoutesState) -> Self {
        Self {
            clients: value.clients,
            web_config_path: value.web_config_path,
            clock: value.clock,
        }
    }
}

async fn session(state: &OverviewRoutesState, vault_name: &str) -> Result<Arc<McpClient>, Box<Response>> {
    let client = crate::routes::primary_session(&state.clients)?;
    super::tasks::ensure_vault(&client, vault_name).await?;
    Ok(client)
}

fn rendered(template: &impl Template) -> Response {
    html_response(
        template
            .render()
            .unwrap_or_else(|_| "<p class=\"hint\">This panel could not be rendered.</p>".to_owned()),
    )
    .into_response()
}

/// `GET /{{vault_name}}/overview/bridge`: the chief of staff, their state, liveness, and open tasks.
pub async fn bridge(State(state): State<OverviewRoutesState>, Path(vault_name): Path<String>) -> Response {
    let client = match session(&state, &vault_name).await {
        Ok(client) => client,
        Err(response) => return *response,
    };
    let urls = VaultUrls::from(vault_name.as_str());
    let team = crate::team::load_team(&client, &vault_name).await;
    let chief = match chief_of_staff(&team) {
        Some(member) => {
            let overdue_after = {
                let path = Arc::clone(&state.web_config_path);
                tokio::task::spawn_blocking(move || crate::config::current_agents(&path).run_overdue())
                    .await
                    .unwrap_or_else(|_| crate::config::AgentsConfig::default().run_overdue())
            };
            let slugs = [member.slug.clone()];
            let (liveness, assigned) = tokio::join!(
                crate::agents_load::load_liveness(&client, &vault_name, &slugs, state.clock.now(), overdue_after),
                super::tasks::open_tasks_assigned_to(&client, &vault_name, &member.slug),
            );
            Some(ChiefCard::from(ChiefFacts {
                member,
                liveness: liveness.get(&member.slug),
                assigned,
            }))
        }
        None => None,
    };
    rendered(&BridgeView {
        refresh_href: urls.overview_panel("bridge"),
        chief,
        agents_href: urls.agents(),
        decisions_href: super::vault_management::category_href(&vault_name, ManagementCategory::Decisions, ""),
    })
}

/// `GET /{{vault_name}}/overview/status`: the MCP servers, vault health, the live stream, and the last day of events.
pub async fn status(State(state): State<OverviewRoutesState>, Path(vault_name): Path<String>) -> Response {
    let client = match session(&state, &vault_name).await {
        Ok(client) => client,
        Err(response) => return *response,
    };
    let urls = VaultUrls::from(vault_name.as_str());
    let now = state.clock.now();
    let api = EventsApi {
        client: &client,
        vault_name: &vault_name,
    };
    let columns = crate::signals_load::decision_columns();
    let (status, events) = tokio::join!(
        crate::rendering::shell::mcp_status(&state.clients),
        api.oldest_since(
            crate::signals_event::EVENT_NOTES_FILTER,
            Some(now - Duration::days(1)),
            now,
            TALLY_CEILING,
            &columns,
        ),
    );
    let (tally, truncated) = match events {
        Ok(batch) => {
            let day_ago = now - Duration::days(1);
            let severities: Vec<_> = batch
                .events
                .iter()
                .filter(|event| event.at >= day_ago)
                .map(|event| event.severity)
                .collect();
            (Some(EventTally::from(severities.as_slice())), batch.truncated)
        }
        Err(_) => (None, false),
    };
    rendered(&StatusView {
        refresh_href: urls.overview_panel("status"),
        status,
        tally,
        truncated,
        health_summary_href: urls.health_summary("footer"),
        health_href: urls.settings_health(),
        signals_href: urls.signals(),
        settings_href: crate::urls::SERVER_SETTINGS.to_owned(),
    })
}

/// `GET /{{vault_name}}/overview/inputs`: the Inbox, each scheduled job, and each source that records events.
pub async fn inputs(State(state): State<OverviewRoutesState>, Path(vault_name): Path<String>) -> Response {
    let client = match session(&state, &vault_name).await {
        Ok(client) => client,
        Err(response) => return *response,
    };
    let urls = VaultUrls::from(vault_name.as_str());
    let api = EventsApi {
        client: &client,
        vault_name: &vault_name,
    };
    let server_names = state.clients.names();
    let inbox = async {
        let config = crate::inbox_outbox_config::load_inbox_outbox_config(&client, &vault_name).await;
        crate::nav_counts::pending_inbox_items(&client, &vault_name, &config).await
    };
    let columns = crate::signals_load::decision_columns();
    let filter = crate::signals_query::recorded_filter();
    let (waiting, jobs, recorded, labels) = tokio::join!(
        inbox,
        super::workflows::scheduled_job_cards(&client, &vault_name, &server_names),
        api.newest_first(&filter, RECORDED_LIMIT, &columns),
        api.kind_catalogue(),
    );
    let labels = labels.unwrap_or_default();
    let mut rows = vec![InputRow::inbox(waiting, urls.inbox())];
    let incomplete = jobs.is_none() || recorded.is_err();
    rows.extend(jobs.unwrap_or_default().into_iter().map(|card| InputRow {
        icon: "jobs",
        label: card.name,
        meta: card.last_run,
        state: card.state,
        href: card.href,
    }));
    if let Ok(batch) = recorded {
        let events = batch
            .events
            .into_iter()
            .map(|event| (event.kind, event.at, event.severity))
            .collect();
        rows.extend(newest_per_kind(events).into_iter().map(|(kind, (latest, severity))| {
            InputRow::recorded(&RecordedSource {
                label: labels.label(&kind),
                href: urls.signals_with(&format!("kind={}", crate::inbox_outbox::encode_query_value(&kind))),
                kind,
                latest,
                severity,
            })
        }));
    }
    rendered(&InputsView {
        refresh_href: urls.overview_panel("inputs"),
        rows,
        incomplete,
        signals_href: urls.signals(),
    })
}
