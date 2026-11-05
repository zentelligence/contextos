use super::*;

#[test]
fn every_failure_kind_has_its_stable_machine_readable_code() {
    let expected = [
        (WorkflowFailureKind::EffectDenied, "workflow/effect-denied"),
        (WorkflowFailureKind::VaultOutOfScope, "workflow/vault-out-of-scope"),
        (WorkflowFailureKind::VaultUnavailable, "workflow/vault-unavailable"),
        (WorkflowFailureKind::ServerUnavailable, "workflow/server-unavailable"),
        (WorkflowFailureKind::ToolError, "workflow/tool-error"),
        (WorkflowFailureKind::ResultUnreadable, "workflow/result-unreadable"),
        (WorkflowFailureKind::PackageUnavailable, "workflow/package-unavailable"),
        (WorkflowFailureKind::InputInvalid, "workflow/input-invalid"),
        (WorkflowFailureKind::JobBusy, "workflow/job-busy"),
        (WorkflowFailureKind::JobNotRunnable, "workflow/job-not-runnable"),
        (WorkflowFailureKind::FilePathInvalid, "workflow/file-path-invalid"),
        (WorkflowFailureKind::FileUndeclared, "workflow/file-undeclared"),
        (WorkflowFailureKind::FileNotFound, "workflow/file-not-found"),
        (WorkflowFailureKind::FileNotText, "workflow/file-not-text"),
        (WorkflowFailureKind::FileTooLarge, "workflow/file-too-large"),
        (WorkflowFailureKind::FileUnreadable, "workflow/file-unreadable"),
    ];

    for (kind, code) in expected {
        assert_eq!(kind.code(), code);
    }
}

#[test]
fn every_failure_kind_has_a_distinct_stable_code() {
    let expected = [
        (WorkflowFailureKind::EffectDenied, "workflow/effect-denied"),
        (WorkflowFailureKind::VaultOutOfScope, "workflow/vault-out-of-scope"),
        (WorkflowFailureKind::VaultUnavailable, "workflow/vault-unavailable"),
        (WorkflowFailureKind::ServerUnavailable, "workflow/server-unavailable"),
        (WorkflowFailureKind::ToolError, "workflow/tool-error"),
        (WorkflowFailureKind::ResultUnreadable, "workflow/result-unreadable"),
        (WorkflowFailureKind::PackageUnavailable, "workflow/package-unavailable"),
        (WorkflowFailureKind::InputInvalid, "workflow/input-invalid"),
        (WorkflowFailureKind::JobBusy, "workflow/job-busy"),
        (WorkflowFailureKind::TimedOut, "workflow/timed-out"),
        (WorkflowFailureKind::JobNotRunnable, "workflow/job-not-runnable"),
        (WorkflowFailureKind::FilePathInvalid, "workflow/file-path-invalid"),
        (WorkflowFailureKind::FileUndeclared, "workflow/file-undeclared"),
        (WorkflowFailureKind::FileNotFound, "workflow/file-not-found"),
        (WorkflowFailureKind::FileNotText, "workflow/file-not-text"),
        (WorkflowFailureKind::FileTooLarge, "workflow/file-too-large"),
        (WorkflowFailureKind::FileUnreadable, "workflow/file-unreadable"),
        (WorkflowFailureKind::ScriptError, "workflow/script-error"),
        (WorkflowFailureKind::EntryUnreadable, "workflow/entry-unreadable"),
        (WorkflowFailureKind::OutputInvalid, "workflow/output-invalid"),
        (WorkflowFailureKind::Internal, "workflow/internal-error"),
        (WorkflowFailureKind::StateUnavailable, "workflow/state-unavailable"),
    ];

    for (kind, code) in expected {
        assert_eq!(kind.code(), code, "{kind:?}");
    }
    let mut codes: Vec<&str> = expected.iter().map(|(kind, _)| kind.code()).collect();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), expected.len(), "no two kinds share a code");
}

#[test]
fn a_run_that_could_not_use_the_job_state_ended_before_its_script_started() {
    assert!(ends_before_the_script(Some(
        WorkflowFailureKind::StateUnavailable.code()
    )));
}

#[test]
fn a_failure_code_from_before_the_script_started_is_told_from_one_that_came_after_it() {
    for code in PRE_START_CODES {
        assert!(ends_before_the_script(Some(code)), "{code}");
    }
    for code in [
        "workflow/script-error",
        "workflow/timed-out",
        "workflow/tool-error",
        "workflow/output-invalid",
    ] {
        assert!(!ends_before_the_script(Some(code)), "{code}");
    }
    assert!(
        !ends_before_the_script(None),
        "a run with no code is taken to have started"
    );
}
