//! Starting a vault job's run by hand, for `contextos-web jobs run` and for a job's page.
//!
//! A run is prepared first, which claims the job and creates its record and answers at once with why it could not start,
//! then driven to its end. The command line drives it where it stands and reports the outcome. A page drives it on a
//! task of its own, so the request that started it is answered while it runs. A scheduled run is prepared and driven the
//! same way. All of them take the job's claim, so a run of one job never overlaps another run of it, whichever process
//! started it.
//!
//! A dry-run scenario is vault content (`registry/workflows/{{package}}/{{workflow}}/tests/{{name}}.toml`),
//! read the same way every other vault-resident file this crate touches
//! is: through the primary MCP session, never `std::fs` directly.

use std::sync::Arc;

use contextos_workflows::{
    Clock, InputOverrides, JobFingerprint, JobKey, JobLifecycle, McpCaller, OverriddenInputs, RunFailure, RunOutcome,
    WorkflowFailureKind, run_record_path,
};
use serde::Deserialize;
use serde_json::{Map, Value};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::dry_run_caller::DryRunMcpCaller;
use crate::dry_run_scenario::{Scenario, ScenarioError, ScenarioMatcher};
use crate::job_lifecycle::NotRegistered;
use crate::job_validation::{DryRunRecorded, record_dry_run};
use crate::jobs::{RegisteredJob, WebMcpCaller, WorkflowFolder};
use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
pub use crate::run_records::RecordWriteError;
use crate::run_records::{
    RunCompletion, RunRecordHeader, Trigger, complete_run, create_run_record, log_run_completion, status_of,
};
use crate::run_report::{RunControls, RunReport};
use crate::run_settings::HostContext;
use crate::run_summary::{HostWarning, RecordLocation, Registration, VaultRecord, codes};
use crate::scheduler::{self, RunExecution, RunKind};
use crate::vault_content::read_attached_text;
use crate::vault_runs::{ClaimRunError, ClaimedRun, claim_run};

const TESTS_DIR_NAME: &str = "tests";

/// Why a CLI-triggered run could not even start, or could not load its
/// own requested dry-run scenario.
#[derive(Debug, Error)]
pub enum CliRunError {
    #[error("the primary MCP server is not reachable")]
    Unreachable,
    #[error(
        "no job named {slug:?} is registered under workflow {workflow_slug:?} of package {package:?} in vault {vault_name:?}"
    )]
    JobNotFound {
        vault_name: String,
        package: String,
        workflow_slug: String,
        slug: String,
    },
    #[error("workflow/job-busy: a run of this job is already active")]
    Busy,
    #[error("the host's claim or run state for this job could not be used; see the log for the cause")]
    HostUnavailable,
    #[error(
        "workflow/job-not-runnable: the job is {state}, so a {kind} run is not available; {remedy}{why}",
        kind = if *dry_run { "dry" } else { "live" },
        remedy = if *dry_run {
            "re-activate it first"
        } else {
            "a job is run by hand once it is registered, which needs it validated and then dry-run"
        },
        why = suspension.as_ref().map(|record| format!(" (suspension: {record})")).unwrap_or_default()
    )]
    NotRunnable {
        state: JobLifecycle,
        dry_run: bool,
        /// Why the job is suspended, when it is and its state says.
        suspension: Option<contextos_workflows::SuspensionRecord>,
    },
    /// The job is one of a chain and the other job holds it, so a live run by hand is not available.
    #[error(
        "workflow/job-not-runnable: a live run by hand is not available because {0}; a chain of jobs has one job in use at a time, and a successor is tried by dry runs"
    )]
    ChainHeld(contextos_workflows::ChainHold),
    #[error("workflow/state-changed: the job state changed while the run was being claimed; try again")]
    StateChanged,
    #[error("no valid workflow {workflow_slug:?} of package {package:?} is registered in vault {vault_name:?}")]
    WorkflowNotFound {
        vault_name: String,
        package: String,
        workflow_slug: String,
    },
    #[error("workflow {workflow_slug:?} has no tests/ directory to load a dry-run scenario from")]
    NoScenariosAvailable { workflow_slug: String },
    #[error("workflow {workflow_slug:?} has {count} scenarios under tests/; name one explicitly with --scenario")]
    ScenarioAmbiguous { workflow_slug: String, count: usize },
    #[error("no scenario named {scenario:?} exists under workflow {workflow_slug:?}'s own tests/ directory")]
    ScenarioNotFound { workflow_slug: String, scenario: String },
    #[error("scenario {scenario:?} could not be read: {source}")]
    ScenarioUnreadable {
        scenario: String,
        #[source]
        source: McpCallError,
    },
    #[error("scenario {scenario:?} is invalid: {source}")]
    ScenarioInvalid {
        scenario: String,
        #[source]
        source: Box<ScenarioError>,
    },
    #[error("the run record could not be created: {0}")]
    RunRecordFailed(#[source] RecordWriteError),
    /// An input given for this run only is not one the job's workflow can take. The message never quotes the value.
    #[error("workflow/input-invalid: {0}")]
    InputInvalid(String),
    /// The job's owner is missing or not a member of the team, so it is not run live. Names the slug, never a member.
    #[error("workflow/owner-unresolved: the job's owner {} is not a member of the team", .owner.as_ref().map_or_else(|| "is not declared, so it".to_owned(), |slug| format!("{slug:?}")))]
    OwnerUnresolved {
        owner: Option<contextos_core::DeclaredOwner>,
    },
    /// The vault's team is invalid, so a job is not run live. Names the rule, never a member.
    #[error("workflow/team-invalid: the vault's team breaks the rule {}", .rule.as_str())]
    TeamInvalid { rule: contextos_core::TeamRule },
}

