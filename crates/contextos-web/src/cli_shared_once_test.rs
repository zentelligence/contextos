//! A one-off run of a shared workflow takes the release its choice names from what the host's state records.

use super::*;

impl Host {
    /// Takes the draft job through a passing validation, a dry run, and the real enabling on release 1.0.0, so the state
    /// records that release as the workflow's current one.
    fn enable_on_current_release(&self) -> Result<(), Box<dyn std::error::Error>> {
        let version = self.version()?;
        let now = self.now;
        self.state
            .update(|jobs: &mut JobsState| -> Result<(), Box<dyn std::error::Error>> {
                let mut result = contextos_workflows::StoredValidation::from(contextos_workflows::ValidationRecord {
                    report: &contextos_workflows::ValidationReport::default(),
                    fingerprint: Some(version.clone()),
                    at: now,
                });
                result.made_from = Some(contextos_workflows::VersionBasis {
                    package: "acme".to_owned(),
                    release: semver::Version::new(1, 0, 0),
                    lock: LibraryLock::default(),
                    predecessor: None,
                });
                jobs.keep_validation("acme", "sync", "nightly", result, now);
                jobs.transition(
                    "acme",
                    "sync",
                    "nightly",
                    Actor::DryRun,
                    &Transition::DryRunCompleted(version.clone()),
                    now,
                )?;
                jobs.enable(
                    contextos_workflows::JobScope {
                        package: "acme",
                        workflow: "sync",
                        job: "nightly",
                    },
                    contextos_workflows::EnableRequest {
                        schedule: Some(JobSchedule::try_from("0 3 * * *")?),
                        version,
                        predecessor: None,
                        installed: &[semver::Version::new(1, 0, 0)],
                        predecessor_running: false,
                    },
                    now,
                )?;
                Ok(())
            })??;
        Ok(())
    }

    async fn run_once_choosing(
        &self,
        release: contextos_workflows::ReleaseChoice,
    ) -> Result<crate::run_report::RunReport, CliSharedError> {
        let clients = Arc::new(
            McpClientSet::connect(&[])
                .await
                .map_err(|_| CliSharedError::NoClients)?,
        );
        run_shared_once_reported(
            self.host(clients),
            OneOffRequest {
                package: "acme".to_owned(),
                workflow: "sync".to_owned(),
                release,
                arguments: Vec::new(),
                dry_run_scenario: None,
            },
        )
        .await
    }
}

#[tokio::test]
async fn a_one_off_run_of_current_runs_the_release_the_state_records_as_current()
-> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("version = \"1.0.0\"\n")?;
    host.enable_on_current_release()?;

    let report = host
        .run_once_choosing(contextos_workflows::ReleaseChoice::Current)
        .await?;

    assert!(matches!(report.outcome, RunOutcome::Succeeded), "{:?}", report.outcome);
    assert_eq!(report.release.map(|release| release.version), Some("1.0.0".to_owned()));
    Ok(())
}

#[tokio::test]
async fn a_one_off_run_of_current_is_refused_before_any_call_when_the_workflow_has_no_current_release()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.add_job("version = \"1.0.0\"\n")?;

    let report = host
        .run_once_choosing(contextos_workflows::ReleaseChoice::Current)
        .await?;

    assert!(
        failed_with(&report.outcome, WorkflowFailureKind::JobNotRunnable),
        "{:?}",
        report.outcome
    );
    assert!(!report.started);
    assert_eq!(host.calls(), 0);
    Ok(())
}

#[tokio::test]
async fn a_one_off_run_of_previous_or_future_is_refused_before_any_call_when_the_state_records_neither()
-> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("version = \"1.0.0\"\n")?;
    host.enable_on_current_release()?;

    for choice in [
        contextos_workflows::ReleaseChoice::Previous,
        contextos_workflows::ReleaseChoice::Future,
    ] {
        let report = host.run_once_choosing(choice.clone()).await?;

        assert!(
            failed_with(&report.outcome, WorkflowFailureKind::JobNotRunnable),
            "{choice:?}: {:?}",
            report.outcome
        );
        assert!(!report.started, "{choice:?}");
    }
    assert_eq!(host.calls(), 0);
    Ok(())
}

#[tokio::test]
async fn a_one_off_run_of_a_retired_release_is_refused_before_any_call() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("version = \"1.0.0\"\n")?;
    host.enable_on_current_release()?;

    let report = host
        .run_once_choosing(contextos_workflows::ReleaseChoice::Version(semver::Version::new(
            0, 9, 0,
        )))
        .await?;

    assert!(
        failed_with(&report.outcome, WorkflowFailureKind::JobNotRunnable),
        "{:?}",
        report.outcome
    );
    assert_eq!(host.calls(), 0);
    Ok(())
}
