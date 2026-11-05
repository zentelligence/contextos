use super::*;

use contextos_workflows::SuccessorRefusal;

use crate::shared_jobs::{SharedJobFile, UnusableReason};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const PREDECESSOR: &str = "name = \"Nightly\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nready = true\n";

fn v(text: &str) -> Version {
    Version::parse(text).unwrap_or_else(|_| Version::new(0, 0, 0))
}

fn state(text: &str) -> Result<JobsState, toml::de::Error> {
    toml::from_str(text)
}

/// Releases 1.0.0 current, 0.9.0 previous, and 1.1.0 future, with `nightly` enabled on the current release.
fn running() -> Result<JobsState, toml::de::Error> {
    state(
        "[acme.sync]\ncurrent = \"1.0.0\"\nprevious = \"0.9.0\"\nfuture = \"1.1.0\"\n\n[acme.sync.nightly]\nlifecycle = \"enabled\"\n",
    )
}

fn file(stem: &str, text: &str) -> JobFileEntry {
    JobFileEntry::Usable(SharedJobFile {
        key: SharedJobKey {
            package: "acme".to_owned(),
            workflow: "sync".to_owned(),
            job: stem.to_owned(),
        },
        text: text.to_owned(),
    })
}

fn one(job: &str, new_job: &str) -> SuccessorScope {
    SuccessorScope::One {
        job: job.to_owned(),
        new_job: new_job.to_owned(),
    }
}

struct Plan<'a> {
    scope: SuccessorScope,
    to: Version,
    presence: Presence,
    state: &'a JobsState,
    entries: Vec<JobFileEntry>,
}

impl<'a> Plan<'a> {
    fn new(scope: SuccessorScope, state: &'a JobsState, entries: Vec<JobFileEntry>) -> Self {
        Self {
            scope,
            to: v("1.1.0"),
            presence: Presence::Installed,
            state,
            entries,
        }
    }

    fn run(&self) -> Result<Vec<PlannedSuccessor>, Vec<SuccessorRefused>> {
        self.run_checking(&|_key, _text| Ok(()))
    }

    fn run_checking(
        &self,
        check_file: &dyn Fn(&str, &str) -> Result<(), AuthoringError>,
    ) -> Result<Vec<PlannedSuccessor>, Vec<SuccessorRefused>> {
        plan_successors(&PlanInput {
            package: "acme",
            workflow: "sync",
            scope: &self.scope,
            to: &self.to,
            presence: self.presence,
            state: self.state,
            entries: &self.entries,
            check_file,
        })
    }
}

fn refusals(outcome: Result<Vec<PlannedSuccessor>, Vec<SuccessorRefused>>) -> Vec<SuccessorRefused> {
    outcome.err().unwrap_or_default()
}

fn codes(refusals: &[SuccessorRefused]) -> Vec<(Option<&str>, &'static str)> {
    refusals
        .iter()
        .map(|refused| (refused.job.as_deref(), refused.error.code()))
        .collect()
}

#[test]
fn a_successor_is_the_predecessors_file_with_its_version_replaced_and_the_predecessor_added() -> TestResult {
    let held = running()?;
    let planned = Plan::new(
        one("nightly", "nightly-next"),
        &held,
        vec![file("nightly", PREDECESSOR)],
    )
    .run();
    let planned = planned.map_err(|refused| format!("{refused:?}"))?;
    let [successor] = planned.as_slice() else {
        return Err("expected one successor".into());
    };
    assert_eq!(successor.predecessor, "nightly");
    assert_eq!(successor.key, "nightly-next");
    assert_eq!((successor.from.clone(), successor.to.clone()), (v("1.0.0"), v("1.1.0")));
    assert_eq!(successor.step, ReleaseStep::Minor);
    let table: toml::Table = toml::from_str(&successor.text)?;
    assert_eq!(table.get("version").and_then(toml::Value::as_str), Some("1.1.0"));
    assert_eq!(table.get("predecessor").and_then(toml::Value::as_str), Some("nightly"));
    assert_eq!(table.get("name").and_then(toml::Value::as_str), Some("Nightly"));
    assert_eq!(table.get("timeout_secs").and_then(toml::Value::as_integer), Some(120));
    assert_eq!(table.get("ready").and_then(toml::Value::as_bool), Some(true));
    Ok(())
}

