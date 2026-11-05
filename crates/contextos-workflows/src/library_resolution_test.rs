use super::*;
use proptest::prelude::*;

fn name(raw: &str) -> Result<PackageName, PackageError> {
    PackageName::try_from(raw)
}

fn release(
    raw_name: &str,
    version: &str,
    dependencies: &[(&str, &str)],
) -> Result<ReleaseRequirements, Box<dyn std::error::Error>> {
    Ok(ReleaseRequirements {
        release: ReleaseRef {
            name: name(raw_name)?,
            version: Version::parse(version)?,
        },
        dependencies: dependencies
            .iter()
            .map(|(dependency, requirement)| {
                Ok(PackageDependency {
                    name: name(dependency)?,
                    requirement: VersionReq::parse(requirement)?,
                })
            })
            .collect::<Result<_, Box<dyn std::error::Error>>>()?,
    })
}

type Release<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);

fn inventory(releases: &[Release<'_>]) -> Result<ReleaseInventory, Box<dyn std::error::Error>> {
    releases
        .iter()
        .map(|(name, version, dependencies)| release(name, version, dependencies))
        .collect()
}

fn resolve(
    root: &ReleaseRequirements,
    inventory: &ReleaseInventory,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(resolve_libraries(&LibraryResolution {
        root,
        catalogue: inventory,
    })
    .map(|resolved| resolved.releases().map(|release| release.to_string()).collect())
    .map_err(|error| error.to_string())?)
}

fn failure(root: &ReleaseRequirements, inventory: &ReleaseInventory) -> Option<PackageError> {
    resolve_libraries(&LibraryResolution {
        root,
        catalogue: inventory,
    })
    .err()
}

#[test]
fn the_newest_release_satisfying_the_requirement_is_chosen() -> Result<(), Box<dyn std::error::Error>> {
    let root = release("app", "1.0.0", &[("acme-text", "^1.2")])?;
    let installed = inventory(&[
        ("acme-text", "1.1.0", &[]),
        ("acme-text", "1.2.0", &[]),
        ("acme-text", "1.9.3", &[]),
        ("acme-text", "2.0.0", &[]),
    ])?;

    assert_eq!(resolve(&root, &installed)?, ["acme-text 1.9.3"]);
    Ok(())
}

#[test]
fn a_bare_requirement_means_caret() -> Result<(), Box<dyn std::error::Error>> {
    let root = release("app", "1.0.0", &[("acme-text", "1.4.2")])?;
    let installed = inventory(&[
        ("acme-text", "1.4.2", &[]),
        ("acme-text", "1.5.0", &[]),
        ("acme-text", "2.0.0", &[]),
    ])?;

    assert_eq!(resolve(&root, &installed)?, ["acme-text 1.5.0"]);
    Ok(())
}

#[test]
fn a_pre_release_is_chosen_only_when_a_requirement_admits_it() -> Result<(), Box<dyn std::error::Error>> {
    let installed = inventory(&[("acme-text", "1.0.0", &[]), ("acme-text", "1.1.0-beta.1", &[])])?;

    let plain = release("app", "1.0.0", &[("acme-text", "^1.0")])?;
    assert_eq!(resolve(&plain, &installed)?, ["acme-text 1.0.0"]);

    let admitting = release("app", "1.0.0", &[("acme-text", ">=1.1.0-beta.0, <2.0.0")])?;
    assert_eq!(resolve(&admitting, &installed)?, ["acme-text 1.1.0-beta.1"]);
    Ok(())
}

#[test]
fn only_a_pre_release_installed_leaves_a_plain_requirement_unsatisfied() -> Result<(), Box<dyn std::error::Error>> {
    let root = release("app", "1.0.0", &[("acme-text", "^1.0")])?;
    let installed = inventory(&[("acme-text", "1.1.0-beta.1", &[])])?;

    let error = failure(&root, &installed);

    assert!(
        matches!(error, Some(PackageError::NoReleaseSatisfies { .. })),
        "{error:?}"
    );
    Ok(())
}

#[test]
fn libraries_are_resolved_transitively_in_name_order() -> Result<(), Box<dyn std::error::Error>> {
    let root = release("app", "1.0.0", &[("b-lib", "^1")])?;
    let installed = inventory(&[
        ("b-lib", "1.0.0", &[("a-lib", "^1")]),
        ("a-lib", "1.0.0", &[("c-lib", "^2")]),
        ("c-lib", "2.1.0", &[]),
    ])?;

    assert_eq!(
        resolve(&root, &installed)?,
        ["a-lib 1.0.0", "b-lib 1.0.0", "c-lib 2.1.0"]
    );
    Ok(())
}

#[test]
fn a_package_needed_by_two_libraries_is_chosen_once_against_both_requirements() -> Result<(), Box<dyn std::error::Error>>
{
    let root = release("app", "1.0.0", &[("left", "^1"), ("right", "^1")])?;
    let installed = inventory(&[
        ("left", "1.0.0", &[("shared", ">=1.1, <3")]),
        ("right", "1.0.0", &[("shared", "<2")]),
        ("shared", "1.0.0", &[]),
        ("shared", "1.5.0", &[]),
        ("shared", "2.5.0", &[]),
    ])?;

    assert_eq!(
        resolve(&root, &installed)?,
        ["left 1.0.0", "right 1.0.0", "shared 1.5.0"]
    );
    Ok(())
}

#[test]
fn a_later_requirement_that_excludes_a_release_already_chosen_fails_without_retrying()
-> Result<(), Box<dyn std::error::Error>> {
    // `a-lib` is chosen in the first round at 2.0.0, its newest; `b-lib` then asks for `<2`, and an older `a-lib`
    // that would satisfy both is installed, but nothing is retried.
    let root = release("app", "1.0.0", &[("a-lib", ">=1"), ("b-lib", "^1")])?;
    let installed = inventory(&[
        ("a-lib", "1.0.0", &[]),
        ("a-lib", "2.0.0", &[]),
        ("b-lib", "1.0.0", &[("a-lib", "<2")]),
    ])?;

    let error = failure(&root, &installed);

    let Some(PackageError::RequirementExcludesChosen {
        name,
        chosen,
        requirements,
    }) = error
    else {
        return Err(format!("expected a conflict, got {error:?}").into());
    };
    assert_eq!(name.as_str(), "a-lib");
    assert_eq!(chosen, Version::parse("2.0.0")?);
    let named: Vec<String> = requirements.iter().map(ToString::to_string).collect();
    assert_eq!(named, ["app 1.0.0 requires >=1", "b-lib 1.0.0 requires <2"]);
    Ok(())
}

#[test]
fn a_name_nothing_satisfies_names_every_requirement_and_who_made_it() -> Result<(), Box<dyn std::error::Error>> {
    let root = release("app", "1.0.0", &[("a-lib", "^1"), ("b-lib", "^1")])?;
    let installed = inventory(&[
        ("a-lib", "1.0.0", &[("c-lib", "^3")]),
        ("b-lib", "1.0.0", &[("c-lib", "^2")]),
        ("c-lib", "2.0.0", &[]),
        ("c-lib", "3.0.0", &[]),
    ])?;

    let error = failure(&root, &installed);

    let Some(PackageError::NoReleaseSatisfies {
        name,
        requirements,
        installed,
    }) = error
    else {
        return Err(format!("expected unsatisfied, got {error:?}").into());
    };
    assert_eq!(name.as_str(), "c-lib");
    let named: Vec<String> = requirements.iter().map(ToString::to_string).collect();
    assert_eq!(named, ["a-lib 1.0.0 requires ^3", "b-lib 1.0.0 requires ^2"]);
    assert_eq!(installed.len(), 2);
    Ok(())
}

#[test]
fn a_name_with_no_installed_release_is_reported_as_not_installed() -> Result<(), Box<dyn std::error::Error>> {
    let root = release("app", "1.0.0", &[("missing", "^1")])?;

    let error = failure(&root, &ReleaseInventory::default());

    let Some(PackageError::LibraryNotInstalled { name, requirements }) = error else {
        return Err(format!("expected not installed, got {error:?}").into());
    };
    assert_eq!(name.as_str(), "missing");
    assert_eq!(requirements.len(), 1);
    Ok(())
}

#[test]
fn a_dependency_loop_fails_naming_the_loop() -> Result<(), Box<dyn std::error::Error>> {
    let root = release("app", "1.0.0", &[("a-lib", "^1")])?;
    let installed = inventory(&[
        ("a-lib", "1.0.0", &[("b-lib", "^1")]),
        ("b-lib", "1.0.0", &[("c-lib", "^1")]),
        ("c-lib", "1.0.0", &[("a-lib", "^1")]),
    ])?;

    let error = failure(&root, &installed);

    let Some(PackageError::DependencyCycle { path }) = error else {
        return Err(format!("expected a cycle, got {error:?}").into());
    };
    let named: Vec<&str> = path.iter().map(PackageName::as_str).collect();
    assert_eq!(named, ["c-lib", "a-lib", "b-lib", "c-lib"]);
    Ok(())
}

#[test]
fn a_package_that_depends_on_itself_is_a_loop() -> Result<(), Box<dyn std::error::Error>> {
    let root = release("app", "1.0.0", &[("a-lib", "^1")])?;
    let installed = inventory(&[("a-lib", "1.0.0", &[("a-lib", "^1")])])?;

    assert!(matches!(
        failure(&root, &installed),
        Some(PackageError::DependencyCycle { .. })
    ));
    Ok(())
}

#[test]
fn a_library_that_depends_on_the_job_s_own_package_is_a_loop() -> Result<(), Box<dyn std::error::Error>> {
    let root = release("app", "1.0.0", &[("a-lib", "^1")])?;
    let installed = inventory(&[("a-lib", "1.0.0", &[("app", "^1")]), ("app", "1.0.0", &[])])?;

    let error = failure(&root, &installed);

    let Some(PackageError::DependencyCycle { path }) = error else {
        return Err(format!("expected a cycle, got {error:?}").into());
    };
    let named: Vec<&str> = path.iter().map(PackageName::as_str).collect();
    assert_eq!(named, ["a-lib", "app", "a-lib"]);
    Ok(())
}

#[test]
fn two_libraries_sharing_a_dependency_are_not_a_loop() -> Result<(), Box<dyn std::error::Error>> {
    let root = release("app", "1.0.0", &[("left", "^1"), ("right", "^1")])?;
    let installed = inventory(&[
        ("left", "1.0.0", &[("shared", "^1")]),
        ("right", "1.0.0", &[("shared", "^1")]),
        ("shared", "1.0.0", &[]),
    ])?;

    assert_eq!(resolve(&root, &installed)?.len(), 3);
    Ok(())
}

/// A chain `lib-1 -> lib-2 -> ...` of `length` libraries, all at 1.0.0, with the job depending on `lib-1`.
fn chain(length: usize) -> Result<(ReleaseRequirements, ReleaseInventory), Box<dyn std::error::Error>> {
    let root = release("app", "1.0.0", &[("lib-01", "^1")])?;
    let mut inventory = ReleaseInventory::default();
    for index in 1..=length {
        let this = format!("lib-{index:02}");
        let next = format!("lib-{:02}", index + 1);
        let dependencies: Vec<(&str, &str)> = if index == length {
            vec![]
        } else {
            vec![(next.as_str(), "^1")]
        };
        inventory.extend([release(&this, "1.0.0", &dependencies)?]);
    }
    Ok((root, inventory))
}

#[test]
fn eight_levels_of_libraries_resolve_and_nine_do_not() -> Result<(), Box<dyn std::error::Error>> {
    let (root, installed) = chain(MAX_LIBRARY_DEPTH)?;
    assert_eq!(resolve(&root, &installed)?.len(), MAX_LIBRARY_DEPTH);

    let (root, installed) = chain(MAX_LIBRARY_DEPTH + 1)?;
    let error = failure(&root, &installed);
    assert!(
        matches!(error, Some(PackageError::DependencyTooDeep { limit, ref name }) if limit == MAX_LIBRARY_DEPTH && name.as_str() == "lib-09"),
        "{error:?}"
    );
    Ok(())
}

#[test]
fn thirty_two_libraries_resolve_and_thirty_three_do_not() -> Result<(), Box<dyn std::error::Error>> {
    let wide = |count: usize| -> Result<(ReleaseRequirements, ReleaseInventory), Box<dyn std::error::Error>> {
        let names: Vec<String> = (0..count).map(|index| format!("lib-{index:02}")).collect();
        let dependencies: Vec<(&str, &str)> = names.iter().map(|name| (name.as_str(), "^1")).collect();
        let root = release("app", "1.0.0", &dependencies)?;
        let inventory = names
            .iter()
            .map(|name| release(name, "1.0.0", &[]))
            .collect::<Result<_, _>>()?;
        Ok((root, inventory))
    };
    let (root, installed) = wide(MAX_LIBRARY_PACKAGES)?;
    assert_eq!(resolve(&root, &installed)?.len(), MAX_LIBRARY_PACKAGES);

    let (root, installed) = wide(MAX_LIBRARY_PACKAGES + 1)?;
    let error = failure(&root, &installed);
    assert!(
        matches!(error, Some(PackageError::TooManyDependencies { limit }) if limit == MAX_LIBRARY_PACKAGES),
        "{error:?}"
    );
    Ok(())
}

#[test]
fn a_package_with_no_dependencies_resolves_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let root = release("app", "1.0.0", &[])?;

    assert_eq!(resolve(&root, &ReleaseInventory::default())?, Vec::<String>::new());
    Ok(())
}

