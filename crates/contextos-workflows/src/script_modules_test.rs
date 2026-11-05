use super::*;
use crate::library_manifest::LibraryManifest;
use crate::package_contents::PackageKind;
use crate::script_engine::script_engine;
use crate::test_sources::{MemorySource, script_with};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn workflow_key(name: &str) -> ModuleKey {
    ModuleKey {
        owner: ModuleOwner::Workflow,
        name: name.to_owned(),
    }
}

fn declared(paths: &[&str]) -> Result<DeclaredModules, DeclaredModulesError> {
    DeclaredModules::try_from(paths.iter().map(|path| (*path).to_owned()).collect::<Vec<_>>())
}

#[test]
fn a_module_is_a_rhai_file_named_by_a_plain_relative_path() {
    assert!(declared(&["dates.rhai", "lib/text/format.rhai"]).is_ok());

    for (path, reason) in [
        ("lib/dates", "is not a `.rhai` file"),
        ("lib/.rhai", "is not a `.rhai` file"),
        ("lib/dates.txt", "is not a `.rhai` file"),
        ("../outside.rhai", "is not a plain relative path"),
        ("/etc/outside.rhai", "is not a plain relative path"),
        ("C:/outside.rhai", "is not a plain relative path"),
        ("lib\\dates.rhai", "is not a plain relative path"),
        ("lib//dates.rhai", "is not a plain relative path"),
        ("contextos/text.rhai", "built-in library"),
    ] {
        let refused = declared(&[path]);

        assert!(
            matches!(refused, Err(ref error) if error.path == path && error.to_string().contains(reason)),
            "{path}: {refused:?}"
        );
    }
}

#[test]
fn a_module_declared_twice_is_refused() {
    let refused = declared(&["lib/dates.rhai", "lib/dates.rhai"]);

    assert!(
        matches!(refused, Err(ref error) if error.reason.contains("twice")),
        "{refused:?}"
    );
}

#[test]
fn a_workflow_declares_at_most_the_module_limit() {
    let paths: Vec<String> = (0..=MAX_MODULES).map(|n| format!("m{n}.rhai")).collect();

    assert!(DeclaredModules::try_from(paths[..MAX_MODULES].to_vec()).is_ok());
    assert!(DeclaredModules::try_from(paths).is_err());
}

#[tokio::test]
async fn loading_reads_each_declared_module_and_names_it_by_its_path_without_the_extension() -> TestResult {
    let script = script_with("dates::today()", &[("lib/dates.rhai", "fn today() { 1 }")]).await?;

    let module = script
        .module(&workflow_key("lib/dates"))
        .ok_or("the module was not loaded")?;
    assert_eq!(module.path().as_str(), "lib/dates.rhai");
    assert_eq!(module.text(), "fn today() { 1 }");
    assert!(
        script.module(&workflow_key("lib/dates.rhai")).is_none(),
        "imported without its extension"
    );
    Ok(())
}

#[tokio::test]
async fn a_declared_module_that_is_missing_cannot_be_loaded() -> TestResult {
    let declared = declared(&["lib/dates.rhai"])?;

    let loaded = WorkflowScript::load(String::new(), &declared, &MemorySource::of(&[])).await;

    assert!(
        matches!(loaded, Err(ref error) if error.path.as_str() == "lib/dates.rhai" && error.failure.kind == WorkflowFailureKind::FileNotFound),
        "{loaded:?}"
    );
    Ok(())
}

#[test]
fn reading_without_waiting_takes_the_same_modules_and_refuses_one_over_the_size_limit() -> TestResult {
    let both = declared(&["lib/dates.rhai", "big.rhai"])?;
    let over_the_limit = usize::try_from(MAX_WORKFLOW_FILE_BYTES)? + 1;
    let file = |content: String| WorkflowFile {
        content,
        content_hash: contextos_core::ContentHash::from([0; 32]),
    };

    let read = WorkflowScript::read(String::new(), &both, |path| {
        Ok(file(match path.as_str() {
            "big.rhai" => "x".repeat(over_the_limit),
            _ => "fn today() { 1 }".to_owned(),
        }))
    });

    assert!(
        matches!(read, Err(ref error) if error.path.as_str() == "big.rhai" && error.failure.kind == WorkflowFailureKind::FileTooLarge),
        "{read:?}"
    );
    let small = declared(&["lib/dates.rhai"])?;
    let script = WorkflowScript::read(String::new(), &small, |_| Ok(file("fn today() { 1 }".to_owned())))?;
    assert_eq!(
        script.module(&workflow_key("lib/dates")).map(ModuleSource::text),
        Some("fn today() { 1 }")
    );
    Ok(())
}

