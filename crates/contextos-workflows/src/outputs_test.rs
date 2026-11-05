use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::json;
use time::Duration;

use super::*;
use crate::caller::{McpCallFailure, McpCaller, WorkflowFailureKind};
use crate::clock::SystemClock;
use crate::inputs::ValidatedInputs;
use crate::package::FieldKind;
use crate::script::{RunFailure, RunOutcome, RunResult, RunSettings, run_workflow_declaring};
use crate::workflow_files::WorkflowFiles;

/// Answers no call: these scripts never make one.
struct NoCalls;

impl McpCaller for NoCalls {
    fn call<'a>(
        &'a self,
        _server: &'a str,
        _tool: &'a str,
        _arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, McpCallFailure>> + Send + 'a>> {
        Box::pin(async move { Ok(serde_json::Value::Null) })
    }
}

fn field(name: &str, kind: FieldKind, required: bool) -> TypedField {
    TypedField {
        name: name.to_owned(),
        kind,
        required,
    }
}

async fn run(script: &str, declared: Vec<TypedField>) -> RunResult {
    run_workflow_declaring(
        script.to_owned(),
        ValidatedInputs::default(),
        DeclaredOutputs::from(declared),
        Arc::new(NoCalls),
        WorkflowFiles::none(),
        Arc::new(SystemClock),
        RunSettings::new(Duration::seconds(300)),
    )
    .await
}

fn failure(result: &RunResult) -> Result<&RunFailure, String> {
    match &result.outcome {
        RunOutcome::Failed(failure) => Ok(failure),
        other => Err(format!("expected a failed run, got {other:?}")),
    }
}

fn value_of<'a>(result: &'a RunResult, name: &str) -> Option<&'a serde_json::Value> {
    result.outputs.as_ref().and_then(|outputs| outputs.values().get(name))
}

#[tokio::test]
async fn a_declared_output_set_with_a_value_of_its_kind_is_kept_in_its_normalised_form() {
    let result = run(
        r#"set_output("when", "2026-10-21T10:00:00+00:00");"#,
        vec![field("when", FieldKind::DateTime, true)],
    )
    .await;

    assert_eq!(result.outcome, RunOutcome::Succeeded);
    assert_eq!(value_of(&result, "when"), Some(&json!("2026-10-21T10:00:00Z")));
}

#[tokio::test]
async fn setting_an_undeclared_output_throws_output_invalid_that_the_script_can_catch() {
    let script = r#"
        let seen = "";
        try { set_output("nope", 1); } catch (e) { seen = e; }
        set_output("seen", seen);
    "#;

    let result = run(script, vec![field("seen", FieldKind::String, true)]).await;

    assert_eq!(
        result.outcome,
        RunOutcome::Succeeded,
        "a refusal the script catches does not end the run"
    );
    let seen = value_of(&result, "seen")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert!(seen.starts_with("workflow/output-invalid: "), "the script saw {seen:?}");
    assert!(seen.contains("nope"), "the script saw {seen:?}");
}

#[tokio::test]
async fn setting_an_output_of_the_wrong_kind_throws_output_invalid_naming_the_output_and_kind_but_not_the_value() {
    let script = r#"
        let seen = "";
        try { set_output("count", "secret-value-123"); } catch (e) { seen = e; }
        try { set_output("when", "secret-not-a-datetime"); } catch (e) { seen += " | " + e; }
        set_output("seen", seen);
    "#;
    let declared = vec![
        field("count", FieldKind::Number, false),
        field("when", FieldKind::DateTime, false),
        field("seen", FieldKind::String, true),
    ];

    let result = run(script, declared).await;

    let seen = value_of(&result, "seen")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert!(seen.contains("workflow/output-invalid: "), "the script saw {seen:?}");
    assert!(
        seen.contains("count") && seen.contains("number"),
        "the script saw {seen:?}"
    );
    assert!(
        seen.contains("when") && seen.contains("datetime"),
        "the script saw {seen:?}"
    );
    assert!(!seen.contains("secret"), "a value is never quoted: {seen:?}");
}

#[tokio::test]
async fn an_output_larger_than_64_kib_is_refused_and_nothing_is_kept() {
    let script = r#"
        let big = "";
        big.pad(70000, 'x');
        let seen = "";
        try { set_output("blob", big); } catch (e) { seen = e; }
        set_output("seen", seen);
    "#;
    let declared = vec![
        field("blob", FieldKind::String, false),
        field("seen", FieldKind::String, true),
    ];

    let result = run(script, declared).await;

    let seen = value_of(&result, "seen")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert!(seen.starts_with("workflow/output-invalid: "), "the script saw {seen:?}");
    assert!(seen.contains("blob"), "the script saw {seen:?}");
    assert_eq!(value_of(&result, "blob"), None, "an oversize value is not kept");
}

#[tokio::test]
async fn setting_an_output_twice_keeps_the_last_value() {
    let result = run(
        r#"set_output("n", 1); set_output("n", 2);"#,
        vec![field("n", FieldKind::Number, true)],
    )
    .await;

    assert_eq!(result.outcome, RunOutcome::Succeeded);
    assert_eq!(value_of(&result, "n"), Some(&json!(2)));
}

