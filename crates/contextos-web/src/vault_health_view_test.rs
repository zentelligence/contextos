use std::sync::Arc;

use super::*;
use crate::vault_health::DoctorResult;
use crate::vault_health_mcp::PlanDigest;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn report_of(checks: serde_json::Value) -> Result<HealthReport, Box<dyn std::error::Error>> {
    let wrapper = serde_json::Value::Object(serde_json::Map::from_iter([("checks".to_owned(), checks)]));
    Ok(HealthReport::try_from(serde_json::from_value::<DoctorResult>(
        wrapper,
    )?)?)
}

fn snapshot(checks: serde_json::Value, age: Duration) -> Result<Snapshot, Box<dyn std::error::Error>> {
    Ok(Snapshot {
        report: Arc::new(report_of(checks)?),
        age,
    })
}

fn healthy() -> Result<Snapshot, Box<dyn std::error::Error>> {
    snapshot(
        serde_json::json!([{"kind": "configuration", "status": "pass", "message": "valid", "auto_fixable": false}]),
        Duration::from_secs(5),
    )
}

fn failing() -> Result<Snapshot, Box<dyn std::error::Error>> {
    snapshot(
        serde_json::json!([
            {"kind": "managed_indexes", "status": "fail", "message": "2 indexes are missing", "auto_fixable": true,
             "action": "Call vault_index_rebuild."},
            {"kind": "frontmatter", "status": "fail", "message": "broken.md is invalid", "auto_fixable": false,
             "action": "Fix the YAML."},
            {"kind": "configuration", "status": "pass", "message": "valid", "auto_fixable": false}
        ]),
        Duration::from_secs(125),
    )
}

#[test]
fn age_is_worded_at_each_boundary() {
    let label = |seconds| age_label(Duration::from_secs(seconds));

    assert_eq!(label(0), "just now");
    assert_eq!(label(9), "just now");
    assert_eq!(label(10), "10 seconds ago");
    assert_eq!(label(59), "59 seconds ago");
    assert_eq!(label(60), "1 minute ago");
    assert_eq!(label(119), "1 minute ago");
    assert_eq!(label(120), "2 minutes ago");
    assert_eq!(label(3599), "59 minutes ago");
    assert_eq!(label(3600), "1 hour ago");
    assert_eq!(label(7200), "2 hours ago");
}

#[test]
fn a_surface_is_header_or_footer_and_nothing_else() {
    assert_eq!(ChipSurface::try_from("header"), Ok(ChipSurface::Header));
    assert_eq!(ChipSurface::try_from("footer"), Ok(ChipSurface::Footer));
    assert_eq!(
        ChipSurface::try_from("sidebar"),
        Err(UnknownSurface("sidebar".to_owned()))
    );
    assert_eq!(ChipSurface::try_from(""), Err(UnknownSurface(String::new())));
}

#[test]
fn a_chip_state_reflects_the_assessment_and_never_reads_a_failure_as_healthy() -> TestResult {
    let ok = healthy()?;
    let bad = failing()?;

    assert_eq!(ChipState::from(Ok(&ok)), ChipState::Healthy);
    assert_eq!(ChipState::from(Ok(&bad)), ChipState::NeedsAttention { failures: 2 });
    assert_eq!(ChipState::from(Err(&HealthError::Unreachable)), ChipState::Unavailable);
    Ok(())
}

#[test]
fn the_summary_text_pluralises() {
    assert_eq!(ChipState::Healthy.summary_text(), "Healthy");
    assert_eq!(ChipState::NeedsAttention { failures: 1 }.summary_text(), "1 issue");
    assert_eq!(ChipState::NeedsAttention { failures: 3 }.summary_text(), "3 issues");
    assert_eq!(ChipState::Unavailable.summary_text(), "Unavailable");
}

fn chip(surface: ChipSurface, state: ChipState) -> String {
    render_chip(&ChipInput {
        vault: "alpha",
        surface,
        state,
    })
}