#[test]
fn a_catalogue_that_cannot_be_read_fails_resolution_naming_the_package() -> Result<(), Box<dyn std::error::Error>> {
    struct Unreadable;
    impl ReleaseCatalogue for Unreadable {
        fn versions(&self, name: &PackageName) -> Result<Vec<Version>, PackageError> {
            Err(PackageError::CatalogueUnreadable {
                name: name.to_string(),
                reason: "the disk is gone".to_owned(),
            })
        }

        fn dependencies(&self, _release: &ReleaseRef) -> Result<Vec<PackageDependency>, PackageError> {
            Ok(Vec::new())
        }
    }
    let root = release("app", "1.0.0", &[("acme-text", "^1")])?;

    let error = resolve_libraries(&LibraryResolution {
        root: &root,
        catalogue: &Unreadable,
    })
    .err();

    assert!(
        matches!(error, Some(PackageError::CatalogueUnreadable { .. })),
        "{error:?}"
    );
    Ok(())
}

#[test]
fn a_snapshot_holds_at_most_the_file_limit_and_refuses_the_next_before_it_is_read() {
    let mut budget = SnapshotBudget::default();

    for index in 0..MAX_SNAPSHOT_FILES {
        assert!(budget.charge(&format!("m{index}.rhai"), 1).is_ok(), "file {index}");
    }
    let refused = budget.charge("one-too-many.rhai", 1);

    assert!(
        matches!(refused, Err(PackageError::SnapshotOverLimit { ref path, .. }) if path == "one-too-many.rhai"),
        "{refused:?}"
    );
}

