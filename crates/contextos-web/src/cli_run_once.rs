//! One-off runs of a vault workflow: no job, no claim, no run record, and never scheduled.
//!
//! The code is a release in the vault's package store (`current`, `future`, `previous`, or an exact version), read and
//! verified like a saved job's with the library lock recorded for it, or the workflow's source in `registry/`, which is
//! run without installing it.

use std::sync::Arc;

use contextos_workflows::{
    Clock, CommandLineInputs, LogLevel, McpCaller, OneOffRelease, PackageKind, PackageName, ReleaseChoice, RunFailure,
    RunLabel, RunOutcome, RunSettings, ValidatedInputs, WorkflowFailureKind, WorkflowScript, run_workflow_declaring,
};
use semver::Version;
use tokio_util::sync::CancellationToken;

use crate::cli_run::{CliRunError, load_scenario};
use crate::dry_run_caller::DryRunMcpCaller;
use crate::dry_run_scenario::ScenarioMatcher;
use crate::job_discovery::{PackageFileStanding, read_package_file};
use crate::jobs::{RegisteredWorkflow, WebMcpCaller, WorkflowFolder, declarations_from_text, discover_workflows};
use crate::mcp_client::{McpClient, McpClientSet};
use crate::release_record::ever_recorded;
use crate::run_records::{RunLibrary, RunRelease};
use crate::run_report::RunReport;
use crate::run_settings::RunPlan;
use crate::scheduler::{self, StateError, read_state};
use crate::vault_release_snapshot::{ReleaseRequest, open_release, read_opened_release};
use crate::vault_source_libraries::{SourceLibrariesFailure, SourceLibrariesRequest, read_source_libraries};
use crate::vault_workflow_files::{VaultWorkflowFiles, VaultWorkflowFolder};
use crate::vault_workflow_script::{ScriptReadError, SourceCode, read_source_script};

/// Which code a one-off run of a vault workflow takes: a release in the vault's package store, or the source in `registry/`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OneOffCode {
    /// The workflow's `current`, `future`, or `previous` release, or an exact one.
    Release(contextos_workflows::ReleaseChoice),
    /// The source in `registry/`, without installing it, for development testing.
    Source,
}

impl From<(bool, ReleaseChoice)> for OneOffCode {
    /// The source when `--source` was given, otherwise the release chosen.
    fn from((source, release): (bool, ReleaseChoice)) -> Self {
        if source { Self::Source } else { Self::Release(release) }
    }
}

/// One workflow registered in a vault, to run once.
pub struct OneOffVaultRun {
    /// Which code to run.
    pub code: OneOffCode,
    pub vault: String,
    /// The vault package the workflow belongs to.
    pub package: String,
    pub workflow_slug: String,
    /// One `name=value` argument per input value.
    pub arguments: Vec<String>,
    /// As for a saved job: `Some(None)` requires exactly one scenario, `Some(Some(name))` names one.
    pub dry_run_scenario: Option<Option<String>>,
    /// `[workflows] log_level` from `web.toml`, the default the workflow may override.
    pub log_level: Option<LogLevel>,
}

