use super::*;

fn check(kind: &str, status: &str, auto_fixable: bool) -> WireCheck {
    WireCheck {
        kind: kind.to_owned(),
        status: status.to_owned(),
        message: format!("{kind} is {status}"),
        action: (status == "fail").then(|| "do something".to_owned()),
        auto_fixable,
    }
}

fn report(checks: Vec<WireCheck>) -> Result<HealthReport, UnrecognisedStatus> {
    HealthReport::try_from(DoctorResult { checks })
}

#[test]
fn a_report_of_only_passing_checks_is_healthy() -> Result<(), UnrecognisedStatus> {
    let report = report(vec![
        check("configuration", "pass", false),
        check("managed_indexes", "pass", false),
    ])?;

    assert_eq!(report.summary(), HealthSummary::Healthy);
    Ok(())
}

#[test]
fn the_summary_counts_failures_and_how_many_are_auto_fixable() -> Result<(), UnrecognisedStatus> {
    let report = report(vec![
        check("configuration", "pass", false),
        check("managed_indexes", "fail", true),
        check("git_recovery", "fail", true),
        check("frontmatter", "fail", false),
        // An auto-fixable classification on a passing check is not a failure to count.
        check("uncommitted_changes", "pass", true),
    ])?;

    assert_eq!(
        report.summary(),
        HealthSummary::NeedsAttention {
            failures: 3,
            fixable: 2
        }
    );
    Ok(())
}

#[test]
fn an_unrecognised_status_is_rejected_rather_than_read_as_healthy() {
    let outcome = report(vec![check("configuration", "degraded", false)]);

    assert_eq!(outcome, Err(UnrecognisedStatus("degraded".to_owned())));
}

#[test]
fn an_unrecognised_kind_is_kept_under_its_own_code_in_a_catch_all_section() -> Result<(), UnrecognisedStatus> {
    let report = report(vec![check("disk_pressure", "fail", false)])?;

    assert_eq!(
        report.checks[0].kind,
        CheckKind::Unrecognised("disk_pressure".to_owned())
    );
    assert_eq!(report.checks[0].kind.label(), "disk_pressure");
    let groups = report.groups();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].title, "Other checks");
    Ok(())
}

#[test]
fn every_server_kind_is_recognised() {
    for code in [
        "configuration",
        "vault_access",
        "managed_indexes",
        "git_recovery",
        "uncommitted_changes",
        "semantic_search",
        "frontmatter",
        "duplicate_ids",
        "record_conformance",
        "team_config",
        "fence_vocabulary",
        "text_index_store",
        "link_graph_store",
        "vector_store",
    ] {
        assert!(
            !matches!(CheckKind::from(code), CheckKind::Unrecognised(_)),
            "{code} is not recognised"
        );
    }
}

#[test]
fn groups_appear_in_a_fixed_order_and_omit_empty_sections() -> Result<(), UnrecognisedStatus> {
    let report = report(vec![
        check("semantic_search", "pass", false),
        check("git_recovery", "pass", false),
        check("configuration", "pass", false),
        check("frontmatter", "pass", false),
        check("managed_indexes", "pass", false),
    ])?;

    let titles = report.groups().iter().map(|group| group.title).collect::<Vec<_>>();

    assert_eq!(titles, vec!["Setup", "Content", "Indexes and recovery", "Search"]);
    let recovery = &report.groups()[2];
    assert_eq!(
        recovery
            .checks
            .iter()
            .map(|check| check.kind.label())
            .collect::<Vec<_>>(),
        vec!["Git recovery", "Managed indexes"],
        "findings keep the server's order within a section"
    );
    Ok(())
}

#[test]
fn the_fence_vocabulary_is_a_setup_check_with_a_plain_name() {
    let kind = CheckKind::from("fence_vocabulary");
    assert_eq!(kind.label(), "Fence vocabulary");
    assert_eq!(kind.group(), CheckGroup::Setup);
}

#[test]
fn record_conformance_is_labelled_record_conformance_on_the_vault_health_page() {
    let kind = CheckKind::from("record_conformance");
    assert_eq!(kind.label(), "Record conformance");
    assert_eq!(kind.group(), CheckGroup::Content);
}

#[test]
fn the_retired_base_property_kind_is_no_longer_named_and_is_shown_under_its_own_code() {
    let kind = CheckKind::from("base_property");
    assert_eq!(kind, CheckKind::Unrecognised("base_property".to_owned()));
    assert_eq!(kind.label(), "base_property");
}
