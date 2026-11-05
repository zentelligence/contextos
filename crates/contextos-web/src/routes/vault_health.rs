//! Vault health: `GET /{vault}/health` (the Vault Health pane), `GET /{vault}/health/summary?surface=` (the compact
//! header and footer indicators), and the three commands under `/{vault}/health/execute/`: `refresh`,
//! `repair-preview`, and `repair`.
//!
//! Every surface reads the one [`HealthCache`], so a page with a header icon, a footer chip, and an open pane makes
//! one `doctor` call, and they cannot disagree. The indicators are HTMX fragments the shell requests after it loads,
//! so no page render ever waits on `doctor`. Anything that changes what health would report answers with the
//! `vault-health-changed` event, which every indicator listens for.
//!
//! A repair is never a single request. `repair-preview` runs `doctor_resolve` as a dry run and shows exactly what
//! it would do. `repair` runs it for real only when the plan it is given is still what a fresh preview shows, so
//! the operator confirms what will happen, not something similar. Both are `POST`, and `repair` refuses a request
//! that names no plan.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::routes::CommonRoutesState;
use crate::routes::vault::{error_response, html_response, not_found, unreachable};
use crate::vault_health_cache::{HealthCache, HealthError, Snapshot};
use crate::vault_health_mcp::{self, McpHealthSource, PlanDigest, RepairRefusal};
use crate::vault_health_view::{
    ChipInput, ChipState, ChipSurface, ConfirmInput, ConfirmView, Notice, PaneInput, PaneView, render_chip,
};

/// How long an assessment answers reads before the next one asks `doctor` again. `doctor` scans the whole vault, so
/// this is deliberately generous; an operator who wants a fresh answer sooner has "Check again", and a repair
/// updates it at once.
const HEALTH_TTL: Duration = Duration::from_secs(300);

/// How long one `doctor` call may run before it is abandoned and reported as a failure, so a stalled server cannot
/// hold every reader of a vault behind it.
const ASSESS_TIMEOUT: Duration = Duration::from_secs(120);

/// The event every health indicator listens for.
const HEALTH_CHANGED: &str = "vault-health-changed";

#[derive(Clone)]
pub struct VaultHealthRoutesState {
    clients: Arc<McpClientSet>,
    cache: Arc<HealthCache<McpHealthSource>>,
}

impl From<CommonRoutesState> for VaultHealthRoutesState {
    fn from(value: CommonRoutesState) -> Self {
        let source = McpHealthSource::new(Arc::clone(&value.clients));
        Self {
            clients: value.clients,
            cache: Arc::new(HealthCache::new(source, HEALTH_TTL, ASSESS_TIMEOUT)),
        }
    }
}

impl VaultHealthRoutesState {
    fn client(&self) -> Result<Arc<McpClient>, Box<Response>> {
        crate::routes::primary_session(&self.clients)
    }

    /// The client, once `vault` is known to be configured.
    async fn client_for(&self, vault: &str) -> Result<Arc<McpClient>, Box<Response>> {
        let client = self.client()?;
        match crate::apps::vault_exists(&client, vault).await {
            Ok(true) => Ok(client),
            Ok(false) => Err(Box::new(not_found())),
            Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => Err(Box::new(unreachable())),
        }
    }
}

fn pane_response(vault: &str, assessment: Result<&Snapshot, &HealthError>, notice: Option<Notice>) -> Response {
    html_response(
        PaneView::from(PaneInput {
            vault,
            assessment,
            notice,
        })
        .render_html(),
    )
}

/// Whether a request that changes something came from this site's own pages. A browser states where a request came
/// from: `Sec-Fetch-Site` where it supports it, `Origin` on every cross-site `POST`. A request carrying neither is
/// not from a browser page, so there is no other site to defend against. A repair writes to the vault, so a page
/// on another site must not be able to submit one on the operator's behalf.
fn from_own_origin(headers: &HeaderMap) -> bool {
    if let Some(site) = headers.get("sec-fetch-site") {
        return site.to_str().is_ok_and(|site| site == "same-origin");
    }
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let host = headers.get(header::HOST).and_then(|host| host.to_str().ok());
    let authority = origin
        .to_str()
        .ok()
        .and_then(|origin| origin.split_once("://"))
        .map(|(_, authority)| authority);
    matches!((authority, host), (Some(authority), Some(host)) if authority.eq_ignore_ascii_case(host))
}

fn cross_origin_refused() -> Response {
    error_response(StatusCode::FORBIDDEN, "health/cross-origin-request")
}

