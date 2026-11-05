//! Scripts whose files belong to several owners: a package's workflow and the library packages it depends on.

use super::*;
use crate::library_manifest::LibraryManifest;
use crate::package_contents::PackageKind;
use crate::script_engine::script_engine;
use crate::validation::check_script;
use crate::workflows_file::WorkflowsFile;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// One package: its dependencies, the modules it exports, and its files as `(path, text)`. Every file is a declared
/// module. A package that exports nothing declares a workflow, since a package must declare one or the other.
fn package(
    name: &str,
    dependencies: &[&str],
    exports: &[&str],
    files: &[(&str, &str)],
) -> Result<(PackageName, PackageCode), Box<dyn std::error::Error>> {
    let list = |entries: Vec<&str>| {
        let quoted: Vec<String> = entries.iter().map(|entry| format!("\"{entry}\"")).collect();
        format!("[{}]", quoted.join(", "))
    };
    let libraries: String = dependencies
        .iter()
        .map(|library| format!("[[libraries]]\nname = \"{library}\"\nrequirement = \"^1\"\n"))
        .collect::<Vec<_>>()
        .concat();
    let modules = list(files.iter().map(|(path, _)| *path).collect());
    let kind = if exports.is_empty() {
        PackageKind::Workflows(WorkflowsFile::try_from(
            format!("workflows = [\"run\"]\nmodules = {modules}\n{libraries}").as_str(),
        )?)
    } else {
        PackageKind::Library(LibraryManifest::try_from(
            format!("modules = {modules}\nexports = {}\n{libraries}", list(exports.to_vec())).as_str(),
        )?)
    };
    let package_name = PackageName::try_from(name)?;
    let to_read = PackageToRead {
        name: package_name.clone(),
        kind,
    };
    let code = PackageCode::read(to_read, |path| {
        files
            .iter()
            .find(|(candidate, _)| *candidate == path.as_str())
            .map(|(_, text)| WorkflowFile {
                content: (*text).to_owned(),
                content_hash: contextos_core::ContentHash::from([0; 32]),
            })
            .ok_or_else(|| WorkflowFileFailure::new(WorkflowFailureKind::FileNotFound, format!("{path} is missing")))
    })?;
    Ok((package_name, code))
}

/// The script of `entry`, which is `root`'s workflow's, over `packages`.
fn script(
    entry: &str,
    root: &str,
    packages: Vec<(PackageName, PackageCode)>,
) -> Result<WorkflowScript, Box<dyn std::error::Error>> {
    Ok(WorkflowScript::from(ScriptParts {
        entry: entry.to_owned(),
        package: Some(PackageName::try_from(root)?),
        packages: packages.into_iter().collect(),
    }))
}

fn evaluate(script: WorkflowScript) -> Result<rhai::Dynamic, Box<EvalAltResult>> {
    let mut engine = script_engine();
    let entry = script.entry().to_owned();
    engine.set_module_resolver(DeclaredModuleResolver::new(script));
    engine.eval::<rhai::Dynamic>(&entry)
}

/// `app` depends on `acme-text`, which depends on `acme-core`. `app` and `acme-text` each have a `util` module.
fn three_packages(entry: &str) -> Result<WorkflowScript, Box<dyn std::error::Error>> {
    script(
        entry,
        "app",
        vec![
            package("app", &["acme-text"], &[], &[("util.rhai", "fn who() { \"app\" }")])?,
            package(
                "acme-text",
                &["acme-core"],
                &["strings.rhai", "util.rhai"],
                &[
                    (
                        "strings.rhai",
                        "import \"util\" as util;\nimport \"acme-core:core\" as core;\nfn shout() { util::who() + core::mark() }",
                    ),
                    ("util.rhai", "fn who() { \"text\" }"),
                    ("private.rhai", "fn hidden() { 1 }"),
                ],
            )?,
            package(
                "acme-core",
                &[],
                &["core.rhai"],
                &[("core.rhai", "fn mark() { \"!\" }")],
            )?,
        ],
    )
}

#[test]
fn a_workflow_and_a_library_that_each_have_a_module_of_the_same_name_run_their_own() -> TestResult {
    let script = three_packages(
        "import \"util\" as mine;\nimport \"acme-text:util\" as theirs;\nmine::who() + \"/\" + theirs::who()",
    )?;

    assert_eq!(evaluate(script)?.into_string()?, "app/text");
    Ok(())
}