/// Runs a workflow registered in a vault once, with no saved job, stopping when `cancel` is cancelled, and returns what
/// the run reported.
///
/// It takes no claim and writes no run record, so it neither blocks nor is blocked by any job. It is confined to its
/// own vault and its workflow's call rules, exactly as a job run is, and its inputs are validated before any MCP
/// call.
///
/// # Errors
///
/// Returns [`CliRunError`] when the primary server is unreachable, the workflow is not registered, or a requested
/// dry-run scenario cannot be loaded. Failures inside the run itself are returned as [`RunOutcome::Failed`].
pub async fn run_workflow_once_reported(
    clients: Arc<McpClientSet>,
    primary_server: &str,
    once: OneOffVaultRun,
    cancel: CancellationToken,
) -> Result<RunReport, CliRunError> {
    let client = clients.get(primary_server).ok_or(CliRunError::Unreachable)?;
    let CodeRead {
        workflow,
        release: read_release,
        libraries,
        script: source,
    } = match choose_code(&client, &clients, &once).await {
        Ok(chosen) => chosen,
        Err(CodeNotChosen::Failed(error)) => return Err(error),
        Err(CodeNotChosen::Ended(report)) => return Ok(*report),
    };
    let inputs = match ValidatedInputs::try_from(CommandLineInputs {
        declared: workflow.inputs.fields(),
        arguments: once.arguments,
    }) {
        Ok(inputs) => inputs,
        Err(error) => {
            let kind = WorkflowFailureKind::InputInvalid;
            return Ok(RunReport::ended_before_start(RunOutcome::Failed(RunFailure {
                kind: Some(kind),
                message: format!("{}: {error}", kind.code()),
            })));
        }
    };
    let mut scenario_name = None;
    let inner: Arc<dyn McpCaller> = match &once.dry_run_scenario {
        Some(requested) => {
            let (name, scenario) = load_scenario(
                &client,
                &once.vault,
                &workflow.slug,
                workflow.folder(),
                requested.as_deref(),
            )
            .await?;
            scenario_name = Some(name);
            Arc::new(DryRunMcpCaller::new(
                Arc::clone(&clients),
                ScenarioMatcher::new(scenario),
            ))
        }
        None => Arc::new(WebMcpCaller::new(Arc::clone(&clients))),
    };
    let reported = |report: RunReport| RunReport {
        scenario: scenario_name.clone(),
        read_release: read_release.clone(),
        libraries: libraries.clone(),
        ..report
    };
    let caller =
        match scheduler::scoped_caller_for_run(&client, primary_server, &once.vault, &workflow.calls, inner).await {
            Ok(caller) => caller,
            Err(failure) => return Ok(reported(RunReport::ended_before_start(RunOutcome::Failed(failure)))),
        };
    let script = OneOffScript {
        clients,
        vault: once.vault,
        workflow,
        inputs,
        caller,
        source,
        log_level: once.log_level,
        cancel,
    };
    Ok(reported(execute_one_off(script).await))
}

/// Why the code of a one-off run could not be chosen.
enum CodeNotChosen {
    /// The command cannot report a run at all.
    Failed(CliRunError),
    /// The run ends before its script starts, as this report says.
    Ended(Box<RunReport>),
}

/// The workflow a one-off run runs, as the code it takes declares it, and the release it read when it took one.
async fn choose_code(
    client: &McpClient,
    clients: &McpClientSet,
    once: &OneOffVaultRun,
) -> Result<CodeRead, CodeNotChosen> {
    let ended = |failure: RunFailure| {
        CodeNotChosen::Ended(Box::new(RunReport::ended_before_start(RunOutcome::Failed(failure))))
    };
    match &once.code {
        OneOffCode::Source => {
            let workflows = discover_workflows(client, &once.vault, &clients.names())
                .await
                .map_err(|_| CodeNotChosen::Failed(CliRunError::Unreachable))?;
            let workflow = workflows
                .into_iter()
                .find(|workflow| workflow.package == once.package && workflow.slug == once.workflow_slug)
                .ok_or_else(|| {
                    CodeNotChosen::Failed(CliRunError::WorkflowNotFound {
                        vault_name: once.vault.clone(),
                        package: once.package.clone(),
                        workflow_slug: once.workflow_slug.clone(),
                    })
                })?;
            source_code(client, once, workflow).await
        }
        OneOffCode::Release(choice) => match read_release(client, once, choice).await {
            Ok(read) => Ok(read),
            Err(ReleaseReadFailure::Outage) => Err(CodeNotChosen::Failed(CliRunError::Unreachable)),
            Err(ReleaseReadFailure::Ended(failure)) => Err(ended(failure)),
        },
    }
}