#[test]
fn a_snapshot_holds_at_most_the_byte_limit_and_refuses_the_file_that_passes_it() {
    let mut budget = SnapshotBudget::default();

    assert!(budget.charge("big.rhai", MAX_SNAPSHOT_BYTES - 1).is_ok());
    assert!(budget.charge("one-byte.rhai", 1).is_ok());
    let refused = budget.charge("over.rhai", 1);

    assert!(
        matches!(refused, Err(PackageError::SnapshotOverLimit { ref path, .. }) if path == "over.rhai"),
        "{refused:?}"
    );
}

#[test]
fn bytes_a_file_held_beyond_its_charged_size_count_against_the_byte_limit() {
    let mut budget = SnapshotBudget::default();
    assert!(budget.charge("a.rhai", MAX_SNAPSHOT_BYTES - 5).is_ok());

    assert!(budget.charge_growth("a.rhai", 5).is_ok());
    let refused = budget.charge_growth("a.rhai", 1);

    assert!(
        matches!(refused, Err(PackageError::SnapshotOverLimit { ref path, .. }) if path == "a.rhai"),
        "{refused:?}"
    );
}

#[test]
fn a_file_that_is_refused_does_not_use_up_the_budget() {
    let mut budget = SnapshotBudget::default();
    assert!(budget.charge("over.rhai", MAX_SNAPSHOT_BYTES + 1).is_err());

    assert!(budget.charge("fits.rhai", MAX_SNAPSHOT_BYTES).is_ok());
}

