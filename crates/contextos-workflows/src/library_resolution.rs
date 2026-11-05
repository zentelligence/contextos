//! Which library releases a package's job runs with: a pure choice over the releases a catalogue offers.
//!
//! A package names the packages it depends on, each with a version requirement, and those packages name theirs in
//! turn. The choice is made by one fixed procedure with no backtracking, so two hosts with the same releases installed
//! choose the same ones:
//!
//! 1. The requirements start as the job's own package's `libraries`.
//! 2. In rounds, every name that has a requirement and no choice yet is taken in name order. For each, the newest
//!    installed release that satisfies every requirement collected so far on that name is chosen, by the `semver`
//!    crate's matching, so a pre-release is chosen only when a requirement admits it. The chosen release's own
//!    `libraries` are then added to the requirements.
//! 3. A requirement added later that excludes a release already chosen fails the resolution, and nothing is retried. A
//!    name no installed release satisfies, and a package reached again through its own dependencies, fail it too. Every failure names the package and every requirement on it with the
//!    release that made it.
//! 4. At most [`MAX_LIBRARY_PACKAGES`] packages are chosen, and at most [`MAX_LIBRARY_DEPTH`] rounds are needed, a
//!    package's round being its distance from the job's own package.
//!
//! Nothing here reads a store. A [`ReleaseCatalogue`] says what is installed, so the choice is the same whether it is
//! made over a map held in memory or over a host's package store.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use semver::{Version, VersionReq};

use crate::package::{PackageDependency, PackageError, PackageManifest};
use crate::package_contents::PackageKind;
use crate::package_name::PackageName;

/// The most library packages one job may reach.
pub const MAX_LIBRARY_PACKAGES: usize = 32;
/// The deepest a chain of library dependencies may go, a direct dependency being the first level.
pub const MAX_LIBRARY_DEPTH: usize = 8;

/// The most module files one job's snapshot may hold, across its own package and every library.
pub const MAX_SNAPSHOT_FILES: usize = 500;
/// The most module text one job's snapshot may hold, in bytes.
pub const MAX_SNAPSHOT_BYTES: u64 = 10 * 1024 * 1024;

/// What a snapshot may still hold. A file is charged by its size from its details, before it is read, so a snapshot
/// never reads a file that would take it past a limit, and then charged for any bytes it turned out to hold beyond that
/// size, so a file that grew in between cannot take the snapshot past a limit either.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SnapshotBudget {
    files: usize,
    bytes: u64,
}

impl SnapshotBudget {
    /// Charges the file at `path`, of `size` bytes, against the limits.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::SnapshotOverLimit`] naming `path` when the file would take the snapshot past
    /// [`MAX_SNAPSHOT_FILES`] files or [`MAX_SNAPSHOT_BYTES`] bytes. A refused file is not charged.
    pub fn charge(&mut self, path: &str, size: u64) -> Result<(), PackageError> {
        let over = |limit: &'static str| PackageError::SnapshotOverLimit {
            path: path.to_owned(),
            limit,
        };
        if self.files >= MAX_SNAPSHOT_FILES {
            return Err(over("500 module files"));
        }
        let bytes = self
            .bytes
            .checked_add(size)
            .filter(|total| *total <= MAX_SNAPSHOT_BYTES)
            .ok_or_else(|| over("10 MiB of module text"))?;
        self.files += 1;
        self.bytes = bytes;
        Ok(())
    }

    /// Charges `extra` bytes more for the file at `path`, already charged by its size, because reading it gave more
    /// than that size.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::SnapshotOverLimit`] naming `path` when the bytes would take the snapshot past
    /// [`MAX_SNAPSHOT_BYTES`]. Refused bytes are not charged.
    pub fn charge_growth(&mut self, path: &str, extra: u64) -> Result<(), PackageError> {
        self.bytes = self
            .bytes
            .checked_add(extra)
            .filter(|total| *total <= MAX_SNAPSHOT_BYTES)
            .ok_or_else(|| PackageError::SnapshotOverLimit {
                path: path.to_owned(),
                limit: "10 MiB of module text",
            })?;
        Ok(())
    }
}

/// One release of a package.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReleaseRef {
    pub name: PackageName,
    pub version: Version,
}

impl fmt::Display for ReleaseRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.name, self.version)
    }
}

/// A release and what it requires of other packages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseRequirements {
    pub release: ReleaseRef,
    pub dependencies: Vec<PackageDependency>,
}