/// Reads the source of `workflow` for a run of source: its code from `registry/workflows/`, each library it names without
/// a requirement from `registry/libraries/`, and each it names with one from the newest installed library release that
/// satisfies it. A requirement no installed library release satisfies ends the run before its script starts. No lock is
/// recorded and nothing is installed.
async fn source_code(
    client: &McpClient,
    once: &OneOffVaultRun,
    workflow: RegisteredWorkflow,
) -> Result<CodeRead, CodeNotChosen> {
    let ended = |kind: WorkflowFailureKind, why: String| {
        CodeNotChosen::Ended(Box::new(RunReport::ended_before_start(RunOutcome::Failed(
            RunFailure {
                kind: Some(kind),
                message: format!("{}: {why}, so this run did not start", kind.code()),
            },
        ))))
    };
    let outage = |_| CodeNotChosen::Failed(CliRunError::Unreachable);
    let file = match read_package_file(client, &once.vault, &once.package)
        .await
        .map_err(outage)?
    {
        PackageFileStanding::Listed(package) => package.file,
        PackageFileStanding::Refused(failure) => {
            return Err(ended(WorkflowFailureKind::JobNotRunnable, failure.message));
        }
    };
    let package = PackageName::try_from(once.package.as_str())
        .map_err(|error| ended(WorkflowFailureKind::JobNotRunnable, error.to_string()))?;
    let libraries = read_source_libraries(SourceLibrariesRequest {
        client,
        vault: &once.vault,
        package: &package,
        requirements: file.libraries(),
    })
    .await
    .map_err(|failure| match failure {
        SourceLibrariesFailure::Unreachable => CodeNotChosen::Failed(CliRunError::Unreachable),
        SourceLibrariesFailure::Unreadable(why) => ended(WorkflowFailureKind::EntryUnreadable, why),
        SourceLibrariesFailure::Unavailable(why) => ended(
            WorkflowFailureKind::PackageUnavailable,
            format!("the source's libraries cannot be had: {why}"),
        ),
    })?;
    let script = read_source_script(
        client,
        &once.vault,
        SourceCode {
            folder: workflow.folder(),
            entry: &workflow.entry,
            file: &file,
            libraries: libraries.code,
        },
    )
    .await
    .map_err(|error| match error {
        ScriptReadError::Unreachable(_) => CodeNotChosen::Failed(CliRunError::Unreachable),
        unreadable @ ScriptReadError::Unreadable(_) => CodeNotChosen::Ended(Box::new(RunReport::ended_before_start(
            RunOutcome::Failed(RunFailure::from(unreadable)),
        ))),
    })?;
    Ok(CodeRead {
        libraries: libraries.libraries,
        workflow,
        release: None,
        script,
    })
}

/// What reading the code of a one-off run found: the workflow as the code declares it, the code, and the releases the
/// run is made of.
struct CodeRead {
    workflow: crate::jobs::RegisteredWorkflow,
    script: WorkflowScript,
    /// The release the code was read from, or none for source.
    release: Option<RunRelease>,
    libraries: Vec<RunLibrary>,
}

/// Why a release could not be read for a one-off run.
enum ReleaseReadFailure {
    /// The server could not be asked.
    Outage,
    /// The run ends before its script starts, with this failure.
    Ended(RunFailure),
}

