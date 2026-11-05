use semver::Version;
use time::macros::datetime;

use super::*;
use crate::job_release::PredecessorKey;
use crate::lifecycle::{Actor, JobLifecycle, Transition};
use crate::release_state::{
    IdentityIssuer, JobIdentity, RandomIdentityIssuer, ReleaseKind, ReleaseKinds, ReleaseStateError, RetirementCause,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn v(text: &str) -> Version {
    Version::new(
        text.split('.').next().and_then(|n| n.parse().ok()).unwrap_or(0),
        text.split('.').nth(1).and_then(|n| n.parse().ok()).unwrap_or(0),
        text.split('.').nth(2).and_then(|n| n.parse().ok()).unwrap_or(0),
    )
}

/// A package `crm` whose workflow `sync` has `current` 1.2.0 and `previous` 1.1.0, release 1.0.0 marked retired, and
/// release 0.9.0 older than `current` but unmarked.
const RECORDED: &str = r#"
[packages.crm.workflows.sync]
current     = "1.2.0"
current_at  = "2026-11-03T03:00:00Z"
previous    = "1.1.0"
previous_at = "2026-11-03T03:00:00Z"
future      = "1.3.0"

[packages.crm.releases."1.0.0".workflows.sync.retired]
cause = "superseded"
at    = "2026-11-03T03:00:00Z"
"#;

fn recorded() -> Result<PackageJobsState, toml::de::Error> {
    toml::from_str(RECORDED)
}

#[test]
fn a_release_is_current_previous_retired_or_future_by_the_first_matching_row() -> TestResult {
    let state = recorded()?;
    let kind = |version: &str| state.release_kind("crm", "sync", &v(version));

    assert_eq!(kind("1.2.0"), ReleaseKind::Current);
    assert_eq!(kind("1.1.0"), ReleaseKind::Previous);
    assert_eq!(kind("1.0.0"), ReleaseKind::Retired, "a recorded marker");
    assert_eq!(
        kind("1.3.0"),
        ReleaseKind::Future,
        "newer than current, and the recorded future"
    );
    assert_eq!(
        kind("1.2.5"),
        ReleaseKind::Future,
        "newer than current, though older than the recorded future"
    );
    assert_eq!(
        kind("0.9.0"),
        ReleaseKind::Retired,
        "older than current and never current or previous"
    );
    Ok(())
}

#[test]
fn a_workflow_with_no_job_ever_enabled_has_only_future_releases() {
    let state = PackageJobsState::default();

    for version in ["0.0.1", "1.0.0", "9.9.9"] {
        assert_eq!(state.release_kind("crm", "sync", &v(version)), ReleaseKind::Future);
    }
}

#[test]
fn a_marker_is_recorded_per_workflow_so_a_sibling_workflow_of_the_release_keeps_its_current_and_previous() -> TestResult
{
    let state: PackageJobsState = toml::from_str(
        r#"
[packages.crm.workflows.sync]
current = "1.2.0"

[packages.crm.workflows.report]
current = "1.2.0"

[packages.crm.releases."1.0.0".workflows.sync.retired]
cause = "superseded"
at    = "2026-11-03T03:00:00Z"
"#,
    )?;

    assert_eq!(state.release_kind("crm", "sync", &v("1.0.0")), ReleaseKind::Retired);
    assert_eq!(
        state.release_kind("crm", "report", &v("1.0.0")),
        ReleaseKind::Retired,
        "older than that workflow's current"
    );
    assert_eq!(
        state.release_kind("crm", "report", &v("1.3.0")),
        ReleaseKind::Future,
        "newer than that workflow's current"
    );
    Ok(())
}

#[test]
fn the_recorded_current_wins_over_a_marker_on_the_same_release() -> TestResult {
    let state: PackageJobsState = toml::from_str(
        r#"
[packages.crm.workflows.sync]
current = "1.2.0"

[packages.crm.releases."1.2.0".workflows.sync.retired]
cause = "superseded"
at    = "2026-11-03T03:00:00Z"
"#,
    )?;

    assert_eq!(state.release_kind("crm", "sync", &v("1.2.0")), ReleaseKind::Current);
    Ok(())
}

#[test]
fn another_packages_current_and_previous_do_not_decide_a_release() -> TestResult {
    let state = recorded()?;

    assert_eq!(state.release_kind("other", "sync", &v("1.2.0")), ReleaseKind::Future);
    Ok(())
}

const VAULT_LAYOUT: &str = r#"
[workflows.sync.jobs.nightly-v1-1-0]
favourite = false
ever_enabled = true
lifecycle = "enabled"
schedule  = "0 3 * * *"
identity  = "job-2"
release   = "1.1.0"

[workflows.sync.jobs.nightly-v1-1-0.lock]
crm = { version = "1.0.0", digest = "sha256:aaaa" }

[workflows.sync.jobs.nightly-v1-1-0.link]
identity = "job-1"
retired  = false

[workflows.sync.jobs.nightly]
favourite = false
ever_enabled = true
lifecycle = "retired"

[workflows.sync.jobs.nightly.retirement]
by          = "system"
cause       = "successor-enabled"
at          = "2026-11-03T03:00:00Z"
replaced_by = { job = "nightly-v1-1-0", identity = "job-2" }

[packages.sync.workflows.sync]
current     = "1.1.0"
current_at  = "2026-11-03T03:00:00Z"
previous    = "1.0.0"
previous_at = "2026-11-03T03:00:00Z"

[packages.sync.releases."1.1.0".workflows.sync.lock]
crm = { version = "1.0.0", digest = "sha256:aaaa" }

[packages.sync.releases."1.0.1".workflows.sync.retired]
cause = "superseded"
at    = "2026-11-03T03:00:00Z"
"#;

#[test]
fn a_vault_state_with_releases_versions_markers_locks_and_retirement_is_written_back_as_it_was() -> TestResult {
    let state: PackageJobsState = toml::from_str(VAULT_LAYOUT)?;

    assert_eq!(
        toml::Value::try_from(&state)?,
        toml::from_str::<toml::Value>(VAULT_LAYOUT)?
    );
    let job = state.job("sync", "nightly-v1-1-0");
    assert_eq!(job.and_then(|job| job.release().cloned()), Some(Version::new(1, 1, 0)));
    assert_eq!(
        job.and_then(|job| job.identity().map(JobIdentity::as_str)),
        Some("job-2")
    );
    assert_eq!(job.and_then(|job| job.link().map(|link| link.retired)), Some(false));
    assert_eq!(
        state
            .job("sync", "nightly")
            .and_then(|job| job.retirement())
            .map(|retirement| (retirement.by, retirement.cause)),
        Some((Actor::System, RetirementCause::SuccessorEnabled))
    );
    Ok(())
}

/// Issues "job-1", "job-2", and so on, so a test knows which identity a key was given.
struct Counting(std::cell::Cell<u32>);

impl Counting {
    fn new() -> Self {
        Self(std::cell::Cell::new(0))
    }
}

impl IdentityIssuer for Counting {
    fn issue(&self) -> JobIdentity {
        self.0.set(self.0.get() + 1);
        JobIdentity::try_from(format!("job-{}", self.0.get())).unwrap_or_else(|_| unreachable!("a fixed form"))
    }
}

/// Issues the same identity every time.
struct Repeating;

impl IdentityIssuer for Repeating {
    fn issue(&self) -> JobIdentity {
        JobIdentity::try_from("same".to_owned()).unwrap_or_else(|_| unreachable!("a fixed form"))
    }
}

fn identity_of(state: &PackageJobsState, job: &str) -> Option<String> {
    state
        .job("sync", job)
        .and_then(|job| job.identity())
        .map(|id| id.as_str().to_owned())
}

#[test]
fn a_key_is_given_an_identity_when_it_first_enters_state_and_keeps_it() -> TestResult {
    let issuer = Counting::new();
    let mut state = PackageJobsState::default();

    let first = state.ensure_identity("sync", "nightly", &issuer)?.clone();
    let again = state.ensure_identity("sync", "nightly", &issuer)?.clone();

    assert_eq!(first, again);
    assert_eq!(first.as_str(), "job-1");
    assert_eq!(issuer.0.get(), 1, "an identity already given is not issued again");
    assert_eq!(identity_of(&state, "nightly").as_deref(), Some("job-1"));
    Ok(())
}

#[test]
fn two_keys_get_two_identities() -> TestResult {
    let issuer = Counting::new();
    let mut state = PackageJobsState::default();

    state.ensure_identity("sync", "a", &issuer)?;
    state.ensure_identity("sync", "b", &issuer)?;

    assert_ne!(identity_of(&state, "a"), identity_of(&state, "b"));
    Ok(())
}

#[test]
fn a_key_deleted_and_used_again_is_a_job_of_another_identity() -> TestResult {
    let issuer = Counting::new();
    let mut state = PackageJobsState::default();
    state.ensure_identity("sync", "nightly", &issuer)?;
    let before = identity_of(&state, "nightly");
    state.transition("sync", "nightly", Actor::Operator, &Transition::Retire, NOW)?;
    state.transition("sync", "nightly", Actor::Operator, &Transition::Delete, NOW)?;

    state.ensure_identity("sync", "nightly", &issuer)?;

    assert_ne!(identity_of(&state, "nightly"), before);
    Ok(())
}

#[test]
fn an_identity_already_held_is_never_issued_again_and_nothing_is_recorded() -> TestResult {
    let mut state = PackageJobsState::default();
    state.ensure_identity("sync", "a", &Repeating)?;

    let refused = state.ensure_identity("sync", "b", &Repeating);

    assert!(
        matches!(refused, Err(ReleaseStateError::IdentityReused { .. })),
        "{refused:?}"
    );
    assert!(
        state.job("sync", "b").is_none(),
        "no entry was created for the refused key"
    );
    Ok(())
}

#[test]
fn a_lifecycle_change_keeps_the_identity() -> TestResult {
    let issuer = Counting::new();
    let mut state = PackageJobsState::default();
    state.ensure_identity("sync", "nightly", &issuer)?;

    state.transition("sync", "nightly", Actor::Operator, &Transition::Retire, NOW)?;

    assert_eq!(identity_of(&state, "nightly").as_deref(), Some("job-1"));
    Ok(())
}

#[test]
fn an_identity_must_be_a_non_empty_token_without_spaces_or_control_characters() {
    for bad in ["", " ", "a b", "a\nb", "tab\t"] {
        assert!(JobIdentity::try_from(bad.to_owned()).is_err(), "{bad:?}");
    }
    let too_long = "x".repeat(129);
    assert!(JobIdentity::try_from(too_long).is_err());
    assert!(JobIdentity::try_from("job-1".to_owned()).is_ok());
    let read: Result<JobState, _> = toml::from_str("identity = \"a b\"");
    assert!(read.is_err(), "a state file naming a malformed identity is refused");
}

#[test]
fn the_random_issuer_never_repeats_and_issues_valid_identities() {
    let issuer = RandomIdentityIssuer;

    let all: std::collections::BTreeSet<_> = (0..64).map(|_| issuer.issue()).collect();

    assert_eq!(all.len(), 64);
}

const NOW: time::OffsetDateTime = datetime!(2026-11-03 03:00:00 UTC);

fn key(text: &str) -> PredecessorKey {
    PredecessorKey::try_from(text).unwrap_or_else(|_| unreachable!("a slug"))
}

/// A workflow `sync` whose job `nightly` has been through `stage` with identity `job-1`.
fn with_predecessor(at_stage: JobLifecycle) -> Result<PackageJobsState, Box<dyn std::error::Error>> {
    let issuer = Counting::new();
    let mut state = PackageJobsState::default();
    state.ensure_identity("sync", "nightly", &issuer)?;
    let fingerprint = crate::fingerprint::JobFingerprint::from(&crate::fingerprint::ReleaseFingerprintInput {
        store: crate::fingerprint::StoreKind::Host,
        package: "acme",
        release: "1.0.0",
        digest: "sha256:0",
        job_toml: "()",
        release_manifest: "",
        libraries: &[],
    });
    let schedule = crate::lifecycle::JobSchedule::try_from("0 3 * * *")?;
    let path: &[Transition] = &[
        Transition::Validated(fingerprint.clone()),
        Transition::DryRunCompleted(fingerprint.clone()),
        Transition::Enable(schedule, fingerprint),
    ];
    let steps = match at_stage {
        JobLifecycle::Draft => 0,
        JobLifecycle::Validated => 1,
        JobLifecycle::Registered => 2,
        _ => 3,
    };
    for (index, transition) in path.iter().take(steps).enumerate() {
        let actor = [Actor::Validator, Actor::DryRun, Actor::Operator][index];
        state.transition("sync", "nightly", actor, transition, NOW)?;
    }
    match at_stage {
        JobLifecycle::Suspended => {
            let reason = crate::lifecycle::SuspensionReason::try_from("stopped")?;
            state.transition(
                "sync",
                "nightly",
                Actor::Operator,
                &Transition::Suspend(crate::lifecycle::Suspension::operator(reason)),
                NOW,
            )?;
        }
        JobLifecycle::Retired => {
            state.transition("sync", "nightly", Actor::Operator, &Transition::Retire, NOW)?;
        }
        _ => {}
    }
    Ok(state)
}

#[test]
fn a_successor_may_name_a_predecessor_that_is_enabled_suspended_or_retired() -> TestResult {
    for stage in [JobLifecycle::Enabled, JobLifecycle::Suspended, JobLifecycle::Retired] {
        let state = with_predecessor(stage)?;

        let link = state.check_link("sync", "nightly-v2", &key("nightly"))?;

        assert_eq!(link.identity.as_str(), "job-1", "{stage:?}");
        assert_eq!(link.retired, stage == JobLifecycle::Retired, "{stage:?}");
    }
    Ok(())
}

#[test]
fn a_predecessor_that_is_a_draft_validated_or_registered_is_refused() -> TestResult {
    for stage in [JobLifecycle::Draft, JobLifecycle::Validated, JobLifecycle::Registered] {
        let state = with_predecessor(stage)?;

        let refused = state.check_link("sync", "nightly-v2", &key("nightly"));

        assert!(
            matches!(&refused, Err(LinkRefusal::PredecessorStage { lifecycle, .. }) if *lifecycle == stage),
            "{stage:?}: {refused:?}"
        );
    }
    Ok(())
}

#[test]
fn a_predecessor_that_does_not_exist_is_refused_before_a_link_is_recorded() -> TestResult {
    let state = with_predecessor(JobLifecycle::Enabled)?;

    let refused = state.check_link("sync", "nightly-v2", &key("absent"));

    assert!(
        matches!(refused, Err(LinkRefusal::PredecessorMissing { .. })),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn a_job_cannot_be_its_own_predecessor() -> TestResult {
    let state = with_predecessor(JobLifecycle::Enabled)?;

    let refused = state.check_link("sync", "nightly", &key("nightly"));

    assert!(
        matches!(refused, Err(LinkRefusal::PredecessorIsItself { .. })),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn a_predecessor_in_another_workflow_is_not_found() -> TestResult {
    let state = with_predecessor(JobLifecycle::Enabled)?;

    let refused = state.check_link("other", "nightly-v2", &key("nightly"));

    assert!(
        matches!(refused, Err(LinkRefusal::PredecessorMissing { .. })),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn a_predecessor_with_no_identity_cannot_be_linked() -> TestResult {
    let mut state = with_predecessor(JobLifecycle::Enabled)?;
    state.job_entry("sync", "nightly").identity = None;

    let refused = state.check_link("sync", "nightly-v2", &key("nightly"));

    assert!(
        matches!(refused, Err(LinkRefusal::PredecessorUnidentified { .. })),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn recording_a_link_stores_the_predecessors_identity_and_whether_it_was_retired() -> TestResult {
    let mut state = with_predecessor(JobLifecycle::Retired)?;

    state.record_link("sync", "nightly-v2", &key("nightly"))?;

    let link = state.job("sync", "nightly-v2").and_then(|job| job.link());
    assert_eq!(
        link.map(|link| (link.identity.as_str(), link.retired)),
        Some(("job-1", true))
    );
    Ok(())
}

#[test]
fn a_refused_link_records_nothing_and_creates_no_entry() -> TestResult {
    let mut state = with_predecessor(JobLifecycle::Draft)?;
    let before = state.clone();

    let refused = state.record_link("sync", "nightly-v2", &key("nightly"));

    assert!(refused.is_err());
    assert_eq!(state, before);
    Ok(())
}

#[test]
fn a_recorded_link_is_not_changed_by_a_later_validation_of_the_same_predecessor() -> TestResult {
    let mut state = with_predecessor(JobLifecycle::Enabled)?;
    state.record_link("sync", "nightly-v2", &key("nightly"))?;
    state.transition("sync", "nightly", Actor::Operator, &Transition::Retire, NOW)?;

    state.record_link("sync", "nightly-v2", &key("nightly"))?;

    let link = state.job("sync", "nightly-v2").and_then(|job| job.link());
    assert_eq!(
        link.map(|link| link.retired),
        Some(false),
        "what was true at the first passing validation stands"
    );
    Ok(())
}

#[test]
fn the_predecessors_key_held_by_a_job_of_another_identity_fails_a_later_validation() -> TestResult {
    let mut state = with_predecessor(JobLifecycle::Enabled)?;
    state.record_link("sync", "nightly-v2", &key("nightly"))?;
    state.transition("sync", "nightly", Actor::Operator, &Transition::Retire, NOW)?;
    state.transition("sync", "nightly", Actor::Operator, &Transition::Delete, NOW)?;
    state.ensure_identity("sync", "nightly", &Counting(std::cell::Cell::new(10)))?;

    let refused = state.check_link("sync", "nightly-v2", &key("nightly"));

    assert!(
        matches!(&refused, Err(LinkRefusal::IdentityChanged { recorded, found, .. })
            if recorded.as_str() == "job-1" && found.as_ref().map(JobIdentity::as_str) == Some("job-11")),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn a_recorded_link_whose_predecessor_has_since_been_deleted_still_stands() -> TestResult {
    let mut state = with_predecessor(JobLifecycle::Enabled)?;
    state.record_link("sync", "nightly-v2", &key("nightly"))?;
    state.transition("sync", "nightly", Actor::Operator, &Transition::Retire, NOW)?;
    state.transition("sync", "nightly", Actor::Operator, &Transition::Delete, NOW)?;

    let link = state.check_link("sync", "nightly-v2", &key("nightly"))?;

    assert_eq!(link.identity.as_str(), "job-1");
    Ok(())
}

#[test]
fn a_predecessor_with_another_standing_successor_is_refused_and_a_retired_successor_does_not_stand() -> TestResult {
    let mut state = with_predecessor(JobLifecycle::Enabled)?;
    state.ensure_identity("sync", "first", &Counting(std::cell::Cell::new(10)))?;
    state.record_link("sync", "first", &key("nightly"))?;

    let refused = state.check_link("sync", "second", &key("nightly"));
    assert!(
        matches!(&refused, Err(LinkRefusal::AnotherSuccessor { successor, .. }) if successor == "first"),
        "{refused:?}"
    );
    assert_eq!(state.standing_successor_of("sync", "nightly"), Some("first"));

    state.transition("sync", "first", Actor::Operator, &Transition::Retire, NOW)?;
    assert_eq!(state.standing_successor_of("sync", "nightly"), None);
    assert!(state.check_link("sync", "second", &key("nightly")).is_ok());
    Ok(())
}

#[test]
fn a_successor_that_was_enabled_stays_standing_for_its_predecessor_after_it_is_retired() -> TestResult {
    let mut state: PackageJobsState = toml::from_str(VAULT_LAYOUT)?;
    assert_eq!(state.standing_successor_of("sync", "nightly"), Some("nightly-v1-1-0"));

    state.transition("sync", "nightly-v1-1-0", Actor::Operator, &Transition::Retire, NOW)?;

    assert_eq!(
        state.standing_successor_of("sync", "nightly"),
        Some("nightly-v1-1-0"),
        "the predecessor's retirement record names the successor that took its place"
    );
    Ok(())
}

#[test]
fn a_job_with_a_standing_successor_may_not_run_live_by_hand() -> TestResult {
    let mut state = with_predecessor(JobLifecycle::Enabled)?;
    state.ensure_identity("sync", "next", &Counting(std::cell::Cell::new(10)))?;
    state.record_link("sync", "next", &key("nightly"))?;

    let hold = state.chain_hold_on_live_run("sync", "nightly");

    assert_eq!(
        hold,
        Some(ChainHold {
            job: "next".to_owned(),
            relation: ChainRelation::Successor
        })
    );
    assert_eq!(
        hold.map(|hold| hold.to_string()).unwrap_or_default(),
        "it has the standing successor next"
    );
    Ok(())
}

#[test]
fn a_successor_may_not_run_live_by_hand_while_its_predecessor_is_enabled_or_suspended() -> TestResult {
    for stage in [JobLifecycle::Enabled, JobLifecycle::Suspended] {
        let mut state = with_predecessor(stage)?;
        state.ensure_identity("sync", "next", &Counting(std::cell::Cell::new(10)))?;
        state.record_link("sync", "next", &key("nightly"))?;

        let hold = state.chain_hold_on_live_run("sync", "next");

        assert_eq!(
            hold,
            Some(ChainHold {
                job: "nightly".to_owned(),
                relation: ChainRelation::Predecessor(stage)
            }),
            "{stage:?}"
        );
        assert_eq!(
            hold.map(|hold| hold.to_string()).unwrap_or_default(),
            format!("its predecessor nightly is {stage}")
        );
    }
    Ok(())
}

#[test]
fn a_successor_of_a_retired_predecessor_and_a_job_with_no_chain_may_run_live_by_hand() -> TestResult {
    let mut state = with_predecessor(JobLifecycle::Retired)?;
    state.ensure_identity("sync", "next", &Counting(std::cell::Cell::new(10)))?;
    state.record_link("sync", "next", &key("nightly"))?;

    assert_eq!(state.chain_hold_on_live_run("sync", "next"), None);
    assert_eq!(state.chain_hold_on_live_run("sync", "unrelated"), None);
    Ok(())
}

#[test]
fn a_predecessor_retired_by_its_enabled_successor_is_held_by_that_successor_even_once_it_is_retired() -> TestResult {
    let mut state: PackageJobsState = toml::from_str(VAULT_LAYOUT)?;
    assert_eq!(
        state.chain_hold_on_live_run("sync", "nightly").map(|hold| hold.job),
        Some("nightly-v1-1-0".to_owned())
    );

    state.transition("sync", "nightly-v1-1-0", Actor::Operator, &Transition::Retire, NOW)?;

    assert_eq!(
        state.chain_hold_on_live_run("sync", "nightly").map(|hold| hold.job),
        Some("nightly-v1-1-0".to_owned())
    );
    Ok(())
}

/// The predecessor `nightly`, taken through `stages` from a draft, then retired by the operator.
fn retired_after(through: &[Transition]) -> Result<PackageJobsState, Box<dyn std::error::Error>> {
    let mut state = with_predecessor(JobLifecycle::Draft)?;
    let actors = [Actor::Validator, Actor::DryRun, Actor::Operator];
    for (transition, actor) in through.iter().zip(actors) {
        state.transition("sync", "nightly", actor, transition, NOW)?;
    }
    state.transition("sync", "nightly", Actor::Operator, &Transition::Retire, NOW)?;
    Ok(state)
}

fn validated_dry_run_and_enabled() -> Result<Vec<Transition>, Box<dyn std::error::Error>> {
    let enabled = with_predecessor(JobLifecycle::Enabled)?;
    let fingerprint = enabled
        .job("sync", "nightly")
        .and_then(|job| job.fingerprint().cloned())
        .ok_or("the enabled predecessor holds its fingerprint")?;
    Ok(vec![
        Transition::Validated(fingerprint.clone()),
        Transition::DryRunCompleted(fingerprint.clone()),
        Transition::Enable(crate::lifecycle::JobSchedule::try_from("0 3 * * *")?, fingerprint),
    ])
}

#[test]
fn a_job_retired_without_ever_being_enabled_cannot_be_a_predecessor() -> TestResult {
    let enabling = validated_dry_run_and_enabled()?;
    for (label, through) in [
        ("a draft", &enabling[..0]),
        ("validated", &enabling[..1]),
        ("registered", &enabling[..2]),
    ] {
        let state = retired_after(through)?;

        let refused = state.check_link("sync", "nightly-v2", &key("nightly"));

        let Err(refusal) = refused else {
            return Err(format!("{label}: a job retired before it was enabled was accepted as a predecessor").into());
        };
        assert_eq!(refusal.code(), "validation/artefact-invalid", "{label}");
        assert!(
            refusal.to_string().contains("nightly") && refusal.to_string().contains("never"),
            "{label}: the refusal names the predecessor and says it was never enabled: {refusal}"
        );
        assert_eq!(
            state.job("sync", "nightly-v2").map(JobState::lifecycle),
            None,
            "{label}: a refused link creates no entry"
        );
    }
    Ok(())
}

#[test]
fn a_job_retired_after_it_was_enabled_may_still_be_a_predecessor_whatever_became_of_it_since() -> TestResult {
    let enabling = validated_dry_run_and_enabled()?;
    let state = retired_after(&enabling)?;

    let link = state.check_link("sync", "nightly-v2", &key("nightly"))?;

    assert_eq!(link.identity.as_str(), "job-1");
    assert!(
        link.retired,
        "the link records that enabling the successor retires nothing"
    );
    Ok(())
}

#[test]
fn a_job_suspended_then_retired_after_it_was_enabled_may_still_be_a_predecessor() -> TestResult {
    let mut state = with_predecessor(JobLifecycle::Suspended)?;
    state.transition("sync", "nightly", Actor::Operator, &Transition::Retire, NOW)?;

    assert!(state.check_link("sync", "nightly-v2", &key("nightly")).is_ok());
    Ok(())
}

#[test]
fn a_job_retired_never_enabled_then_re_activated_and_enabled_may_be_a_predecessor() -> TestResult {
    let enabling = validated_dry_run_and_enabled()?;
    let mut state = retired_after(&enabling[..0])?;
    state.transition("sync", "nightly", Actor::Operator, &Transition::Reactivate, NOW)?;
    for (transition, actor) in enabling.iter().zip([Actor::Validator, Actor::DryRun, Actor::Operator]) {
        state.transition("sync", "nightly", actor, transition, NOW)?;
    }
    state.transition("sync", "nightly", Actor::Operator, &Transition::Retire, NOW)?;

    assert!(
        state.check_link("sync", "nightly-v2", &key("nightly")).is_ok(),
        "it was enabled after all"
    );
    Ok(())
}