#[tokio::test]
async fn a_completed_script_missing_a_required_output_fails_with_output_invalid_naming_every_missing_one()
-> Result<(), String> {
    let declared = vec![
        field("alpha", FieldKind::Number, true),
        field("beta", FieldKind::String, true),
        field("gamma", FieldKind::Bool, false),
    ];

    let result = run(r#"set_output("gamma", true);"#, declared).await;

    let failure = failure(&result)?;
    assert_eq!(failure.kind, Some(WorkflowFailureKind::OutputInvalid));
    assert!(
        failure.message.starts_with("workflow/output-invalid: "),
        "{:?}",
        failure.message
    );
    assert!(
        failure.message.contains("alpha") && failure.message.contains("beta"),
        "{:?}",
        failure.message
    );
    assert!(!failure.message.contains("gamma"), "{:?}", failure.message);
    Ok(())
}

#[tokio::test]
async fn a_missing_optional_output_is_absent_and_the_run_succeeds() {
    let declared = vec![
        field("required", FieldKind::Number, true),
        field("optional", FieldKind::String, false),
    ];

    let result = run(r#"set_output("required", 1);"#, declared).await;

    assert_eq!(result.outcome, RunOutcome::Succeeded);
    assert_eq!(value_of(&result, "required"), Some(&json!(1)));
    assert_eq!(value_of(&result, "optional"), None);
}

#[tokio::test]
async fn an_uncaught_output_refusal_fails_the_run_with_the_output_invalid_kind() -> Result<(), String> {
    let result = run(
        r#"set_output("nope", 1);"#,
        vec![field("known", FieldKind::Number, false)],
    )
    .await;

    let failure = failure(&result)?;
    assert_eq!(failure.kind, Some(WorkflowFailureKind::OutputInvalid));
    assert!(
        failure.message.starts_with("workflow/output-invalid: "),
        "{:?}",
        failure.message
    );
    Ok(())
}

#[tokio::test]
async fn outputs_are_returned_for_succeeded_and_degraded_runs_and_only_their_names_otherwise() {
    let declared = || vec![field("n", FieldKind::Number, true)];

    let succeeded = run(r#"set_output("n", 1);"#, declared()).await;
    let degraded = run(r#"set_output("n", 2); flag_degraded("slow");"#, declared()).await;
    let failed = run(r#"set_output("n", 3); throw "boom";"#, declared()).await;

    assert_eq!(value_of(&succeeded, "n"), Some(&json!(1)));
    assert!(
        matches!(degraded.outcome, RunOutcome::Degraded(_)),
        "{:?}",
        degraded.outcome
    );
    assert_eq!(value_of(&degraded, "n"), Some(&json!(2)));
    assert!(matches!(failed.outcome, RunOutcome::Failed(_)), "{:?}", failed.outcome);
    assert_eq!(failed.outputs, None, "a failed run returns no values");
    assert_eq!(
        failed.output_names,
        vec!["n".to_owned()],
        "but its record can still name what was set"
    );
    assert_eq!(succeeded.output_names, vec!["n".to_owned()]);
}

fn vault_outputs(source: &str) -> Result<DeclaredOutputs, toml::de::Error> {
    #[derive(serde::Deserialize)]
    struct Manifest {
        #[serde(default, rename = "output")]
        outputs: DeclaredOutputs,
    }
    toml::from_str::<Manifest>(source).map(|manifest| manifest.outputs)
}

#[test]
fn duplicate_output_names_are_refused_for_package_and_vault_declarations() {
    let vault = vault_outputs(
        "[[output]]\nname = \"report\"\nkind = \"string\"\n\n[[output]]\nname = \"report\"\nkind = \"number\"\n",
    );
    assert!(
        vault
            .as_ref()
            .err()
            .is_some_and(|error| error.to_string().contains("report") && error.to_string().contains("more than once")),
        "a vault declaration with a repeated name is refused naming it: {vault:?}"
    );

    let package = crate::workflow_definition::WorkflowDefinition::try_from(crate::workflow_definition::WorkflowText {
        text: "name = \"Sync\"\nentry = \"sync.rhai\"\n\n[[output]]\nname = \"report\"\nkind = \"string\"\n\n[[output]]\nname = \"report\"\nkind = \"number\"\n",
        folder: "sync",
    });
    assert!(
        package
            .as_ref()
            .err()
            .is_some_and(|error| error.to_string().contains("report") && error.to_string().contains("more than once")),
        "a declaration with a repeated name is refused naming it: {package:?}"
    );
}

#[test]
fn a_vault_kind_output_is_refused_for_a_vault_declaration() -> Result<(), toml::de::Error> {
    let top = vault_outputs("[[output]]\nname = \"target\"\nkind = \"vault\"\n")?.without_vault_kind();
    assert!(
        matches!(&top, Err(PackageError::VaultOutputNotPermitted { path }) if path == "target"),
        "{top:?}"
    );

    let nested = vault_outputs("[[output]]\nname = \"targets\"\nkind = \"array\"\nitems = { kind = \"vault\" }\n")?
        .without_vault_kind();
    assert!(
        matches!(&nested, Err(PackageError::VaultOutputNotPermitted { path }) if path == "targets[]"),
        "{nested:?}"
    );

    let plain = vault_outputs("[[output]]\nname = \"report\"\nkind = \"string\"\n")?.without_vault_kind();
    assert!(plain.is_ok(), "{plain:?}");
    Ok(())
}
