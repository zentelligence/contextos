//! Installing a vault's source into the vault's package store: `workflows install` and `libraries install`.
//!
//! What is installed is what was tested. The source is read once through the primary server, checked as validation
//! checks a job's code, and made into a release whose version the store works out from the ledger. A library the source
//! names without a requirement is installed from its own source first, and the release requires `^` the version it was
//! tested on. Nothing is written until every check has passed.

use std::collections::{BTreeMap, BTreeSet};

use contextos_packages::{
    InstallOutcome, InstallRequest, LedgerProblem, LedgerState, PackageStore, PackagesError, ReadCheck,
    ReleaseManifest, ReleaseOrigin, StoreHome, VerifiedRelease,
};
use contextos_workflows::call_rules::CallRules;
use contextos_workflows::{
    JobsState, PackageKind, PackageName, ReleaseKind, StoreKind, ValidationCode, ValidationFailure,
};
use semver::Version;
use serde::Serialize;

use crate::job_validation::{check_syntax, workflow_calls};
use crate::library_checks::library_tool_failures;
use crate::mcp_client::McpClient;
use crate::package_futures::FutureRecord;
use crate::package_release_manifest::{PackageFilesOf, PackageRelease};
use crate::scheduler::{read_state, write_state};
use crate::source_install_error::SourceInstallError;
use crate::source_overlay::OverlayStore;
use crate::store_retention::{RetentionWarning, retain_vault_held};
use crate::vault_job_release::ReleaseFinding;
use crate::vault_release_snapshot::{
    ReleaseFailure, ReleaseRequest, open_release, read_library_code, read_opened_release,
};
use crate::vault_source_plan::{Holdings, Plan, PlanRequest, PlannedAction, PlannedRelease, plan};
use crate::vault_source_read::{SourceKind, SourceTarget, read_source};

/// How many times recording `future` repeats when another writer changes the job state under it.
const STATE_ATTEMPTS: usize = 3;

/// What an install did to one release.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceOutcome {
    /// A new release was made and installed.
    Installed,
    /// An installed release holds exactly this content, so nothing was installed and that release is reported.
    AlreadyInstalled,
    /// The release recorded under the version was removed, damaged, or incomplete and is restored from source.
    Repaired,
    /// The release recorded under the version is present and verified, so nothing changed.
    Unchanged,
}

/// One release, as an install reports it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReleaseReport {
    pub package: String,
    pub version: String,
    pub digest: String,
    pub outcome: SourceOutcome,
}

/// A library a release requires and the requirement it states.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RequirementReport {
    pub library: String,
    pub requirement: String,
}

/// What installing a source package did.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SourceInstalled {
    /// The package's release.
    pub release: ReleaseReport,
    /// Whether the package is a workflow package or a library.
    pub kind: &'static str,
    /// For a workflow package, whether the release is current, previous, future, or a retired release, for each workflow.
    pub standing: BTreeMap<String, String>,
    /// The libraries the release requires.
    pub requirements: Vec<RequirementReport>,
    /// The libraries installed or found installed from source on the way, each before the package that needs it.
    pub libraries: Vec<ReleaseReport>,
    /// Retention could not finish after the install. The release is installed all the same.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<RetentionWarning>,
    /// Whether retention removed the release again at once, because nothing keeps it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub removed_by_retention: bool,
}

/// An install from source into one vault's package store.
pub struct SourceInstall<'a> {
    /// A session to the vault's primary server, which source is read through and the store is reached by.
    pub client: &'a McpClient,
    /// The vault's name.
    pub vault: &'a str,
}