#[test]
fn the_rest_of_the_predecessors_file_is_kept_as_written() -> TestResult {
    let held = running()?;
    let text = "# every night\nname = \"Nightly\"   # shown on the page\nversion = \"1.0.0\"\n";
    let planned = Plan::new(one("nightly", "nightly-next"), &held, vec![file("nightly", text)])
        .run()
        .map_err(|refused| format!("{refused:?}"))?;
    let kept = planned
        .first()
        .map(|successor| successor.text.as_str())
        .unwrap_or_default();
    assert!(
        kept.starts_with("# every night\nname = \"Nightly\"   # shown on the page\n"),
        "{kept}"
    );
    Ok(())
}

#[test]
fn a_predecessor_line_and_a_slug_line_in_the_file_name_the_new_job() -> TestResult {
    let held = running()?;
    let text = "version = \"1.0.0\"\npredecessor = \"older\"\nslug = \"nightly\"\n";
    let planned = Plan::new(one("nightly", "nightly-next"), &held, vec![file("nightly-file", text)])
        .run()
        .map_err(|refused| format!("{refused:?}"))?;
    let table: toml::Table = toml::from_str(planned.first().map(|s| s.text.as_str()).unwrap_or_default())?;
    assert_eq!(table.get("predecessor").and_then(toml::Value::as_str), Some("nightly"));
    assert_eq!(table.get("slug").and_then(toml::Value::as_str), Some("nightly-next"));
    Ok(())
}

#[test]
fn a_release_that_is_not_installed_refuses_the_whole_request() -> TestResult {
    let held = running()?;
    let mut plan = Plan::new(
        one("nightly", "nightly-next"),
        &held,
        vec![file("nightly", PREDECESSOR)],
    );
    plan.presence = Presence::Missing;
    assert_eq!(codes(&refusals(plan.run())), [(None, "workflow/package-unavailable")]);
    Ok(())
}

#[test]
fn a_predecessor_with_no_file_is_not_found() -> TestResult {
    let held = running()?;
    let plan = Plan::new(one("nightly", "nightly-next"), &held, Vec::new());
    assert_eq!(
        codes(&refusals(plan.run())),
        [(Some("nightly"), "workflow/job-not-found")]
    );
    Ok(())
}

#[test]
fn a_predecessor_never_enabled_is_refused_with_the_way_on() -> TestResult {
    let held = state("[acme.sync]\ncurrent = \"1.0.0\"\nfuture = \"1.1.0\"\n")?;
    let plan = Plan::new(
        one("nightly", "nightly-next"),
        &held,
        vec![file("nightly", PREDECESSOR)],
    );
    let refused = refusals(plan.run());
    assert_eq!(codes(&refused), [(Some("nightly"), "workflow/transition-refused")]);
    assert!(refused[0].error.to_string().contains("change its version in place"));
    Ok(())
}

#[test]
fn a_predecessor_that_already_has_a_successor_is_refused() -> TestResult {
    let held = running()?;
    let entries = vec![
        file("nightly", PREDECESSOR),
        file("nightly-b", "version = \"1.1.0\"\npredecessor = \"nightly\"\n"),
    ];
    let refused = refusals(Plan::new(one("nightly", "nightly-next"), &held, entries).run());
    assert_eq!(codes(&refused), [(Some("nightly"), "workflow/transition-refused")]);
    assert!(
        refused[0].error.to_string().contains("nightly-b"),
        "{}",
        refused[0].error
    );
    Ok(())
}

