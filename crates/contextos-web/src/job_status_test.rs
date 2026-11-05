use askama::Template;
use contextos_workflows::{
    Actor, JobFingerprint, JobSchedule, JobsState, StoredFailure, StoredValidation, SuspensionReason, Transition,
    ValidationRecord, ValidationReport,
};
use time::macros::datetime;

use super::*;

const BASE: &str = "/harbour/workflows/ops/sync/jobs/nightly";
const NOW: time::OffsetDateTime = datetime!(2026-10-01 03:00:00 UTC);

fn version() -> JobFingerprint {
    JobFingerprint::from(&contextos_workflows::ReleaseFingerprintInput {
        store: contextos_workflows::StoreKind::Vault,
        package: "ops",
        release: "1.0.0",
        digest: "sha256:00",
        job_toml: "",
        release_manifest: "",
        libraries: &[],
    })
}

fn step(state: &mut JobsState, actor: Actor, transition: &Transition) -> Result<(), Box<dyn std::error::Error>> {
    state.transition("ops", "sync", "nightly", actor, transition, NOW)?;
    Ok(())
}

fn registered() -> Result<JobsState, Box<dyn std::error::Error>> {
    let mut state = JobsState::default();
    step(&mut state, Actor::Validator, &Transition::Validated(version()))?;
    step(&mut state, Actor::DryRun, &Transition::DryRunCompleted(version()))?;
    Ok(state)
}

fn enabled() -> Result<JobsState, Box<dyn std::error::Error>> {
    let mut state = registered()?;
    step(
        &mut state,
        Actor::Operator,
        &Transition::Enable(JobSchedule::try_from("0 3 * * *")?, version()),
    )?;
    Ok(state)
}

fn render(state: &JobsState, message: Option<StatusMessage>) -> Result<String, askama::Error> {
    JobStatusView::from(JobStatusInput {
        base_path: BASE,
        stored: state.job("ops", "sync", "nightly"),
        message,
        zone: "UTC",
        run_link: None,
    })
    .render()
}

#[test]
fn a_job_nothing_is_stored_for_is_a_draft_that_needs_attention_and_offers_only_validation_and_retirement()
-> Result<(), askama::Error> {
    let html = render(&JobsState::default(), None)?;

    assert!(html.contains("draft"), "{html}");
    assert!(html.contains("Needs attention"), "{html}");
    assert!(html.contains(&format!("hx-put=\"{BASE}/execute/validate\"")), "{html}");
    assert!(
        html.contains(&format!("hx-put=\"{BASE}/execute/retire\"")),
        "a job that has not been enabled can be retired:\n{html}"
    );
    for absent in [
        "execute/suspend",
        "execute/resume",
        "execute/reactivate",
        "confirm-delete",
    ] {
        assert!(
            !html.contains(absent),
            "{absent} must not be offered for a draft:\n{html}"
        );
    }
    assert!(
        html.contains("Never validated") && html.contains("Never dry-run"),
        "{html}"
    );
    Ok(())
}

#[test]
fn an_enabled_job_shows_its_schedule_and_its_trial_and_offers_suspend_clear_replace_and_retire()
-> Result<(), Box<dyn std::error::Error>> {
    let html = render(&enabled()?, None)?;

    assert!(html.contains("0 3 * * *"), "{html}");
    assert!(html.contains("first live run is on trial"), "{html}");
    assert!(!html.contains("Needs attention"), "{html}");
    for offered in ["execute/validate", "execute/suspend", "execute/retire"] {
        assert!(
            html.contains(&format!("hx-put=\"{BASE}/{offered}\"")),
            "{offered}:\n{html}"
        );
    }
    assert!(
        html.contains(&format!("hx-patch=\"{BASE}\"")),
        "the schedule form patches the job:\n{html}"
    );
    assert!(html.contains("Clear schedule"), "{html}");
    assert!(
        html.contains("name=\"reason\"") && html.contains("required"),
        "a suspension asks why:\n{html}"
    );
    assert!(
        !html.contains("execute/resume") && !html.contains("execute/reactivate"),
        "{html}"
    );
    Ok(())
}

