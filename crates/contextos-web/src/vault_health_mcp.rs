//! The MCP side of vault health: the `doctor` assessment the cache reads through, and the `doctor_resolve` repair
//! the operator explicitly confirms. Every call goes to the server's own tools; nothing here inspects a vault
//! directly or re-implements a check.
//!
//! A repair is two steps by design. [`preview_repair`] runs `doctor_resolve` with `dry_run` and returns exactly what
//! it would do, with a digest of that plan and the vault it is for. [`apply_repair`] runs it for real only when the
//! digest of a fresh preview still matches the one the operator confirmed, so what runs is the set of repairs they
//! were shown, and a finding that appeared or went away in between sends them back to review. The digest binds a
//! confirmation to a plan; it is not a credential, so the routes separately refuse requests from other sites. `doctor_resolve` repairs every auto-fixable finding for the vault it is scoped to, so the
//! unit of a repair is the vault, never a single finding.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::vault_health::{CheckKind, DoctorResult, HealthReport};
use crate::vault_health_cache::{HealthError, HealthSource};
use crate::vault_health_releases::{STORE_CHECK_LIMIT, store_checks, within};

/// Assesses vaults by calling `doctor` on the primary MCP server.
pub(crate) struct McpHealthSource {
    clients: Arc<McpClientSet>,
    /// How long the checks of the vault's package store may take, so a slow store costs only its own findings.
    store_limit: Duration,
}

impl McpHealthSource {
    pub(crate) fn new(clients: Arc<McpClientSet>) -> Self {
        Self {
            clients,
            store_limit: STORE_CHECK_LIMIT,
        }
    }
}

impl HealthSource for McpHealthSource {
    async fn assess(&self, vault: &str) -> Result<HealthReport, HealthError> {
        let client = self.clients.primary().ok_or(HealthError::Unreachable)?;
        let result = call(&client, "doctor", scope(vault, false)).await?;
        let doctor = result
            .into_typed::<DoctorResult>()
            .map_err(|error| HealthError::Malformed(error.to_string()))?;
        let report = HealthReport::try_from(doctor).map_err(|error| HealthError::Malformed(error.to_string()))?;
        // The server's `doctor` knows nothing of the package store, so the checks of its releases are made here.
        let releases = within(
            self.store_limit,
            vault,
            store_checks(&client, &self.clients.names(), vault),
        )
        .await;
        Ok(report.with_checks(releases))
    }
}

/// The arguments that scope a tool to one vault. `dry_run` is sent only to `doctor_resolve`.
fn scope(vault: &str, dry_run: bool) -> Map<String, Value> {
    let mut arguments = Map::new();
    arguments.insert("path".to_owned(), Value::String(format!("{vault}://.")));
    if dry_run {
        arguments.insert("dry_run".to_owned(), Value::Bool(true));
    }
    arguments
}

/// The structured body of a tool that failed.
#[derive(Debug, Deserialize)]
struct ToolFailure {
    code: String,
    message: String,
}

/// Calls a tool and separates a transport failure, a tool that answered with an error, and success.
async fn call(
    client: &McpClient,
    tool: &str,
    arguments: Map<String, Value>,
) -> Result<rmcp::model::CallToolResult, HealthError> {
    let result = client
        .call_tool(tool.to_owned(), arguments)
        .await
        .map_err(|(McpCallError::Unreachable { .. } | McpCallError::Refused { .. })| HealthError::Unreachable)?;
    if result.is_error != Some(true) {
        return Ok(result);
    }
    Err(match result.into_typed::<ToolFailure>() {
        Ok(failure) => HealthError::Failed {
            code: failure.code,
            message: failure.message,
        },
        Err(error) => HealthError::Malformed(error.to_string()),
    })
}

/// One thing a repair would do, or did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RepairStep {
    pub kind: CheckKind,
    pub message: String,
    /// Whether the step took effect. Always `false` in a preview.
    pub resolved: bool,
}

/// What a repair would do to a vault, and a digest that identifies exactly that plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RepairPlan {
    pub steps: Vec<RepairStep>,
    pub digest: PlanDigest,
}

/// Identifies a plan so a confirmation can be tied to what the operator was shown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlanDigest(String);

