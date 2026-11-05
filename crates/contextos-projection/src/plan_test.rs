use super::*;
use crate::{DefinitionRejection, Rejection, Repair};

type TestResult = Result<(), ProjectionError>;

use crate::ProjectionError;

fn hash(seed: u8) -> ContentHash {
    ContentHash::from([seed; 32])
}

fn configuration(name: &str) -> Result<ConfigurationId, ProjectionError> {
    ConfigurationId::try_from(name)
}

fn valid(vault: &str, seed: u8) -> Candidate {
    Candidate::Valid {
        vault: vault.to_owned(),
        source_path: "registry/agents/reviewer.md".to_owned(),
        source_hash: hash(seed),
    }
}

fn invalid(vault: &str, seed: Option<u8>) -> Candidate {
    Candidate::Invalid {
        vault: vault.to_owned(),
        source_hash: seed.map(hash),
        rejection: Rejection {
            field: "model",
            reason: DefinitionRejection::Unrecognised,
        },
    }
}

fn recorded(vault: &str, source: u8, projected: u8, owner: &ConfigurationId) -> Result<ManifestEntry, ProjectionError> {
    ManifestEntry::new(
        vault,
        "registry/agents/reviewer.md",
        hash(source),
        hash(projected),
        owner.clone(),
    )
}

fn recording(vault: &str, source: u8, projected: u8) -> Recording {
    Recording {
        vault: vault.to_owned(),
        source_path: "registry/agents/reviewer.md".to_owned(),
        source_hash: hash(source),
        projected_hash: hash(projected),
    }
}

fn finding(kind: FindingKind, vault: Option<&str>, other: Option<&str>, field: Option<&'static str>) -> PlanFinding {
    PlanFinding {
        kind,
        vault: vault.map(str::to_owned),
        other_vault: other.map(str::to_owned),
        rejection: field.map(|field| Rejection {
            field,
            reason: DefinitionRejection::Unrecognised,
        }),
    }
}

struct Case {
    candidates: Vec<Candidate>,
    target: TargetState,
    entry: Option<ManifestEntry>,
    projected: Option<ContentHash>,
    mode: Mode,
}

impl Case {
    fn plan(&self, ours: &ConfigurationId) -> Plan {
        Plan::from(PlanInput {
            candidates: &self.candidates,
            target: &self.target,
            entry: self.entry.as_ref(),
            configuration: ours,
            projected: self.projected.as_ref(),
            mode: self.mode,
        })
    }
}

fn case(candidates: Vec<Candidate>, target: TargetState, entry: Option<ManifestEntry>, mode: Mode) -> Case {
    Case {
        candidates,
        target,
        entry,
        projected: Some(hash(50)),
        mode,
    }
}

#[test]
fn a_new_definition_is_written_and_reported_stale_when_the_target_is_empty() -> TestResult {
    let ours = configuration("ours")?;
    let plan = case(vec![valid("a", 1)], TargetState::Absent, None, Mode::Event).plan(&ours);
    assert_eq!(plan.action, Some(Action::Write(recording("a", 1, 50))));
    assert_eq!(
        plan.findings,
        vec![finding(FindingKind::ProjectionStale, Some("a"), None, None)]
    );
    Ok(())
}

#[test]
fn the_first_vault_that_defines_the_slug_wins() -> TestResult {
    let ours = configuration("ours")?;
    let plan = case(
        vec![Candidate::Absent, valid("b", 2), valid("c", 2)],
        TargetState::Absent,
        None,
        Mode::Event,
    )
    .plan(&ours);
    assert_eq!(plan.action, Some(Action::Write(recording("b", 2, 50))));
    Ok(())
}