#[test]
fn a_retired_successor_no_longer_stands_in_the_way() -> TestResult {
    let held = state(
        "[acme.sync]\ncurrent = \"1.0.0\"\nfuture = \"1.1.0\"\n\n[acme.sync.nightly]\nlifecycle = \"enabled\"\n\n[acme.sync.nightly-b]\nlifecycle = \"retired\"\n",
    )?;
    let entries = vec![
        file("nightly", PREDECESSOR),
        file("nightly-b", "version = \"1.1.0\"\npredecessor = \"nightly\"\n"),
    ];
    let planned = Plan::new(one("nightly", "nightly-next"), &held, entries).run();
    assert!(planned.is_ok(), "{:?}", planned.err());
    Ok(())
}

#[test]
fn a_predecessor_already_retired_may_have_a_successor() -> TestResult {
    let held = state(
        "[acme.sync]\ncurrent = \"1.0.0\"\nfuture = \"1.1.0\"\n\n[acme.sync.nightly]\nlifecycle = \"retired\"\never_enabled = true\n",
    )?;
    let planned = Plan::new(
        one("nightly", "nightly-next"),
        &held,
        vec![file("nightly", PREDECESSOR)],
    )
    .run();
    assert!(planned.is_ok(), "{:?}", planned.err());
    Ok(())
}

#[test]
fn a_target_that_is_a_retired_release_is_refused() -> TestResult {
    let held = running()?;
    let mut plan = Plan::new(
        one("nightly", "nightly-next"),
        &held,
        vec![file("nightly", PREDECESSOR)],
    );
    plan.to = v("0.5.0");
    assert_eq!(
        codes(&refusals(plan.run())),
        [(Some("nightly"), "workflow/transition-refused")]
    );
    Ok(())
}

#[test]
fn a_return_to_previous_is_a_successor_of_a_job_on_current() -> TestResult {
    let held = running()?;
    let mut plan = Plan::new(
        one("nightly", "nightly-back"),
        &held,
        vec![file("nightly", PREDECESSOR)],
    );
    plan.to = v("0.9.0");
    let planned = plan.run().map_err(|refused| format!("{refused:?}"))?;
    assert_eq!(
        planned.first().map(|successor| successor.step),
        Some(ReleaseStep::Major)
    );
    Ok(())
}

#[test]
fn a_key_that_is_not_a_usable_job_name_is_refused() -> TestResult {
    let held = running()?;
    for key in ["current", "Upper", "has space", ""] {
        let plan = Plan::new(one("nightly", key), &held, vec![file("nightly", PREDECESSOR)]);
        assert_eq!(
            codes(&refusals(plan.run())),
            [(Some("nightly"), "workflow/job-key-invalid")],
            "{key:?}"
        );
    }
    Ok(())
}

#[test]
fn a_file_already_at_the_new_key_is_refused_as_existing() -> TestResult {
    let held = running()?;
    let entries = vec![
        file("nightly", PREDECESSOR),
        file("nightly-next", "version = \"1.0.0\"\n"),
    ];
    let plan = Plan::new(one("nightly", "nightly-next"), &held, entries);
    assert_eq!(codes(&refusals(plan.run())), [(Some("nightly"), "workflow/job-exists")]);
    Ok(())
}

#[test]
fn an_unusable_file_at_the_new_key_is_refused_as_existing() -> TestResult {
    let held = running()?;
    let entries = vec![
        file("nightly", PREDECESSOR),
        JobFileEntry::Unusable {
            key: SharedJobKey {
                package: "acme".to_owned(),
                workflow: "sync".to_owned(),
                job: "nightly-next".to_owned(),
            },
            reason: UnusableReason::NotText,
        },
    ];
    let plan = Plan::new(one("nightly", "nightly-next"), &held, entries);
    assert_eq!(codes(&refusals(plan.run())), [(Some("nightly"), "workflow/job-exists")]);
    Ok(())
}

