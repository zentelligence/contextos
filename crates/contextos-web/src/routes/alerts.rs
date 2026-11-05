//! `GET /{{vault_name}}/alerts`: what needs the operator ([`crate::alerts`]), as the fragment the top-bar bell's
//! dropdown and the Overview's Alert rail load, or as a page; `GET /{{vault_name}}/alerts/summary`: the bell's count.
//!
//! The alerts are read on every request from what the vault already holds: the task records (open tasks past their
//! due date, and open operational questions, the `question` tasks), the Decisions register (open vault questions), and
//! the events (warnings and errors nothing has cleared). A source that cannot be read is left out and the list says
//! so, so one unreadable source never hides the others.

use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use time::{Date, OffsetDateTime};

use crate::alerts::{Alert, OpenQuestion, OpenTask, most_recent_first};
use crate::config;
use crate::mcp_client::McpClient;
use crate::register_records::list_records;
use crate::rendering::page;
use crate::rendering::shell::{self, ActiveScreen};
use crate::routes::vault::html_response;
use crate::signals_query::EventsApi;
use crate::vault_catalogue::ManagementCategory;
use crate::vault_management_config::RecordType;

use super::tasks::{TasksRoutesState, ensure_vault, fetch_task_rows};

/// The most alerts one list shows; the rest are counted.
const ALERT_LIMIT: usize = 50;

/// The task type whose open tasks are operational questions.
const QUESTION_TASK_TYPE: &str = "question";

/// Every alert, most recent first, and whether a source could not be read.
pub(crate) struct LoadedAlerts {
    pub(crate) alerts: Vec<Alert>,
    pub(crate) incomplete: bool,
}

/// The alerts of `vault_name` as of `today` (the operator's local date, for due dates) and `now` (for events).
pub(crate) async fn load_alerts(
    client: &McpClient,
    vault_name: &str,
    today: Date,
    now: OffsetDateTime,
) -> LoadedAlerts {
    let (tasks, register, attention) = tokio::join!(
        task_alerts(client, vault_name, today),
        register_question_alerts(client, vault_name),
        attention_alerts(client, vault_name, now),
    );
    let incomplete = tasks.is_none() || register.is_none() || attention.is_none();
    let alerts = [tasks, register, attention].into_iter().flatten().flatten().collect();
    LoadedAlerts {
        alerts: most_recent_first(alerts),
        incomplete,
    }
}

/// Open tasks past their due date, and open operational questions, from the task records.
async fn task_alerts(client: &McpClient, vault_name: &str, today: Date) -> Option<Vec<Alert>> {
    let (rows, config) = tokio::join!(
        fetch_task_rows(client, vault_name),
        crate::tasks_config::load_tasks_config(client, vault_name),
    );
    let rows = rows.ok()?;
    let mut alerts = Vec::new();
    for row in rows {
        if !config.is_open(&row.task_type, &row.state) {
            continue;
        }
        let href = row.detail_href(vault_name);
        if row.task_type.eq_ignore_ascii_case(QUESTION_TASK_TYPE) {
            alerts.push(Alert::from(OpenQuestion {
                id: row.id,
                title: row.title,
                updated: row.updated,
                href,
            }));
            continue;
        }
        let task = OpenTask {
            id: row.id,
            title: row.title,
            due: row.due,
            href,
        };
        if task.is_overdue(today) {
            alerts.push(Alert::from(task));
        }
    }
    Some(alerts)
}

/// Open vault questions from the Decisions register.
async fn register_question_alerts(client: &McpClient, vault_name: &str) -> Option<Vec<Alert>> {
    let loaded = crate::vault_management_config::load_vault_management_config(client, vault_name).await;
    let records = list_records(client, vault_name, loaded.config.register()).await.ok()?;
    Some(
        records
            .into_iter()
            .filter(|record| record.record_type == Some(RecordType::Question) && record.status == "open")
            .map(|record| {
                let href =
                    super::vault_management::category_href(vault_name, ManagementCategory::Decisions, &record.stem);
                Alert::from(OpenQuestion {
                    id: record.id,
                    title: record.title,
                    updated: (!record.updated.is_empty()).then_some(record.updated),
                    href,
                })
            })
            .collect(),
    )
}