#[test]
fn identical_later_copies_give_nothing_and_divergent_ones_are_shadowed() -> TestResult {
    let ours = configuration("ours")?;
    let plan = case(
        vec![
            valid("a", 1),
            valid("b", 1),
            valid("c", 3),
            invalid("d", Some(1)),
            invalid("e", None),
        ],
        TargetState::Present(hash(50)),
        Some(recorded("a", 1, 50, &ours)?),
        Mode::Event,
    )
    .plan(&ours);
    assert_eq!(plan.action, None);
    assert_eq!(
        plan.findings,
        vec![
            finding(FindingKind::ProjectionShadowed, Some("a"), Some("c"), None),
            finding(FindingKind::ProjectionShadowed, Some("a"), Some("e"), None),
        ]
    );
    Ok(())
}

#[test]
fn an_invalid_winner_is_never_skipped_for_a_later_valid_copy() -> TestResult {
    let ours = configuration("ours")?;
    for target in [TargetState::Absent, TargetState::Present(hash(9))] {
        let entry = Some(recorded("a", 1, 9, &ours)?);
        let mut subject = case(vec![invalid("a", Some(1)), valid("b", 2)], target, entry, Mode::Event);
        subject.projected = None;
        let plan = subject.plan(&ours);
        assert_eq!(plan.action, None);
        assert_eq!(
            plan.findings,
            vec![
                finding(FindingKind::ProjectionInvalidDefinition, Some("a"), None, Some("model")),
                finding(FindingKind::ProjectionShadowed, Some("a"), Some("b"), None),
            ]
        );
    }
    Ok(())
}

#[test]
fn an_entry_recorded_by_another_configuration_is_left_alone() -> TestResult {
    let ours = configuration("ours")?;
    let theirs = configuration("theirs")?;
    for target in [
        TargetState::Absent,
        TargetState::Present(hash(9)),
        TargetState::Present(hash(50)),
    ] {
        let plan = case(
            vec![valid("a", 1)],
            target,
            Some(recorded("a", 1, 9, &theirs)?),
            Mode::Resolve,
        )
        .plan(&ours);
        assert_eq!(plan.action, None);
        assert_eq!(
            plan.findings,
            vec![finding(FindingKind::ProjectionForeignOwner, Some("a"), None, None)]
        );
    }
    Ok(())
}

#[test]
fn an_unrecorded_entry_with_the_projected_bytes_is_adopted() -> TestResult {
    let ours = configuration("ours")?;
    let plan = case(vec![valid("a", 1)], TargetState::Present(hash(50)), None, Mode::Event).plan(&ours);
    assert_eq!(plan.action, Some(Action::Adopt(recording("a", 1, 50))));
    assert_eq!(plan.findings, vec![]);
    Ok(())
}

#[test]
fn an_unrecorded_entry_with_other_bytes_is_a_collision_and_untouched() -> TestResult {
    let ours = configuration("ours")?;
    for mode in [Mode::Event, Mode::Refresh, Mode::Assess, Mode::Resolve] {
        let plan = case(vec![valid("a", 1)], TargetState::Present(hash(7)), None, mode).plan(&ours);
        assert_eq!(plan.action, None, "{mode:?}");
        assert_eq!(
            plan.findings,
            vec![finding(FindingKind::ProjectionUnownedCollision, Some("a"), None, None)]
        );
    }
    Ok(())
}

#[test]
fn a_recorded_entry_in_step_with_its_source_needs_nothing() -> TestResult {
    let ours = configuration("ours")?;
    let plan = case(
        vec![valid("a", 1)],
        TargetState::Present(hash(50)),
        Some(recorded("a", 1, 50, &ours)?),
        Mode::Resolve,
    )
    .plan(&ours);
    assert_eq!(plan, Plan::default());
    Ok(())
}

#[test]
fn a_changed_source_rewrites_the_recorded_entry() -> TestResult {
    let ours = configuration("ours")?;
    for mode in [Mode::Event, Mode::Refresh, Mode::Assess, Mode::Resolve] {
        let plan = case(
            vec![valid("a", 2)],
            TargetState::Present(hash(40)),
            Some(recorded("a", 1, 40, &ours)?),
            mode,
        )
        .plan(&ours);
        assert_eq!(plan.action, Some(Action::Write(recording("a", 2, 50))), "{mode:?}");
        assert_eq!(
            plan.findings,
            vec![finding(FindingKind::ProjectionStale, Some("a"), None, None)]
        );
    }
    Ok(())
}