/// Options a CLI-triggered run accepts, independent and freely
/// combinable.
#[derive(Clone, Debug, Default)]
pub struct CliRunOptions {
    /// `Some(true)` to dry-run; the inner value names a specific scenario
    /// under the workflow's own `tests/` directory, or `None` to require
    /// exactly one file there.
    pub dry_run_scenario: Option<Option<String>>,
    /// Whether a run record is written under `memory/jobs/...` and an
    /// operation-log entry appended.
    pub record: bool,
}

/// Which scenario a dry run uses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ScenarioChoice {
    /// The only scenario the workflow has, which is refused when it has none or several.
    Only,
    Named(String),
}

impl ScenarioChoice {
    /// The name to look for, or none when the only scenario is wanted.
    fn name(&self) -> Option<&str> {
        match self {
            Self::Only => None,
            Self::Named(name) => Some(name),
        }
    }
}

impl From<Option<String>> for ScenarioChoice {
    fn from(requested: Option<String>) -> Self {
        requested.map_or(Self::Only, Self::Named)
    }
}

/// What a run is, and how it is recorded.
#[derive(Clone, Debug)]
pub(crate) struct RunRequest {
    pub kind: RunKind,
    pub trigger: Trigger,
    /// The scenario of a dry run, or none for a live run.
    pub scenario: Option<ScenarioChoice>,
    /// Whether a record is written and an operation-log entry appended.
    pub record: bool,
    /// One `name=value` argument per input whose value replaces the job's own for this run only.
    pub overrides: Vec<String>,
    /// Cancelling it stops the run: the operator pressed Ctrl-C, or the process was asked to end.
    pub cancel: CancellationToken,
}

impl RunRequest {
    /// A live run the scheduler starts, which is always recorded.
    pub(crate) fn scheduled() -> Self {
        Self {
            kind: RunKind::Live,
            trigger: Trigger::Scheduled,
            scenario: None,
            record: true,
            overrides: Vec::new(),
            cancel: CancellationToken::new(),
        }
    }

    /// A run by hand, live or, with `scenario`, against a scenario. Always recorded, so a page can show how it ended.
    pub(crate) fn by_hand(scenario: Option<ScenarioChoice>) -> Self {
        Self {
            kind: if scenario.is_some() {
                RunKind::Dry
            } else {
                RunKind::Live
            },
            trigger: Trigger::Manual,
            scenario,
            record: true,
            overrides: Vec::new(),
            cancel: CancellationToken::new(),
        }
    }
}