/// Evaluates `script`'s entry with its modules resolvable, as a run does, without any host function.
fn evaluate(script: WorkflowScript) -> Result<rhai::Dynamic, Box<EvalAltResult>> {
    let mut engine = script_engine();
    let entry = script.entry().to_owned();
    engine.set_module_resolver(DeclaredModuleResolver::new(script));
    engine.eval::<rhai::Dynamic>(&entry)
}

#[tokio::test]
async fn a_script_calls_the_functions_of_a_module_it_imports_and_they_call_each_other() -> TestResult {
    let script = script_with(
        "import \"lib/maths\" as maths;\nmaths::twice_plus_one(20)",
        &[(
            "lib/maths.rhai",
            "fn twice(n) { n * 2 }\nfn twice_plus_one(n) { twice(n) + 1 }",
        )],
    )
    .await?;

    assert_eq!(evaluate(script)?.as_int()?, 41);
    Ok(())
}

#[tokio::test]
async fn a_module_imports_another_by_its_name_from_the_workflow_root_and_a_built_in_module() -> TestResult {
    let script = script_with(
        "import \"lib/greeting\" as greeting;\ngreeting::hello(\"  world  \")",
        &[
            (
                "lib/greeting.rhai",
                "import \"lib/words/trim\" as trim;\nfn hello(name) { \"hello \" + trim::tidy(name) }",
            ),
            (
                "lib/words/trim.rhai",
                "import \"contextos:text\" as text;\nfn tidy(value) { text::trimmed(value) }",
            ),
        ],
    )
    .await?;

    assert_eq!(evaluate(script)?.into_string()?, "hello world");
    Ok(())
}