/// The warnings and errors nothing has cleared, as the Signals screen's attention region reads them.
async fn attention_alerts(client: &McpClient, vault_name: &str, now: OffsetDateTime) -> Option<Vec<Alert>> {
    let api = EventsApi { client, vault_name };
    let urls = crate::urls::VaultUrls::from(vault_name);
    let labels = api.kind_catalogue().await.unwrap_or_default();
    let view = crate::signals_load::load_attention(&api, &urls, &labels, None, now)
        .await
        .ok()?;
    Some(view.lines.iter().map(Alert::from).collect())
}

#[derive(Template)]
#[template(path = "alerts_list.html")]
struct AlertsListTemplate<'a> {
    refresh_href: String,
    alerts: &'a [Alert],
    count: usize,
    more: usize,
    incomplete: bool,
}

/// The alerts list as a fragment, refreshing itself when the live stream says attention may have changed.
pub(crate) fn render_alerts(vault_name: &str, loaded: &LoadedAlerts) -> String {
    let shown = &loaded.alerts[..loaded.alerts.len().min(ALERT_LIMIT)];
    AlertsListTemplate {
        refresh_href: crate::urls::VaultUrls::from(vault_name).alerts(),
        alerts: shown,
        count: loaded.alerts.len(),
        more: loaded.alerts.len() - shown.len(),
        incomplete: loaded.incomplete,
    }
    .render()
    .unwrap_or_else(|_| "<p class=\"hint\">The alerts could not be rendered.</p>".to_owned())
}

#[derive(Template)]
#[template(path = "alerts_summary.html")]
struct AlertsSummaryTemplate {
    refresh_href: String,
    count: usize,
}

/// `GET /{{vault_name}}/alerts`.
pub async fn list(
    State(state): State<TasksRoutesState>,
    Path(vault_name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let today = state.vault_time.today().await;
    let loaded = Box::pin(load_alerts(&client, &vault_name, today, OffsetDateTime::now_utc())).await;
    let fragment = render_alerts(&vault_name, &loaded);
    if headers.contains_key("hx-request") {
        return html_response(fragment).into_response();
    }
    let web_config_path = Arc::clone(&state.web_config_path);
    let known_mcp_servers = state.clients.names();
    let base = crate::urls::VaultUrls::from(vault_name.as_str()).alerts();
    let (mut nav, appearance, mcp_status) = tokio::join!(
        Box::pin(shell::build_nav(
            &client,
            ActiveScreen::Vault,
            Some(&vault_name),
            Some("alerts"),
            None,
            Some(&base),
            &known_mcp_servers,
        )),
        async move {
            tokio::task::spawn_blocking(move || config::current_appearance(&web_config_path))
                .await
                .unwrap_or_default()
        },
        shell::mcp_status(&state.clients),
    );
    nav.appearance = appearance;
    nav.sidebar_wide = crate::routes::sidebar_wide_from_headers(&headers);
    nav.mcp_status = mcp_status;
    html_response(page::render_page(&nav, "Alerts", &fragment)).into_response()
}

/// `GET /{{vault_name}}/alerts/summary`: the bell's count, refreshing itself when attention may have changed.
pub async fn summary(State(state): State<TasksRoutesState>, Path(vault_name): Path<String>) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let today = state.vault_time.today().await;
    let loaded = Box::pin(load_alerts(&client, &vault_name, today, OffsetDateTime::now_utc())).await;
    let fragment = AlertsSummaryTemplate {
        refresh_href: crate::urls::VaultUrls::from(vault_name.as_str()).alerts_summary(),
        count: loaded.alerts.len(),
    }
    .render()
    .unwrap_or_default();
    html_response(fragment).into_response()
}
