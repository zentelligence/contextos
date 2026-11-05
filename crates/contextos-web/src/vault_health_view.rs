//! What the operator sees of a vault's health: the compact header and footer indicators, the Vault Health pane, and
//! the confirmation shown before an automatic repair writes to the vault. Every one of them is built from the same
//! [`Snapshot`] the cache holds, so they always agree.
//!
//! The wording is for someone who has not read the server's documentation: a finding says what is wrong and what to
//! do, and an automatic repair says plainly that it writes to the vault, and how, before it can be confirmed.

use std::time::Duration;

use askama::Template;

use crate::vault_health::{CheckKind, CheckStatus, HealthCheck, HealthReport, HealthSummary};
use crate::vault_health_cache::{HealthError, Snapshot};
use crate::vault_health_mcp::{RepairPlan, RepairStep};

/// Where a compact indicator is placed. The two differ in markup only, never in what they say.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChipSurface {
    Header,
    Footer,
}

/// A surface other than `header` or `footer`: malformed input, not something to guess a placement for.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
#[error("unknown health indicator surface: {0}")]
pub(crate) struct UnknownSurface(pub String);

impl TryFrom<&str> for ChipSurface {
    type Error = UnknownSurface;

    fn try_from(surface: &str) -> Result<Self, Self::Error> {
        match surface {
            "header" => Ok(Self::Header),
            "footer" => Ok(Self::Footer),
            other => Err(UnknownSurface(other.to_owned())),
        }
    }
}

/// How long ago an assessment was made, in words.
pub(crate) fn age_label(age: Duration) -> String {
    let seconds = age.as_secs();
    let plural = |count: u64, unit: &str| format!("{count} {unit}{} ago", if count == 1 { "" } else { "s" });
    match seconds {
        0..=9 => "just now".to_owned(),
        10..=59 => plural(seconds, "second"),
        60..=3599 => plural(seconds / 60, "minute"),
        _ => plural(seconds / 3600, "hour"),
    }
}

impl CheckKind {
    /// What repairing a finding of this kind does to the vault, in the operator's terms.
    pub(crate) fn repair_effect(&self) -> &'static str {
        match self {
            Self::ManagedIndexes => "Rebuild the missing or stale index.md files, writing them into the vault.",
            Self::GitRecovery => {
                "Initialise Git for this vault. This starts an automatic commit after every future change, so it is a \
                 standing change to how the vault behaves, not a one-off fix."
            }
            Self::UncommittedChanges => {
                "Commit the changes that MCP tools made and that were never committed. Changes made outside MCP are \
                 left alone."
            }
            _ => "Apply the repair the server names for this finding.",
        }
    }
}

// ---- compact indicators -------------------------------------------------------------------------------------------

/// The state a compact indicator shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChipState {
    Healthy,
    NeedsAttention {
        failures: usize,
    },
    /// Health could not be assessed. Never shown as healthy: not knowing is not the same as being well.
    Unavailable,
}

impl From<Result<&Snapshot, &HealthError>> for ChipState {
    fn from(assessment: Result<&Snapshot, &HealthError>) -> Self {
        match assessment.map(|snapshot| snapshot.report.summary()) {
            Ok(HealthSummary::Healthy) => Self::Healthy,
            Ok(HealthSummary::NeedsAttention { failures, .. }) => Self::NeedsAttention { failures },
            Err(_) => Self::Unavailable,
        }
    }
}

impl ChipState {
    fn css_class(self) -> &'static str {
        match self {
            Self::Healthy => "health-ok",
            Self::NeedsAttention { .. } => "health-attention",
            Self::Unavailable => "health-unknown",
        }
    }

    /// The short status text, also the accessible name of the header icon.
    pub(crate) fn summary_text(self) -> String {
        match self {
            Self::Healthy => "Healthy".to_owned(),
            Self::NeedsAttention { failures: 1 } => "1 issue".to_owned(),
            Self::NeedsAttention { failures } => format!("{failures} issues"),
            Self::Unavailable => "Unavailable".to_owned(),
        }
    }
}

pub(crate) struct ChipInput<'a> {
    pub vault: &'a str,
    pub surface: ChipSurface,
    pub state: ChipState,
}

#[derive(Template)]
#[template(path = "vault_health_chip.html")]
struct ChipTemplate {
    vault: String,
    header: bool,
    css_class: &'static str,
    summary: String,
}

/// Renders one compact indicator.
pub(crate) fn render_chip(input: &ChipInput<'_>) -> String {
    ChipTemplate {
        vault: input.vault.to_owned(),
        header: input.surface == ChipSurface::Header,
        css_class: input.state.css_class(),
        summary: input.state.summary_text(),
    }
    .render()
    .unwrap_or_else(|_| {
        "<span class=\"health-chip health-unknown\" role=\"status\">Vault health could not be displayed.</span>"
            .to_owned()
    })
}

// ---- the pane -----------------------------------------------------------------------------------------------------

/// One line of feedback above the report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Notice {
    pub tone: &'static str,
    pub title: String,
    pub lines: Vec<String>,
}

impl From<&[RepairStep]> for Notice {
    fn from(steps: &[RepairStep]) -> Self {
        let done = steps.iter().filter(|step| step.resolved).count();
        let tone = if done == steps.len() { "success" } else { "warning" };
        let title = match (steps.len(), done) {
            (0, _) => "Nothing needed repairing.".to_owned(),
            (_, 0) => "The repair did not complete.".to_owned(),
            (total, done) if done == total => format!("Repaired {done} {}.", noun(done)),
            (total, done) => format!("Repaired {done} of {total} {}.", noun(total)),
        };
        let lines = steps
            .iter()
            .map(|step| format!("{}: {}", step.kind.label(), step.message))
            .collect();
        Self { tone, title, lines }
    }
}

