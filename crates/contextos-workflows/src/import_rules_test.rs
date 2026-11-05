use super::*;

use crate::library_manifest::LibraryManifest;
use crate::workflows_file::WorkflowsFile;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn package(
    name: &str,
    libraries: &[&str],
    exports: &[&str],
    modules: &[&str],
) -> Result<(PackageName, PackageKind), PackageError> {
    let list = |entries: &[&str]| {
        let quoted: Vec<String> = entries.iter().map(|entry| format!("\"{entry}\"")).collect();
        format!("[{}]", quoted.join(", "))
    };
    let libraries: String = libraries
        .iter()
        .map(|library| format!("[[libraries]]\nname = \"{library}\"\nrequirement = \"^1\"\n"))
        .collect::<Vec<_>>()
        .concat();
    // A package that exports nothing is a workflow package; one that exports is a library.
    let kind = if exports.is_empty() {
        PackageKind::Workflows(WorkflowsFile::try_from(
            format!("workflows = [\"run\"]\nmodules = {}\n{libraries}", list(modules)).as_str(),
        )?)
    } else {
        PackageKind::Library(LibraryManifest::try_from(
            format!("modules = {}\nexports = {}\n{libraries}", list(modules), list(exports)).as_str(),
        )?)
    };
    Ok((PackageName::try_from(name)?, kind))
}

/// `app` depends on `acme-text`, which depends on `acme-core`; `acme-text` exports `strings` and keeps `private`.
fn rules() -> Result<ImportRules, PackageError> {
    let mut rules = ImportRules::default();
    let (name, kind) = package("app", &["acme-text"], &[], &["own.rhai"])?;
    rules.add_package(&name, &kind);
    let (name, kind) = package(
        "acme-text",
        &["acme-core"],
        &["strings.rhai", "lib/deep.rhai"],
        &["strings.rhai", "lib/deep.rhai", "private.rhai"],
    )?;
    rules.add_package(&name, &kind);
    let (name, kind) = package("acme-core", &[], &["core.rhai"], &["core.rhai"])?;
    rules.add_package(&name, &kind);
    Ok(rules)
}

fn app() -> Result<ModuleOwner, PackageError> {
    Ok(ModuleOwner::Package(PackageName::try_from("app")?))
}

fn key(package: &str, name: &str) -> Result<ImportTarget, PackageError> {
    Ok(ImportTarget::Module(ModuleKey {
        owner: ModuleOwner::Package(PackageName::try_from(package)?),
        name: name.to_owned(),
    }))
}

#[test]
fn a_name_without_a_colon_is_a_module_of_the_importing_files_own_owner() -> TestResult {
    let rules = rules()?;

    assert_eq!(rules.target(&app()?, "own")?, key("app", "own")?);
    assert_eq!(
        rules.target(&ModuleOwner::Workflow, "lib/dates")?,
        ImportTarget::Module(ModuleKey {
            owner: ModuleOwner::Workflow,
            name: "lib/dates".to_owned()
        })
    );
    Ok(())
}

#[test]
fn an_exported_module_of_a_direct_dependency_is_imported_by_package_and_module() -> TestResult {
    let rules = rules()?;

    assert_eq!(
        rules.target(&app()?, "acme-text:strings")?,
        key("acme-text", "strings")?
    );
    assert_eq!(
        rules.target(&app()?, "acme-text:lib/deep")?,
        key("acme-text", "lib/deep")?
    );
    Ok(())
}

#[test]
fn a_library_imports_the_exports_of_its_own_dependencies_not_the_callers() -> TestResult {
    let rules = rules()?;
    let library = ModuleOwner::Package(PackageName::try_from("acme-text")?);

    assert_eq!(rules.target(&library, "acme-core:core")?, key("acme-core", "core")?);
    assert_eq!(rules.target(&library, "private")?, key("acme-text", "private")?);
    Ok(())
}

#[test]
fn a_module_a_package_keeps_private_is_not_imported_from_outside() -> TestResult {
    let refused = rules()?.target(&app()?, "acme-text:private");

    assert!(
        matches!(refused, Err(PackageError::ImportNotExported { ref module, .. }) if module == "private"),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn a_dependency_of_a_dependency_is_not_imported_unless_the_package_names_it_itself() -> TestResult {
    let refused = rules()?.target(&app()?, "acme-core:core");

    assert!(
        matches!(refused, Err(PackageError::ImportNotDependency { ref package }) if package.as_str() == "acme-core"),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn a_package_does_not_import_its_own_modules_qualified_with_its_own_name() -> TestResult {
    let refused = rules()?.target(&app()?, "app:own");

    assert!(
        matches!(refused, Err(PackageError::ImportOfOwnPackage { .. })),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn a_vault_workflow_never_imports_a_package() -> TestResult {
    let refused = rules()?.target(&ModuleOwner::Workflow, "acme-text:strings");

    assert!(
        matches!(refused, Err(PackageError::ImportNotDependency { .. })),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn the_built_in_library_is_imported_by_every_owner_and_an_unknown_built_in_names_nothing() -> TestResult {
    let rules = rules()?;

    for owner in [ModuleOwner::Workflow, app()?] {
        assert_eq!(rules.target(&owner, "contextos:text")?, ImportTarget::BuiltIn);
        assert_eq!(rules.target(&owner, "contextos:missing")?, ImportTarget::Nothing);
    }
    Ok(())
}

#[test]
fn a_name_that_is_not_a_package_and_a_module_names_nothing() -> TestResult {
    let rules = rules()?;

    for name in [
        "Acme-Text:strings",
        "acme-text:",
        "acme-text:a:b",
        ":strings",
        "C:/windows/x",
        "../x:y",
    ] {
        assert_eq!(rules.target(&app()?, name)?, ImportTarget::Nothing, "{name}");
    }
    Ok(())
}
