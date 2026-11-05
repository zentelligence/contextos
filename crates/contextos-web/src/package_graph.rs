//! The installed releases as a graph of what each one needs, checked by the procedure a run resolves its libraries with.
//!
//! An install does not leave an installed release that resolved before unable to resolve, so a release that installs also
//! runs. It asks the same question a run asks, of every installed release in turn: with the releases there are, can its
//! libraries be chosen? The answer comes from [`contextos_workflows::resolve_libraries`] over a catalogue held in memory,
//! so the store is read once and nothing here depends on how a run reads it.

use std::collections::BTreeMap;

use contextos_workflows::{
    LibraryResolution, ReleaseContents, ReleaseInventory, ReleaseRef, ReleaseRequirements, resolve_libraries,
};

/// An installed release whose libraries cannot be chosen, and why.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnresolvableRelease {
    pub release: ReleaseRef,
    /// The resolution failure, which names the library and every requirement on it.
    pub reason: String,
}

/// The installed releases and what each requires.
#[derive(Clone, Debug, Default)]
pub struct StoreGraph {
    releases: BTreeMap<ReleaseRef, ReleaseRequirements>,
}

impl<'a> FromIterator<ReleaseContents<'a>> for StoreGraph {
    fn from_iter<T: IntoIterator<Item = ReleaseContents<'a>>>(releases: T) -> Self {
        Self {
            releases: releases
                .into_iter()
                .map(|contents| {
                    let requirements = ReleaseRequirements::from(contents);
                    (requirements.release.clone(), requirements)
                })
                .collect(),
        }
    }
}

impl StoreGraph {
    /// The graph with `contents` installed as well.
    #[must_use]
    pub fn with(&self, contents: ReleaseContents<'_>) -> Self {
        let mut graph = self.clone();
        let requirements = ReleaseRequirements::from(contents);
        graph.releases.insert(requirements.release.clone(), requirements);
        graph
    }

    /// Whether `release` is installed.
    #[must_use]
    pub fn contains(&self, release: &ReleaseRef) -> bool {
        self.releases.contains_key(release)
    }

    /// Every installed release whose libraries cannot be chosen, in release order. A release is resolved as a job of it
    /// would be.
    #[must_use]
    pub fn unresolvable(&self) -> Vec<UnresolvableRelease> {
        let catalogue: ReleaseInventory = self.releases.values().cloned().collect();
        self.releases
            .values()
            .filter_map(|root| {
                resolve_libraries(&LibraryResolution {
                    root,
                    catalogue: &catalogue,
                })
                .err()
                .map(|error| UnresolvableRelease {
                    release: root.release.clone(),
                    reason: error.to_string(),
                })
            })
            .collect()
    }

    /// The releases that resolve in `self` and do not in `after`, and any release in `after` that is not in `self` and
    /// does not resolve. A release that could not resolve before is not made worse by a change, so it is not blamed on it.
    #[must_use]
    pub fn broken_by(&self, after: &Self) -> Vec<UnresolvableRelease> {
        let before: Vec<ReleaseRef> = self
            .unresolvable()
            .into_iter()
            .map(|unresolvable| unresolvable.release)
            .collect();
        after
            .unresolvable()
            .into_iter()
            .filter(|now| !before.contains(&now.release))
            .collect()
    }
}