#[tokio::test]
async fn an_import_of_anything_not_declared_or_built_in_is_refused_without_reading_a_file() -> TestResult {
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("outside.rhai"), "fn secret() { 42 }\n")?;
    let outside = dir.path().join("outside").to_string_lossy().replace('\\', "/");

    for name in [outside.as_str(), "outside", "lib/undeclared", "contextos:missing"] {
        let script = script_with(&format!("let name = \"{name}\"; import name as m; m::secret()"), &[]).await?;

        let refused = evaluate(script);

        assert!(
            matches!(refused, Err(ref error) if error.to_string().contains("names no module")),
            "{name}: {refused:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_module_holding_a_statement_is_refused_when_imported_without_running_it() -> TestResult {
    let script = script_with(
        "import \"lib/noisy\" as noisy;",
        &[("lib/noisy.rhai", "fn ok() { 1 }\nthrow \"the module's code ran\";")],
    )
    .await?;

    let refused = evaluate(script);

    let Err(error) = refused else {
        return Err("a module with a statement was imported".into());
    };
    assert!(error.to_string().contains(MODULE_HOLDS_CODE), "{error}");
    assert!(!error.to_string().contains("the module's code ran"), "{error}");
    Ok(())
}

#[tokio::test]
async fn modules_that_import_each_other_are_refused_when_imported() -> TestResult {
    let script = script_with(
        "import \"a\" as a;",
        &[
            ("a.rhai", "import \"b\" as b;\nfn f() { 1 }"),
            ("b.rhai", "import \"a\" as a;\nfn g() { 2 }"),
        ],
    )
    .await?;

    let refused = evaluate(script);

    assert!(
        matches!(refused, Err(ref error) if error.to_string().contains("import each other")),
        "{refused:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_module_imported_twice_is_built_once_and_shared() -> TestResult {
    let script = script_with(
        "import \"lib/one\" as first;\nimport \"lib/one\" as second;\nfirst::value() + second::value()",
        &[("lib/one.rhai", "fn value() { 21 }")],
    )
    .await?;
    let resolver = DeclaredModuleResolver::new(script.clone());
    let engine = script_engine();

    let first = resolver.resolve(&engine, None, "lib/one", Position::NONE)?;
    let second = resolver.resolve(&engine, None, "lib/one", Position::NONE)?;

    assert!(Shared::ptr_eq(&first, &second), "the module was built twice");
    assert_eq!(evaluate(script)?.as_int()?, 42);
    Ok(())
}

#[tokio::test]
async fn a_module_importing_inside_a_function_is_refused_when_imported_without_running_it() -> TestResult {
    let script = script_with(
        "import \"lib/helpers\" as helpers;\nhelpers::tidy(\" a \")",
        &[(
            "lib/helpers.rhai",
            "fn tidy(value) { import \"contextos:text\" as t; t::trimmed(value) }",
        )],
    )
    .await?;

    let refused = evaluate(script);

    assert!(
        matches!(refused, Err(ref error) if error.to_string().contains("top level")),
        "{refused:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_module_shares_exported_constants_whose_values_are_written_out() -> TestResult {
    let script = script_with(
        "import \"scripts/layout\" as layout;\nlayout::FOLDERS.len() * 10 + layout::DOMAINS.len() + layout::count()",
        &[(
            "scripts/layout.rhai",
            "export const FOLDERS = [\"inbox\", \"memory\", \"memory/decisions\"];\n\
             export const DOMAINS = #{ \"chief-of-staff\": \"owner\", librarian: \"knowledge\" };\n\
             export const TEAM = \"general\";\n\
             fn count() { global::FOLDERS.len() }",
        )],
    )
    .await?;

    assert_eq!(evaluate(script)?.as_int()?, 35);
    Ok(())
}

#[tokio::test]
async fn a_module_constant_that_is_not_exported_or_not_written_out_is_refused_without_running_it() -> TestResult {
    for statement in [
        "const SECRET = 1;",
        "export let COUNTER = 1;",
        "export const VALUE = make();",
        "export const LIST = [1, make()];",
        "export const MAP = #{ a: make() };",
        "export const TEXT = `made ${make()}`;",
    ] {
        let script = script_with(
            "import \"lib/data\" as data;",
            &[(
                "lib/data.rhai",
                &format!("fn make() {{ throw \"the module's code ran\"; }}\n{statement}"),
            )],
        )
        .await?;

        let refused = evaluate(script);

        let Err(error) = refused else {
            return Err(format!("{statement} was accepted").into());
        };
        assert!(error.to_string().contains(MODULE_HOLDS_CODE), "{statement}: {error}");
        assert!(
            !error.to_string().contains("the module's code ran"),
            "{statement}: {error}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_module_function_reads_its_own_module_constant_even_when_the_caller_has_one_of_the_same_name() -> TestResult {
    let script = script_with(
        "const FOLDERS = [\"the caller's\"];\nimport \"scripts/layout\" as layout;\nlayout::count()",
        &[(
            "scripts/layout.rhai",
            "export const FOLDERS = [\"a\", \"b\", \"c\"];\nfn count() { global::FOLDERS.len() }",
        )],
    )
    .await?;

    assert_eq!(evaluate(script)?.as_int()?, 3);
    Ok(())
}

fn missing_file(path: &WorkflowFilePath) -> Result<WorkflowFile, WorkflowFileFailure> {
    Err(WorkflowFileFailure::new(
        WorkflowFailureKind::FileNotFound,
        format!("{path} is missing"),
    ))
}

#[test]
fn an_unreadable_module_of_a_package_names_its_owner_as_package_and_path() -> TestResult {
    let package = PackageToRead {
        name: PackageName::try_from("acme-text")?,
        kind: PackageKind::Library(LibraryManifest::try_from(
            "modules = [\"lib/strings.rhai\"]\nexports = [\"lib/strings.rhai\"]\n",
        )?),
    };

    let unreadable = PackageCode::read(package, missing_file)
        .err()
        .ok_or("the package should not have been read")?;

    assert_eq!(unreadable.artefact(), "acme-text:lib/strings.rhai");
    assert!(
        unreadable.to_string().contains("acme-text:lib/strings.rhai"),
        "{unreadable}"
    );
    Ok(())
}

#[test]
fn an_unreadable_module_of_a_vault_workflow_names_its_path_alone() -> TestResult {
    let unreadable = WorkflowScript::read("1".to_owned(), &declared(&["util.rhai"])?, missing_file)
        .err()
        .ok_or("the workflow should not have been read")?;

    assert_eq!(unreadable.artefact(), "util.rhai");
    Ok(())
}