#[test]
fn a_hand_edit_is_replaced_only_by_resolution_and_left_by_events_and_refresh_when_the_source_is_unchanged() -> TestResult
{
    let ours = configuration("ours")?;
    for (mode, rewritten) in [
        (Mode::Event, false),
        (Mode::Resolve, true),
        (Mode::Assess, true),
        (Mode::Refresh, false),
    ] {
        let plan = case(
            vec![valid("a", 1)],
            TargetState::Present(hash(77)),
            Some(recorded("a", 1, 40, &ours)?),
            mode,
        )
        .plan(&ours);
        assert_eq!(plan.action.is_some(), rewritten, "{mode:?}");
        assert_eq!(
            plan.findings,
            vec![finding(FindingKind::ProjectionHandEdited, Some("a"), None, None)]
        );
    }
    Ok(())
}

#[test]
fn refresh_replaces_a_hand_edit_when_the_source_has_changed() -> TestResult {
    let ours = configuration("ours")?;
    let changed_content = case(
        vec![valid("a", 2)],
        TargetState::Present(hash(77)),
        Some(recorded("a", 1, 40, &ours)?),
        Mode::Refresh,
    )
    .plan(&ours);
    assert_eq!(changed_content.action, Some(Action::Write(recording("a", 2, 50))));
    let changed_vault = case(
        vec![valid("b", 1)],
        TargetState::Present(hash(77)),
        Some(recorded("a", 1, 40, &ours)?),
        Mode::Refresh,
    )
    .plan(&ours);
    assert_eq!(changed_vault.action, Some(Action::Write(recording("b", 1, 50))));
    Ok(())
}

#[test]
fn a_recorded_entry_whose_target_vanished_is_written_again() -> TestResult {
    let ours = configuration("ours")?;
    let plan = case(
        vec![valid("a", 1)],
        TargetState::Absent,
        Some(recorded("a", 1, 50, &ours)?),
        Mode::Refresh,
    )
    .plan(&ours);
    assert_eq!(plan.action, Some(Action::Write(recording("a", 1, 50))));
    Ok(())
}

#[test]
fn a_record_that_names_another_source_is_corrected_without_a_write() -> TestResult {
    let ours = configuration("ours")?;
    let plan = case(
        vec![valid("b", 2)],
        TargetState::Present(hash(50)),
        Some(recorded("a", 1, 50, &ours)?),
        Mode::Event,
    )
    .plan(&ours);
    assert_eq!(plan.action, Some(Action::Adopt(recording("b", 2, 50))));
    Ok(())
}

#[test]
fn an_edit_that_happens_to_equal_the_new_projection_is_recorded_not_rewritten() -> TestResult {
    let ours = configuration("ours")?;
    let plan = case(
        vec![valid("a", 2)],
        TargetState::Present(hash(50)),
        Some(recorded("a", 1, 40, &ours)?),
        Mode::Event,
    )
    .plan(&ours);
    assert_eq!(plan.action, Some(Action::Adopt(recording("a", 2, 50))));
    assert_eq!(plan.findings, vec![]);
    Ok(())
}

#[test]
fn an_owned_entry_with_no_defining_vault_is_withdrawn_and_reported_orphaned() -> TestResult {
    let ours = configuration("ours")?;
    for target in [TargetState::Absent, TargetState::Present(hash(40))] {
        let plan = case(
            vec![Candidate::Absent, Candidate::Absent],
            target,
            Some(recorded("a", 1, 40, &ours)?),
            Mode::Event,
        )
        .plan(&ours);
        assert_eq!(plan.action, Some(Action::Withdraw));
        assert_eq!(
            plan.findings,
            vec![finding(FindingKind::ProjectionOrphaned, Some("a"), None, None)]
        );
    }
    Ok(())
}

