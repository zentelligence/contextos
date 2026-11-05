//! What `doctor` reports about one vault, as the web UI models it: findings grouped by their stable kind, each with
//! the auto-fixable classification the server assigned. Nothing here re-implements a check. The server decides
//! what is wrong and what may be repaired automatically; this module only reads that verdict faithfully and
//! summarises it. A finding whose kind or status this build does not recognise is kept and shown, never dropped,
//! and an unrecognised status is a malformed report, never guessed at.

use serde::Deserialize;

/// The stable category of one finding, as the server names it in `kind`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CheckKind {
    Configuration,
    VaultAccess,
    ManagedIndexes,
    GitRecovery,
    UncommittedChanges,
    SemanticSearch,
    Frontmatter,
    DuplicateIds,
    RecordConformance,
    TeamConfig,
    FenceVocabulary,
    TextIndexStore,
    LinkGraphStore,
    VectorStore,
    ReleaseIntegrity,
    PackageStore,
    /// A kind a newer server reports that this build has no name for. Shown under its own code so it is never
    /// silently hidden.
    Unrecognised(String),
}

impl From<&str> for CheckKind {
    fn from(code: &str) -> Self {
        match code {
            "configuration" => Self::Configuration,
            "vault_access" => Self::VaultAccess,
            "managed_indexes" => Self::ManagedIndexes,
            "git_recovery" => Self::GitRecovery,
            "uncommitted_changes" => Self::UncommittedChanges,
            "semantic_search" => Self::SemanticSearch,
            "frontmatter" => Self::Frontmatter,
            "duplicate_ids" => Self::DuplicateIds,
            "record_conformance" => Self::RecordConformance,
            "team_config" => Self::TeamConfig,
            "fence_vocabulary" => Self::FenceVocabulary,
            "text_index_store" => Self::TextIndexStore,
            "link_graph_store" => Self::LinkGraphStore,
            "vector_store" => Self::VectorStore,
            "release_integrity" => Self::ReleaseIntegrity,
            "package_store" => Self::PackageStore,
            other => Self::Unrecognised(other.to_owned()),
        }
    }
}

impl CheckKind {
    /// The plain-language name the operator sees for this kind of check.
    pub(crate) fn label(&self) -> &str {
        match self {
            Self::Configuration => "Configuration",
            Self::VaultAccess => "Vault access",
            Self::ManagedIndexes => "Managed indexes",
            Self::GitRecovery => "Git recovery",
            Self::UncommittedChanges => "Uncommitted changes",
            Self::SemanticSearch => "Semantic search",
            Self::Frontmatter => "Frontmatter validity",
            Self::DuplicateIds => "Duplicate identifiers",
            Self::RecordConformance => "Record conformance",
            Self::TeamConfig => "Team configuration",
            Self::FenceVocabulary => "Fence vocabulary",
            Self::TextIndexStore => "Text index store",
            Self::LinkGraphStore => "Link graph store",
            Self::VectorStore => "Vector store",
            Self::ReleaseIntegrity => "Workflow releases",
            Self::PackageStore => "Package store",
            Self::Unrecognised(code) => code,
        }
    }

    fn group(&self) -> CheckGroup {
        match self {
            Self::Configuration | Self::VaultAccess | Self::FenceVocabulary => CheckGroup::Setup,
            Self::ManagedIndexes | Self::GitRecovery | Self::UncommittedChanges => CheckGroup::Recovery,
            Self::Frontmatter | Self::DuplicateIds | Self::RecordConformance | Self::TeamConfig => CheckGroup::Content,
            Self::SemanticSearch | Self::TextIndexStore | Self::LinkGraphStore | Self::VectorStore => {
                CheckGroup::Search
            }
            Self::ReleaseIntegrity | Self::PackageStore => CheckGroup::Workflows,
            Self::Unrecognised(_) => CheckGroup::Other,
        }
    }
}

/// The section a kind of check is presented under. Declaration order is presentation order.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum CheckGroup {
    Setup,
    Content,
    Workflows,
    Recovery,
    Search,
    Other,
}