impl From<&CliRunOptions> for RunRequest {
    fn from(options: &CliRunOptions) -> Self {
        Self {
            kind: if options.dry_run_scenario.is_some() {
                RunKind::Dry
            } else {
                RunKind::Live
            },
            trigger: Trigger::Manual,
            scenario: options.dry_run_scenario.clone().map(ScenarioChoice::from),
            record: options.record,
            overrides: Vec::new(),
            cancel: CancellationToken::new(),
        }
    }
}

/// A run that holds its claim and has its number, ready to be driven to its end.
pub(crate) struct PreparedRun {
    clients: Arc<McpClientSet>,
    host: HostContext,
    vault_name: String,
    request: RunRequest,
    claimed: ClaimedRun,
    caller: Arc<dyn McpCaller>,
    matcher: Option<Arc<ScenarioMatcher>>,
    /// The record a live run completes. A dry run's record is written when it ends.
    run_path: Option<String>,
    /// The input names given on the command line for this run.
    overridden: Vec<String>,
    /// The name of the scenario a dry run uses.
    scenario_name: Option<String>,
}

impl PreparedRun {
    pub(crate) const fn run_id(&self) -> u64 {
        self.claimed.run_id
    }

    /// Runs to the end and says how it ended. A live run releases its claim and settles its first-run trial. A dry run
    /// takes no claim, writes its record when it ends, and registers the job when it completes.
    pub(crate) async fn drive(self) -> RunOutcome {
        Box::pin(self.drive_reported()).await.outcome
    }

    /// As [`PreparedRun::drive`], reporting what the run came to: its number and record, outputs, and what a dry run
    /// did to the job's registration.
    pub(crate) async fn drive_reported(self) -> RunReport {
        let Self {
            clients,
            host,
            vault_name,
            request,
            claimed,
            caller,
            matcher,
            run_path,
            overridden,
            scenario_name,
        } = self;
        let ClaimedRun {
            client,
            job,
            run_id,
            started,
            daily_partition,
            claim,
        } = claimed;
        let clock = Arc::clone(&host.clock);
        let started_at = clock.now();
        let owner = job.owner.as_ref().map(|owner| owner.name.clone());
        let execution = RunExecution {
            caller,
            clients,
            host,
            vault_name: vault_name.clone(),
            job: job.clone(),
            run_id,
            run_path: run_path.clone(),
            claim,
            cancel: request.cancel.clone(),
            overridden: overridden.clone(),
            review: crate::run_snapshot::ReviewGate::from(request.trigger),
        };
        let mut report = match request.kind {
            RunKind::Live => {
                let mut report = scheduler::execute_live_run(execution).await;
                if let Some(path) = run_path {
                    report.record = Some(RecordLocation::from(VaultRecord {
                        vault: &vault_name,
                        path: &path,
                    }));
                }
                report
            }
            RunKind::Dry => {
                let mut report = scheduler::execute_run(execution).await;
                if request.record {
                    let written = DryRunRecord {
                        run_id,
                        ended: DryRunEnd {
                            started,
                            completed: clock.now(),
                            daily_partition,
                        },
                        overridden: &overridden,
                    };
                    attach_dry_record(&client, &vault_name, &job, written, &mut report).await;
                }
                if let Some(matcher) = matcher {
                    let registration = registration_of_dry_run(
                        &client,
                        &vault_name,
                        &job,
                        DryRunFacts {
                            version: report.version.clone(),
                            outcome: &report.outcome,
                            matcher: &matcher,
                            overridden: &overridden,
                            now: clock.now(),
                        },
                    )
                    .await;
                    if registration == Registration::NotRegistered(NotRegistered::StateUnavailable) {
                        report.host_warnings.push(HostWarning {
                            code: codes::STATE_UNAVAILABLE.to_owned(),
                            message: "the dry run's completion could not be saved to the job's state".to_owned(),
                        });
                    }
                    report.registration = Some(registration);
                }
                report
            }
        };
        report.run_id = Some(run_id);
        report.overridden_inputs = overridden;
        report.scenario = scenario_name;
        report.started_at = Some(started_at);
        if matches!(report.outcome, RunOutcome::Failed(_)) {
            report.owner = owner;
        }
        report
    }
}

