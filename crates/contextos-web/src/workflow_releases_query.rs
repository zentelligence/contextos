//! Reading what `workflows releases` reports from a package store, above the store port: the same code reports a vault's
//! store and the host's.

use std::collections::BTreeSet;

use contextos_packages::{
    EntryProvenance, LedgerState, PackageStore, ReadCheck, ReleaseKey, ReleaseName, ReleaseVersion,
};
use contextos_workflows::{DeclaredFiles, JobsState, PackageKind, PackageName};
use semver::Version;

use crate::job_discovery::exists_in_package;
use crate::mcp_client::McpClient;
use crate::package_release_manifest::{PackageFilesOf, PackageRelease};
use crate::shared_job_authoring::JobListing;
use crate::workflow_releases::{ReleaseCondition, ReleaseFacts, ReleasesInput, WorkflowReleases, recorded_versions};
use crate::workflow_releases_error::WorkflowReleasesError;

/// Where a release's declared templates are read from when a run reads them.
#[derive(Clone, Copy)]
pub(crate) enum TemplateHome<'a> {
    /// From the release itself, so a whole release cannot have lost one.
    Release,
    /// Live from the package's source folder in a vault, which a release's declared template can outlive.
    VaultSource { client: &'a McpClient, vault: &'a str },
}

/// One workflow's releases to report from one store.
pub(crate) struct ReleasesQuery<'a> {
    /// The word the report names the store with: `shared`, or the vault's name.
    pub(crate) store_name: &'a str,
    /// The package.
    pub(crate) package: &'a PackageName,
    /// The workflow.
    pub(crate) workflow: &'a str,
    /// The state file of the same home as the store.
    pub(crate) state: &'a JobsState,
    /// Every job of the same home.
    pub(crate) jobs: &'a [JobListing],
    /// Where the templates a release declares are read from.
    pub(crate) templates: TemplateHome<'a>,
}

impl ReleasesQuery<'_> {
    /// Reads `store` and reports.
    ///
    /// # Errors
    ///
    /// Returns the failure of the store port when the store cannot be listed or named.
    pub(crate) async fn report<S>(&self, store: &S) -> Result<WorkflowReleases, WorkflowReleasesError>
    where
        S: PackageStore<Manifest = PackageRelease>,
    {
        let package = self.package.as_str();
        let mut versions: BTreeSet<Version> = store
            .list()
            .await?
            .releases
            .iter()
            .filter(|key| key.name.as_str() == package)
            .map(|key| key.version.as_semver().clone())
            .collect();
        versions.extend(recorded_versions(self.state, package, self.workflow));
        let ledger = match store.ledger().await?.state {
            LedgerState::Present(ledger) => Some(ledger),
            LedgerState::Absent | LedgerState::Unusable(_) => None,
        };
        let name = ReleaseName::try_from(package)?;
        let mut facts = Vec::new();
        for version in versions {
            let key = ReleaseKey {
                name: name.clone(),
                version: ReleaseVersion::try_from(version.to_string().as_str())?,
            };
            let entry = ledger
                .as_ref()
                .and_then(|ledger| ledger.entries.iter().find(|entry| entry.release.key == key));
            let (installed_at, origin) = match entry.map(|entry| entry.provenance) {
                Some(EntryProvenance::Installed { origin, installed_at }) => (Some(installed_at), Some(origin)),
                Some(EntryProvenance::Rebuilt) | None => (None, None),
            };
            let check = store.read_release(&key).await;
            let (condition, holds_workflow) = self.condition_of(&check);
            let missing_templates = self.missing_templates(&check).await?;
            facts.push(ReleaseFacts {
                version,
                condition,
                holds_workflow,
                installed_at,
                origin,
                missing_templates,
            });
        }
        Ok(WorkflowReleases::from(ReleasesInput {
            store: self.store_name,
            package,
            workflow: self.workflow,
            facts,
            state: self.state,
            jobs: self.jobs,
        }))
    }

    /// The templates a whole release declares that its template home no longer holds, in the order declared. A release
    /// that is not whole is reported by its condition, and says nothing of its templates.
    ///
    /// # Errors
    ///
    /// Returns the failure to ask the vault's server about a template.
    async fn missing_templates(&self, check: &ReadCheck<PackageRelease>) -> Result<Vec<String>, WorkflowReleasesError> {
        let TemplateHome::VaultSource { client, vault } = self.templates else {
            return Ok(Vec::new());
        };
        let ReadCheck::Verified(verified) = check else {
            return Ok(Vec::new());
        };
        let Ok(kind) = PackageKind::try_from(PackageFilesOf {
            manifest: verified.manifest.manifest(),
            files: &verified.files,
        }) else {
            return Ok(Vec::new());
        };
        let mut missing = Vec::new();
        for template in kind.templates().into_iter().flat_map(DeclaredFiles::entries) {
            if !exists_in_package(client, vault, self.package.as_str(), template.as_str()).await? {
                missing.push(template.as_str().to_owned());
            }
        }
        Ok(missing)
    }

    /// What a read check found, and whether a whole release lists the workflow.
    fn condition_of(&self, check: &ReadCheck<PackageRelease>) -> (ReleaseCondition, bool) {
        let holds = match check {
            ReadCheck::Verified(verified) => PackageKind::try_from(PackageFilesOf {
                manifest: verified.manifest.manifest(),
                files: &verified.files,
            })
            .is_ok_and(|kind| kind.workflows().iter().any(|listed| listed == self.workflow)),
            // Only a whole release can say what it lists; any other holds the workflow.
            _ => true,
        };
        (ReleaseCondition::from(check), holds)
    }
}

#[cfg(test)]
#[path = "workflow_releases_query_test.rs"]
mod tests;
