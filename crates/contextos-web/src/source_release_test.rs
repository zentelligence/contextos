use super::*;

use contextos_packages::{ReleaseFilePath, ReleaseRules, StoreRules};
use contextos_workflows::{LibraryManifest, WorkflowsFile};

use crate::package_release_manifest::{
    InterfaceEffect, InterfaceField, InterfaceKind, PackageRelease, WorkflowInterface,
};

type BoxError = Box<dyn std::error::Error>;

fn tested(entries: &[(&str, &str)]) -> Result<BTreeMap<PackageName, Version>, BoxError> {
    entries
        .iter()
        .map(|(name, version)| Ok((PackageName::try_from(*name)?, Version::parse(version)?)))
        .collect()
}

fn rewritten(text: &str, tested: &BTreeMap<PackageName, Version>) -> Result<String, BoxError> {
    Ok(ReleasePackageFile::try_from(PackageFileRewrite { text, tested })?.0)
}

#[test]
fn a_library_stating_no_requirement_requires_the_caret_of_the_version_it_was_tested_on() -> Result<(), BoxError> {
    let text = "workflows = [\"sync\"]\n\n[[libraries]]\nname = \"crm\"\n";
    let installed = rewritten(text, &tested(&[("crm", "1.5.2")])?)?;

    let file = WorkflowsFile::try_from(installed.as_str())?;
    let entries = file.libraries().entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name.as_str(), "crm");
    assert_eq!(entries[0].requirement.to_string(), "^1.5.2");
    Ok(())
}

#[test]
fn a_requirement_the_source_states_is_kept_as_written() -> Result<(), BoxError> {
    let text = "workflows = [\"sync\"]\n\n[[libraries]]\nname = \"crm\"\nrequirement = \">=1.2, <1.9\"\n";
    let installed = rewritten(text, &tested(&[])?)?;

    let file = WorkflowsFile::try_from(installed.as_str())?;
    assert_eq!(file.libraries().entries()[0].requirement.to_string(), ">=1.2, <1.9");
    Ok(())
}

#[test]
fn a_library_package_file_keeps_its_modules_exports_and_tools_when_a_requirement_is_filled_in() -> Result<(), BoxError>
{
    let text = "modules = [\"m.rhai\"]\nexports = [\"m.rhai\"]\n\n[[libraries]]\nname = \"base\"\n";
    let installed = rewritten(text, &tested(&[("base", "2.0.0")])?)?;

    let manifest = LibraryManifest::try_from(installed.as_str())?;
    assert_eq!(manifest.exports().entries().len(), 1);
    assert_eq!(manifest.modules().entries().len(), 1);
    assert_eq!(manifest.libraries().entries()[0].requirement.to_string(), "^2.0.0");
    Ok(())
}

#[test]
fn a_library_without_a_requirement_and_without_a_tested_version_is_refused() -> Result<(), BoxError> {
    let text = "workflows = [\"sync\"]\n\n[[libraries]]\nname = \"crm\"\n";
    let refused = rewritten(text, &tested(&[("other", "1.0.0")])?);

    let message = refused.err().map(|error| error.to_string()).unwrap_or_default();
    assert!(message.contains("crm"), "{message}");
    Ok(())
}

#[test]
fn a_package_file_that_is_not_toml_is_refused() -> Result<(), BoxError> {
    assert!(rewritten("workflows = [", &tested(&[])?).is_err());
    Ok(())
}

fn one_file() -> Result<ReleaseFiles, BoxError> {
    let mut files = BTreeMap::new();
    files.insert(
        ReleaseFilePath::try_from("workflows.toml")?,
        b"workflows = [\"sync\"]\n".to_vec(),
    );
    files.insert(
        ReleaseFilePath::try_from("sync/workflow.toml")?,
        b"name = \"Sync\"\nentry = \"sync.rhai\"\n".to_vec(),
    );
    files.insert(ReleaseFilePath::try_from("sync/sync.rhai")?, b"let x = 1;".to_vec());
    Ok(ReleaseFiles(files))
}

#[test]
fn a_manifest_names_the_release_and_lists_each_file_with_its_hash_so_the_store_reads_it_back() -> Result<(), BoxError> {
    let files = one_file()?;
    let digest = StoreRules.digest(&files);
    let name = PackageName::try_from("ops")?;
    let version = Version::new(1, 2, 3);

    let text = ManifestText::from(ManifestParts {
        name: &name,
        version: &version,
        digest: &digest,
        files: &files,
    })
    .0;

    let release = PackageRelease::try_from(text.as_str()).map_err(|rejection| rejection.message)?;
    let identity = &release.manifest().identity;
    assert_eq!(identity.name.as_str(), "ops");
    assert_eq!(identity.version, version);
    assert_eq!(identity.digest.as_str(), digest.as_str());
    let listed: Vec<String> = release
        .manifest()
        .files
        .iter()
        .map(|file| file.path.as_str().to_owned())
        .collect();
    assert_eq!(listed, ["sync/sync.rhai", "sync/workflow.toml", "workflows.toml"]);
    Ok(())
}

