use super::*;
use crate::test_sources::script_with;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The problems validation finds in a workflow whose code is `entry` alone.
fn check(entry: &str) -> Vec<ScriptSyntaxError> {
    check_script(&WorkflowScript::from(entry.to_owned()))
}

/// The one problem validation finds in `script`, failing the test for none or several.
fn only_error(errors: Vec<ScriptSyntaxError>) -> Result<ScriptSyntaxError, Box<dyn std::error::Error>> {
    match <[ScriptSyntaxError; 1]>::try_from(errors) {
        Ok([error]) => Ok(error),
        Err(errors) => Err(format!("expected one problem, found {errors:?}").into()),
    }
}

#[test]
fn a_script_that_parses_passes_the_syntax_check_without_being_run() {
    // `call_mcp` is not registered in the checker, and a parse does not need it, so this proves nothing was executed.
    assert_eq!(
        check("let x = call_mcp(\"contextos\", \"vault_info\", #{});\nlog_info(\"done\");"),
        []
    );
    assert_eq!(check("loop {}"), [], "an infinite loop parses, and is never run");
}

#[test]
fn a_syntax_error_is_reported_with_its_message_and_position() -> TestResult {
    let error = only_error(check("let x = 1;\nlet y = (2 +;\n"))?;

    assert_eq!(error.module, None);
    assert_eq!(error.line, Some(2));
    assert!(error.position.is_some());
    assert!(!error.message.is_empty(), "`error.message` should not be empty");
    Ok(())
}

#[test]
fn an_empty_script_is_a_valid_script() {
    assert_eq!(check(""), []);
}

#[test]
fn a_report_with_no_failures_and_nothing_unverifiable_passed() {
    assert!(ValidationReport::default().passed());
}

#[test]
fn a_failure_or_an_unverifiable_check_means_the_report_did_not_pass() {
    let failed = ValidationReport {
        failures: vec![ValidationFailure {
            code: ValidationCode::ScriptSyntax,
            artefact: "sync.rhai".to_owned(),
            message: "unexpected token".to_owned(),
        }],
        unverifiable: Vec::new(),
    };
    let unverified = ValidationReport {
        failures: Vec::new(),
        unverifiable: vec![Unverifiable {
            check: "servers".to_owned(),
            reason: "contextos is not connected".to_owned(),
        }],
    };

    assert!(!failed.passed());
    assert!(!unverified.passed());
}

#[test]
fn every_validation_code_has_a_stable_machine_readable_code() {
    for (code, expected) in [
        (ValidationCode::ArtefactMissing, "validation/artefact-missing"),
        (ValidationCode::ArtefactInvalid, "validation/artefact-invalid"),
        (ValidationCode::ScriptSyntax, "validation/script-syntax"),
        (ValidationCode::InputInvalid, "validation/input-invalid"),
        (ValidationCode::ServerNotConfigured, "validation/server-not-configured"),
        (ValidationCode::OwnerUnresolved, "validation/owner-unresolved"),
        (ValidationCode::ToolNotFound, "validation/tool-not-found"),
        (ValidationCode::EntityUnresolved, "validation/entity-unresolved"),
    ] {
        assert_eq!(code.as_str(), expected);
    }
}

#[test]
fn a_script_that_evaluates_text_fails_validation_at_the_eval() -> TestResult {
    let error = only_error(check("let code = \"40 + 2\";\nlet x = eval(code);\n"))?;

    assert!(error.message.contains("cannot evaluate text"), "{}", error.message);
    assert_eq!(error.line, Some(2));
    Ok(())
}

#[tokio::test]
async fn a_script_importing_its_declared_modules_and_the_built_in_library_passes() -> TestResult {
    let script = script_with(
        "import \"lib/dates\" as dates;\nimport \"contextos:text\" as text;\ndates::today()",
        &[
            (
                "lib/dates.rhai",
                "import \"lib/format\" as format;\nfn today() { format::iso(1) }",
            ),
            ("lib/format.rhai", "import \"contextos:path\" as p;\nfn iso(n) { n }"),
        ],
    )
    .await?;

    assert_eq!(check_script(&script), []);
    Ok(())
}