#[test]
fn a_key_another_file_is_tracked_under_is_taken() -> TestResult {
    let held = running()?;
    let entries = vec![
        file("nightly", PREDECESSOR),
        file("other-file", "version = \"1.0.0\"\nslug = \"nightly-next\"\n"),
    ];
    let plan = Plan::new(one("nightly", "nightly-next"), &held, entries);
    assert_eq!(
        codes(&refusals(plan.run())),
        [(Some("nightly"), "workflow/job-key-taken")]
    );
    Ok(())
}

#[test]
fn a_key_with_stored_state_and_no_file_is_refused() -> TestResult {
    let held = state(
        "[acme.sync]\ncurrent = \"1.0.0\"\nfuture = \"1.1.0\"\n\n[acme.sync.nightly]\nlifecycle = \"enabled\"\n\n[acme.sync.nightly-next]\nlifecycle = \"retired\"\n",
    )?;
    let plan = Plan::new(
        one("nightly", "nightly-next"),
        &held,
        vec![file("nightly", PREDECESSOR)],
    );
    assert_eq!(
        codes(&refusals(plan.run())),
        [(Some("nightly"), "workflow/job-state-remains")]
    );
    Ok(())
}

#[test]
fn a_key_a_standing_successor_names_is_taken() -> TestResult {
    let held = running()?;
    let entries = vec![
        file("nightly", PREDECESSOR),
        file("later", "version = \"1.1.0\"\npredecessor = \"nightly-next\"\n"),
    ];
    let plan = Plan::new(one("nightly", "nightly-next"), &held, entries);
    assert_eq!(
        codes(&refusals(plan.run())),
        [(Some("nightly"), "workflow/job-key-taken")]
    );
    Ok(())
}

#[test]
fn a_successor_the_store_would_not_load_is_refused() -> TestResult {
    let held = running()?;
    let plan = Plan::new(
        one("nightly", "nightly-next"),
        &held,
        vec![file("nightly", PREDECESSOR)],
    );
    let refused = refusals(plan.run_checking(&|_key, _text| Err(AuthoringError::invalid_file("no good"))));
    assert_eq!(codes(&refused), [(Some("nightly"), "workflow/job-file-invalid")]);
    Ok(())
}

fn fleet() -> Result<JobsState, toml::de::Error> {
    state(
        "[acme.sync]\ncurrent = \"1.0.0\"\nprevious = \"0.9.0\"\nfuture = \"2.0.0\"\n\n\
         [acme.sync.a]\nlifecycle = \"enabled\"\n\n\
         [acme.sync.b-v1-0-0]\nlifecycle = \"suspended\"\n\n\
         [acme.sync.c]\nlifecycle = \"draft\"\n\n\
         [acme.sync.d]\nlifecycle = \"retired\"\n\n\
         [acme.sync.e]\nlifecycle = \"enabled\"\n",
    )
}

fn fleet_files() -> Vec<JobFileEntry> {
    let on = |version: &str| format!("version = \"{version}\"\n");
    vec![
        file("a", &on("1.0.0")),
        file("b-v1-0-0", &on("1.0.0")),
        file("c", &on("1.0.0")),
        file("d", &on("1.0.0")),
        file("e", &on("0.9.0")),
    ]
}

#[test]
fn all_makes_a_successor_for_each_enabled_or_suspended_job_on_current() -> TestResult {
    let held = fleet()?;
    let mut plan = Plan::new(SuccessorScope::All, &held, fleet_files());
    plan.to = v("2.0.0");
    let planned = plan.run().map_err(|refused| format!("{refused:?}"))?;
    let made: Vec<(&str, &str, ReleaseStep)> = planned
        .iter()
        .map(|s| (s.predecessor.as_str(), s.key.as_str(), s.step))
        .collect();
    assert_eq!(
        made,
        [
            ("a", "a-v2-0-0", ReleaseStep::Major),
            ("b-v1-0-0", "b-v2-0-0", ReleaseStep::Major)
        ]
    );
    Ok(())
}

