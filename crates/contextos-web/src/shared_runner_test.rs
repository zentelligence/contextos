use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use contextos_workflows::{
    FieldKind, McpCallFailure, PackageIdentity, RunOutcome, SuppliedInputs, TypedField, ValidatedInputs,
};
use serde_json::{Map, Value, json};

use super::*;
use contextos_workflows::call_rules::{CallLists, CallRules, ToolRef};

#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<(String, String)>>,
}

impl McpCaller for Recorder {
    fn call<'a>(
        &'a self,
        server: &'a str,
        tool: &'a str,
        _arguments: Map<String, Value>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, McpCallFailure>> + Send + 'a>> {
        Box::pin(async move {
            if let Ok(mut calls) = self.calls.lock() {
                calls.push((server.to_owned(), tool.to_owned()));
            }
            Ok(Value::Null)
        })
    }
}

/// Rules that permit exactly these `(server, tool)` calls: the servers are allowed, and each tool allow restricts its server.
fn calls(pairs: &[(&str, &str)]) -> CallRules {
    CallRules::try_from(CallLists {
        server_allow: Some(pairs.iter().map(|(server, _)| (*server).to_owned()).collect()),
        tool_allow: pairs
            .iter()
            .map(|(server, tool)| ToolRef {
                server: (*server).to_owned(),
                tool: (*tool).to_owned(),
            })
            .collect(),
        ..CallLists::default()
    })
    .unwrap_or_default()
}

fn identity() -> Result<PackageIdentity, Box<dyn std::error::Error>> {
    Ok(PackageIdentity {
        name: contextos_workflows::PackageName::try_from("acme")?,
        version: semver::Version::parse("1.0.0")?,
        digest: contextos_workflows::ContentDigest::try_new(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )?,
    })
}

fn execution(
    script: &str,
    effects: CallRules,
    inputs: ValidatedInputs,
    recorder: &Arc<Recorder>,
) -> Result<SharedExecution, Box<dyn std::error::Error>> {
    Ok(SharedExecution {
        clock: std::sync::Arc::new(contextos_workflows::SystemClock),
        run: SharedRun {
            identity: identity()?,
            libraries: Vec::new(),
            basis: crate::release_basis::ReleaseBasis {
                package: crate::release_basis::BasisRelease::from((identity()?, String::new())),
                libraries: Vec::new(),
            },
            script: script.to_owned().into(),
            inputs,
            overridden: Vec::new(),
            outputs: contextos_workflows::DeclaredOutputs::default(),
            calls: effects,
            log_level: None,
            files: contextos_workflows::WorkflowFiles::none(),
        },
        timeout_secs: 30,
        log_level: contextos_workflows::LogLevel::Info,
        label: contextos_workflows::RunLabel::default(),
        caller: Arc::clone(recorder) as Arc<dyn McpCaller>,
        primary_server: "contextos".to_owned(),
        reported_vaults: vec!["client-a".to_owned(), "client-b".to_owned()],
        cancel: tokio_util::sync::CancellationToken::new(),
        journal: None,
    })
}

fn vault_input(name: &str) -> Result<ValidatedInputs, Box<dyn std::error::Error>> {
    let declared = [TypedField {
        name: "target".to_owned(),
        kind: FieldKind::Vault,
        required: true,
    }];
    let mut values = Map::new();
    values.insert("target".to_owned(), json!(name));
    Ok(ValidatedInputs::try_from(SuppliedInputs {
        declared: &declared,
        values,
    })?)
}

#[tokio::test]
async fn a_permitted_call_is_issued_and_the_run_succeeds() -> Result<(), Box<dyn std::error::Error>> {
    let recorder = Arc::new(Recorder::default());
    let run = execution(
        r#"call_mcp("contextos", "vault_info", #{});"#,
        calls(&[("contextos", "vault_info")]),
        ValidatedInputs::default(),
        &recorder,
    )?;

    let outcome = execute_shared(run).await.outcome;

    assert!(matches!(outcome, RunOutcome::Succeeded), "{outcome:?}");
    assert_eq!(recorder.calls.lock().map(|calls| calls.len()).unwrap_or_default(), 1);
    Ok(())
}

