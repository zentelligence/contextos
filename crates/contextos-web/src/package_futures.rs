//! Recording each workflow's `future` release in a state file, for the host's store and a vault's alike.
//!
//! `future` follows from the releases installed, so whichever store changed them records it in the same state change:
//! one rule, [`contextos_workflows::JobsState::record_future`], applied to every workflow of the package.

use std::collections::BTreeMap;

use contextos_workflows::{JobsState, PackageName};
use semver::Version;

/// A package whose `future` releases are recorded in `state`.
pub(crate) struct FutureRecord<'a> {
    pub package: &'a PackageName,
    pub state: &'a mut JobsState,
}

impl FutureRecord<'_> {
    /// Records `future` for every workflow of the package from `installed`, each a workflow and a release that holds it
    /// and reads whole now. A workflow no installed release holds any longer, though the state records it, has its
    /// `future` cleared. Returns whether any recorded value changed.
    pub(crate) fn record(self, installed: impl IntoIterator<Item = (String, Version)>) -> bool {
        let mut held: BTreeMap<String, Vec<Version>> = self
            .state
            .package_releases(self.package.as_str())
            .map(|recorded| {
                recorded
                    .workflows
                    .keys()
                    .map(|workflow| (workflow.clone(), Vec::new()))
                    .collect()
            })
            .unwrap_or_default();
        for (workflow, version) in installed {
            held.entry(workflow).or_default().push(version);
        }
        let mut changed = false;
        for (workflow, versions) in &held {
            changed |= self.state.record_future(self.package.as_str(), workflow, versions);
        }
        changed
    }
}