#[test]
fn a_library_module_resolves_its_own_imports_against_the_library_not_the_workflow_that_called_it() -> TestResult {
    // `strings` imports `util` and `acme-core:core`: its own package's module, though `app` has a `util` of its own, and
    // a dependency of `acme-text` that `app` does not name.
    let script = three_packages("import \"acme-text:strings\" as strings;\nstrings::shout()")?;

    assert_eq!(evaluate(script)?.into_string()?, "text!");
    Ok(())
}

#[test]
fn a_module_a_library_does_not_export_is_refused_when_imported_and_when_validated() -> TestResult {
    let entry = "let first = 1;\nimport \"acme-text:private\" as p;\np::hidden()";
    let run = evaluate(three_packages(entry)?);
    let checked = check_script(&three_packages(entry)?);

    assert!(
        matches!(run, Err(ref error) if error.to_string().contains("does not export")),
        "{run:?}"
    );
    let [error] = checked.as_slice() else {
        return Err(format!("{checked:?}").into());
    };
    assert!(error.message.contains("does not export"), "{}", error.message);
    assert_eq!(error.line, Some(2));
    Ok(())
}

#[test]
fn a_dependency_of_a_dependency_is_refused_when_imported_and_when_validated() -> TestResult {
    let entry = "import \"acme-core:core\" as core;\ncore::mark()";
    let run = evaluate(three_packages(entry)?);
    let checked = check_script(&three_packages(entry)?);

    assert!(
        matches!(run, Err(ref error) if error.to_string().contains("not one of the importing package's own `libraries`")),
        "{run:?}"
    );
    assert_eq!(checked.len(), 1, "{checked:?}");
    Ok(())
}

#[test]
fn a_package_importing_its_own_module_by_its_own_name_is_refused_when_imported_and_when_validated() -> TestResult {
    let entry = "import \"app:util\" as u;\nu::who()";
    let run = evaluate(three_packages(entry)?);
    let checked = check_script(&three_packages(entry)?);

    assert!(
        matches!(run, Err(ref error) if error.to_string().contains("names the importing package itself")),
        "{run:?}"
    );
    assert_eq!(checked.len(), 1, "{checked:?}");
    Ok(())
}

#[test]
fn a_package_that_names_no_library_cannot_import_one_that_is_installed() -> TestResult {
    let script = script(
        "import \"acme-text:util\" as u;\nu::who()",
        "app",
        vec![
            package("app", &[], &[], &[])?,
            package(
                "acme-text",
                &[],
                &["util.rhai"],
                &[("util.rhai", "fn who() { \"text\" }")],
            )?,
        ],
    )?;

    assert!(evaluate(script.clone()).is_err());
    assert_eq!(check_script(&script).len(), 1);
    Ok(())
}

#[test]
fn a_valid_script_over_several_owners_passes_validation() -> TestResult {
    let script = three_packages(
        "import \"util\" as mine;\nimport \"acme-text:strings\" as strings;\nimport \"contextos:text\" as text;\nstrings::shout()",
    )?;

    assert_eq!(check_script(&script), []);
    Ok(())
}

#[test]
fn a_library_module_that_imports_what_its_package_may_not_is_reported_in_that_module() -> TestResult {
    let script = script(
        "import \"acme-text:strings\" as s;",
        "app",
        vec![
            package("app", &["acme-text"], &[], &[])?,
            package(
                "acme-text",
                &[],
                &["strings.rhai"],
                &[("strings.rhai", "import \"acme-core:core\" as core;\nfn f() { 1 }")],
            )?,
            package("acme-core", &[], &["core.rhai"], &[("core.rhai", "fn mark() { 1 }")])?,
        ],
    )?;

    let [error] = check_script(&script)
        .try_into()
        .map_err(|errors| format!("{errors:?}"))?;

    assert_eq!(
        error.module.as_ref().map(WorkflowFilePath::as_str),
        Some("strings.rhai")
    );
    assert!(error.message.contains("acme-core"), "{}", error.message);
    Ok(())
}

#[test]
fn modules_of_different_packages_that_import_each_other_in_a_loop_fail_validation() -> TestResult {
    let script = script(
        "import \"left:a\" as a;",
        "app",
        vec![
            package("app", &["left"], &[], &[])?,
            package(
                "left",
                &["right"],
                &["a.rhai"],
                &[("a.rhai", "import \"right:b\" as b;\nfn f() { 1 }")],
            )?,
            package(
                "right",
                &["left"],
                &["b.rhai"],
                &[("b.rhai", "import \"left:a\" as a;\nfn g() { 1 }")],
            )?,
        ],
    )?;

    let errors = check_script(&script);

    assert!(
        errors.iter().any(|error| error.message.contains("import each other")),
        "{errors:?}"
    );
    Ok(())
}