impl CheckGroup {
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Setup => "Setup",
            Self::Content => "Content",
            Self::Workflows => "Workflow releases",
            Self::Recovery => "Indexes and recovery",
            Self::Search => "Search",
            Self::Other => "Other checks",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CheckStatus {
    Pass,
    Fail,
}

/// A status other than `pass` or `fail`: a report this build cannot read faithfully.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
#[error("doctor reported an unrecognised check status: {0}")]
pub(crate) struct UnrecognisedStatus(String);

impl TryFrom<&str> for CheckStatus {
    type Error = UnrecognisedStatus;

    fn try_from(status: &str) -> Result<Self, Self::Error> {
        match status {
            "pass" => Ok(Self::Pass),
            "fail" => Ok(Self::Fail),
            other => Err(UnrecognisedStatus(other.to_owned())),
        }
    }
}

/// One finding for the vault, exactly as `doctor` classified it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HealthCheck {
    pub kind: CheckKind,
    pub status: CheckStatus,
    pub message: String,
    /// What the operator should do about a failing check.
    pub action: Option<String>,
    /// Whether `doctor_resolve` may repair this finding without a further decision.
    pub auto_fixable: bool,
}

/// One section of the report: the findings that share a [`CheckGroup`].
pub(crate) struct HealthGroup<'a> {
    pub title: &'static str,
    pub checks: Vec<&'a HealthCheck>,
}

/// The overall verdict a compact indicator shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HealthSummary {
    Healthy,
    /// `failures` findings need attention, `fixable` of which `doctor_resolve` can repair automatically.
    NeedsAttention {
        failures: usize,
        fixable: usize,
    },
}

/// Every finding for one vault (plus the global configuration check), in the server's order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HealthReport {
    checks: Vec<HealthCheck>,
}

impl HealthReport {
    /// The report with `more` findings after the server's own, for checks this interface makes itself.
    #[must_use]
    pub(crate) fn with_checks(mut self, more: Vec<HealthCheck>) -> Self {
        self.checks.extend(more);
        self
    }

    pub(crate) fn summary(&self) -> HealthSummary {
        let failing = self.checks.iter().filter(|check| check.status == CheckStatus::Fail);
        let failures = failing.clone().count();
        if failures == 0 {
            return HealthSummary::Healthy;
        }
        HealthSummary::NeedsAttention {
            failures,
            fixable: failing.filter(|check| check.auto_fixable).count(),
        }
    }

    /// The findings under their section headings, in a fixed presentation order, sections with no findings omitted.
    pub(crate) fn groups(&self) -> Vec<HealthGroup<'_>> {
        let mut groups: Vec<(CheckGroup, Vec<&HealthCheck>)> = Vec::new();
        for check in &self.checks {
            let group = check.kind.group();
            match groups.iter_mut().find(|(existing, _)| *existing == group) {
                Some((_, checks)) => checks.push(check),
                None => groups.push((group, vec![check])),
            }
        }
        groups.sort_by_key(|(group, _)| *group);
        groups
            .into_iter()
            .map(|(group, checks)| HealthGroup {
                title: group.title(),
                checks,
            })
            .collect()
    }
}

/// The `doctor` tool's structured result, as the wire carries it.
#[derive(Debug, Deserialize)]
pub(crate) struct DoctorResult {
    checks: Vec<WireCheck>,
}

#[derive(Debug, Deserialize)]
struct WireCheck {
    kind: String,
    status: String,
    message: String,
    #[serde(default)]
    action: Option<String>,
    auto_fixable: bool,
}

impl TryFrom<DoctorResult> for HealthReport {
    type Error = UnrecognisedStatus;

    fn try_from(result: DoctorResult) -> Result<Self, Self::Error> {
        let checks = result
            .checks
            .into_iter()
            .map(|wire| {
                Ok(HealthCheck {
                    kind: CheckKind::from(wire.kind.as_str()),
                    status: CheckStatus::try_from(wire.status.as_str())?,
                    message: wire.message,
                    action: wire.action,
                    auto_fixable: wire.auto_fixable,
                })
            })
            .collect::<Result<Vec<_>, UnrecognisedStatus>>()?;
        Ok(Self { checks })
    }
}

#[cfg(test)]
#[path = "vault_health_test.rs"]
mod tests;
