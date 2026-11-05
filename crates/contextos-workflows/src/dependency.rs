//! Pure dependency resolution over a package's own declared
//! [`PackageDependency`](crate::package::PackageDependency) requirements
//! against a caller-supplied installed-version set. No I/O, no host
//! storage: which packages are actually installed on a host is a
//! separate concern entirely outside this crate.

use std::collections::BTreeMap;

use semver::Version;

use crate::package::{PackageDependency, PackageError, PackageIdentity};
use crate::package_name::PackageName;

/// Resolves every one of `dependencies` against `installed` (name ->
/// installed version), consulting `overrides` first when a name appears
/// in both: an explicit override always takes precedence over whatever
/// `installed` would otherwise resolve to, even when the override itself
/// would not satisfy the dependency's own requirement (the caller asked
/// for it explicitly; this function still reports
/// [`PackageError::IncompatibleVersion`] in that case rather than
/// silently accepting it).
///
/// # Errors
///
/// Returns the first [`PackageError::UnresolvedDependency`] or
/// [`PackageError::IncompatibleVersion`] found, in declaration order.
pub fn resolve_dependencies(
    dependencies: &[PackageDependency],
    installed: &BTreeMap<PackageName, Version>,
    overrides: &BTreeMap<PackageName, Version>,
) -> Result<BTreeMap<PackageName, Version>, PackageError> {
    let mut resolved = BTreeMap::new();
    for dependency in dependencies {
        let version = overrides
            .get(&dependency.name)
            .or_else(|| installed.get(&dependency.name))
            .ok_or_else(|| PackageError::UnresolvedDependency(dependency.name.to_string()))?;
        if !dependency.requirement.matches(version) {
            return Err(PackageError::IncompatibleVersion {
                name: dependency.name.to_string(),
                installed: version.clone(),
                requirement: dependency.requirement.clone(),
            });
        }
        resolved.insert(dependency.name.clone(), version.clone());
    }
    Ok(resolved)
}

/// Rejects a set of package identities containing two entries with the
/// identical `(name, version)` pair: a pure structural check over
/// whatever set the caller supplies, not itself a storage or uniqueness
/// guarantee - a host-level package store is responsible for calling
/// this against its own real inventory.
///
/// # Errors
///
/// Returns [`PackageError::DuplicateIdentity`] naming the first duplicate
/// found.
pub fn validate_no_duplicate_identity(identities: &[PackageIdentity]) -> Result<(), PackageError> {
    let mut seen = std::collections::HashSet::new();
    for identity in identities {
        let key = (identity.name.clone(), identity.version.clone());
        if !seen.insert(key) {
            return Err(PackageError::DuplicateIdentity {
                name: identity.name.to_string(),
                version: identity.version.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "dependency_test.rs"]
mod tests;
