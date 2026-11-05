use serde_json::Value;

use super::*;

fn a_refused_summary() -> RunSummary {
    RunSummary::refused(
        RunInvocation {
            command: RunCommand::JobsRun,
            scope: RunScope::Vault {
                vault: "work".to_owned(),
                package: "ops".to_owned(),
                workflow: "sync".to_owned(),
                job: Some("nightly".to_owned()),
                release: None,
            },
            dry_run: false,
            scenario: None,
            overridden_inputs: Vec::new(),
            timings: Timings::default(),
        },
        Refusal::new(codes::JOB_NOT_FOUND, &"no such job"),
    )
}

fn keys_of(document: &Value) -> Vec<String> {
    document
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

#[test]
fn the_fallback_document_carries_every_field_of_a_summary_with_null_where_it_knows_nothing()
-> Result<(), serde_json::Error> {
    let summary = a_refused_summary();
    let full: Value = serde_json::to_value(&summary)?;

    let fallback: Value = serde_json::from_str(&fallback_document(&summary))?;

    assert_eq!(
        keys_of(&fallback),
        keys_of(&full),
        "a caller reading any field of the summary finds it, whichever way the document was made"
    );
    assert_eq!(fallback["summary_version"], full["summary_version"]);
    assert_eq!(fallback["outcome"], full["outcome"]);
    assert_eq!(
        fallback["exit_code"], full["exit_code"],
        "the document states the status the process exits with"
    );
    assert_eq!(fallback["code"], serde_json::json!("workflow/internal-error"));
    Ok(())
}

#[tokio::test]
async fn a_run_command_given_no_resolved_configuration_path_is_refused_as_config_invalid() {
    // The seam the process entry point fills: it resolves the path from the flag, the environment, and the home folder,
    // and hands the run command `None` when none of them names one. The home folder is never read here.
    let refusal = open_session(None).await.err();

    assert_eq!(
        refusal.as_ref().map(|refusal| refusal.code.as_str()),
        Some("workflow/config-invalid"),
        "{refusal:?}"
    );
    let message = refusal.map(|refusal| refusal.message).unwrap_or_default();
    assert!(
        message.contains("--config") && message.contains("CONTEXTOS_WEB_CONFIG"),
        "the refusal says how to name a configuration file: {message}"
    );
}