/// Claims `package`/`workflow_slug`/`slug` in `vault_name` and prepares its run.
///
/// # Errors
///
/// Returns [`CliRunError`] naming why the run could not even start (unreachable, not found, not runnable, busy), why a
/// requested dry-run scenario could not be resolved or loaded, or why the run record could not be created. A claim
/// taken for a run that cannot start is released.
pub(crate) async fn prepare_run(
    clients: Arc<McpClientSet>,
    host: &HostContext,
    vault_name: &str,
    package: &str,
    workflow_slug: &str,
    slug: &str,
    request: RunRequest,
) -> Result<PreparedRun, CliRunError> {
    let claimed = claim_run(&clients, host, vault_name, package, workflow_slug, slug, request.kind)
        .await
        .map_err(|error| claim_failure(&error, request.kind, (vault_name, package, workflow_slug, slug)))?;
    prepare_claimed(clients, host, vault_name, claimed, request).await
}

/// Prepares a run whose claim and number are already taken.
///
/// # Errors
///
/// As [`prepare_run`], less the claim failures.
pub(crate) async fn prepare_claimed(
    clients: Arc<McpClientSet>,
    host: &HostContext,
    vault_name: &str,
    mut claimed: ClaimedRun,
    request: RunRequest,
) -> Result<PreparedRun, CliRunError> {
    // Overrides are checked first, before a scenario is read or a record is created, so a refusal leaves nothing
    // behind but the claim, which is released as `claimed` is dropped. The run number it took is not reused.
    let mut overridden = Vec::new();
    if !request.overrides.is_empty() {
        let merged = OverriddenInputs::try_from(InputOverrides {
            declared: claimed.job.declared_inputs.fields(),
            job_values: claimed.job.inputs.values().clone(),
            arguments: request.overrides.clone(),
        })
        .map_err(|error| CliRunError::InputInvalid(error.to_string()))?;
        claimed.job.inputs = merged.inputs;
        overridden = merged.overridden;
    }
    let mut matcher: Option<Arc<ScenarioMatcher>> = None;
    let mut scenario_name = None;
    let caller: Arc<dyn McpCaller> = match &request.scenario {
        Some(choice) => {
            let (name, scenario) = load_scenario(
                &claimed.client,
                vault_name,
                &claimed.job.workflow_slug,
                WorkflowFolder {
                    package: &claimed.job.package,
                    directory: &claimed.job.workflow_slug,
                },
                choice.name(),
            )
            .await?;
            scenario_name = Some(name);
            let shared = Arc::new(ScenarioMatcher::new(scenario));
            matcher = Some(Arc::clone(&shared));
            Arc::new(DryRunMcpCaller::sharing(Arc::clone(&clients), shared))
        }
        None => Arc::new(WebMcpCaller::new(Arc::clone(&clients))),
    };
    // A live run's record says `running` from the start, so a run that dies leaves the calls it made. A dry run's is
    // written when it ends, so one that dies leaves nothing for a later run to mistake for an abandoned live run.
    let run_path = if request.record && request.kind == RunKind::Live {
        let path = run_record_path(
            &claimed.job.package,
            &claimed.job.workflow_slug,
            &claimed.job.slug,
            claimed.run_id,
            claimed.started,
            claimed.daily_partition,
        );
        let header = RunRecordHeader {
            job: &claimed.job,
            run_id: claimed.run_id,
            trigger: request.trigger,
            status: "running",
            started: claimed.started,
            completed: None,
            dry_run: false,
            overrides: &overridden,
        };
        create_run_record(&claimed.client, vault_name, &path, &header)
            .await
            .map_err(CliRunError::RunRecordFailed)?;
        Some(path)
    } else {
        None
    };
    Ok(PreparedRun {
        clients,
        host: host.clone(),
        vault_name: vault_name.to_owned(),
        request,
        claimed,
        caller,
        matcher,
        run_path,
        overridden,
        scenario_name,
    })
}