#[test]
fn an_import_of_a_file_that_is_not_declared_fails_validation_at_the_import() -> TestResult {
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("helpers.rhai"), "fn help() { 1 }\n")?;
    let outside = dir.path().join("helpers").to_string_lossy().replace('\\', "/");

    for name in [outside.as_str(), "helpers", "lib/undeclared", "contextos:missing"] {
        let error = only_error(check(&format!("let x = 1;\nimport \"{name}\" as h;\nh::help();\n")))?;

        assert!(error.message.contains("names no module"), "{name}: {}", error.message);
        assert_eq!(error.line, Some(2), "{name}");
    }
    Ok(())
}

#[test]
fn an_import_naming_its_module_by_anything_but_a_string_literal_fails_validation() -> TestResult {
    for form in [
        "let name = \"lib/dates\";\nimport name as m;",
        "const NAME = \"lib/dates\";\nimport NAME as m;",
        "let x = 1;\nimport \"lib/\" + \"dates\" as m;",
    ] {
        let error = only_error(check(form))?;

        assert!(error.message.contains("string literal"), "{form}: {}", error.message);
        assert_eq!(error.line, Some(2), "{form}");
    }
    Ok(())
}

#[tokio::test]
async fn each_file_reports_its_own_first_problem_and_names_the_module_it_is_in() -> TestResult {
    let script = script_with(
        "import \"lib/broken\" as broken;\nlet y = (2 +;",
        &[
            ("lib/broken.rhai", "fn ok() { 1 }\nfn bad( { }"),
            ("lib/code.rhai", "fn ok() { 1 }\n\nlet ran = true;"),
            ("lib/strays.rhai", "import \"lib/nowhere\" as n;\nfn f() { 1 }"),
        ],
    )
    .await?;

    let errors = check_script(&script);

    let found: Vec<(Option<&str>, Option<usize>)> = errors
        .iter()
        .map(|error| (error.module.as_ref().map(WorkflowFilePath::as_str), error.line))
        .collect();
    assert_eq!(
        found,
        [
            (None, Some(2)),
            (Some("lib/broken.rhai"), Some(2)),
            (Some("lib/code.rhai"), Some(3)),
            (Some("lib/strays.rhai"), Some(1)),
        ],
        "{errors:?}"
    );
    assert!(errors[2].message.contains(MODULE_HOLDS_CODE), "{}", errors[2].message);
    assert!(errors[3].message.contains("names no module"), "{}", errors[3].message);
    Ok(())
}

#[tokio::test]
async fn modules_that_import_each_other_fail_validation_at_the_import_that_closes_the_loop() -> TestResult {
    let script = script_with(
        "import \"a\" as a;",
        &[
            ("a.rhai", "import \"b\" as b;\nfn f() { 1 }"),
            ("b.rhai", "import \"c\" as c;\nfn g() { 2 }"),
            ("c.rhai", "fn i() { 3 }\n\nimport \"a\" as a;"),
        ],
    )
    .await?;

    let error = only_error(check_script(&script))?;

    assert_eq!(error.module.as_ref().map(WorkflowFilePath::as_str), Some("c.rhai"));
    assert_eq!(error.line, Some(3));
    assert!(error.message.contains("import each other"), "{}", error.message);
    Ok(())
}

#[tokio::test]
async fn a_module_that_imports_itself_fails_validation() -> TestResult {
    let script = script_with("", &[("a.rhai", "import \"a\" as me;\nfn f() { 1 }")]).await?;

    let error = only_error(check_script(&script))?;

    assert!(error.message.contains("import each other"), "{}", error.message);
    Ok(())
}

#[tokio::test]
async fn an_import_inside_a_modules_function_fails_validation_because_it_would_resolve_against_the_caller()
-> Result<(), Box<dyn std::error::Error>> {
    let script = script_with(
        "import \"lib/helpers\" as helpers;",
        &[(
            "lib/helpers.rhai",
            "fn ok() { 1 }\n\nfn tidy(value) { import \"contextos:text\" as t; t::trimmed(value) }",
        )],
    )
    .await?;

    let error = only_error(check_script(&script))?;

    assert_eq!(
        error.module.as_ref().map(WorkflowFilePath::as_str),
        Some("lib/helpers.rhai")
    );
    assert_eq!(error.line, Some(3));
    assert!(error.message.contains("top level"), "{}", error.message);
    Ok(())
}

