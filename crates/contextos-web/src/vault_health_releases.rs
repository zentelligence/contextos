//! What the vault health check says about a vault's package store: releases that are incomplete or damaged, templates a
//! release declares that are gone, folders an unfinished operation left behind, a ledger that disagrees with the
//! releases, and jobs that name a release the store does not hold. The server's `doctor` knows nothing of the store, so
//! these findings are made here, from the same reading of the store that `packages list` and `workflows releases` use.

use std::future::Future;
use std::time::Duration;

use contextos_packages::RemovalCause;
use contextos_workflows::{JobLifecycle, ReleaseRef};

use crate::job_discovery::exists_in_package;
use crate::mcp_client::{McpCallError, McpClient};
use crate::package_standing::{LedgerStanding, StoreStanding};
use crate::package_stores::VaultPackagesOf;
use crate::shared_job_authoring::JobListing;
use crate::vault_health::{CheckKind, CheckStatus, HealthCheck};
use crate::vault_job_authoring::VaultJobAuthoring;
use crate::workflow_releases::ReleaseCondition;

/// How long the checks of a vault's package store may take before they are given up on. The `doctor` report is read
/// first and is kept whatever the store checks do, so a slow or failing store costs only its own findings.
pub(crate) const STORE_CHECK_LIMIT: Duration = Duration::from_secs(30);

/// Runs `work`, the store checks of `vault`, for at most `limit`. Running out of time is itself a finding, never a pass.
pub(crate) async fn within<F>(limit: Duration, vault: &str, work: F) -> Vec<HealthCheck>
where
    F: Future<Output = Vec<HealthCheck>>,
{
    match tokio::time::timeout(limit, work).await {
        Ok(checks) => checks,
        Err(_elapsed) => {
            tracing::warn!(
                vault,
                "the vault's workflow releases did not finish being read for the health check"
            );
            vec![finding(
                CheckKind::PackageStore,
                format!(
                    "The vault's workflow releases were not read within {} seconds, so they were not checked.",
                    limit.as_secs()
                ),
                "Check that the vault's primary server answers, then check again.",
            )]
        }
    }
}

/// Reads the vault's package store and its jobs through the session to its primary server, once each, and makes the
/// findings from them. A store that cannot be read is itself a finding, never a pass, and so is a job listing that
/// cannot be read, which leaves the findings about the releases in place.
pub(crate) async fn store_checks(client: &McpClient, known_servers: &[String], vault: &str) -> Vec<HealthCheck> {
    let standing = match (VaultPackagesOf { client, vault }).standing().await {
        Ok(standing) => standing,
        Err(error) => {
            let code = error.code();
            tracing::warn!(
                vault,
                code,
                "the vault's workflow releases could not be read for the health check"
            );
            return vec![finding(
                CheckKind::PackageStore,
                format!("The vault's workflow releases could not be read ({code}), so they were not checked."),
                "Check that the vault's package store, job state, and job files can be read, then check again.",
            )];
        }
    };
    let jobs = VaultJobAuthoring {
        client,
        vault,
        known_servers,
    }
    .list()
    .await
    .map_err(|error| error.code());
    let templates = probe_templates(client, vault, &standing).await;
    StoreChecks::from(StoreFacts {
        standing: &standing,
        templates: &templates,
        jobs: jobs.as_deref().map_err(|code| *code),
    })
    .0
}

/// Asks the vault's source whether the templates each whole release declares are still there. A release's templates are
/// probed once, and each of its workflows is told what was found, or that it could not be found out.
async fn probe_templates(client: &McpClient, vault: &str, standing: &StoreStanding) -> Vec<TemplateProbe> {
    let mut probes = Vec::new();
    for held in &standing.releases {
        let Some(contents) = held.contents.as_ref().filter(|contents| !contents.templates.is_empty()) else {
            continue;
        };
        let mut missing = Vec::new();
        let mut outcome = None;
        for template in &contents.templates {
            match exists_in_package(client, vault, held.release.name.as_str(), template).await {
                Ok(true) => {}
                Ok(false) => missing.push(template.clone()),
                Err(McpCallError::Unreachable { .. }) => {
                    outcome = Some(TemplateOutcome::Unchecked("server-unreachable"));
                    break;
                }
                Err(McpCallError::Refused { .. }) => {
                    outcome = Some(TemplateOutcome::Unchecked("server-refused"));
                    break;
                }
            }
        }
        let outcome = outcome.unwrap_or(TemplateOutcome::Missing(missing));
        for workflow in &contents.workflows {
            probes.push(TemplateProbe {
                release: held.release.clone(),
                workflow: workflow.workflow.clone(),
                outcome: outcome.clone(),
            });
        }
    }
    probes
}

