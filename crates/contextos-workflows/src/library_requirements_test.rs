use super::*;
use crate::package::DependencyRaw;
use semver::Version;

fn raw(name: &str, requirement: Option<&str>) -> DependencyRaw {
    DependencyRaw {
        name: name.to_owned(),
        requirement: requirement.map(str::to_owned),
    }
}

#[test]
fn a_source_library_with_no_requirement_is_unstated_and_one_with_a_requirement_is_an_entry() -> Result<(), PackageError>
{
    let requirements = LibraryRequirements::try_from((
        vec![raw("crm", Some("^2.1")), raw("text", None)],
        PackageFileOrigin::Source,
    ))?;

    let stated: Vec<String> = requirements
        .entries()
        .iter()
        .map(|dependency| dependency.name.to_string())
        .collect();
    let unstated: Vec<String> = requirements.unstated().iter().map(ToString::to_string).collect();
    assert_eq!(stated, ["crm"]);
    assert_eq!(unstated, ["text"]);
    assert_eq!(requirements.names().len(), 2);
    assert!(requirements.entries()[0].requirement.matches(&Version::new(2, 5, 0)));
    assert!(!requirements.entries()[0].requirement.matches(&Version::new(3, 0, 0)));
    Ok(())
}