#[test]
fn no_vault_is_configured_withdraws_only_when_doctor_asks() -> TestResult {
    let ours = configuration("ours")?;
    for (mode, withdrawn) in [
        (Mode::Event, false),
        (Mode::Refresh, false),
        (Mode::Assess, true),
        (Mode::Resolve, true),
    ] {
        let plan = case(
            vec![],
            TargetState::Present(hash(40)),
            Some(recorded("a", 1, 40, &ours)?),
            mode,
        )
        .plan(&ours);
        assert_eq!(plan.action, withdrawn.then_some(Action::Withdraw), "{mode:?}");
        assert_eq!(
            plan.findings,
            vec![finding(FindingKind::ProjectionOrphaned, Some("a"), None, None)]
        );
    }
    Ok(())
}

#[test]
fn entries_nobody_owns_are_ignored_when_no_vault_defines_the_slug() -> TestResult {
    let ours = configuration("ours")?;
    let theirs = configuration("theirs")?;
    let nothing = vec![Candidate::Absent];
    assert_eq!(
        case(nothing.clone(), TargetState::Present(hash(1)), None, Mode::Resolve).plan(&ours),
        Plan::default()
    );
    let foreign = case(
        nothing,
        TargetState::Present(hash(1)),
        Some(recorded("a", 1, 1, &theirs)?),
        Mode::Resolve,
    )
    .plan(&ours);
    assert_eq!(foreign.action, None);
    assert_eq!(kinds_of(&foreign), [FindingKind::ProjectionForeignOwner]);
    Ok(())
}

#[test]
fn only_stale_hand_edited_and_orphaned_findings_are_auto_fixable() {
    use FindingKind::*;
    let fixable = [ProjectionStale, ProjectionHandEdited, ProjectionOrphaned];
    let not_fixable = [
        ProjectionInvalidDefinition,
        ProjectionShadowed,
        ProjectionUnownedCollision,
        ProjectionForeignOwner,
        CodexReadOnlyUnenforced,
    ];
    assert!(fixable.iter().all(|kind| Repair::from(*kind) == Repair::Automatic));
    assert!(not_fixable.iter().all(|kind| Repair::from(*kind) == Repair::Manual));
}

#[test]
fn finding_kinds_are_spelled_as_the_doctor_names() {
    assert_eq!(<&str>::from(FindingKind::ProjectionStale), "projection_stale");
    assert_eq!(
        <&str>::from(FindingKind::ProjectionHandEdited),
        "projection_hand_edited"
    );
    assert_eq!(<&str>::from(FindingKind::ProjectionOrphaned), "projection_orphaned");
    assert_eq!(
        <&str>::from(FindingKind::ProjectionInvalidDefinition),
        "projection_invalid_definition"
    );
    assert_eq!(<&str>::from(FindingKind::ProjectionShadowed), "projection_shadowed");
    assert_eq!(
        <&str>::from(FindingKind::ProjectionUnownedCollision),
        "projection_unowned_collision"
    );
    assert_eq!(
        <&str>::from(FindingKind::ProjectionForeignOwner),
        "projection_foreign_owner"
    );
    assert_eq!(
        <&str>::from(FindingKind::CodexReadOnlyUnenforced),
        "codex_read_only_unenforced"
    );
}

#[test]
fn only_assessment_declines_to_perform() {
    assert!(Mode::Event.performs() && Mode::Refresh.performs() && Mode::Resolve.performs());
    assert!(!Mode::Assess.performs());
}

fn kinds_of(plan: &Plan) -> Vec<FindingKind> {
    let mut kinds: Vec<FindingKind> = plan.findings.iter().map(|finding| finding.kind).collect();
    kinds.sort();
    kinds
}