/// What looking for one workflow release's declared templates came to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TemplateOutcome {
    /// The templates the vault's source no longer holds; none when it holds them all.
    Missing(Vec<String>),
    /// The source could not be asked, for the reason with this stable code.
    Unchecked(&'static str),
}

/// The templates one workflow of one release declares, and whether the vault's source still holds them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TemplateProbe {
    pub(crate) release: ReleaseRef,
    pub(crate) workflow: String,
    pub(crate) outcome: TemplateOutcome,
}

/// What the findings are made from.
pub(crate) struct StoreFacts<'a> {
    /// The store as `packages list` reads it.
    pub standing: &'a StoreStanding,
    /// What looking for the templates of each workflow release found.
    pub templates: &'a [TemplateProbe],
    /// Every job of the vault, or the stable code of why they could not be listed.
    pub jobs: Result<&'a [JobListing], &'static str>,
}

/// The findings about one vault's package store, in the order they are shown.
pub(crate) struct StoreChecks(pub(crate) Vec<HealthCheck>);

/// A finding that needs the operator: nothing here is repaired automatically.
fn finding(kind: CheckKind, message: String, action: &str) -> HealthCheck {
    HealthCheck {
        kind,
        status: CheckStatus::Fail,
        message,
        action: Some(action.to_owned()),
        auto_fixable: false,
    }
}