fn noun(count: usize) -> &'static str {
    if count == 1 { "finding" } else { "findings" }
}

pub(crate) struct CheckView {
    label: String,
    passing: bool,
    message: String,
    action: Option<String>,
    fixable: bool,
}

impl From<&HealthCheck> for CheckView {
    fn from(check: &HealthCheck) -> Self {
        let passing = check.status == CheckStatus::Pass;
        Self {
            label: check.kind.label().to_owned(),
            passing,
            message: check.message.clone(),
            action: check.action.clone(),
            fixable: !passing && check.auto_fixable,
        }
    }
}

pub(crate) struct GroupView {
    title: &'static str,
    checks: Vec<CheckView>,
}

pub(crate) struct PaneInput<'a> {
    pub vault: &'a str,
    pub assessment: Result<&'a Snapshot, &'a HealthError>,
    pub notice: Option<Notice>,
}

#[derive(Template)]
#[template(path = "vault_health_pane.html")]
pub(crate) struct PaneView {
    vault: String,
    /// `None` when health could not be assessed.
    report: Option<ReportView>,
    unavailable: Option<String>,
    notice: Option<Notice>,
}

struct ReportView {
    headline: String,
    css_class: &'static str,
    checked: String,
    fixable: usize,
    groups: Vec<GroupView>,
}

impl From<(&HealthReport, Duration)> for ReportView {
    fn from((report, age): (&HealthReport, Duration)) -> Self {
        let (headline, css_class, fixable) = match report.summary() {
            HealthSummary::Healthy => ("Every check passed.".to_owned(), "health-ok", 0),
            HealthSummary::NeedsAttention { failures, fixable } => (
                if failures == 1 {
                    "1 check needs attention.".to_owned()
                } else {
                    format!("{failures} checks need attention.")
                },
                "health-attention",
                fixable,
            ),
        };
        Self {
            headline,
            css_class,
            checked: format!("Checked {}", age_label(age)),
            fixable,
            groups: report
                .groups()
                .into_iter()
                .map(|group| GroupView {
                    title: group.title,
                    checks: group.checks.into_iter().map(CheckView::from).collect(),
                })
                .collect(),
        }
    }
}

impl From<PaneInput<'_>> for PaneView {
    fn from(input: PaneInput<'_>) -> Self {
        let (report, unavailable) = match input.assessment {
            Ok(snapshot) => (Some(ReportView::from((snapshot.report.as_ref(), snapshot.age))), None),
            Err(error) => (None, Some(unavailable_message(error))),
        };
        Self {
            vault: input.vault.to_owned(),
            report,
            unavailable,
            notice: input.notice,
        }
    }
}

/// An actionable sentence for a failed assessment. The server's own message is included because it is the useful
/// part, and it never carries vault content.
fn unavailable_message(error: &HealthError) -> String {
    match error {
        HealthError::Unreachable => {
            "The MCP server could not be reached, so this vault's health is unknown. Check that it is running, then \
             refresh."
                .to_owned()
        }
        HealthError::Failed { code, message } => {
            format!("The health check failed ({code}): {message} Refresh to try again.")
        }
        HealthError::TimedOut(limit) => format!(
            "The health check did not answer within {} seconds, so this vault's health is unknown. Refresh to try \
             again.",
            limit.as_secs()
        ),
        HealthError::Malformed(detail) => format!(
            "The health check returned a result this version of the web UI cannot read ({detail}). Update the web UI \
             and the MCP server so they match."
        ),
    }
}

impl PaneView {
    /// Renders the pane, falling back to a plain message so a template fault never leaves the pane blank.
    pub(crate) fn render_html(&self) -> String {
        self.render()
            .unwrap_or_else(|_| "<p class=\"hint error\">Vault health could not be displayed.</p>".to_owned())
    }
}

// ---- the repair confirmation --------------------------------------------------------------------------------------

struct StepView {
    label: String,
    effect: &'static str,
}

impl From<&RepairStep> for StepView {
    fn from(step: &RepairStep) -> Self {
        Self {
            label: step.kind.label().to_owned(),
            effect: step.kind.repair_effect(),
        }
    }
}

pub(crate) struct ConfirmInput<'a> {
    pub vault: &'a str,
    pub plan: &'a RepairPlan,
    /// Set when the operator had confirmed a different plan, which is no longer what would run.
    pub changed: bool,
}

#[derive(Template)]
#[template(path = "vault_health_confirm.html")]
pub(crate) struct ConfirmView {
    vault: String,
    steps: Vec<StepView>,
    digest: String,
    changed: bool,
}

impl From<ConfirmInput<'_>> for ConfirmView {
    fn from(input: ConfirmInput<'_>) -> Self {
        Self {
            vault: input.vault.to_owned(),
            steps: input.plan.steps.iter().map(StepView::from).collect(),
            digest: input.plan.digest.as_str().to_owned(),
            changed: input.changed,
        }
    }
}

impl ConfirmView {
    pub(crate) fn render_html(&self) -> String {
        self.render()
            .unwrap_or_else(|_| "<p class=\"hint error\">The repair could not be displayed.</p>".to_owned())
    }
}

#[cfg(test)]
#[path = "vault_health_view_test.rs"]
mod tests;