#[test]
fn a_suspended_job_shows_why_by_whom_and_when_and_offers_resume_and_retire() -> Result<(), Box<dyn std::error::Error>> {
    let mut state = enabled()?;
    step(
        &mut state,
        Actor::Operator,
        &Transition::Suspend(contextos_workflows::Suspension::operator(SuspensionReason::try_from(
            "the upstream API is down",
        )?)),
    )?;

    let html = render(&state, None)?;

    assert!(html.contains("the upstream API is down"), "{html}");
    assert!(html.contains("operator") && html.contains("2026-10-01"), "{html}");
    assert!(html.contains("Needs attention"), "{html}");
    for offered in ["execute/resume", "execute/retire", "execute/validate"] {
        assert!(
            html.contains(&format!("hx-put=\"{BASE}/{offered}\"")),
            "{offered}:\n{html}"
        );
    }
    assert!(!html.contains("execute/suspend"), "{html}");
    Ok(())
}

#[test]
fn a_retired_job_offers_delete_through_a_confirmation_and_reactivate() -> Result<(), Box<dyn std::error::Error>> {
    let mut state = enabled()?;
    step(&mut state, Actor::Operator, &Transition::Retire)?;

    let html = render(&state, None)?;

    assert!(
        html.contains(&format!("hx-put=\"{BASE}/execute/reactivate\"")),
        "{html}"
    );
    assert!(
        html.contains(&format!("hx-get=\"{BASE}/confirm-delete\"")),
        "delete asks first:\n{html}"
    );
    assert!(
        !html.contains("hx-delete"),
        "the delete itself is behind the confirmation:\n{html}"
    );
    assert!(
        !html.contains("execute/validate") && !html.contains("execute/resume"),
        "{html}"
    );
    Ok(())
}

#[test]
fn a_failed_validation_lists_every_failure_with_its_code_file_and_message() -> Result<(), Box<dyn std::error::Error>> {
    let mut state = JobsState::default();
    let report = ValidationReport::default();
    let mut stored = StoredValidation::from(ValidationRecord {
        report: &report,
        fingerprint: Some(version()),
        at: NOW,
    });
    stored.passed = false;
    stored.failures = vec![
        StoredFailure {
            code: "validation/script-syntax".to_owned(),
            artefact: "sync.rhai".to_owned(),
            message: "unexpected token on line 3".to_owned(),
        },
        StoredFailure {
            code: "validation/owner-unresolved".to_owned(),
            artefact: "nightly.toml".to_owned(),
            message: "no team member named nobody".to_owned(),
        },
    ];
    stored.unverifiable = vec!["tools: the server could not be reached".to_owned()];
    state.record_validation("ops", "sync", "nightly", stored);

    let html = render(&state, None)?;

    assert!(html.contains("Validation failed"), "{html}");
    for expected in [
        "validation/script-syntax",
        "sync.rhai",
        "unexpected token on line 3",
        "validation/owner-unresolved",
        "nightly.toml",
        "no team member named nobody",
        "tools: the server could not be reached",
    ] {
        assert!(html.contains(expected), "{expected}:\n{html}");
    }
    Ok(())
}

#[test]
fn a_passed_validation_and_a_dry_run_show_when_they_happened() -> Result<(), Box<dyn std::error::Error>> {
    let mut state = registered()?;
    state.record_validation(
        "ops",
        "sync",
        "nightly",
        StoredValidation::from(ValidationRecord {
            report: &ValidationReport::default(),
            fingerprint: Some(version()),
            at: NOW,
        }),
    );

    let html = render(&state, None)?;

    assert!(html.contains("Validation passed"), "{html}");
    assert!(html.contains("2026-10-01T03:00:00Z"), "{html}");
    assert!(!html.contains("Never dry-run"), "{html}");
    Ok(())
}

#[test]
fn text_from_a_failure_is_escaped_so_a_file_cannot_inject_markup() -> Result<(), Box<dyn std::error::Error>> {
    let mut state = JobsState::default();
    let mut stored = StoredValidation::from(ValidationRecord {
        report: &ValidationReport::default(),
        fingerprint: None,
        at: NOW,
    });
    stored.passed = false;
    stored.failures = vec![StoredFailure {
        code: "validation/artefact-invalid".to_owned(),
        artefact: "<img src=x>".to_owned(),
        message: "<script>alert(1)</script>".to_owned(),
    }];
    state.record_validation("ops", "sync", "nightly", stored);

    let html = render(&state, None)?;

    assert!(!html.contains("<script"), "{html}");
    assert!(!html.contains("<img src=x>"), "{html}");
    assert!(
        html.contains("&lt;script&gt;") || html.contains("&#60;script&#62;"),
        "the text is kept, escaped:\n{html}"
    );
    Ok(())
}