impl PlanDigest {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<(&str, &[RepairStep])> for PlanDigest {
    /// The digest covers the vault as well as the steps, so a plan reviewed for one vault never confirms another's.
    fn from((vault, steps): (&str, &[RepairStep])) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(vault.as_bytes());
        hasher.update([0xff]);
        for step in steps {
            hasher.update(step.kind.label().as_bytes());
            hasher.update([0]);
            hasher.update(step.message.as_bytes());
            hasher.update([0xff]);
        }
        Self(URL_SAFE_NO_PAD.encode(hasher.finalize()))
    }
}

impl From<String> for PlanDigest {
    fn from(digest: String) -> Self {
        Self(digest)
    }
}

/// The repair's result: what ran, and the vault's health afterwards, which the server has already assessed.
#[derive(Debug)]
pub(crate) struct Repaired {
    pub steps: Vec<RepairStep>,
    pub report: HealthReport,
}

#[derive(Debug, Deserialize)]
struct WireOutcome {
    kind: String,
    message: String,
    resolved: bool,
}

#[derive(Debug, Deserialize)]
struct WireResolve {
    outcomes: Vec<WireOutcome>,
    report: WireReport,
}

/// The report `doctor_resolve` returns covers every configured vault, each check naming its own.
#[derive(Debug, Deserialize)]
struct WireReport {
    checks: Vec<Value>,
}

impl From<WireOutcome> for RepairStep {
    fn from(outcome: WireOutcome) -> Self {
        Self {
            kind: CheckKind::from(outcome.kind.as_str()),
            message: outcome.message,
            resolved: outcome.resolved,
        }
    }
}

/// Reads the vault's part of a `doctor_resolve` report: its own checks and the global ones, never another vault's.
fn health_of(report: WireReport, vault: &str) -> Result<HealthReport, HealthError> {
    let checks = report
        .checks
        .into_iter()
        .filter(|check| match check.get("vault_name").and_then(Value::as_str) {
            Some(name) => name == vault,
            None => true,
        })
        .collect::<Vec<_>>();
    let doctor = serde_json::from_value::<DoctorResult>(serde_json::json!({ "checks": checks }))
        .map_err(|error| HealthError::Malformed(error.to_string()))?;
    HealthReport::try_from(doctor).map_err(|error| HealthError::Malformed(error.to_string()))
}

async fn resolve(client: &McpClient, vault: &str, dry_run: bool) -> Result<WireResolve, HealthError> {
    call(client, "doctor_resolve", scope(vault, dry_run))
        .await?
        .into_typed::<WireResolve>()
        .map_err(|error| HealthError::Malformed(error.to_string()))
}

/// What a repair of `vault` would do, changing nothing.
pub(crate) async fn preview_repair(client: &McpClient, vault: &str) -> Result<RepairPlan, HealthError> {
    let steps = resolve(client, vault, true)
        .await?
        .outcomes
        .into_iter()
        .map(RepairStep::from)
        .collect::<Vec<_>>();
    let digest = PlanDigest::from((vault, steps.as_slice()));
    Ok(RepairPlan { steps, digest })
}

/// Why a confirmed repair did not run.
#[derive(Debug)]
pub(crate) enum RepairRefusal {
    /// What the repair would do is no longer what the operator confirmed. The fresh plan is returned for them to
    /// review.
    PlanChanged(RepairPlan),
    Failed(HealthError),
}

/// Repairs `vault`, but only when a fresh preview still matches `confirmed`.
pub(crate) async fn apply_repair(
    client: &McpClient,
    vault: &str,
    confirmed: &PlanDigest,
) -> Result<Repaired, RepairRefusal> {
    let current = preview_repair(client, vault).await.map_err(RepairRefusal::Failed)?;
    if &current.digest != confirmed {
        return Err(RepairRefusal::PlanChanged(current));
    }
    let resolved = resolve(client, vault, false).await.map_err(RepairRefusal::Failed)?;
    let report = health_of(resolved.report, vault).map_err(RepairRefusal::Failed)?;
    Ok(Repaired {
        steps: resolved.outcomes.into_iter().map(RepairStep::from).collect(),
        report,
    })
}

#[cfg(test)]
#[path = "vault_health_mcp_test.rs"]
mod tests;