impl SourceInstall<'_> {
    /// Installs the source `target` names, or with `at` restores that exact release when it is removed, damaged, or
    /// incomplete.
    ///
    /// The source is read first, in one snapshot. The vault's store lock is then taken and held until the release is
    /// installed, so no other `contextos-web` process changes the store between the ledger being read and the release
    /// being written; the lock is not waited for. The job state is optimistic, so `future` is recorded last, from the
    /// store as it then stands, and repeated if another writer changed the state in the meantime.
    ///
    /// # Errors
    ///
    /// The [`SourceInstallError`] that refused it; nothing is installed unless the error says otherwise.
    pub async fn install(
        &self,
        target: &SourceTarget,
        at: Option<&Version>,
    ) -> Result<SourceInstalled, SourceInstallError> {
        let snapshot = read_source(self.client, self.vault, target).await?;
        let engine = self.client.vault_packages(self.vault).await?;
        let state = self.state().await?;
        let lock = engine.lock().await?;
        let reading = engine.ledger().await?;
        let ledger = match reading.state {
            LedgerState::Absent => Vec::new(),
            LedgerState::Present(ledger) => ledger.entries,
            LedgerState::Unusable(LedgerProblem::Missing) => return Err(PackagesError::LedgerMissing.into()),
            LedgerState::Unusable(LedgerProblem::Unreadable(reason)) => {
                return Err(PackagesError::LedgerUnreadable { reason }.into());
            }
        };
        let present: BTreeSet<_> = engine.list().await?.releases.into_iter().collect();
        let made = plan(&PlanRequest {
            snapshot: &snapshot,
            holdings: Holdings {
                ledger: &ledger,
                present: &present,
                state: &state.value,
            },
            at,
        })?;
        check(&engine, &made).await?;
        let mut reports = Vec::new();
        for release in &made.releases {
            reports.push(report_of(release, install_one(&engine, &lock, release).await?));
        }
        let root = made.root().ok_or_else(|| SourceInstallError::State {
            action: "planned",
            reason: "the plan holds no release".to_owned(),
        })?;
        let standing = match root.kind {
            SourceKind::Workflows => {
                self.record_futures(&engine, root, root.action == PlannedAction::Install)
                    .await?
            }
            SourceKind::Library => BTreeMap::new(),
        };
        let retained = retain_vault_held(&engine, &lock, self.client, self.vault).await;
        let removed_by_retention = retained
            .as_ref()
            .is_ok_and(|removed| removed.include(&root.name, root.key.version.as_semver()));
        let warnings = retained
            .err()
            .map(|error| RetentionWarning::raised(StoreKind::Vault, &error))
            .into_iter()
            .collect();
        let requirements = root
            .requirements
            .iter()
            .map(|(library, requirement)| RequirementReport {
                library: library.to_string(),
                requirement: requirement.to_string(),
            })
            .collect();
        let release = reports.pop().ok_or_else(|| SourceInstallError::State {
            action: "reported",
            reason: "the plan holds no release".to_owned(),
        })?;
        Ok(SourceInstalled {
            release,
            kind: match root.kind {
                SourceKind::Workflows => "workflows",
                SourceKind::Library => "library",
            },
            standing,
            requirements,
            libraries: reports,
            warnings,
            removed_by_retention,
        })
    }

    async fn state(&self) -> Result<crate::vault_state::Snapshot<JobsState>, SourceInstallError> {
        read_state(self.client, self.vault)
            .await
            .map_err(|error| SourceInstallError::State {
                action: "read",
                reason: error.to_string(),
            })
    }

    /// Records `future` for each workflow of the package from the releases in the store now, when `record` says the
    /// store changed, and returns whether the release is current, previous, or future for each of its workflows.
    async fn record_futures<S>(
        &self,
        engine: &S,
        root: &PlannedRelease,
        record: bool,
    ) -> Result<BTreeMap<String, String>, SourceInstallError>
    where
        S: PackageStore<Manifest = PackageRelease>,
    {
        let installed = if record {
            installed_workflows(engine, &root.name).await?
        } else {
            Vec::new()
        };
        for _ in 0..STATE_ATTEMPTS {
            let mut state = self.state().await?;
            let changed = record
                && FutureRecord {
                    package: &root.name,
                    state: &mut state.value,
                }
                .record(installed.clone());
            if changed {
                match write_state(self.client, self.vault, &state).await {
                    Ok(()) => {}
                    Err(error) if error.is_conflict() => continue,
                    Err(error) => {
                        return Err(SourceInstallError::State {
                            action: "written",
                            reason: error.to_string(),
                        });
                    }
                }
            }
            return Ok(standing_of(&state.value, root));
        }
        Err(SourceInstallError::StateChanged)
    }
}

/// Whether the release is current, previous, future, or a retired release, for each workflow it holds.
fn standing_of(state: &JobsState, root: &PlannedRelease) -> BTreeMap<String, String> {
    let releases = state.package_releases(root.name.as_str()).cloned().unwrap_or_default();
    let interface = PackageRelease::try_from(root.manifest_text.as_str())
        .map(|release| release.interface(&root.files))
        .unwrap_or_default();
    interface
        .workflows
        .iter()
        .map(|workflow| {
            let kind: ReleaseKind = releases.kind_of(&workflow.slug, root.version());
            (workflow.slug.clone(), kind.as_str().to_owned())
        })
        .collect()
}

/// Every workflow of every release of `package` that reads whole now, with the release's version.
pub(crate) async fn installed_workflows<S>(
    engine: &S,
    package: &PackageName,
) -> Result<Vec<(String, Version)>, SourceInstallError>
where
    S: PackageStore<Manifest = PackageRelease>,
{
    let mut found = Vec::new();
    for key in engine.list().await?.releases {
        if key.name.as_str() != package.as_str() {
            continue;
        }
        if let ReadCheck::Verified(verified) = engine.read_release(&key).await
            && let Ok(kind) = PackageKind::try_from(PackageFilesOf {
                manifest: verified.manifest.manifest(),
                files: &verified.files,
            })
        {
            let version = key.version.as_semver().clone();
            found.extend(
                kind.workflows()
                    .iter()
                    .map(|workflow| (workflow.clone(), version.clone())),
            );
        }
    }
    Ok(found)
}