#[test]
fn a_message_is_shown_as_a_status_or_an_alert_inside_the_status_region() -> Result<(), askama::Error> {
    let done = render(
        &JobsState::default(),
        Some(StatusMessage {
            ok: true,
            text: "Validation passed.".to_owned(),
        }),
    )?;
    let refused = render(
        &JobsState::default(),
        Some(StatusMessage {
            ok: false,
            text: "workflow/transition-refused: Suspend is not allowed for a job that is draft".to_owned(),
        }),
    )?;

    assert!(
        done.contains("role=\"status\"") && done.contains("Validation passed."),
        "{done}"
    );
    assert!(
        refused.contains("role=\"alert\"") && refused.contains("workflow/transition-refused"),
        "{refused}"
    );
    Ok(())
}

#[test]
fn every_action_is_an_htmx_request_that_replaces_the_status_region_and_no_script_is_added()
-> Result<(), Box<dyn std::error::Error>> {
    let mut state = enabled()?;
    step(
        &mut state,
        Actor::Operator,
        &Transition::Suspend(contextos_workflows::Suspension::operator(SuspensionReason::try_from(
            "held",
        )?)),
    )?;

    for html in [
        render(&JobsState::default(), None)?,
        render(&enabled()?, None)?,
        render(&state, None)?,
    ] {
        assert!(html.contains("id=\"job-status\""), "{html}");
        assert!(!html.contains("<script") && !html.contains("onclick"), "{html}");
        for element in html
            .split('<')
            .filter(|part| part.contains("hx-put=") || part.contains("hx-patch="))
        {
            assert!(element.contains("hx-target=\"#job-status\""), "{element}");
            assert!(element.contains("hx-swap=\"outerHTML\""), "{element}");
        }
        assert!(
            !html.contains("method=\"put\"") && !html.contains("method=\"patch\""),
            "{html}"
        );
    }
    Ok(())
}

#[test]
fn an_action_that_sends_a_value_sends_it_as_json_like_every_other_htmx_form_here()
-> Result<(), Box<dyn std::error::Error>> {
    let mut state = enabled()?;
    let enabled_html = render(&state, None)?;
    step(
        &mut state,
        Actor::Operator,
        &Transition::Suspend(contextos_workflows::Suspension::operator(SuspensionReason::try_from(
            "held",
        )?)),
    )?;
    let suspended_html = render(&state, None)?;

    for html in [&enabled_html, &suspended_html] {
        let carrying: Vec<&str> = html
            .split('<')
            .filter(|element| {
                element.contains("name=\"schedule\"")
                    || element.contains("name=\"reason\"")
                    || element.contains("hx-vals")
            })
            .collect();
        assert!(!carrying.is_empty(), "{html}");
    }
    for form in enabled_html.split("<form").skip(1) {
        let opening = form.split('>').next().unwrap_or_default();
        assert!(opening.contains("hx-ext=\"json-enc\""), "a form sends JSON: {opening}");
    }
    let clear = enabled_html
        .split('<')
        .find(|element| element.contains("Clear schedule"))
        .ok_or("no clear button")?;
    assert!(
        clear.contains("hx-ext=\"json-enc\"") && clear.contains("hx-vals"),
        "{clear}"
    );
    Ok(())
}

#[test]
fn a_schedule_is_shown_and_set_in_the_operators_zone_by_name() -> Result<(), Box<dyn std::error::Error>> {
    let state = enabled()?;

    let html = JobStatusView::from(JobStatusInput {
        base_path: BASE,
        stored: state.job("ops", "sync", "nightly"),
        message: None,
        zone: "Australia/Sydney",
        run_link: None,
    })
    .render()?;

    assert!(
        html.contains("<code>0 3 * * *</code> (local time, Australia/Sydney)"),
        "{html}"
    );
    assert!(!html.contains("UTC"), "{html}");
    Ok(())
}
