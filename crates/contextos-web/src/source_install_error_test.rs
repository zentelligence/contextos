use super::*;

#[test]
fn a_refusal_that_names_validation_failures_takes_the_code_of_the_first() {
    let error = SourceInstallError::Invalid {
        failures: vec![
            ValidationFailure {
                code: ValidationCode::ScriptSyntax,
                artefact: "sync/sync.rhai".to_owned(),
                message: "line 1".to_owned(),
            },
            ValidationFailure {
                code: ValidationCode::ArtefactMissing,
                artefact: "update/workflow.toml".to_owned(),
                message: "gone".to_owned(),
            },
        ],
    };

    assert_eq!(error.code(), "validation/script-syntax");
    assert_eq!(
        error.to_string(),
        "validation/script-syntax sync/sync.rhai: line 1; validation/artefact-missing update/workflow.toml: gone"
    );
}

#[test]
fn each_refusal_has_its_stable_code() {
    let cases: Vec<(SourceInstallError, &str)> = vec![
        (
            SourceInstallError::DependencyUnsatisfied { reason: "x".to_owned() },
            "packages/dependency-unsatisfied",
        ),
        (
            SourceInstallError::NameTaken {
                name: "crm".to_owned(),
                held: "library",
                wanted: "workflow package",
                wanted_folder: "workflows",
            },
            "packages/duplicate-identity",
        ),
        (
            SourceInstallError::SourceChanged { reason: "x".to_owned() },
            "packages/duplicate-identity",
        ),
        (
            SourceInstallError::NotInstalled {
                package: "ops".to_owned(),
                version: "1.0.0".to_owned(),
            },
            "packages/not-found",
        ),
        (
            SourceInstallError::State {
                action: "read",
                reason: "x".to_owned(),
            },
            "workflow/state-unavailable",
        ),
        (SourceInstallError::StateChanged, "workflow/state-changed"),
        (
            SourceInstallError::ConfigInvalid { reason: "x".to_owned() },
            "workflow/config-invalid",
        ),
        (
            SourceInstallError::NotConnected { reason: "x" },
            "workflow/server-unavailable",
        ),
        (
            SourceInstallError::Invalid { failures: Vec::new() },
            "validation/artefact-invalid",
        ),
    ];
    for (error, code) in cases {
        assert_eq!(error.code(), code, "{error}");
    }
}

#[test]
fn a_name_taken_by_the_other_kind_names_both_the_held_kind_and_the_source_folder() {
    let error = SourceInstallError::NameTaken {
        name: "crm".to_owned(),
        held: "library",
        wanted: "workflow package",
        wanted_folder: "workflows",
    };

    let message = error.to_string();
    assert!(message.contains("crm is already a library"), "{message}");
    assert!(message.contains("registry/workflows/crm"), "{message}");
}