#[test]
fn a_hand_edited_entry_whose_definition_is_gone_is_kept_by_events_and_refreshes() -> TestResult {
    let ours = configuration("ours")?;
    for mode in [Mode::Event, Mode::Refresh] {
        // The entry was recorded as projected with hash 40; the target now holds hash 41, an edit made by hand.
        let plan = case(
            vec![Candidate::Absent],
            TargetState::Present(hash(41)),
            Some(recorded("a", 1, 40, &ours)?),
            mode,
        )
        .plan(&ours);
        assert_eq!(plan.action, None, "{mode:?} must not withdraw a hand edit");
        assert_eq!(
            kinds_of(&plan),
            [FindingKind::ProjectionHandEdited, FindingKind::ProjectionOrphaned],
            "{mode:?}"
        );
    }
    Ok(())
}

#[test]
fn a_hand_edited_entry_is_kept_when_no_vault_is_configured_either() -> TestResult {
    let ours = configuration("ours")?;
    for mode in [Mode::Event, Mode::Refresh] {
        let plan = case(
            vec![],
            TargetState::Present(hash(41)),
            Some(recorded("a", 1, 40, &ours)?),
            mode,
        )
        .plan(&ours);
        assert_eq!(plan.action, None, "{mode:?}");
    }
    Ok(())
}

#[test]
fn assessing_a_hand_edited_orphan_reports_both_findings_and_resolving_withdraws_it() -> TestResult {
    let ours = configuration("ours")?;
    for mode in [Mode::Assess, Mode::Resolve] {
        let plan = case(
            vec![Candidate::Absent],
            TargetState::Present(hash(41)),
            Some(recorded("a", 1, 40, &ours)?),
            mode,
        )
        .plan(&ours);
        assert_eq!(
            kinds_of(&plan),
            [FindingKind::ProjectionHandEdited, FindingKind::ProjectionOrphaned],
            "{mode:?}"
        );
        if mode == Mode::Resolve {
            assert_eq!(plan.action, Some(Action::Withdraw));
        }
    }
    Ok(())
}

#[test]
fn an_unedited_orphan_is_still_withdrawn_by_events_and_reported_only_as_orphaned() -> TestResult {
    let ours = configuration("ours")?;
    let plan = case(
        vec![Candidate::Absent],
        TargetState::Present(hash(40)),
        Some(recorded("a", 1, 40, &ours)?),
        Mode::Event,
    )
    .plan(&ours);
    assert_eq!(plan.action, Some(Action::Withdraw));
    assert_eq!(kinds_of(&plan), [FindingKind::ProjectionOrphaned]);
    Ok(())
}

/// A recorded entry whose bytes the operator then changed: the target holds neither what was recorded nor what the
/// winner renders.
fn hand_edited_case(recorded_source: u8, mode: Mode, ours: &ConfigurationId) -> Result<Case, ProjectionError> {
    Ok(case(
        vec![valid("a", 1)],
        TargetState::Present(hash(99)),
        Some(recorded("a", recorded_source, 50, ours)?),
        mode,
    ))
}

#[test]
fn a_write_event_leaves_a_hand_edited_entry_whose_source_is_unchanged() -> TestResult {
    let ours = configuration("ours")?;
    let plan = hand_edited_case(1, Mode::Event, &ours)?.plan(&ours);
    assert_eq!(plan.action, None);
    assert_eq!(
        plan.findings,
        vec![finding(FindingKind::ProjectionHandEdited, Some("a"), None, None)]
    );
    Ok(())
}

#[test]
fn a_write_event_replaces_a_hand_edited_entry_when_the_source_content_changed() -> TestResult {
    let ours = configuration("ours")?;
    let plan = hand_edited_case(2, Mode::Event, &ours)?.plan(&ours);
    assert_eq!(plan.action, Some(Action::Write(recording("a", 1, 50))));
    assert!(
        plan.findings
            .contains(&finding(FindingKind::ProjectionHandEdited, Some("a"), None, None)),
        "{:?}",
        plan.findings
    );
    Ok(())
}

