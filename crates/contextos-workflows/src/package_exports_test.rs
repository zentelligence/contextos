use super::*;
use crate::library_manifest::LibraryManifest;

fn library(body: &str) -> Result<LibraryManifest, PackageError> {
    LibraryManifest::try_from(body)
}

fn reason_of(body: &str) -> String {
    match library(body) {
        Err(PackageError::InvalidLibraryManifest { reason }) => reason,
        other => format!("not refused for its manifest: {other:?}"),
    }
}

fn exported(manifest: &LibraryManifest) -> Vec<&str> {
    manifest
        .exports()
        .entries()
        .iter()
        .map(WorkflowFilePath::as_str)
        .collect()
}

#[test]
fn a_library_exports_a_subset_of_its_modules() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = library("modules = [\"strings.rhai\", \"lib/private.rhai\"]\nexports = [\"strings.rhai\"]")?;

    assert_eq!(exported(&manifest), ["strings.rhai"]);
    assert_eq!(manifest.modules().entries().len(), 2);
    Ok(())
}

#[test]
fn a_library_with_an_empty_exports_list_is_refused() {
    let reason = reason_of("modules = [\"strings.rhai\"]\nexports = []");

    assert!(reason.contains("`exports` is empty"), "{reason}");
}

#[test]
fn an_export_that_is_not_a_declared_module_fails_naming_the_entry() {
    let reason = reason_of("modules = [\"strings.rhai\"]\nexports = [\"other.rhai\"]");

    assert!(reason.contains("other.rhai"), "{reason}");
    assert!(reason.contains("is not in `modules`"), "{reason}");
}

#[test]
fn an_export_listed_twice_fails_naming_the_entry() {
    let reason = reason_of("modules = [\"strings.rhai\"]\nexports = [\"strings.rhai\", \"strings.rhai\"]");

    assert!(reason.contains("strings.rhai"), "{reason}");
    assert!(reason.contains("twice"), "{reason}");
}

#[test]
fn an_export_that_is_not_a_plain_relative_rhai_path_fails() {
    for exports in ["[\"../strings.rhai\"]", "[\"strings.md\"]", "[\"contextos/text.rhai\"]"] {
        let reason = reason_of(&format!("modules = [\"strings.rhai\"]\nexports = {exports}"));

        assert!(reason.contains("exports entry"), "{exports}: {reason}");
    }
}

#[test]
fn a_library_may_name_other_libraries_with_a_requirement_each() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = library(
        "modules = [\"strings.rhai\"]\nexports = [\"strings.rhai\"]\n[[libraries]]\nname = \"acme-core\"\nrequirement = \"^1\"",
    )?;

    assert_eq!(manifest.libraries().entries().len(), 1);
    assert_eq!(manifest.libraries().entries()[0].name, "acme-core");
    Ok(())
}

#[test]
fn a_library_named_in_two_entries_is_refused() {
    let reason = reason_of(
        "modules = [\"s.rhai\"]\nexports = [\"s.rhai\"]\n[[libraries]]\nname = \"core\"\nrequirement = \"1\"\n[[libraries]]\nname = \"core\"\nrequirement = \"2\"",
    );

    assert!(reason.contains("twice"), "{reason}");
}

#[test]
fn a_library_manifest_cannot_declare_templates_or_workflows() {
    for extra in ["templates = [\"data.json\"]\n", "workflows = [\"sync\"]\n"] {
        let reason = reason_of(&format!("modules = [\"s.rhai\"]\nexports = [\"s.rhai\"]\n{extra}"));

        assert!(reason.contains("unknown field"), "{extra}: {reason}");
    }
}
