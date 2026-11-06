use contextos_workflows::{
    Actor, JobFingerprint, JobSchedule, JobsState, SuspensionReason, Transition, ValidatedInputs,
};
use time::macros::datetime;

use super::*;
use crate::job_operation::needs_attention;
use crate::jobs::RegisteredJob;

fn registered_job() -> RegisteredJob {
    RegisteredJob {
        templates: contextos_workflows::DeclaredFiles::default(),
        modules: contextos_workflows::DeclaredModules::default(),
        libraries: Vec::new(),
        package: "ops".to_owned(),
        workflow_slug: "sync-crm".to_owned(),
        owner: None,
        declared_owner: None,
        entity: crate::jobs::ResolvedEntity::from((None, None)),
        release: contextos_workflows::JobFileRelease {
            version: contextos_workflows::JobVersion::from(contextos_workflows::JobVersionNumbers {
                major: 1,
                minor: 0,
                patch: 0,
            }),
            predecessor: None,
        },
        slug: "nightly-sync".to_owned(),
        name: "Nightly sync".to_owned(),
        entry: "workflow.rhai".to_owned(),
        timeout_secs: 60,
        calls: contextos_workflows::call_rules::CallRules::try_from(contextos_workflows::call_rules::CallLists {
            server_allow: Some(vec!["contextos".to_owned()]),
            ..Default::default()
        })
        .unwrap_or_default(),
        inputs: ValidatedInputs::default(),
        declared_inputs: contextos_workflows::DeclaredInputs::default(),
        outputs: contextos_workflows::DeclaredOutputs::default(),
        workflow_log_level: None,
        log_level: None,
        ready: false,
        file_name: "nightly.toml".to_owned(),
    }
}

fn indexed(loaded: Option<RegisteredJob>) -> IndexedJob {
    IndexedJob {
        slug: "nightly-sync".to_owned(),
        file: "nightly.toml".to_owned(),
        loaded,
    }
}

fn card(job: &IndexedJob, stored: Option<&JobState>) -> JobCard {
    JobCard::from(CardInput {
        vault_name: "vault",
        package: "ops",
        workflow_slug: "sync-crm",
        job,
        stored,
        latest: None,
    })
}

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
    state.transition(
        "ops",
        "sync-crm",
        "nightly-sync",
        actor,
        transition,
        datetime!(2026-10-01 03:00:00 UTC),
    )?;
    Ok(())
}

fn enabled() -> Result<JobsState, Box<dyn std::error::Error>> {
    let mut state = JobsState::default();
    step(&mut state, Actor::Validator, &Transition::Validated(version()))?;
    step(&mut state, Actor::DryRun, &Transition::DryRunCompleted(version()))?;
    step(
        &mut state,
        Actor::Operator,
        &Transition::Enable(JobSchedule::try_from("0 3 * * *")?, version()),
    )?;
    Ok(state)
}

#[test]
fn an_unfavourited_card_renders_an_unpressed_toggle_that_patches_the_job() {
    let card = card(&indexed(Some(registered_job())), None);

    assert!(card.favourite_toggle_html.contains("aria-pressed=\"false\""));
    assert!(
        card.favourite_toggle_html
            .contains("/vault/workflows/ops/sync-crm/jobs/nightly-sync")
    );
}

#[test]
fn a_favourited_card_renders_a_pressed_toggle() {
    let mut stored = JobState::default();
    stored.favourite = true;

    let card = card(&indexed(Some(registered_job())), Some(&stored));

    assert!(card.favourite_toggle_html.contains("aria-pressed=\"true\""));
}

#[test]
fn a_job_nothing_is_stored_for_is_a_draft_that_needs_attention_and_runs_by_hand_only() {
    let card = card(&indexed(Some(registered_job())), None);

    assert_eq!(card.lifecycle_label, "draft");
    assert!(card.attention);
    assert_eq!(card.schedule_label, "Manual trigger only");
    assert_eq!(card.href, "/vault/workflows/ops/sync-crm/jobs/nightly-sync");
}

#[test]
fn the_stored_schedule_and_state_are_what_a_card_shows() -> Result<(), Box<dyn std::error::Error>> {
    let state = enabled()?;

    let card = card(
        &indexed(Some(registered_job())),
        state.job("ops", "sync-crm", "nightly-sync"),
    );

    assert_eq!(card.lifecycle_label, "enabled");
    assert_eq!(card.schedule_label, "0 3 * * *");
    assert!(!card.attention);
    Ok(())
}

#[test]
fn a_suspended_job_needs_attention() -> Result<(), Box<dyn std::error::Error>> {
    let mut state = enabled()?;
    step(
        &mut state,
        Actor::Operator,
        &Transition::Suspend(contextos_workflows::Suspension::operator(SuspensionReason::try_from(
            "held",
        )?)),
    )?;

    let card = card(
        &indexed(Some(registered_job())),
        state.job("ops", "sync-crm", "nightly-sync"),
    );

    assert!(card.attention);
    Ok(())
}

#[test]
fn a_job_that_does_not_register_is_named_by_its_slug_and_says_so() {
    let card = card(&indexed(None), None);

    assert_eq!(card.name, "nightly-sync");
    assert!(!card.registers);
    assert_eq!(card.owner_name, "Unassigned");
}