#[test]
fn a_write_event_replaces_a_hand_edited_entry_when_the_winning_vault_changed() -> TestResult {
    let ours = configuration("ours")?;
    let plan = case(
        vec![valid("a", 1)],
        TargetState::Present(hash(99)),
        Some(recorded("b", 1, 50, &ours)?),
        Mode::Event,
    )
    .plan(&ours);
    assert_eq!(plan.action, Some(Action::Write(recording("a", 1, 50))));
    Ok(())
}

#[test]
fn resolving_replaces_a_hand_edited_entry_even_when_the_source_is_unchanged() -> TestResult {
    let ours = configuration("ours")?;
    let plan = hand_edited_case(1, Mode::Resolve, &ours)?.plan(&ours);
    assert_eq!(plan.action, Some(Action::Write(recording("a", 1, 50))));
    Ok(())
}

#[test]
fn start_up_refresh_keeps_leaving_a_hand_edited_entry_whose_source_is_unchanged() -> TestResult {
    let ours = configuration("ours")?;
    let plan = hand_edited_case(1, Mode::Refresh, &ours)?.plan(&ours);
    assert_eq!(plan.action, None);
    assert_eq!(
        plan.findings,
        vec![finding(FindingKind::ProjectionHandEdited, Some("a"), None, None)]
    );
    Ok(())
}

#[test]
fn start_up_refresh_replaces_a_hand_edited_entry_when_the_source_changed() -> TestResult {
    let ours = configuration("ours")?;
    let plan = hand_edited_case(2, Mode::Refresh, &ours)?.plan(&ours);
    assert_eq!(plan.action, Some(Action::Write(recording("a", 1, 50))));
    Ok(())
}

#[test]
fn doctor_assessing_reports_a_hand_edited_entry_whose_source_is_unchanged() -> TestResult {
    let ours = configuration("ours")?;
    let plan = hand_edited_case(1, Mode::Assess, &ours)?.plan(&ours);
    assert!(
        plan.findings
            .contains(&finding(FindingKind::ProjectionHandEdited, Some("a"), None, None)),
        "{:?}",
        plan.findings
    );
    Ok(())
}

#[test]
fn another_configurations_entry_is_reported_foreign_whether_or_not_a_vault_still_defines_the_slug() -> TestResult {
    let ours = configuration("ours")?;
    let theirs = configuration("theirs")?;
    for candidates in [vec![valid("a", 1)], vec![Candidate::Absent], Vec::new()] {
        for mode in [Mode::Event, Mode::Refresh, Mode::Assess, Mode::Resolve] {
            let plan = case(
                candidates.clone(),
                TargetState::Present(hash(50)),
                Some(recorded("a", 1, 50, &theirs)?),
                mode,
            )
            .plan(&ours);
            let kinds: Vec<FindingKind> = plan.findings.iter().map(|finding| finding.kind).collect();
            assert!(
                kinds.contains(&FindingKind::ProjectionForeignOwner),
                "{candidates:?} {mode:?}: {kinds:?}"
            );
            assert_eq!(plan.action, None, "{candidates:?} {mode:?}");
        }
    }
    Ok(())
}

#[test]
fn a_foreign_entry_with_no_defining_vault_is_neither_orphaned_nor_hand_edited() -> TestResult {
    let ours = configuration("ours")?;
    let theirs = configuration("theirs")?;
    let plan = case(
        vec![Candidate::Absent],
        TargetState::Present(hash(99)),
        Some(recorded("a", 1, 50, &theirs)?),
        Mode::Resolve,
    )
    .plan(&ours);
    let kinds: Vec<FindingKind> = plan.findings.iter().map(|finding| finding.kind).collect();
    assert_eq!(kinds, [FindingKind::ProjectionForeignOwner]);
    assert_eq!(plan.action, None);
    Ok(())
}
