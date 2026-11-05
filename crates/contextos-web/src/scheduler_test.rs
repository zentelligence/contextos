use time::macros::datetime;

use super::*;

fn daily_at_3am() -> Result<CronSchedule, String> {
    CronSchedule::parse("0 3 * * *").map_err(|error| error.to_string())
}

#[test]
fn a_job_with_no_recorded_fire_is_due_when_its_occurrence_falls_within_the_lookback_window() -> Result<(), String> {
    // 3am fell one minute ago, well within `LOOKBACK_SECS` of "now".
    let now = datetime!(2026-09-08 03:01:00 UTC);
    let due = due_tick(&daily_at_3am()?, None, now, &contextos_core::OperatorTimeZone::utc());
    assert_eq!(due, Some(datetime!(2026-09-08 03:00:00 UTC)));
    Ok(())
}

#[test]
fn a_job_with_no_recorded_fire_is_not_retroactively_due_for_an_occurrence_before_the_lookback_window()
-> Result<(), String> {
    // 3am was hours ago: a freshly-registered job (or a restart after
    // real downtime) must not fire every occurrence it missed
    // (a missed tick is simply not run).
    let now = datetime!(2026-09-08 10:00:00 UTC);
    let due = due_tick(&daily_at_3am()?, None, now, &contextos_core::OperatorTimeZone::utc());
    assert_eq!(due, None);
    Ok(())
}

#[test]
fn a_job_is_not_due_before_its_next_occurrence() -> Result<(), String> {
    let now = datetime!(2026-09-08 02:00:00 UTC);
    let due = due_tick(
        &daily_at_3am()?,
        Some(datetime!(2026-09-07 03:00:00 UTC)),
        now,
        &contextos_core::OperatorTimeZone::utc(),
    );
    assert_eq!(due, None);
    Ok(())
}

#[test]
fn a_job_becomes_due_once_its_next_occurrence_after_the_last_recorded_fire_arrives() -> Result<(), String> {
    let now = datetime!(2026-09-08 03:00:30 UTC);
    let due = due_tick(
        &daily_at_3am()?,
        Some(datetime!(2026-09-07 03:00:00 UTC)),
        now,
        &contextos_core::OperatorTimeZone::utc(),
    );
    assert_eq!(due, Some(datetime!(2026-09-08 03:00:00 UTC)));
    Ok(())
}

#[test]
fn a_tick_already_recorded_as_the_last_fire_is_not_due_again() -> Result<(), String> {
    // `next_fire_after` searches strictly after `after`, so once
    // `last_scheduled_fire` is set to a tick, that same tick can never be
    // returned as due again; the next call only surfaces the *following*
    // occurrence.
    let now = datetime!(2026-09-08 03:00:00 UTC);
    let due = due_tick(
        &daily_at_3am()?,
        Some(datetime!(2026-09-08 03:00:00 UTC)),
        now,
        &contextos_core::OperatorTimeZone::utc(),
    );
    assert_eq!(due, None);
    Ok(())
}

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
        timeout_secs: 300,
        calls: contextos_workflows::call_rules::CallRules::default(),
        inputs: contextos_workflows::ValidatedInputs::default(),
        declared_inputs: contextos_workflows::DeclaredInputs::default(),
        outputs: contextos_workflows::DeclaredOutputs::default(),
        workflow_log_level: None,
        log_level: None,
        ready: false,
        file_name: "nightly.toml".to_owned(),
    }
}

mod lifecycle_gate {
    use contextos_workflows::{Actor, JobFingerprint, JobLifecycle, JobSchedule, SuspensionReason, Transition};

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

    use super::*;

    const NOW: OffsetDateTime = datetime!(2026-10-01 03:00:00 UTC);

    /// A state in which the one job is in `target`, taken there through the real transitions.
    fn state_in(target: JobLifecycle) -> Result<JobsState, Box<dyn std::error::Error>> {
        let mut state = JobsState::default();
        let schedule = JobSchedule::try_from("0 3 * * *")?;
        let path: &[(Actor, Transition)] = &[
            (Actor::Validator, Transition::Validated(version())),
            (Actor::DryRun, Transition::DryRunCompleted(version())),
            (Actor::Operator, Transition::Enable(schedule, version())),
            (
                Actor::Operator,
                Transition::Suspend(contextos_workflows::Suspension::operator(SuspensionReason::try_from(
                    "held",
                )?)),
            ),
            (Actor::Operator, Transition::Retire),
        ];
        let stops = [
            JobLifecycle::Validated,
            JobLifecycle::Registered,
            JobLifecycle::Enabled,
            JobLifecycle::Suspended,
            JobLifecycle::Retired,
        ];
        if target == JobLifecycle::Draft {
            return Ok(state);
        }
        for ((actor, transition), stop) in path.iter().zip(stops) {
            state.transition("ops", "sync-crm", "nightly-sync", *actor, transition, NOW)?;
            if stop == target {
                break;
            }
        }
        Ok(state)
    }