#[test]
fn the_built_in_library_is_imported_with_a_colon_and_its_old_names_name_nothing() -> TestResult {
    assert_eq!(check("import \"contextos:text\" as t;"), []);
    let error = only_error(check("import \"contextos/text\" as t;"))?;
    assert!(error.message.contains("names no module"), "{}", error.message);
    Ok(())
}

#[tokio::test]
async fn exported_constants_written_out_pass_and_any_other_constant_or_variable_is_reported_at_its_line() -> TestResult
{
    let accepted = script_with(
        "import \"scripts/layout\" as layout;\nlayout::FOLDERS",
        &[(
            "scripts/layout.rhai",
            "export const FOLDERS = [\"inbox\", -1, 2.5, true, 'c', ()];\nexport const MAP = #{ a: [1, #{ b: \"c\" }] };",
        )],
    )
    .await?;
    assert_eq!(check_script(&accepted), []);

    for statement in [
        "const LOCAL = 1;",
        "export let SHARED = 1;",
        "export const MADE = [1].len();",
    ] {
        let script = script_with(
            "import \"lib/data\" as data;",
            &[("lib/data.rhai", &format!("fn ok() {{ 1 }}\n\n{statement}"))],
        )
        .await?;

        let error = only_error(check_script(&script))?;

        assert_eq!(error.line, Some(3), "{statement}");
        assert!(
            error.message.contains(MODULE_HOLDS_CODE),
            "{statement}: {}",
            error.message
        );
    }
    Ok(())
}

#[test]
fn an_import_inside_the_entry_scripts_function_fails_validation_at_the_import() -> TestResult {
    let error = only_error(check(
        "let x = 1;\nfn tidy(value) { import \"contextos:text\" as t; t::trimmed(value) }",
    ))?;

    assert_eq!(error.module, None, "reported against the entry script");
    assert_eq!(error.line, Some(2));
    assert!(error.position.is_some());
    assert!(error.message.contains("top level"), "{}", error.message);
    Ok(())
}

#[test]
fn an_import_at_the_entry_scripts_top_level_still_passes_validation() {
    assert_eq!(
        check("import \"contextos:text\" as t;\nfn tidy(value) { t::trimmed(value) }"),
        []
    );
}

#[test]
fn each_failure_to_resolve_a_package_dependency_is_reported_as_a_missing_or_an_invalid_artefact() -> TestResult {
    let name = crate::PackageName::try_from("acme-text")?;
    let release = semver::Version::new(1, 0, 0);
    let missing = [
        PackageError::LibraryNotInstalled {
            name: name.clone(),
            requirements: Vec::new(),
        },
        PackageError::NoReleaseSatisfies {
            name: name.clone(),
            requirements: Vec::new(),
            installed: Vec::new(),
        },
        PackageError::RequirementExcludesChosen {
            name: name.clone(),
            chosen: release.clone(),
            requirements: Vec::new(),
        },
        PackageError::CatalogueUnreadable {
            name: "acme-text".to_owned(),
            reason: "denied".to_owned(),
        },
    ];
    let invalid = [
        PackageError::DependencyCycle {
            path: vec![name.clone(), name.clone()],
        },
        PackageError::TooManyDependencies { limit: 32 },
        PackageError::DependencyTooDeep {
            name: name.clone(),
            limit: 8,
        },
        PackageError::InstalledManifestInvalid {
            name,
            version: semver::Version::new(1, 0, 0),
            reason: "missing field `digest`".to_owned(),
        },
        PackageError::SnapshotOverLimit {
            path: "strings.rhai".to_owned(),
            limit: "500 module files",
        },
    ];
    for error in &missing {
        assert_eq!(
            ValidationCode::try_from(error),
            Ok(ValidationCode::ArtefactMissing),
            "{error:?}"
        );
    }
    for error in &invalid {
        assert_eq!(
            ValidationCode::try_from(error),
            Ok(ValidationCode::ArtefactInvalid),
            "{error:?}"
        );
    }
    Ok(())
}