/// Runs the job `job` in `vault_name` to completion, claiming and releasing the job's claim, and returns what
/// the run reported (run number, record, outputs, registration), taking the controls the command line has over it: input
/// overrides and a cancellation token.
///
/// An override the job's workflow cannot take refuses the run before any record, and is reported as a failed run
/// whose script never started.
///
/// # Errors
///
/// As [`prepare_run`].
pub async fn run_job_reported(
    clients: Arc<McpClientSet>,
    host: &HostContext,
    vault_name: &str,
    job: &JobKey,
    options: CliRunOptions,
    controls: RunControls,
) -> Result<RunReport, CliRunError> {
    let RunControls { overrides, cancel } = controls;
    let request = RunRequest {
        overrides,
        cancel,
        ..RunRequest::from(&options)
    };
    match prepare_run(
        clients,
        host,
        vault_name,
        &job.package,
        &job.workflow,
        &job.job,
        request,
    )
    .await
    {
        Ok(prepared) => Ok(Box::pin(prepared.drive_reported()).await),
        Err(CliRunError::InputInvalid(message)) => Ok(RunReport::ended_before_start(RunOutcome::Failed(RunFailure {
            kind: Some(WorkflowFailureKind::InputInvalid),
            message: format!("{}: {message}", WorkflowFailureKind::InputInvalid.code()),
        }))),
        Err(error) => Err(error),
    }
}

/// Why a run could not be claimed, as the command line reports it. `job` is the vault, package, workflow slug, and job slug
/// that were asked for.
fn claim_failure(error: &ClaimRunError, kind: RunKind, job: (&str, &str, &str, &str)) -> CliRunError {
    let (vault_name, package, workflow_slug, slug) = job;
    match error {
        ClaimRunError::Unreachable => CliRunError::Unreachable,
        ClaimRunError::JobNotFound => CliRunError::JobNotFound {
            vault_name: vault_name.to_owned(),
            package: package.to_owned(),
            workflow_slug: workflow_slug.to_owned(),
            slug: slug.to_owned(),
        },
        ClaimRunError::Busy => CliRunError::Busy,
        ClaimRunError::NotRunnable { state, suspension } => CliRunError::NotRunnable {
            state: *state,
            dry_run: kind == RunKind::Dry,
            suspension: suspension.clone(),
        },
        ClaimRunError::ChainHeld(hold) => CliRunError::ChainHeld(hold.clone()),
        ClaimRunError::HostUnavailable => CliRunError::HostUnavailable,
        ClaimRunError::OwnerUnresolved { owner } => CliRunError::OwnerUnresolved { owner: owner.clone() },
        ClaimRunError::TeamInvalid { rule } => CliRunError::TeamInvalid { rule: *rule },
    }
}

/// When a dry run started and ended, read from the host's local clock, and whether its record goes in a daily folder.
struct DryRunEnd {
    started: time::OffsetDateTime,
    completed: time::OffsetDateTime,
    daily_partition: bool,
}

/// What a finished dry run's record says besides its outcome.
struct DryRunRecord<'a> {
    run_id: u64,
    ended: DryRunEnd,
    /// The input names given on the command line, never their values.
    overridden: &'a [String],
}

/// What became of a finished dry run's record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DryRecord {
    /// The record could not be created, so there is nothing to point at.
    NotCreated,
    /// The record exists at this vault path, but could not be completed.
    NotCompleted(String),
    /// The record exists at this vault path and says how the run ended.
    Completed(String),
}