#[test]
fn both_chips_link_to_the_health_pane_and_refresh_when_health_changes() {
    for (surface, marker) in [
        (ChipSurface::Header, "surface=header"),
        (ChipSurface::Footer, "surface=footer"),
    ] {
        let html = chip(surface, ChipState::NeedsAttention { failures: 2 });

        assert!(html.contains("href=\"/alpha/settings/#health\""), "{html}");
        assert!(
            html.contains(&format!("hx-get=\"/alpha/health/summary?{marker}\"")),
            "{html}"
        );
        assert!(html.contains("vault-health-changed from:body"), "{html}");
        assert!(html.contains("health-attention"), "{html}");
        assert!(html.contains("2 issues"), "{html}");
    }
}

#[test]
fn the_header_chip_names_its_state_for_assistive_technology() {
    let html = chip(ChipSurface::Header, ChipState::Healthy);

    assert!(html.contains("aria-label=\"Vault health: Healthy\""), "{html}");
    assert!(html.contains("health-ok"), "{html}");
}

#[test]
fn an_unavailable_chip_is_visibly_unknown_rather_than_ok() {
    let html = chip(ChipSurface::Footer, ChipState::Unavailable);

    assert!(html.contains("health-unknown"), "{html}");
    assert!(!html.contains("health-ok"), "{html}");
    assert!(html.contains("Unavailable"), "{html}");
}

fn pane(assessment: Result<&Snapshot, &HealthError>, notice: Option<Notice>) -> String {
    PaneView::from(PaneInput {
        vault: "alpha",
        assessment,
        notice,
    })
    .render_html()
}

#[test]
fn a_failing_pane_groups_findings_and_distinguishes_fixable_from_needs_you() -> TestResult {
    let snapshot = failing()?;

    let html = pane(Ok(&snapshot), None);

    assert!(html.contains("2 checks need attention."), "{html}");
    assert!(html.contains("Checked 2 minutes ago"), "{html}");
    assert!(html.contains("Indexes and recovery"), "{html}");
    assert!(html.contains("Fixable automatically"), "{html}");
    assert!(html.contains("Needs you"), "{html}");
    assert!(html.contains("Fix the YAML."), "{html}");
    assert!(
        html.contains("hx-post=\"/alpha/health/execute/repair-preview\""),
        "{html}"
    );
    assert!(html.contains("1 finding can be repaired automatically."), "{html}");
    Ok(())
}

#[test]
fn a_pane_with_nothing_fixable_offers_no_repair() -> TestResult {
    let snapshot = snapshot(
        serde_json::json!([{"kind": "frontmatter", "status": "fail", "message": "bad", "auto_fixable": false}]),
        Duration::ZERO,
    )?;

    let html = pane(Ok(&snapshot), None);

    assert!(!html.contains("repair-preview"), "{html}");
    assert!(html.contains("Needs you"), "{html}");
    Ok(())
}

#[test]
fn a_healthy_pane_says_so_and_offers_no_repair() -> TestResult {
    let snapshot = healthy()?;

    let html = pane(Ok(&snapshot), None);

    assert!(html.contains("Every check passed."), "{html}");
    assert!(!html.contains("repair-preview"), "{html}");
    assert!(html.contains("hx-post=\"/alpha/health/execute/refresh\""), "{html}");
    Ok(())
}

#[test]
fn an_unavailable_pane_explains_and_never_claims_health() {
    let html = pane(Err(&HealthError::Unreachable), None);

    assert!(html.contains("could not be reached"), "{html}");
    assert!(!html.contains("Every check passed."), "{html}");
    assert!(html.contains("Check again"), "{html}");
}

#[test]
fn a_findings_text_is_escaped() -> TestResult {
    let snapshot = snapshot(
        serde_json::json!([{"kind": "frontmatter", "status": "fail", "message": "<script>alert(1)</script>.md is invalid",
                            "auto_fixable": false}]),
        Duration::ZERO,
    )?;

    let html = pane(Ok(&snapshot), None);

    assert!(!html.contains("<script>alert(1)</script>"), "{html}");
    assert!(html.contains("&#60;script&#62;"), "{html}");
    assert!(html.contains("1 check needs attention."), "{html}");
    Ok(())
}