#[test]
fn all_with_nothing_to_do_makes_nothing() -> TestResult {
    let held = state("[acme.sync]\ncurrent = \"1.0.0\"\nfuture = \"1.1.0\"\n")?;
    let planned = Plan::new(SuccessorScope::All, &held, vec![file("a", PREDECESSOR)]).run();
    assert_eq!(planned.map_err(|refused| format!("{refused:?}"))?, Vec::new());
    Ok(())
}

#[test]
fn all_to_previous_makes_return_successors() -> TestResult {
    let held = fleet()?;
    let mut plan = Plan::new(SuccessorScope::All, &held, fleet_files());
    plan.to = v("0.9.0");
    let planned = plan.run().map_err(|refused| format!("{refused:?}"))?;
    let keys: Vec<&str> = planned.iter().map(|s| s.key.as_str()).collect();
    assert_eq!(keys, ["a-v0-9-0", "b-v0-9-0"]);
    Ok(())
}

#[test]
fn all_writes_nothing_and_reports_every_refusal_when_any_job_is_refused() -> TestResult {
    let held = fleet()?;
    let mut entries = fleet_files();
    entries.push(file("a-v2-0-0", "version = \"2.0.0\"\n"));
    entries.push(file("b-later", "version = \"2.0.0\"\npredecessor = \"b-v1-0-0\"\n"));
    let mut plan = Plan::new(SuccessorScope::All, &held, entries);
    plan.to = v("2.0.0");
    let refused = refusals(plan.run());
    assert_eq!(
        codes(&refused),
        [
            (Some("a"), "workflow/job-exists"),
            (Some("b-v1-0-0"), "workflow/transition-refused")
        ]
    );
    Ok(())
}

#[test]
fn all_refuses_two_predecessors_that_would_share_a_new_key() -> TestResult {
    let held = state(
        "[acme.sync]\ncurrent = \"1.0.0\"\nfuture = \"2.0.0\"\n\n[acme.sync.x]\nlifecycle = \"enabled\"\n\n[acme.sync.x-v1-0-0]\nlifecycle = \"enabled\"\n",
    )?;
    let entries = vec![
        file("x", "version = \"1.0.0\"\n"),
        file("x-v1-0-0", "version = \"1.0.0\"\n"),
    ];
    let mut plan = Plan::new(SuccessorScope::All, &held, entries);
    plan.to = v("2.0.0");
    assert_eq!(
        codes(&refusals(plan.run())),
        [(Some("x-v1-0-0"), "workflow/job-key-taken")]
    );
    Ok(())
}

#[test]
fn a_refusal_for_the_whole_request_is_not_repeated_for_each_job() -> TestResult {
    let held = fleet()?;
    let mut plan = Plan::new(SuccessorScope::All, &held, fleet_files());
    plan.to = v("2.0.0");
    plan.presence = Presence::Missing;
    assert_eq!(codes(&refusals(plan.run())), [(None, "workflow/package-unavailable")]);
    let refusal = SuccessorRefusal::NoFutureRelease;
    assert_eq!(refusal.code(), "workflow/package-unavailable");
    Ok(())
}

fn unusable(stem: &str) -> JobFileEntry {
    JobFileEntry::Unusable {
        key: SharedJobKey {
            package: "acme".to_owned(),
            workflow: "sync".to_owned(),
            job: stem.to_owned(),
        },
        reason: UnusableReason::NotText,
    }
}

#[test]
fn all_reports_an_enabled_job_whose_file_is_unusable_and_writes_nothing() -> TestResult {
    let held = fleet()?;
    let mut entries = fleet_files();
    entries.retain(|entry| !matches!(entry, JobFileEntry::Usable(file) if file.key.job == "a"));
    entries.push(unusable("a"));
    let mut plan = Plan::new(SuccessorScope::All, &held, entries);
    plan.to = v("2.0.0");

    let refused = refusals(plan.run());

    assert_eq!(codes(&refused), [(Some("a"), "workflow/job-file-invalid")]);
    Ok(())
}