/// Notes in `report` what became of a dry run's record: where it is, or the host warning that says it is not whole.
pub(crate) fn note_dry_record(vault_name: &str, state: DryRecord, report: &mut RunReport) {
    match state {
        DryRecord::Completed(path) => {
            report.record = Some(RecordLocation::from(VaultRecord {
                vault: vault_name,
                path: &path,
            }));
        }
        DryRecord::NotCompleted(path) => {
            report.record = Some(RecordLocation::from(VaultRecord {
                vault: vault_name,
                path: &path,
            }));
            report.host_warnings.push(HostWarning {
                code: codes::RECORD_UNAVAILABLE.to_owned(),
                message: "the dry run's record could not be finished".to_owned(),
            });
        }
        DryRecord::NotCreated => report.host_warnings.push(HostWarning {
            code: codes::RECORD_UNAVAILABLE.to_owned(),
            message: "the dry run's record could not be written".to_owned(),
        }),
    }
}

/// Writes a dry run's record and notes where it is in `report`, or warns that it could not be written. The record names
/// the outputs the report says were set.
async fn attach_dry_record(
    client: &McpClient,
    vault_name: &str,
    job: &RegisteredJob,
    written: DryRunRecord<'_>,
    report: &mut RunReport,
) {
    let state = record_finished_dry_run(client, vault_name, job, written, report).await;
    note_dry_record(vault_name, state, report);
}

/// Writes a dry run's record once it has ended, labelled as a dry run so the history shows it was synthetic, and says
/// what became of it.
async fn record_finished_dry_run(
    client: &McpClient,
    vault_name: &str,
    job: &RegisteredJob,
    written: DryRunRecord<'_>,
    report: &RunReport,
) -> DryRecord {
    let (outcome, outputs) = (&report.outcome, &report.output_names);
    let DryRunRecord {
        run_id,
        ended: DryRunEnd {
            started,
            completed,
            daily_partition,
        },
        overridden,
    } = written;
    let path = run_record_path(
        &job.package,
        &job.workflow_slug,
        &job.slug,
        run_id,
        started,
        daily_partition,
    );
    let header = RunRecordHeader {
        job,
        run_id,
        trigger: Trigger::Manual,
        status: "running",
        started,
        completed: None,
        dry_run: true,
        overrides: overridden,
    };
    if let Err(error) = create_run_record(client, vault_name, &path, &header).await {
        tracing::warn!(vault = vault_name, job = %job.slug, run_id, %error, "a dry run's record could not be written");
        return DryRecord::NotCreated;
    }
    let completed_ok = match complete_run(
        client,
        vault_name,
        &path,
        &RunCompletion {
            outcome,
            outputs,
            completed,
            script_started: report.started,
            release: report.read_release.as_ref(),
            libraries: &report.libraries,
        },
    )
    .await
    {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(vault = vault_name, job = %job.slug, run_id, %error, "a dry run's record could not be completed");
            false
        }
    };
    log_run_completion(client, vault_name, &path, job, run_id, status_of(outcome)).await;
    if completed_ok {
        DryRecord::Completed(path)
    } else {
        DryRecord::NotCompleted(path)
    }
}

/// What a finished dry run of a saved job is judged by.
struct DryRunFacts<'a> {
    /// The version of the code the dry run read, when it could be worked out.
    version: Option<JobFingerprint>,
    outcome: &'a RunOutcome,
    matcher: &'a ScenarioMatcher,
    overridden: &'a [String],
    now: time::OffsetDateTime,
}

/// What a dry run did to the job's registration. A dry run completes when the script succeeded and used every call the
/// scenario declared. That completion, for the version that was dry-run, is what registers a validated job, and
/// anything short of it records nothing. A run with overrides never registers, because the job's version covers its own
/// values and a run with other values says nothing about it.
async fn registration_of_dry_run(
    client: &McpClient,
    vault_name: &str,
    job: &RegisteredJob,
    dry_run: DryRunFacts<'_>,
) -> Registration {
    let DryRunFacts {
        version,
        outcome,
        matcher,
        overridden,
        now,
    } = dry_run;
    if !matches!(outcome, RunOutcome::Succeeded | RunOutcome::Degraded(_)) {
        return Registration::NotRegistered(NotRegistered::DryRunNotCompleted);
    }
    if matcher.unconsumed() != 0 {
        return Registration::NotRegistered(NotRegistered::ScenarioCallsUnused);
    }
    if !overridden.is_empty() {
        return Registration::NotRegistered(NotRegistered::InputsOverridden);
    }
    let Some(version) = version else {
        return Registration::NotRegistered(NotRegistered::StateUnavailable);
    };
    match record_dry_run(client, vault_name, job, version, now).await {
        Ok(DryRunRecorded::Registered) => {
            tracing::info!(vault = vault_name, package = %job.package, workflow = %job.workflow_slug, job = %job.slug, "the dry run completed, so the job is registered");
            Registration::Registered
        }
        Ok(DryRunRecorded::NotRecorded(reason)) => {
            tracing::info!(vault = vault_name, package = %job.package, workflow = %job.workflow_slug, job = %job.slug, %reason, "the dry run completed and registered nothing");
            Registration::NotRegistered(reason)
        }
        Err(error) => {
            tracing::warn!(vault = vault_name, package = %job.package, workflow = %job.workflow_slug, job = %job.slug, %error, "the dry run could not be recorded");
            Registration::NotRegistered(NotRegistered::StateUnavailable)
        }
    }
}