/// The same response, announcing that health may now read differently.
fn announcing_change(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert("hx-trigger", axum::http::HeaderValue::from_static(HEALTH_CHANGED));
    response
}

/// `GET /{vault}/health`: the pane for an HTMX request; a plain browser visit is taken to where the pane lives.
pub async fn get(
    State(state): State<VaultHealthRoutesState>,
    Path(vault): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = state.client_for(&vault).await {
        return *response;
    }
    if !headers.contains_key("hx-request") {
        return (
            StatusCode::FOUND,
            [(
                header::LOCATION,
                crate::urls::VaultUrls::from(vault.as_str()).settings_health(),
            )],
        )
            .into_response();
    }
    let assessment = state.cache.current(&vault).await;
    pane_response(&vault, assessment.as_ref(), None)
}

#[derive(Debug, Deserialize)]
pub struct SummaryQuery {
    #[serde(default)]
    surface: Option<String>,
}

/// `GET /{vault}/health/summary?surface=header|footer`: one compact indicator.
pub async fn summary(
    State(state): State<VaultHealthRoutesState>,
    Path(vault): Path<String>,
    Query(query): Query<SummaryQuery>,
) -> Response {
    if let Err(response) = state.client_for(&vault).await {
        return *response;
    }
    let Some(Ok(surface)) = query.surface.as_deref().map(ChipSurface::try_from) else {
        return error_response(StatusCode::BAD_REQUEST, "health/unknown-surface");
    };
    let assessment = state.cache.current(&vault).await;
    html_response(render_chip(&ChipInput {
        vault: &vault,
        surface,
        state: ChipState::from(assessment.as_ref()),
    }))
}

/// `POST /{vault}/health/execute/refresh`: assess again now.
pub async fn refresh(
    State(state): State<VaultHealthRoutesState>,
    Path(vault): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !from_own_origin(&headers) {
        return cross_origin_refused();
    }
    if let Err(response) = state.client_for(&vault).await {
        return *response;
    }
    let assessment = state.cache.refresh(&vault).await;
    announcing_change(pane_response(&vault, assessment.as_ref(), None))
}

/// `POST /{vault}/health/execute/repair-preview`: what a repair would do, changing nothing.
pub async fn repair_preview(
    State(state): State<VaultHealthRoutesState>,
    Path(vault): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !from_own_origin(&headers) {
        return cross_origin_refused();
    }
    let client = match state.client_for(&vault).await {
        Ok(client) => client,
        Err(response) => return *response,
    };
    match vault_health_mcp::preview_repair(&client, &vault).await {
        Ok(plan) => html_response(
            ConfirmView::from(ConfirmInput {
                vault: &vault,
                plan: &plan,
                changed: false,
            })
            .render_html(),
        ),
        Err(error) => pane_response(&vault, Err(&error), None),
    }
}

#[derive(Debug, Deserialize)]
pub struct RepairQuery {
    #[serde(default)]
    plan: Option<String>,
}

/// `POST /{vault}/health/execute/repair?plan=`: the repair the operator reviewed, if it is still what would run.
pub async fn repair(
    State(state): State<VaultHealthRoutesState>,
    Path(vault): Path<String>,
    Query(query): Query<RepairQuery>,
    headers: HeaderMap,
) -> Response {
    if !from_own_origin(&headers) {
        return cross_origin_refused();
    }
    let client = match state.client_for(&vault).await {
        Ok(client) => client,
        Err(response) => return *response,
    };
    let Some(plan) = query.plan.filter(|plan| !plan.is_empty()).map(PlanDigest::from) else {
        return error_response(StatusCode::BAD_REQUEST, "health/repair-plan-required");
    };
    match vault_health_mcp::apply_repair(&client, &vault, &plan).await {
        Ok(repaired) => {
            let notice = Notice::from(repaired.steps.as_slice());
            state.cache.store(&vault, repaired.report).await;
            let assessment = state.cache.current(&vault).await;
            announcing_change(pane_response(&vault, assessment.as_ref(), Some(notice)))
        }
        Err(RepairRefusal::PlanChanged(current)) => html_response(
            ConfirmView::from(ConfirmInput {
                vault: &vault,
                plan: &current,
                changed: true,
            })
            .render_html(),
        ),
        Err(RepairRefusal::Failed(error)) => {
            // A repair that failed part-way may have written something, so what was assessed is no longer known.
            state.cache.invalidate(&vault).await;
            announcing_change(pane_response(&vault, Err(&error), None))
        }
    }
}

#[cfg(test)]
#[path = "vault_health_test.rs"]
mod tests;