#[test]
fn a_notice_reports_what_a_repair_did() -> TestResult {
    let steps = [
        RepairStep {
            kind: CheckKind::ManagedIndexes,
            message: "vault_index_rebuild created 2 and updated 0 indexes".to_owned(),
            resolved: true,
        },
        RepairStep {
            kind: CheckKind::GitRecovery,
            message: "Git recovery is unavailable for this vault".to_owned(),
            resolved: false,
        },
    ];
    let snapshot = healthy()?;

    let notice = Notice::from(&steps[..]);
    let html = pane(Ok(&snapshot), Some(notice.clone()));

    assert_eq!(notice.tone, "warning");
    assert_eq!(notice.title, "Repaired 1 of 2 findings.");
    assert!(html.contains("Repaired 1 of 2 findings."), "{html}");
    assert!(html.contains("Git recovery: Git recovery is unavailable"), "{html}");
    Ok(())
}

#[test]
fn a_notice_of_only_completed_steps_is_a_success() {
    let steps = [RepairStep {
        kind: CheckKind::ManagedIndexes,
        message: "done".to_owned(),
        resolved: true,
    }];

    let notice = Notice::from(&steps[..]);

    assert_eq!(notice.tone, "success");
    assert_eq!(notice.title, "Repaired 1 finding.");
}

#[test]
fn a_notice_of_no_steps_says_nothing_needed_repairing() {
    let notice = Notice::from(&[][..]);

    assert_eq!(notice.title, "Nothing needed repairing.");
}

fn plan(kinds: &[CheckKind]) -> RepairPlan {
    let steps = kinds
        .iter()
        .map(|kind| RepairStep {
            kind: kind.clone(),
            message: "would".to_owned(),
            resolved: false,
        })
        .collect::<Vec<_>>();
    let digest = PlanDigest::from(("alpha", steps.as_slice()));
    RepairPlan { steps, digest }
}

fn confirm(plan: &RepairPlan, changed: bool) -> String {
    ConfirmView::from(ConfirmInput {
        vault: "alpha",
        plan,
        changed,
    })
    .render_html()
}

#[test]
fn the_confirmation_states_that_it_writes_and_lists_each_change_before_anything_runs() {
    let plan = plan(&[CheckKind::ManagedIndexes, CheckKind::GitRecovery]);

    let html = confirm(&plan, false);

    assert!(html.contains("This will write to your vault."), "{html}");
    assert!(html.contains("Nothing has been changed yet."), "{html}");
    assert!(html.contains("Rebuild the missing or stale index.md files"), "{html}");
    assert!(html.contains("standing change"), "{html}");
    assert!(
        html.contains(&format!(
            "hx-post=\"/alpha/health/execute/repair?plan={}\"",
            plan.digest.as_str()
        )),
        "{html}"
    );
    assert!(!html.contains("has changed since you last looked"), "{html}");
}

#[test]
fn a_changed_plan_is_flagged_for_review_again() {
    let html = confirm(&plan(&[CheckKind::ManagedIndexes]), true);

    assert!(html.contains("has changed since you last looked"), "{html}");
    assert!(html.contains("/alpha/health/execute/repair?plan="), "{html}");
}

#[test]
fn an_empty_plan_offers_no_way_to_apply_anything() {
    let html = confirm(&plan(&[]), false);

    assert!(
        html.contains("Nothing can be repaired automatically right now."),
        "{html}"
    );
    assert!(!html.contains("execute/repair?plan"), "{html}");
}

#[test]
fn a_timed_out_pane_says_how_long_it_waited_and_never_claims_health() {
    let html = pane(Err(&HealthError::TimedOut(Duration::from_secs(120))), None);

    assert!(html.contains("did not answer within 120 seconds"), "{html}");
    assert!(!html.contains("Every check passed."), "{html}");
}
