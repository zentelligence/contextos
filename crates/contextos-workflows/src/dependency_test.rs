use semver::VersionReq;

use super::*;

type BoxError = Box<dyn std::error::Error>;

fn dependency(name: &str, requirement: &str) -> Result<PackageDependency, BoxError> {
    Ok(PackageDependency {
        name: PackageName::try_from(name)?,
        requirement: VersionReq::parse(requirement)?,
    })
}

fn version(raw: &str) -> Result<Version, BoxError> {
    Ok(Version::parse(raw)?)
}

fn identity(name: &str, raw_version: &str) -> Result<PackageIdentity, BoxError> {
    Ok(PackageIdentity {
        name: PackageName::try_from(name)?,
        version: version(raw_version)?,
        digest: crate::package::ContentDigest::try_new(format!("sha256:{}", "a".repeat(64)))?,
    })
}

#[test]
fn a_dependency_satisfied_by_an_installed_version_resolves() -> Result<(), BoxError> {
    let dependencies = [dependency("shared-lib", "^2.0")?];
    let mut installed = BTreeMap::new();
    installed.insert(PackageName::try_from("shared-lib")?, version("2.3.0")?);

    let resolved = resolve_dependencies(&dependencies, &installed, &BTreeMap::new())?;

    assert_eq!(resolved.get("shared-lib"), Some(&version("2.3.0")?));
    Ok(())
}

#[test]
fn a_dependency_with_no_installed_version_is_unresolved() -> Result<(), BoxError> {
    let dependencies = [dependency("shared-lib", "^2.0")?];
    let result = resolve_dependencies(&dependencies, &BTreeMap::new(), &BTreeMap::new());
    assert!(
        matches!(&result, Err(PackageError::UnresolvedDependency(name)) if name == "shared-lib"),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn an_installed_version_that_does_not_satisfy_the_requirement_is_incompatible() -> Result<(), BoxError> {
    let dependencies = [dependency("shared-lib", "^2.0")?];
    let mut installed = BTreeMap::new();
    installed.insert(PackageName::try_from("shared-lib")?, version("1.9.0")?);

    let result = resolve_dependencies(&dependencies, &installed, &BTreeMap::new());

    assert!(
        matches!(result, Err(PackageError::IncompatibleVersion { .. })),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn an_explicit_override_takes_precedence_over_the_installed_version() -> Result<(), BoxError> {
    let dependencies = [dependency("shared-lib", "^2.0")?];
    let mut installed = BTreeMap::new();
    installed.insert(PackageName::try_from("shared-lib")?, version("2.0.0")?);
    let mut overrides = BTreeMap::new();
    overrides.insert(PackageName::try_from("shared-lib")?, version("2.5.0")?);

    let resolved = resolve_dependencies(&dependencies, &installed, &overrides)?;

    assert_eq!(resolved.get("shared-lib"), Some(&version("2.5.0")?));
    Ok(())
}

#[test]
fn an_override_that_does_not_satisfy_the_requirement_is_still_reported_as_incompatible() -> Result<(), BoxError> {
    let dependencies = [dependency("shared-lib", "^2.0")?];
    let mut overrides = BTreeMap::new();
    overrides.insert(PackageName::try_from("shared-lib")?, version("1.0.0")?);

    let result = resolve_dependencies(&dependencies, &BTreeMap::new(), &overrides);

    assert!(
        matches!(result, Err(PackageError::IncompatibleVersion { .. })),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn every_dependency_resolves_independently_in_a_multi_dependency_manifest() -> Result<(), BoxError> {
    let dependencies = [dependency("lib-a", "^1.0")?, dependency("lib-b", "^2.0")?];
    let mut installed = BTreeMap::new();
    installed.insert(PackageName::try_from("lib-a")?, version("1.4.0")?);
    installed.insert(PackageName::try_from("lib-b")?, version("2.1.0")?);

    let resolved = resolve_dependencies(&dependencies, &installed, &BTreeMap::new())?;

    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved.get("lib-a"), Some(&version("1.4.0")?));
    assert_eq!(resolved.get("lib-b"), Some(&version("2.1.0")?));
    Ok(())
}

#[test]
fn distinct_name_version_pairs_pass_duplicate_identity_validation() -> Result<(), BoxError> {
    let identities = [
        identity("sync-crm", "1.0.0")?,
        identity("sync-crm", "1.1.0")?,
        identity("other", "1.0.0")?,
    ];
    assert!(validate_no_duplicate_identity(&identities).is_ok());
    Ok(())
}

#[test]
fn the_identical_name_and_version_registered_twice_is_a_duplicate_identity() -> Result<(), BoxError> {
    let identities = [identity("sync-crm", "1.0.0")?, identity("sync-crm", "1.0.0")?];
    let result = validate_no_duplicate_identity(&identities);
    assert!(
        matches!(result, Err(PackageError::DuplicateIdentity { .. })),
        "{result:?}"
    );
    Ok(())
}