#[test]
fn a_problem_in_a_library_module_is_reported_against_that_library_and_file_with_its_line() -> TestResult {
    let script = script(
        "()",
        "app",
        vec![
            package("app", &["crm"], &[], &[])?,
            package(
                "crm",
                &[],
                &["client.rhai"],
                &[("client.rhai", "fn ok() { 1 }\n\nlet ran = true;\n")],
            )?,
        ],
    )?;

    let [error] = check_script(&script)
        .try_into()
        .map_err(|errors| format!("{errors:?}"))?;

    assert_eq!(error.owner, Some(ModuleOwner::Package(PackageName::try_from("crm")?)));
    assert_eq!(error.module.as_ref().map(WorkflowFilePath::as_str), Some("client.rhai"));
    assert_eq!(error.line, Some(3));
    assert!(error.message.contains("only functions"), "{}", error.message);
    Ok(())
}

#[test]
fn an_import_inside_a_function_of_a_library_module_is_refused() -> TestResult {
    let script = script(
        "()",
        "app",
        vec![
            package("app", &["crm"], &[], &[])?,
            package(
                "crm",
                &[],
                &["client.rhai"],
                &[("client.rhai", "fn late() { import \"contextos:text\" as text; 1 }")],
            )?,
        ],
    )?;

    let checked = check_script(&script);

    assert!(
        checked
            .iter()
            .any(|error| error.message.contains("only at its top level")),
        "{checked:?}"
    );
    Ok(())
}

#[test]
fn modules_of_one_library_that_import_each_other_are_reported_as_a_module_loop() -> TestResult {
    let script = script(
        "()",
        "app",
        vec![
            package("app", &["crm"], &[], &[])?,
            package(
                "crm",
                &[],
                &["a.rhai"],
                &[
                    ("a.rhai", "import \"b\" as b;\nfn a() { 1 }"),
                    ("b.rhai", "import \"a\" as a;\nfn b() { 1 }"),
                ],
            )?,
        ],
    )?;

    let checked = check_script(&script);

    assert!(
        checked.iter().any(|error| error.message.contains("import each other")),
        "{checked:?}"
    );
    Ok(())
}

#[test]
fn a_package_s_modules_are_keyed_by_package_and_name_and_listed_in_that_order() -> TestResult {
    let script = three_packages("")?;

    let keys: Vec<String> = script.modules().map(|module| module.key().to_string()).collect();

    assert_eq!(
        keys,
        [
            "acme-core:core",
            "acme-text:private",
            "acme-text:strings",
            "acme-text:util",
            "app:util"
        ]
    );
    Ok(())
}

mod package_modules {
    use super::*;
    use crate::workflow_files::WorkflowFile;
    use contextos_core::ContentHash;

    fn file(text: &str) -> WorkflowFile {
        WorkflowFile {
            content: text.to_owned(),
            content_hash: ContentHash::from([0; 32]),
        }
    }

    fn package_declaring(modules: &str) -> Result<PackageToRead, Box<dyn std::error::Error>> {
        Ok(PackageToRead {
            name: PackageName::try_from("sync")?,
            kind: PackageKind::Workflows(WorkflowsFile::try_from(
                format!("workflows = [\"one\", \"two\"]\nmodules = {modules}\n").as_str(),
            )?),
        })
    }

    #[test]
    fn every_module_the_package_declares_is_read_from_the_package_root_and_named_by_its_path() -> TestResult {
        let code = PackageCode::read(
            package_declaring("[\"modules/shared.rhai\", \"helpers.rhai\"]")?,
            |path| match path.as_str() {
                "modules/shared.rhai" => Ok(file("fn shared() { 1 }")),
                "helpers.rhai" => Ok(file("fn helper() { 2 }")),
                other => Err(WorkflowFileFailure::new(
                    WorkflowFailureKind::FileNotFound,
                    other.to_owned(),
                )),
            },
        )?;

        let script = WorkflowScript::from(ScriptParts {
            entry: "()".to_owned(),
            package: Some(code.name().clone()),
            packages: std::iter::once((code.name().clone(), code)).collect(),
        });
        let names: Vec<String> = script.modules().map(|module| module.import_name().to_owned()).collect();
        assert_eq!(names, ["helpers", "modules/shared"]);
        Ok(())
    }