#[test]
fn the_state_filter_reads_a_state_name_attention_or_nothing_and_refuses_anything_else() {
    assert_eq!(StateFilter::try_from(None), Ok(StateFilter::All));
    assert_eq!(StateFilter::try_from(Some("")), Ok(StateFilter::All));
    assert_eq!(StateFilter::try_from(Some("all")), Ok(StateFilter::All));
    assert_eq!(StateFilter::try_from(Some("attention")), Ok(StateFilter::Attention));
    assert_eq!(
        StateFilter::try_from(Some("suspended")),
        Ok(StateFilter::State(JobLifecycle::Suspended))
    );
    assert_eq!(StateFilter::try_from(Some("sleeping")), Err(()));
    assert_eq!(
        StateFilter::try_from(Some("Suspended")),
        Err(()),
        "names are case sensitive"
    );
}

#[test]
fn a_filter_admits_exactly_the_states_it_names() {
    for lifecycle in [
        JobLifecycle::Draft,
        JobLifecycle::Validated,
        JobLifecycle::Registered,
        JobLifecycle::Enabled,
        JobLifecycle::Suspended,
        JobLifecycle::Retired,
    ] {
        assert!(StateFilter::All.admits(lifecycle));
        assert_eq!(
            StateFilter::Attention.admits(lifecycle),
            needs_attention(lifecycle),
            "{lifecycle}"
        );
        for wanted in [JobLifecycle::Draft, JobLifecycle::Suspended, JobLifecycle::Retired] {
            assert_eq!(StateFilter::State(wanted).admits(lifecycle), lifecycle == wanted);
        }
    }
}

#[test]
fn the_tiles_offer_every_filter_count_every_job_and_mark_the_one_in_use() {
    let counts: StateCounts = [JobLifecycle::Draft, JobLifecycle::Suspended, JobLifecycle::Enabled]
        .into_iter()
        .collect();

    let tiles = state_tiles("/vault/workflows", StateFilter::Attention, &counts);

    let shown: Vec<(&str, usize)> = tiles.iter().map(|tile| (tile.label, tile.count)).collect();
    assert_eq!(
        shown,
        [
            ("All", 3),
            ("Needs attention", 2),
            ("draft", 1),
            ("validated", 0),
            ("registered", 0),
            ("enabled", 1),
            ("suspended", 1),
            ("retired", 0),
        ]
    );
    let active: Vec<&str> = tiles.iter().filter(|tile| tile.active).map(|tile| tile.label).collect();
    assert_eq!(active, ["Needs attention"]);
    assert_eq!(tiles[0].href, "/vault/workflows");
    assert_eq!(tiles[1].href, "/vault/workflows?state=attention");
    let attention: Vec<&str> = tiles
        .iter()
        .filter(|tile| tile.attention)
        .map(|tile| tile.label)
        .collect();
    assert_eq!(attention, ["Needs attention"]);
}

#[test]
fn a_vaults_tabs_lead_to_its_workflows_the_shared_workflows_and_its_libraries() {
    let tabs = Scope::VaultLibraries("alpha").tabs(&[]);

    let shown: Vec<(&str, &str, bool)> = tabs
        .iter()
        .map(|tab| (tab.label.as_str(), tab.href.as_str(), tab.active))
        .collect();
    assert_eq!(
        shown,
        [
            ("Workflows", "/alpha/workflows", false),
            ("Shared Workflows", "/server/workflows", false),
            ("Library Releases", "/alpha/packages", true),
        ]
    );
}

#[test]
fn the_shared_tabs_lead_back_to_every_vault() {
    let vaults = ["alpha".to_owned(), "beta".to_owned()];

    let tabs = Scope::SharedWorkflows.tabs(&vaults);

    let shown: Vec<(&str, &str, bool)> = tabs
        .iter()
        .map(|tab| (tab.label.as_str(), tab.href.as_str(), tab.active))
        .collect();
    assert_eq!(
        shown,
        [
            ("alpha workflows", "/alpha/workflows", false),
            ("beta workflows", "/beta/workflows", false),
            ("Shared Workflows", "/server/workflows", true),
            ("Shared Jobs", "/server/jobs", false),
            ("Library Releases", "/server/packages", false),
        ]
    );
}

#[test]
fn with_no_vault_the_shared_tabs_are_the_shared_screens_alone() {
    let tabs = Scope::SharedLibraries.tabs(&[]);

    let labels: Vec<&str> = tabs.iter().map(|tab| tab.label.as_str()).collect();
    assert_eq!(labels, ["Shared Workflows", "Shared Jobs", "Library Releases"]);
}

#[test]
fn a_card_offers_a_run_only_for_a_job_that_registers_and_may_be_run_by_hand() -> Result<(), Box<dyn std::error::Error>>
{
    let draft = card(&indexed(Some(registered_job())), None);
    let enabled_state = enabled()?;
    let enabled_card = card(
        &indexed(Some(registered_job())),
        enabled_state.job("ops", "sync-crm", "nightly-sync"),
    );
    let unregistered = card(&indexed(None), enabled_state.job("ops", "sync-crm", "nightly-sync"));

    assert!(!draft.can_run, "a draft cannot be run by hand");
    assert!(enabled_card.can_run);
    assert!(!unregistered.can_run, "a job that does not register has nothing to run");
    Ok(())
}