pub use crate::cli_run_once::{OneOffCode, OneOffVaultRun, run_workflow_once_reported};

#[derive(Debug, Deserialize)]
struct DirEntryResult {
    name: String,
    kind: String,
}

#[derive(Debug, Deserialize)]
struct ListDirectoryResult {
    entries: Vec<DirEntryResult>,
}

pub(crate) async fn load_scenario(
    client: &McpClient,
    vault_name: &str,
    workflow_slug: &str,
    folder: WorkflowFolder<'_>,
    requested: Option<&str>,
) -> Result<(String, Scenario), CliRunError> {
    let tests_dir = format!("registry/workflows/{folder}/{TESTS_DIR_NAME}");
    let scenario_name = match requested {
        Some(name) => name.to_owned(),
        None => sole_scenario_name(client, vault_name, workflow_slug, &tests_dir).await?,
    };

    let read = read_attached_text(client, &format!("{vault_name}://{tests_dir}/{scenario_name}.toml"))
        .await
        .map_err(|source| CliRunError::ScenarioUnreadable {
            scenario: scenario_name.clone(),
            source,
        })?;
    let Some(read) = read else {
        return Err(CliRunError::ScenarioNotFound {
            workflow_slug: workflow_slug.to_owned(),
            scenario: scenario_name,
        });
    };
    match Scenario::parse(&read.text) {
        Ok(scenario) => Ok((scenario_name, scenario)),
        Err(source) => Err(CliRunError::ScenarioInvalid {
            scenario: scenario_name,
            source: Box::new(source),
        }),
    }
}

async fn sole_scenario_name(
    client: &McpClient,
    vault_name: &str,
    workflow_slug: &str,
    tests_dir: &str,
) -> Result<String, CliRunError> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{tests_dir}")));
    let result = client
        .call_tool("fs_list_directory".to_owned(), args)
        .await
        .map_err(|source| CliRunError::ScenarioUnreadable {
            scenario: String::new(),
            source,
        })?;
    if result.is_error == Some(true) {
        return Err(CliRunError::NoScenariosAvailable {
            workflow_slug: workflow_slug.to_owned(),
        });
    }
    let Ok(listing) = result.into_typed::<ListDirectoryResult>() else {
        return Err(CliRunError::NoScenariosAvailable {
            workflow_slug: workflow_slug.to_owned(),
        });
    };
    let mut names: Vec<String> = listing
        .entries
        .into_iter()
        .filter(|entry| {
            entry.kind == "file"
                && std::path::Path::new(&entry.name)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"))
        })
        .map(|entry| entry.name.trim_end_matches(".toml").to_owned())
        .collect();
    names.sort();
    match names.len() {
        0 => Err(CliRunError::NoScenariosAvailable {
            workflow_slug: workflow_slug.to_owned(),
        }),
        1 => Ok(names.remove(0)),
        count => Err(CliRunError::ScenarioAmbiguous {
            workflow_slug: workflow_slug.to_owned(),
            count,
        }),
    }
}

#[cfg(test)]
#[path = "cli_run_test.rs"]
mod tests;