/// A manifest and the package file it lists, read.
#[derive(Clone, Copy, Debug)]
pub struct ReleaseContents<'a> {
    pub manifest: &'a PackageManifest,
    pub kind: &'a PackageKind,
}

impl From<ReleaseContents<'_>> for ReleaseRequirements {
    fn from(contents: ReleaseContents<'_>) -> Self {
        let ReleaseContents { manifest, kind } = contents;
        Self {
            release: ReleaseRef {
                name: manifest.identity.name.clone(),
                version: manifest.identity.version.clone(),
            },
            dependencies: kind.libraries().entries().to_vec(),
        }
    }
}

/// One requirement on a package, and the release that made it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Requirement {
    pub by: ReleaseRef,
    pub requirement: VersionReq,
}

impl fmt::Display for Requirement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} requires {}", self.by, self.requirement)
    }
}

/// What is installed, as far as choosing a release needs to know.
pub trait ReleaseCatalogue {
    /// Every installed release of `name`, in any order, none twice. Empty when the package is not installed.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::CatalogueUnreadable`] when the releases cannot be listed.
    fn versions(&self, name: &PackageName) -> Result<Vec<Version>, PackageError>;

    /// The dependencies the installed `release` declares.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::CatalogueUnreadable`] when the release cannot be read.
    fn dependencies(&self, release: &ReleaseRef) -> Result<Vec<PackageDependency>, PackageError>;
}

/// A catalogue held in memory.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReleaseInventory(BTreeMap<ReleaseRef, Vec<PackageDependency>>);

impl Extend<ReleaseRequirements> for ReleaseInventory {
    fn extend<T: IntoIterator<Item = ReleaseRequirements>>(&mut self, releases: T) {
        self.0
            .extend(releases.into_iter().map(|entry| (entry.release, entry.dependencies)));
    }
}

impl FromIterator<ReleaseRequirements> for ReleaseInventory {
    fn from_iter<T: IntoIterator<Item = ReleaseRequirements>>(releases: T) -> Self {
        let mut inventory = Self::default();
        inventory.extend(releases);
        inventory
    }
}

impl ReleaseCatalogue for ReleaseInventory {
    fn versions(&self, name: &PackageName) -> Result<Vec<Version>, PackageError> {
        Ok(self
            .0
            .keys()
            .filter(|release| release.name == *name)
            .map(|release| release.version.clone())
            .collect())
    }

    fn dependencies(&self, release: &ReleaseRef) -> Result<Vec<PackageDependency>, PackageError> {
        Ok(self.0.get(release).cloned().unwrap_or_default())
    }
}

/// What to resolve.
pub struct LibraryResolution<'a> {
    /// The job's own package release and its dependencies, which start the requirements.
    pub root: &'a ReleaseRequirements,
    pub catalogue: &'a dyn ReleaseCatalogue,
}

/// The library releases a job runs with.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolvedLibraries(BTreeMap<PackageName, Version>);