async fn install_one<S>(
    engine: &S,
    lock: &S::Lock,
    release: &PlannedRelease,
) -> Result<SourceOutcome, SourceInstallError>
where
    S: PackageStore<Manifest = PackageRelease>,
{
    if release.action == PlannedAction::Existing {
        return Ok(SourceOutcome::AlreadyInstalled);
    }
    let installed = engine
        .install(
            lock,
            InstallRequest {
                manifest_text: release.manifest_text.clone(),
                files: release.files.clone(),
                origin: ReleaseOrigin::Source,
            },
        )
        .await?;
    Ok(match installed.outcome {
        InstallOutcome::Installed => SourceOutcome::Installed,
        InstallOutcome::Unchanged => SourceOutcome::Unchanged,
        InstallOutcome::Repaired => SourceOutcome::Repaired,
    })
}

fn report_of(release: &PlannedRelease, outcome: SourceOutcome) -> ReleaseReport {
    ReleaseReport {
        package: release.name.to_string(),
        version: release.version().to_string(),
        digest: release.digest.as_str().to_owned(),
        outcome,
    }
}

/// Checks every release the plan makes, as validation checks a job's code, before any is written: the manifest and its
/// files, the store's own rules for a vault, every script and module, and the tools each library declares.
async fn check<S>(engine: &S, made: &Plan) -> Result<(), SourceInstallError>
where
    S: PackageStore<Manifest = PackageRelease>,
{
    let mut releases = BTreeMap::new();
    for release in made
        .releases
        .iter()
        .filter(|release| release.action == PlannedAction::Install)
    {
        let manifest =
            PackageRelease::try_from(release.manifest_text.as_str()).map_err(PackagesError::InvalidManifest)?;
        manifest
            .check_files(&release.files, StoreHome::Vault)
            .map_err(PackagesError::InvalidManifest)?;
        releases.insert(
            release.key.clone(),
            VerifiedRelease {
                release: contextos_packages::ReleaseId {
                    key: release.key.clone(),
                    digest: release.digest.clone(),
                },
                manifest,
                manifest_text: release.manifest_text.clone(),
                files: release.files.clone(),
            },
        );
    }
    let mut failures = Vec::new();
    for release in made
        .releases
        .iter()
        .filter(|release| release.action == PlannedAction::Install)
    {
        let pinned = made.tested_releases(release);
        let overlay = OverlayStore {
            inner: engine,
            releases: &releases,
            pinned: &pinned,
        };
        failures.extend(check_code(&overlay, release).await?);
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(SourceInstallError::Invalid { failures })
    }
}

/// The failures in the code of one release, read through `overlay` with the libraries it was tested on.
async fn check_code<S>(
    overlay: &OverlayStore<'_, S>,
    release: &PlannedRelease,
) -> Result<Vec<ValidationFailure>, SourceInstallError>
where
    S: PackageStore<Manifest = PackageRelease>,
{
    let mut failures = Vec::new();
    match release.kind {
        SourceKind::Library => {
            let code = read_library_code(overlay, &release.name, release.version())
                .await
                .map_err(|failure| refused(&failure))?;
            failures.extend(check_syntax("", &code.script).await);
            failures.extend(library_tool_failures(&code.script, "", &code.library_tools, &CallRules::default()).await);
        }
        SourceKind::Workflows => {
            let package = release.name.clone();
            let version = release.version().clone();
            let manifest =
                PackageRelease::try_from(release.manifest_text.as_str()).map_err(PackagesError::InvalidManifest)?;
            let kind = PackageKind::try_from(PackageFilesOf {
                manifest: manifest.manifest(),
                files: &release.files,
            })
            .map_err(|error| {
                SourceInstallError::refusing(ValidationCode::ArtefactInvalid, "workflows.toml", error.to_string())
            })?;
            for workflow in kind.workflows() {
                let opened = open_release(overlay, &package, &version)
                    .await
                    .map_err(|failure| refused(&failure))?;
                let snapshot = read_opened_release(
                    overlay,
                    opened,
                    &ReleaseRequest {
                        package: &package,
                        release: &version,
                        workflow,
                        lock: None,
                    },
                )
                .await
                .map_err(|failure| refused(&failure))?;
                let entry = snapshot.definition.entry.clone();
                failures.extend(check_syntax(&entry, &snapshot.script).await);
                failures.extend(
                    library_tool_failures(
                        &snapshot.script,
                        &entry,
                        &snapshot.library_tools,
                        &workflow_calls(Some(&snapshot.workflow_toml)),
                    )
                    .await,
                );
            }
        }
    }
    Ok(failures)
}

/// A release that could not be read for checking, as the refusal it is.
fn refused(failure: &ReleaseFailure) -> SourceInstallError {
    if let ReleaseFailure::Libraries(error) = failure
        && matches!(ValidationCode::try_from(error), Ok(ValidationCode::ArtefactMissing))
    {
        return SourceInstallError::DependencyUnsatisfied {
            reason: error.to_string(),
        };
    }
    let finding = ReleaseFinding::from(failure);
    let (code, artefact) = finding
        .failure
        .unwrap_or((ValidationCode::ArtefactInvalid, "the release".to_owned()));
    SourceInstallError::refusing(code, &artefact, finding.message)
}

#[cfg(test)]
#[path = "vault_source_install_test.rs"]
mod tests;

#[cfg(test)]
#[path = "vault_source_install_retention_test.rs"]
mod retention_tests;