#[test]
fn all_reports_an_enabled_job_whose_file_names_no_version_or_is_missing() -> TestResult {
    let held = fleet()?;
    let mut entries = fleet_files();
    entries.retain(
        |entry| !matches!(entry, JobFileEntry::Usable(file) if file.key.job == "a" || file.key.job == "b-v1-0-0"),
    );
    entries.push(file("a", "name = \"A\"\n"));
    let mut plan = Plan::new(SuccessorScope::All, &held, entries);
    plan.to = v("2.0.0");

    let refused = refusals(plan.run());

    assert_eq!(
        codes(&refused),
        [
            (Some("a"), "workflow/job-file-invalid"),
            (Some("b-v1-0-0"), "workflow/job-not-found")
        ]
    );
    Ok(())
}

/// `nightly` was retired when `nightly-s` was enabled, and `nightly-s` has since been retired as well.
fn replaced_then_retired() -> Result<JobsState, toml::de::Error> {
    state(
        "[acme.sync]\ncurrent = \"1.0.0\"\nfuture = \"1.1.0\"\n\n\
         [acme.sync.nightly]\nlifecycle = \"retired\"\nidentity = \"job-1\"\n\n\
         [acme.sync.nightly.retirement]\nby = \"system\"\ncause = \"successor-enabled\"\nat = \"2026-11-03T03:00:00Z\"\n\
         replaced_by = { job = \"nightly-s\", identity = \"job-2\" }\n\n\
         [acme.sync.nightly-s]\nlifecycle = \"retired\"\nidentity = \"job-2\"\n",
    )
}

#[test]
fn a_successor_that_was_enabled_still_stands_after_it_is_retired() -> TestResult {
    let held = replaced_then_retired()?;
    let entries = vec![
        file("nightly", PREDECESSOR),
        file("nightly-s", "version = \"1.0.0\"\npredecessor = \"nightly\"\n"),
    ];

    let refused = refusals(Plan::new(one("nightly", "nightly-next"), &held, entries).run());

    assert_eq!(codes(&refused), [(Some("nightly"), "workflow/transition-refused")]);
    assert!(
        refused[0].error.to_string().contains("nightly-s"),
        "{}",
        refused[0].error
    );
    Ok(())
}

#[test]
fn a_successor_that_was_enabled_stands_even_when_its_file_is_gone() -> TestResult {
    let held = replaced_then_retired()?;

    let refused = refusals(
        Plan::new(
            one("nightly", "nightly-next"),
            &held,
            vec![file("nightly", PREDECESSOR)],
        )
        .run(),
    );

    assert_eq!(codes(&refused), [(Some("nightly"), "workflow/transition-refused")]);
    Ok(())
}

#[test]
fn the_comments_on_the_version_predecessor_and_slug_lines_are_kept() -> TestResult {
    let held = running()?;
    let text = "version = \"1.0.0\"   # the release\npredecessor = \"older\" # who came before\nslug = \"nightly\"  # its key\n";
    let planned = Plan::new(one("nightly", "nightly-next"), &held, vec![file("nightly-file", text)])
        .run()
        .map_err(|refused| format!("{refused:?}"))?;
    let kept = planned
        .first()
        .map(|successor| successor.text.as_str())
        .unwrap_or_default();
    assert_eq!(
        kept,
        "version = \"1.1.0\"   # the release\npredecessor = \"nightly\" # who came before\nslug = \"nightly-next\"  # its key\n"
    );
    Ok(())
}

#[test]
fn a_package_store_that_cannot_be_read_is_an_outage_not_a_missing_release() -> TestResult {
    let held = running()?;
    let mut plan = Plan::new(
        one("nightly", "nightly-next"),
        &held,
        vec![file("nightly", PREDECESSOR)],
    );
    plan.presence = Presence::Unreadable;

    let refused = refusals(plan.run());

    assert_eq!(codes(&refused), [(None, "workflow/job-store-unavailable")]);
    assert!(
        refused[0].error.to_string().contains("package store"),
        "{}",
        refused[0].error
    );
    Ok(())
}