fn field(name: &str, required: bool) -> InterfaceField {
    InterfaceField {
        name: name.to_owned(),
        kind: InterfaceKind::String,
        required,
    }
}

fn interface(inputs: Vec<InterfaceField>) -> PackageInterface {
    PackageInterface {
        workflows: vec![WorkflowInterface {
            slug: "sync".to_owned(),
            inputs,
            outputs: Vec::new(),
            tools: Vec::new(),
            server_allow: None,
            server_deny: Vec::new(),
            tool_allow: Vec::new(),
            tool_deny: Vec::new(),
        }],
        exports: Vec::new(),
    }
}

fn next(highest: Option<(&Version, &PackageInterface)>, now: &PackageInterface) -> Version {
    Version::from(VersionRequest {
        highest: highest.map(|(version, interface)| RecordedHighest { version, interface }),
        interface: now,
    })
}

#[test]
fn the_first_release_of_a_package_is_one_point_zero_point_zero() {
    assert_eq!(next(None, &interface(Vec::new())), Version::new(1, 0, 0));
}

#[test]
fn an_unchanged_interface_steps_the_patch() {
    let before = interface(vec![field("limit", true)]);
    assert_eq!(
        next(Some((&Version::new(1, 4, 2), &before)), &before),
        Version::new(1, 4, 3)
    );
}

#[test]
fn a_new_optional_input_steps_the_minor_and_resets_the_patch() {
    let before = interface(vec![field("limit", true)]);
    let now = interface(vec![field("limit", true), field("note", false)]);
    assert_eq!(
        next(Some((&Version::new(1, 4, 2), &before)), &now),
        Version::new(1, 5, 0)
    );
}

#[test]
fn an_input_removed_steps_the_major_and_resets_minor_and_patch() {
    let before = interface(vec![field("limit", true), field("note", false)]);
    let now = interface(vec![field("limit", true)]);
    assert_eq!(
        next(Some((&Version::new(1, 4, 2), &before)), &now),
        Version::new(2, 0, 0)
    );
}

#[test]
fn a_new_required_input_steps_the_major() {
    let before = interface(vec![field("limit", true)]);
    let now = interface(vec![field("limit", true), field("scope", true)]);
    assert_eq!(
        next(Some((&Version::new(3, 0, 1), &before)), &now),
        Version::new(4, 0, 0)
    );
}

#[test]
fn an_entry_added_to_a_tool_list_steps_the_minor() {
    let before = interface(Vec::new());
    let mut now = interface(Vec::new());
    now.workflows[0].tool_deny.push(InterfaceEffect {
        server: "contextos".to_owned(),
        tool: "fs_delete_file".to_owned(),
    });
    assert_eq!(
        next(Some((&Version::new(2, 1, 0), &before)), &now),
        Version::new(2, 2, 0)
    );
}

#[test]
fn a_workflow_added_steps_the_minor_and_a_workflow_removed_steps_the_major() {
    let before = interface(Vec::new());
    let mut added = interface(Vec::new());
    added.workflows.push(WorkflowInterface {
        slug: "update".to_owned(),
        ..added.workflows[0].clone()
    });
    assert_eq!(
        next(Some((&Version::new(1, 0, 0), &before)), &added),
        Version::new(1, 1, 0)
    );
    assert_eq!(
        next(Some((&Version::new(1, 1, 0), &added)), &before),
        Version::new(2, 0, 0)
    );
}

#[test]
fn a_library_export_added_steps_the_minor_and_removed_steps_the_major() {
    let before = PackageInterface {
        workflows: Vec::new(),
        exports: vec!["a.rhai".to_owned()],
    };
    let added = PackageInterface {
        workflows: Vec::new(),
        exports: vec!["a.rhai".to_owned(), "b.rhai".to_owned()],
    };
    assert_eq!(
        next(Some((&Version::new(1, 0, 0), &before)), &added),
        Version::new(1, 1, 0)
    );
    assert_eq!(
        next(Some((&Version::new(1, 1, 0), &added)), &before),
        Version::new(2, 0, 0)
    );
}