#[tokio::test]
async fn a_call_outside_the_permitted_effects_fails_the_run_and_is_never_issued()
-> Result<(), Box<dyn std::error::Error>> {
    let recorder = Arc::new(Recorder::default());
    let run = execution(
        r#"call_mcp("contextos", "fs_delete_file", #{});"#,
        calls(&[("contextos", "vault_info")]),
        ValidatedInputs::default(),
        &recorder,
    )?;

    let outcome = execute_shared(run).await.outcome;

    assert!(
        matches!(&outcome, RunOutcome::Failed(failure) if failure.kind == Some(WorkflowFailureKind::EffectDenied)),
        "{outcome:?}"
    );
    assert_eq!(recorder.calls.lock().map(|calls| calls.len()).unwrap_or_default(), 0);
    Ok(())
}

#[tokio::test]
async fn a_shared_run_may_name_only_the_vaults_it_was_given() -> Result<(), Box<dyn std::error::Error>> {
    let recorder = Arc::new(Recorder::default());
    let effects = calls(&[("contextos", "fs_read_text_file")]);
    let allowed = execution(
        r#"call_mcp("contextos", "fs_read_text_file", #{ path: inputs.target + "://a.md" });"#,
        effects.clone(),
        vault_input("client-a")?,
        &recorder,
    )?;
    let other = execution(
        r#"call_mcp("contextos", "fs_read_text_file", #{ path: "client-b://a.md" });"#,
        effects,
        vault_input("client-a")?,
        &recorder,
    )?;

    let allowed = execute_shared(allowed).await.outcome;
    let other = execute_shared(other).await.outcome;

    assert!(matches!(allowed, RunOutcome::Succeeded), "{allowed:?}");
    assert!(
        matches!(&other, RunOutcome::Failed(failure) if failure.kind == Some(WorkflowFailureKind::VaultOutOfScope)),
        "{other:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_run_with_no_vault_input_may_name_no_configured_vault() -> Result<(), Box<dyn std::error::Error>> {
    let recorder = Arc::new(Recorder::default());
    let run = execution(
        r#"call_mcp("contextos", "fs_read_text_file", #{ path: "client-a://a.md" });"#,
        calls(&[("contextos", "fs_read_text_file")]),
        ValidatedInputs::default(),
        &recorder,
    )?;

    let outcome = execute_shared(run).await.outcome;

    assert!(
        matches!(&outcome, RunOutcome::Failed(failure) if failure.kind == Some(WorkflowFailureKind::VaultOutOfScope)),
        "{outcome:?}"
    );
    Ok(())
}

#[test]
fn a_vault_input_naming_an_unconfigured_vault_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let failure = require_vaults_configured(&vault_input("client-z")?, &["client-a".to_owned()]).err();

    assert_eq!(
        failure.and_then(|failure| failure.kind),
        Some(WorkflowFailureKind::VaultUnavailable)
    );
    Ok(())
}

#[test]
fn a_vault_input_matches_a_configured_vault_without_regard_to_case() -> Result<(), Box<dyn std::error::Error>> {
    assert!(require_vaults_configured(&vault_input("CLIENT-A")?, &["client-a".to_owned()]).is_ok());
    assert!(require_vaults_configured(&ValidatedInputs::default(), &[]).is_ok());
    Ok(())
}

#[tokio::test]
async fn the_script_reads_the_files_its_resolved_release_declares() -> Result<(), Box<dyn std::error::Error>> {
    let release = tempfile::tempdir()?;
    std::fs::write(release.path().join("t.md"), "one")?;
    let recorder = Arc::new(Recorder::default());
    let mut run = execution(
        r#"if workflow_file("t.md").content != "one" { throw "the declared file was not read"; }"#,
        CallRules::default(),
        ValidatedInputs::default(),
        &recorder,
    )?;
    run.run.files = contextos_workflows::WorkflowFiles::new(
        contextos_workflows::DeclaredFiles::try_from(vec!["t.md".to_owned()])?,
        Arc::new(crate::package_files::InstalledPackageFiles::from(
            release.path().to_path_buf(),
        )),
    );

    let outcome = execute_shared(run).await.outcome;

    assert_eq!(outcome, RunOutcome::Succeeded);
    Ok(())
}