/// (package index, major, minor, [(dependency index, required major)]).
type Generated = (usize, u64, u64, Vec<(usize, u64)>);

fn arbitrary_inventory() -> impl Strategy<Value = Vec<Generated>> {
    // (package index, major, minor, [(dependency index, required major)]) over five names and three majors.
    proptest::collection::vec(
        (
            0usize..5,
            1u64..4,
            0u64..3,
            proptest::collection::vec((0usize..5, 1u64..4), 0..3),
        ),
        0..14,
    )
    // Two entries for one release would be a catalogue that disagrees with itself, so the first of each is kept.
    .prop_map(|generated| {
        let mut seen = BTreeSet::new();
        generated
            .into_iter()
            .filter(|(index, major, minor, _)| seen.insert((*index, *major, *minor)))
            .collect()
    })
}

const NAMES: [&str; 5] = ["pa", "pb", "pc", "pd", "pe"];

fn built(generated: &[Generated]) -> Result<ReleaseInventory, Box<dyn std::error::Error>> {
    generated
        .iter()
        .map(|(index, major, minor, dependencies)| {
            let requirements: Vec<(String, String)> = dependencies
                .iter()
                .map(|(dependency, required)| (NAMES[*dependency].to_owned(), format!("^{required}")))
                .collect();
            let borrowed: Vec<(&str, &str)> = requirements.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            release(NAMES[*index], &format!("{major}.{minor}.0"), &borrowed)
        })
        .collect()
}

