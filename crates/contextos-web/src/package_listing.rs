//! What the installed packages offer and who needs it: each release's exports, and the installed releases that depend on
//! it, for the packages page and `packages list`.

use std::fmt;

use contextos_workflows::{PackageIdentity, ReleaseRef};
use semver::VersionReq;

use crate::package_store::{Inventory, UnreadableRelease};

/// An installed release that depends on a package, and what it asks of it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dependent {
    pub release: ReleaseRef,
    pub requirement: VersionReq,
}

impl fmt::Display for Dependent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({})", self.release, self.requirement)
    }
}

/// One installed release as listed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListedRelease {
    pub identity: PackageIdentity,
    /// The modules other packages may import, as the manifest writes them.
    pub exports: Vec<String>,
    /// The slugs of the workflows it declares.
    pub workflows: Vec<String>,
    /// The installed releases that depend on this package with a requirement this release satisfies, in release order.
    pub required_by: Vec<Dependent>,
}

/// Every installed release, and the ones that could not be read.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PackageListing {
    releases: Vec<ListedRelease>,
    pub unreadable: Vec<UnreadableRelease>,
}

impl From<Inventory> for PackageListing {
    fn from(inventory: Inventory) -> Self {
        let installed: Vec<_> = inventory.releases.iter().collect();
        let mut releases: Vec<ListedRelease> = installed
            .iter()
            .map(|release| {
                let identity = &release.manifest.identity;
                let mut required_by: Vec<Dependent> = installed
                    .iter()
                    .flat_map(|other| {
                        let other_identity = &other.manifest.identity;
                        other
                            .kind
                            .libraries()
                            .entries()
                            .iter()
                            .filter(|dependency| {
                                dependency.name == identity.name && dependency.requirement.matches(&identity.version)
                            })
                            .map(|dependency| Dependent {
                                release: ReleaseRef {
                                    name: other_identity.name.clone(),
                                    version: other_identity.version.clone(),
                                },
                                requirement: dependency.requirement.clone(),
                            })
                    })
                    .collect();
                required_by.sort_by(|left, right| left.release.cmp(&right.release));
                ListedRelease {
                    identity: identity.clone(),
                    exports: release
                        .kind
                        .exports()
                        .map(|exports| exports.entries().iter().map(ToString::to_string).collect())
                        .unwrap_or_default(),
                    workflows: release.kind.workflows().to_vec(),
                    required_by,
                }
            })
            .collect();
        releases.sort_by(|left, right| {
            (&left.identity.name, &left.identity.version).cmp(&(&right.identity.name, &right.identity.version))
        });
        Self {
            releases,
            unreadable: inventory.unreadable,
        }
    }
}

impl PackageListing {
    /// Every readable release, by name then version.
    #[must_use]
    pub fn releases(&self) -> &[ListedRelease] {
        &self.releases
    }

    /// The listing of the release `name` `version`, when it is installed and readable.
    #[must_use]
    pub fn release(&self, name: &str, version: &str) -> Option<&ListedRelease> {
        self.releases
            .iter()
            .find(|listed| listed.identity.name.as_str() == name && listed.identity.version.to_string() == version)
    }
}