fn names(items: impl IntoIterator<Item = impl std::fmt::Display>) -> String {
    items
        .into_iter()
        .map(|item| item.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The findings about the releases themselves: any that cannot be read whole, and the templates each declares that are
/// no longer where a run reads them.
fn release_findings(facts: &StoreFacts<'_>) -> Vec<HealthCheck> {
    let mut found = Vec::new();
    for held in &facts.standing.releases {
        let release = &held.release;
        let (words, problems, action) = match &held.condition {
            ReleaseCondition::Whole | ReleaseCondition::Removed => continue,
            ReleaseCondition::Incomplete(problems) => (
                "is incomplete",
                problems.clone(),
                "Restore or remove the entries listed, or reinstall the release at its version.",
            ),
            ReleaseCondition::Damaged(problems) => (
                "is damaged",
                problems.clone(),
                "Reinstall the release: the same bundle for one installed from a bundle, or an install at its version \
                 from unchanged source. Then resume any job it suspended.",
            ),
            ReleaseCondition::Unverifiable(reason) => (
                "could not be verified",
                vec![reason.clone()],
                "Check that the vault's package store can be read, then check again. Nothing is changed meanwhile.",
            ),
        };
        found.push(finding(
            CheckKind::ReleaseIntegrity,
            format!("{release} {words}: {}.", problems.join("; ")),
            action,
        ));
    }
    for probe in facts.templates {
        let (package, workflow, version) = (&probe.release.name, &probe.workflow, &probe.release.version);
        match &probe.outcome {
            TemplateOutcome::Missing(missing) if missing.is_empty() => {}
            TemplateOutcome::Missing(missing) => found.push(finding(
                CheckKind::ReleaseIntegrity,
                format!(
                    "{package}/{workflow} release {version} is missing the templates {}.",
                    missing.join(", ")
                ),
                "Restore the templates in the workflow's source, or install a release that no longer declares them.",
            )),
            TemplateOutcome::Unchecked(code) => found.push(finding(
                CheckKind::ReleaseIntegrity,
                format!(
                    "The templates of {package}/{workflow} release {version} could not be looked for ({code}), so they were not checked."
                ),
                "Check that the vault's primary server answers, then check again.",
            )),
        }
    }
    found
}

/// The findings about the store as a whole: the folders unfinished operations left, and the ledger.
fn store_findings(standing: &StoreStanding) -> Vec<HealthCheck> {
    let mut found = Vec::new();
    if !standing.staging.is_empty() {
        found.push(finding(
            CheckKind::PackageStore,
            format!(
                "An install that did not finish left staging folders: {}.",
                names(&standing.staging)
            ),
            "The next install or removal in this vault deletes them.",
        ));
    }
    if !standing.removing.is_empty() {
        found.push(finding(
            CheckKind::PackageStore,
            format!(
                "A removal that did not finish left folders: {}.",
                names(&standing.removing)
            ),
            "The next install or removal in this vault puts back or deletes them, as the ledger records.",
        ));
    }
    if !standing.unrecognised.is_empty() {
        found.push(finding(
            CheckKind::PackageStore,
            format!(
                "The package store holds entries that are not releases: {}.",
                names(&standing.unrecognised)
            ),
            "Move them out of the vault's package store.",
        ));
    }
    match &standing.ledger {
        LedgerStanding::Missing => found.push(finding(
            CheckKind::PackageStore,
            "The package store holds releases and has no ledger, so installs and removals are refused.".to_owned(),
            "Restore the ledger, or rebuild it from the releases present with `packages list --rebuild-ledger`.",
        )),
        LedgerStanding::Unreadable(reason) => found.push(finding(
            CheckKind::PackageStore,
            format!("The package store's ledger cannot be read: {reason}."),
            "Restore the ledger, or rebuild it from the releases present with `packages list --rebuild-ledger`.",
        )),
        LedgerStanding::Present { .. } => {
            let unrecorded: Vec<&ReleaseRef> = standing
                .releases
                .iter()
                .filter(|held| held.provenance.is_none())
                .map(|held| &held.release)
                .collect();
            if !unrecorded.is_empty() {
                found.push(finding(
                    CheckKind::PackageStore,
                    format!(
                        "The ledger does not record these releases, though the store holds them: {}.",
                        names(unrecorded)
                    ),
                    "Reinstall each release, or rebuild the ledger from the releases present.",
                ));
            }
            let vanished: Vec<&ReleaseRef> = standing
                .removed
                .iter()
                .filter(|removed| removed.cause == RemovalCause::Unrecorded)
                .map(|removed| &removed.release)
                .collect();
            if !vanished.is_empty() {
                found.push(finding(
                    CheckKind::PackageStore,
                    format!(
                        "These releases are gone from the store with no removal recorded in the ledger: {}.",
                        names(vanished)
                    ),
                    "Reinstall them if a job needs them, or ignore this if they were removed on purpose.",
                ));
            }
        }
        LedgerStanding::Absent => {}
    }
    found
}

/// The findings about jobs whose release the store does not hold, which a retired job cannot be hurt by.
fn job_findings(standing: &StoreStanding, jobs: &[JobListing]) -> Vec<HealthCheck> {
    let store_is_missing = matches!(standing.ledger, LedgerStanding::Absent) && standing.releases.is_empty();
    let naming: Vec<&JobListing> = jobs
        .iter()
        .filter(|job| job.lifecycle != JobLifecycle::Retired)
        .filter(|job| {
            job.version.as_ref().is_some_and(|version| {
                !standing
                    .releases
                    .iter()
                    .any(|held| held.release.name.as_str() == job.key.package && held.release.version == *version)
            })
        })
        .collect();
    if naming.is_empty() {
        return Vec::new();
    }
    if store_is_missing {
        return vec![finding(
            CheckKind::PackageStore,
            format!(
                "There is no package store in this vault, but these jobs name releases: {}.",
                names(naming.iter().map(|job| &job.key))
            ),
            "Install the workflow packages these jobs name, or restore the vault's package store from a copy.",
        )];
    }
    vec![finding(
        CheckKind::PackageStore,
        format!(
            "These jobs name releases the package store does not hold: {}.",
            names(naming.iter().map(|job| format!(
                "{} ({})",
                job.key,
                job.version.as_ref().map_or_else(String::new, ToString::to_string)
            )))
        ),
        "Reinstall the release each job names, or make a successor of the job on a release that is installed.",
    )]
}

impl From<StoreFacts<'_>> for StoreChecks {
    fn from(facts: StoreFacts<'_>) -> Self {
        let mut found = release_findings(&facts);
        found.extend(store_findings(facts.standing));
        match facts.jobs {
            Ok(jobs) => found.extend(job_findings(facts.standing, jobs)),
            Err(code) => found.push(finding(
                CheckKind::PackageStore,
                format!("The vault's jobs could not be listed ({code}), so the releases they name were not checked."),
                "Check that the vault's job files and job state can be read, then check again.",
            )),
        }
        if found.is_empty() && !facts.standing.releases.is_empty() {
            found.push(HealthCheck {
                kind: CheckKind::PackageStore,
                status: CheckStatus::Pass,
                message: format!(
                    "All {} releases in the package store are whole, and the ledger agrees with them.",
                    facts.standing.releases.len()
                ),
                action: None,
                auto_fixable: false,
            });
        }
        Self(found)
    }
}

#[cfg(test)]
#[path = "vault_health_releases_test.rs"]
mod tests;