proptest! {
    #[test]
    fn resolution_is_deterministic_and_chooses_the_newest_release_every_requirement_admits(
        generated in arbitrary_inventory(),
        roots in proptest::collection::vec((0usize..5, 1u64..4), 0..4),
    ) {
        let forward = built(&generated).map_err(|error| TestCaseError::fail(error.to_string()))?;
        let mut reversed_input = generated.clone();
        reversed_input.reverse();
        let backward = built(&reversed_input).map_err(|error| TestCaseError::fail(error.to_string()))?;
        let requirements: Vec<(&str, String)> = roots.iter().map(|(index, major)| (NAMES[*index], format!("^{major}"))).collect();
        let borrowed: Vec<(&str, &str)> = requirements.iter().map(|(a, b)| (*a, b.as_str())).collect();
        let root = release("app", "1.0.0", &borrowed).map_err(|error| TestCaseError::fail(error.to_string()))?;
        let request = |catalogue| LibraryResolution { root: &root, catalogue };

        let first = resolve_libraries(&request(&forward));
        let second = resolve_libraries(&request(&backward));
        prop_assert_eq!(first.as_ref().map_err(ToString::to_string), second.as_ref().map_err(ToString::to_string));

        if let Ok(resolved) = first {
            for chosen in resolved.releases() {
                let installed = forward.versions(&chosen.name).map_err(|error| TestCaseError::fail(error.to_string()))?;
                prop_assert!(installed.contains(&chosen.version));
                // Every requirement made by the job or by a chosen library on this name is satisfied.
                let requirers = std::iter::once(root.release.clone())
                    .chain(resolved.releases())
                    .map(|requirer| {
                        let dependencies = if requirer.name == root.release.name {
                            root.dependencies.clone()
                        } else {
                            forward.dependencies(&requirer).unwrap_or_default()
                        };
                        (requirer, dependencies)
                    })
                    .collect::<Vec<_>>();
                let applying: Vec<&VersionReq> = requirers
                    .iter()
                    .flat_map(|(_, dependencies)| dependencies.iter())
                    .filter(|dependency| dependency.name == chosen.name)
                    .map(|dependency| &dependency.requirement)
                    .collect();
                prop_assert!(applying.iter().all(|requirement| requirement.matches(&chosen.version)));
                prop_assert!(!installed
                    .iter()
                    .any(|version| *version > chosen.version && applying.iter().all(|requirement| requirement.matches(version))));
            }
        }
    }
}