/// Reads the release `choice` names for `once`, with the library lock recorded for it, or the libraries chosen from the
/// store's library releases for this run only when none is recorded. Nothing is written.
async fn read_release(
    client: &McpClient,
    once: &OneOffVaultRun,
    choice: &ReleaseChoice,
) -> Result<CodeRead, ReleaseReadFailure> {
    let ended = |kind: WorkflowFailureKind, why: String| {
        ReleaseReadFailure::Ended(RunFailure {
            kind: Some(kind),
            message: format!("{}: {why}, so this run did not start", kind.code()),
        })
    };
    let state = match read_state(client, &once.vault).await {
        Ok(state) => state,
        Err(StateError::Mcp(_)) => return Err(ReleaseReadFailure::Outage),
        Err(other) => {
            return Err(ended(
                WorkflowFailureKind::StateUnavailable,
                format!("the job state recording the workflow's releases could not be read: {other}"),
            ));
        }
    };
    let version = Version::try_from(OneOffRelease {
        choice,
        workflow: &once.workflow_slug,
        releases: state.value.package_releases(&once.package),
    })
    .map_err(|error| ended(WorkflowFailureKind::JobNotRunnable, error.to_string()))?;
    let lock = state
        .value
        .library_lock(&once.package, &once.workflow_slug, &version)
        .cloned();
    let unavailable = |why: String| ended(WorkflowFailureKind::PackageUnavailable, why);
    let engine = client
        .vault_packages(&once.vault)
        .await
        .map_err(|error| unavailable(error.to_string()))?;
    let package = PackageName::try_from(once.package.as_str()).map_err(|error| unavailable(error.to_string()))?;
    if matches!(choice, ReleaseChoice::Version(_)) {
        let recorded = ever_recorded(&engine, &package, &version)
            .await
            .map_err(|error| unavailable(error.to_string()))?;
        if !recorded {
            return Err(ended(
                WorkflowFailureKind::JobNotRunnable,
                format!("the vault's package store has never recorded {package} {version}"),
            ));
        }
    }
    let opened = open_release(&engine, &package, &version)
        .await
        .map_err(|failure| unavailable(failure.to_string()))?;
    let snapshot = read_opened_release(
        &engine,
        opened,
        &ReleaseRequest {
            package: &package,
            release: &version,
            workflow: &once.workflow_slug,
            lock: lock.as_ref(),
        },
    )
    .await
    .map_err(|failure| unavailable(failure.to_string()))?;
    let folder = WorkflowFolder {
        package: &once.package,
        directory: &once.workflow_slug,
    };
    let workflow = declarations_from_text(&snapshot.workflow_toml, folder).map_err(|found| {
        ended(
            WorkflowFailureKind::JobNotRunnable,
            format!("the release's workflow.toml cannot be used ({})", found.message),
        )
    })?;
    let workflow = match &snapshot.kind {
        PackageKind::Workflows(file) => workflow.declaring(file),
        PackageKind::Library(_) => workflow,
    };
    Ok(CodeRead {
        workflow,
        release: Some(RunRelease::from(&snapshot.manifest)),
        libraries: snapshot.libraries.iter().map(RunLibrary::from).collect(),
        script: snapshot.script,
    })
}

/// A one-off workflow run that is ready to execute: everything it needs has been found and checked.
struct OneOffScript {
    clients: Arc<McpClientSet>,
    vault: String,
    workflow: crate::jobs::RegisteredWorkflow,
    inputs: ValidatedInputs,
    caller: Arc<dyn McpCaller>,
    source: WorkflowScript,
    log_level: Option<LogLevel>,
    cancel: CancellationToken,
}

/// Runs the script of a one-off run and reports how it ended, with the outputs it set.
async fn execute_one_off(script: OneOffScript) -> RunReport {
    let OneOffScript {
        clients,
        vault,
        workflow,
        inputs,
        caller,
        source,
        log_level,
        cancel,
    } = script;
    let clock: Arc<dyn Clock> = crate::vault_time::VaultTime::warmed(Arc::clone(&clients)).await;
    let started_at = clock.now();
    let settings = RunSettings::from(RunPlan {
        timeout_secs: workflow.timeout_secs,
        log_level: contextos_workflows::effective_log_level(log_level, workflow.log_level, None),
        label: RunLabel {
            workflow: workflow.slug.clone(),
            job: None,
            run_id: None,
        },
        cancel,
    });
    let result = run_workflow_declaring(
        source,
        inputs,
        workflow.outputs.clone(),
        caller,
        contextos_workflows::WorkflowFiles::new(
            workflow.templates.clone(),
            Arc::new(VaultWorkflowFiles::from(VaultWorkflowFolder {
                clients: Arc::clone(&clients),
                vault: &vault,
                package: &workflow.package,
            })),
        ),
        Arc::clone(&clock),
        settings,
    )
    .await;
    RunReport {
        started: result.started,
        outputs: result.outputs.map(|outputs| outputs.values().clone()),
        output_names: result.output_names,
        started_at: Some(started_at),
        ended_at: Some(clock.now()),
        ..RunReport::from(result.outcome)
    }
}