    #[test]
    fn a_module_the_release_does_not_hold_is_refused_naming_its_package_and_path() -> TestResult {
        let refused = PackageCode::read(package_declaring("[\"missing.rhai\"]")?, |path| {
            Err(WorkflowFileFailure::new(
                WorkflowFailureKind::FileNotFound,
                path.to_string(),
            ))
        });

        let Err(unreadable) = refused else {
            return Err("a missing module should be refused".into());
        };
        assert_eq!(unreadable.artefact(), "sync:missing.rhai");
        Ok(())
    }
}

#[tokio::test]
async fn a_library_read_from_source_is_imported_by_the_package_that_names_it() -> TestResult {
    let memory = crate::test_sources::MemoryLibraries::new().simple("crm", &[]);
    let named = [crate::library_name::LibraryName::try_from("crm")?];
    let read = crate::vault_libraries::read_libraries(
        &named,
        &memory,
        &mut crate::library_resolution::SnapshotBudget::default(),
    )
    .await?;
    let crm = read.iter().next().ok_or("crm was not read")?;
    let converted = PackageCode::from(crm);
    let app = package("app", &["crm"], &[], &[("util.rhai", "fn who() { 1 }")])?;

    let script = script(
        "import \"crm:client\" as crm;\ncrm::id()",
        "app",
        vec![app, (converted.name().clone(), converted)],
    )?;

    assert_eq!(evaluate(script)?.as_int()?, 1);
    Ok(())
}

/// Records the calls it is passed.
#[derive(Default)]
struct RecordingCaller {
    calls: std::sync::Mutex<Vec<String>>,
}

impl crate::caller::McpCaller for RecordingCaller {
    fn call<'a>(
        &'a self,
        _server: &'a str,
        tool: &'a str,
        _arguments: serde_json::Map<String, serde_json::Value>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<serde_json::Value, crate::caller::McpCallFailure>> + Send + 'a>,
    > {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(tool.to_owned());
        Box::pin(async { Ok(serde_json::Value::Null) })
    }
}

#[tokio::test]
async fn a_call_a_library_makes_is_held_to_the_workflows_call_rules_at_run_time() -> TestResult {
    use std::sync::Arc;

    use crate::call_rules::{CallLists, CallRules, ToolRef};
    use crate::clock::SystemClock;
    use crate::inputs::ValidatedInputs;
    use crate::outputs::DeclaredOutputs;
    use crate::scoped_caller::ScopedMcpCaller;
    use crate::script::{RunOutcome, RunSettings, run_workflow_declaring};
    use crate::workflow_files::WorkflowFiles;

    // The library names no tool in its manifest at all, as a library read from source need not; only the workflow's own
    // call rules decide what is issued.
    let script = script(
        "import \"crm:client\" as crm;\ncrm::wipe()",
        "app",
        vec![
            package("app", &["crm"], &[], &[("util.rhai", "fn who() { 1 }")])?,
            package(
                "crm",
                &[],
                &["client.rhai"],
                &[(
                    "client.rhai",
                    "fn wipe() { call_mcp(\"contextos\", \"fs_delete_file\", #{}) }",
                )],
            )?,
        ],
    )?;
    let recorder = Arc::new(RecordingCaller::default());
    let rules = CallRules::try_from(CallLists {
        tool_deny: vec![ToolRef {
            server: "contextos".to_owned(),
            tool: "fs_delete_file".to_owned(),
        }],
        ..CallLists::default()
    })?;
    let caller = Arc::new(ScopedMcpCaller::new(Arc::clone(&recorder) as Arc<_>, rules));

    let result = run_workflow_declaring(
        script,
        ValidatedInputs::default(),
        DeclaredOutputs::default(),
        caller,
        WorkflowFiles::none(),
        Arc::new(SystemClock),
        RunSettings::new(time::Duration::seconds(300)),
    )
    .await;

    let RunOutcome::Failed(failure) = result.outcome else {
        return Err(format!("expected the run to fail, got {:?}", result.outcome).into());
    };
    assert!(
        failure.message.contains("workflow/effect-denied") && failure.message.contains("fs_delete_file"),
        "{}",
        failure.message
    );
    assert!(
        recorder
            .calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty(),
        "the denied call must never be issued"
    );
    Ok(())
}