impl ResolvedLibraries {
    /// Every chosen release, in name order.
    pub fn releases(&self) -> impl Iterator<Item = ReleaseRef> + '_ {
        self.0.iter().map(|(name, version)| ReleaseRef {
            name: name.clone(),
            version: version.clone(),
        })
    }

    /// The release chosen for `name`.
    #[must_use]
    pub fn get(&self, name: &PackageName) -> Option<&Version> {
        self.0.get(name)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Chooses the library releases `request.root` runs with, by the procedure in this module's documentation.
///
/// # Errors
///
/// Returns the first failure the procedure meets: [`PackageError::LibraryNotInstalled`],
/// [`PackageError::NoReleaseSatisfies`], [`PackageError::RequirementExcludesChosen`],
/// [`PackageError::DependencyCycle`], [`PackageError::TooManyDependencies`],
/// [`PackageError::DependencyTooDeep`], or [`PackageError::CatalogueUnreadable`].
pub fn resolve_libraries(request: &LibraryResolution<'_>) -> Result<ResolvedLibraries, PackageError> {
    let mut state = Resolution::default();
    state.require(&request.root.release, &request.root.dependencies)?;
    let mut depth = 0;
    loop {
        // Every name that has a requirement and no choice yet, in name order. A name a choice in this round adds is for
        // the next round, which is what makes a round a level.
        let pending: Vec<PackageName> = state
            .requirements
            .keys()
            .filter(|name| !state.chosen.contains_key(*name))
            .cloned()
            .collect();
        let Some(first) = pending.first() else {
            return Ok(ResolvedLibraries(state.chosen));
        };
        depth += 1;
        if depth > MAX_LIBRARY_DEPTH {
            return Err(PackageError::DependencyTooDeep {
                name: first.clone(),
                limit: MAX_LIBRARY_DEPTH,
            });
        }
        for name in pending {
            let version = state.choose(&name, request.catalogue)?;
            let release = ReleaseRef {
                name: name.clone(),
                version: version.clone(),
            };
            let dependencies = request.catalogue.dependencies(&release)?;
            state.chosen.insert(name, version);
            state.require(&release, &dependencies)?;
        }
    }
}

/// What the procedure has collected and decided so far.
#[derive(Default)]
struct Resolution {
    requirements: BTreeMap<PackageName, Vec<Requirement>>,
    chosen: BTreeMap<PackageName, Version>,
    /// Which packages each package whose requirements are known depends on, the job's own package included, for finding
    /// a loop.
    edges: BTreeMap<PackageName, BTreeSet<PackageName>>,
}

impl Resolution {
    /// Records what `by` requires, failing when that closes a loop or excludes a release already chosen.
    fn require(&mut self, by: &ReleaseRef, dependencies: &[PackageDependency]) -> Result<(), PackageError> {
        self.edges
            .entry(by.name.clone())
            .or_default()
            .extend(dependencies.iter().map(|dependency| dependency.name.clone()));
        for dependency in dependencies {
            if let Some(path) = self.path_back(&dependency.name, &by.name) {
                let mut cycle = vec![by.name.clone()];
                cycle.extend(path);
                return Err(PackageError::DependencyCycle { path: cycle });
            }
            self.requirements
                .entry(dependency.name.clone())
                .or_default()
                .push(Requirement {
                    by: by.clone(),
                    requirement: dependency.requirement.clone(),
                });
            if let Some(chosen) = self.chosen.get(&dependency.name)
                && !dependency.requirement.matches(chosen)
            {
                return Err(PackageError::RequirementExcludesChosen {
                    name: dependency.name.clone(),
                    chosen: chosen.clone(),
                    requirements: self.requirements_on(&dependency.name),
                });
            }
        }
        Ok(())
    }

    fn requirements_on(&self, name: &PackageName) -> Vec<Requirement> {
        self.requirements.get(name).cloned().unwrap_or_default()
    }

    /// The release to use for `name`: the newest installed release every requirement so far admits.
    fn choose(&self, name: &PackageName, catalogue: &dyn ReleaseCatalogue) -> Result<Version, PackageError> {
        let requirements = self.requirements_on(name);
        let admits = |version: &Version| requirements.iter().all(|entry| entry.requirement.matches(version));
        let installed: BTreeSet<Version> = catalogue.versions(name)?.into_iter().collect();
        let version = if installed.is_empty() {
            return Err(PackageError::LibraryNotInstalled {
                name: name.clone(),
                requirements,
            });
        } else {
            let Some(newest) = installed.iter().rev().find(|version| admits(version)).cloned() else {
                return Err(PackageError::NoReleaseSatisfies {
                    name: name.clone(),
                    requirements,
                    installed: installed.into_iter().collect(),
                });
            };
            newest
        };
        if self.chosen.len() >= MAX_LIBRARY_PACKAGES {
            return Err(PackageError::TooManyDependencies {
                limit: MAX_LIBRARY_PACKAGES,
            });
        }
        Ok(version)
    }

    /// The packages from `from` to `to`, both included, along the dependencies known so far, if `to` is reachable from
    /// `from`.
    fn path_back(&self, from: &PackageName, to: &PackageName) -> Option<Vec<PackageName>> {
        if from == to {
            return Some(vec![to.clone()]);
        }
        let mut parents: BTreeMap<&PackageName, &PackageName> = BTreeMap::new();
        let mut seen: BTreeSet<&PackageName> = BTreeSet::from([from]);
        let mut queue = std::collections::VecDeque::from([from]);
        while let Some(current) = queue.pop_front() {
            for next in self.edges.get(current).into_iter().flatten() {
                if !seen.insert(next) {
                    continue;
                }
                parents.insert(next, current);
                if next == to {
                    let mut path = vec![to.clone()];
                    let mut walk = to;
                    while let Some(parent) = parents.get(walk) {
                        path.push((*parent).clone());
                        walk = parent;
                    }
                    path.reverse();
                    return Some(path);
                }
                queue.push_back(next);
            }
        }
        None
    }
}

#[cfg(test)]
#[path = "library_resolution_test.rs"]
mod tests;