#[test]
fn a_new_key_that_belongs_to_a_retired_job_is_refused_with_the_way_on() -> TestResult {
    let held = state(
        "[acme.sync]\ncurrent = \"1.0.0\"\nfuture = \"1.1.0\"\n\n[acme.sync.nightly]\nlifecycle = \"enabled\"\n\n[acme.sync.nightly-next]\nlifecycle = \"retired\"\n",
    )?;
    let entries = vec![
        file("nightly", PREDECESSOR),
        file("nightly-next", "version = \"1.0.0\"\n"),
    ];

    let refused = refusals(Plan::new(one("nightly", "nightly-next"), &held, entries).run());

    assert_eq!(codes(&refused), [(Some("nightly"), "workflow/job-exists")]);
    let message = refused[0].error.to_string();
    assert!(message.contains("delete that job"), "{message}");
    assert!(message.contains("another key"), "{message}");
    Ok(())
}

fn fingerprint() -> contextos_workflows::JobFingerprint {
    contextos_workflows::JobFingerprint::from(&contextos_workflows::ReleaseFingerprintInput {
        store: contextos_workflows::StoreKind::Host,
        package: "acme",
        release: "1.0.0",
        digest: "sha256:0",
        job_toml: "()",
        release_manifest: "",
        libraries: &[],
    })
}

/// Releases 1.0.0 current and 1.1.0 future, with `nightly` taken through its real transitions: through enabling when
/// `enabled_first`, then retired by the operator.
fn retired(enabled_first: bool) -> Result<JobsState, Box<dyn std::error::Error>> {
    use contextos_workflows::{Actor, JobSchedule, Transition};
    let mut held = state("[acme.sync]\ncurrent = \"1.0.0\"\nfuture = \"1.1.0\"\n")?;
    let now = time::macros::datetime!(2026-11-04 03:00:00 UTC);
    if enabled_first {
        let steps = [
            (Actor::Validator, Transition::Validated(fingerprint())),
            (Actor::DryRun, Transition::DryRunCompleted(fingerprint())),
            (
                Actor::Operator,
                Transition::Enable(JobSchedule::try_from("0 3 * * *")?, fingerprint()),
            ),
        ];
        for (actor, transition) in &steps {
            held.transition("acme", "sync", "nightly", *actor, transition, now)?;
        }
    }
    held.transition("acme", "sync", "nightly", Actor::Operator, &Transition::Retire, now)?;
    Ok(held)
}

#[test]
fn a_job_retired_without_ever_being_enabled_has_no_successor_made_and_the_refusal_gives_the_reason() -> TestResult {
    let held = retired(false)?;
    let plan = Plan::new(
        one("nightly", "nightly-next"),
        &held,
        vec![file("nightly", PREDECESSOR)],
    );

    let refused = refusals(plan.run());

    assert_eq!(codes(&refused), [(Some("nightly"), "workflow/transition-refused")]);
    let message = refused[0].error.to_string();
    assert!(
        message.contains("never") && message.contains("enabled"),
        "the refusal names that the job was never enabled: {message}"
    );
    Ok(())
}

#[test]
fn a_job_retired_after_it_was_enabled_may_have_a_successor() -> TestResult {
    let held = retired(true)?;
    let planned = Plan::new(
        one("nightly", "nightly-next"),
        &held,
        vec![file("nightly", PREDECESSOR)],
    )
    .run();
    assert!(planned.is_ok(), "{:?}", planned.err());
    Ok(())
}

#[test]
fn all_makes_no_successor_for_a_retired_job_whether_or_not_it_was_ever_enabled() -> TestResult {
    for enabled_first in [false, true] {
        let held = retired(enabled_first)?;
        let planned = Plan::new(SuccessorScope::All, &held, vec![file("nightly", PREDECESSOR)]).run();
        assert!(
            planned.as_ref().is_ok_and(Vec::is_empty),
            "enabled_first={enabled_first}: {planned:?}"
        );
    }
    Ok(())
}