    #[test]
    fn only_an_enabled_job_is_handed_to_the_scheduler_with_its_stored_schedule()
    -> Result<(), Box<dyn std::error::Error>> {
        for lifecycle in [
            JobLifecycle::Draft,
            JobLifecycle::Validated,
            JobLifecycle::Registered,
            JobLifecycle::Enabled,
            JobLifecycle::Suspended,
            JobLifecycle::Retired,
        ] {
            let state = state_in(lifecycle)?;

            let due = scheduled_jobs(vec![registered_job()], &state);

            if lifecycle == JobLifecycle::Enabled {
                assert_eq!(due.len(), 1, "{lifecycle:?}");
                assert_eq!(due[0].1, "0 3 * * *");
            } else {
                assert!(due.is_empty(), "{lifecycle:?} must not be scheduled");
            }
        }
        Ok(())
    }

    #[test]
    fn a_live_run_by_hand_is_allowed_only_when_registered_enabled_or_suspended() {
        for (lifecycle, allowed) in [
            (JobLifecycle::Draft, false),
            (JobLifecycle::Validated, false),
            (JobLifecycle::Registered, true),
            (JobLifecycle::Enabled, true),
            (JobLifecycle::Suspended, true),
            (JobLifecycle::Retired, false),
        ] {
            assert_eq!(run_allowed(lifecycle, RunKind::Live), allowed, "{lifecycle:?}");
        }
    }

    #[test]
    fn a_dry_run_is_allowed_in_every_state_but_retired() {
        for lifecycle in [
            JobLifecycle::Draft,
            JobLifecycle::Validated,
            JobLifecycle::Registered,
            JobLifecycle::Enabled,
            JobLifecycle::Suspended,
        ] {
            assert!(run_allowed(lifecycle, RunKind::Dry), "{lifecycle:?}");
        }
        assert!(!run_allowed(JobLifecycle::Retired, RunKind::Dry));
    }
}

#[test]
fn a_daily_seven_am_schedule_is_due_at_seven_am_where_the_operator_is() -> Result<(), Box<dyn std::error::Error>> {
    let sydney =
        contextos_core::OperatorTimeZone::from(contextos_core::ConfiguredTimeZone::try_from("Australia/Sydney")?);
    let seven_am = CronSchedule::parse("0 7 * * *")?;
    // 20:00:30 UTC on 8 October is 07:00:30 on 9 October in Sydney, under daylight saving (+11:00).
    let now = datetime!(2026-10-08 20:00:30 UTC);

    assert_eq!(
        due_tick(&seven_am, None, now, &sydney),
        Some(datetime!(2026-10-09 07:00 +11:00))
    );
    assert_eq!(
        due_tick(&seven_am, None, now, &contextos_core::OperatorTimeZone::utc()),
        None,
        "on UTC's wall clock it is 20:00, long past 07:00 and not yet the next one"
    );
    Ok(())
}

#[test]
fn only_the_latest_of_several_missed_ticks_is_due() -> Result<(), String> {
    // Hourly, last fired at 03:00, and nothing looked until 06:30: the 04:00, 05:00, and 06:00 ticks were all missed,
    // and only the latest runs, so a backlog does not fire one tick per pass.
    let hourly = CronSchedule::parse("0 * * * *").map_err(|error| error.to_string())?;
    let due = due_tick(
        &hourly,
        Some(datetime!(2026-09-08 03:00:00 UTC)),
        datetime!(2026-09-08 06:30:00 UTC),
        &contextos_core::OperatorTimeZone::utc(),
    );
    assert_eq!(due, Some(datetime!(2026-09-08 06:00:00 UTC)));
    Ok(())
}

#[test]
fn a_single_due_tick_is_due_as_before() -> Result<(), String> {
    let hourly = CronSchedule::parse("0 * * * *").map_err(|error| error.to_string())?;
    let due = due_tick(
        &hourly,
        Some(datetime!(2026-09-08 03:00:00 UTC)),
        datetime!(2026-09-08 04:00:20 UTC),
        &contextos_core::OperatorTimeZone::utc(),
    );
    assert_eq!(due, Some(datetime!(2026-09-08 04:00:00 UTC)));
    Ok(())
}
