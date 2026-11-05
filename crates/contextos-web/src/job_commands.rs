//! The `contextos-web jobs` subcommands: the run commands and the commands an operator uses to move a job through its
//! lifecycle.
//!
//! The lifecycle commands only parse their arguments and print what came back. What each one means is decided by
//! `contextos_web::job_operation`, so the rules are in one place for whatever calls it.

use std::error::Error;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use crate::job_authoring_commands::{AuthoringAction, OutputFormat};
use crate::job_target::JobTarget;
use clap::{Args, Subcommand};
use contextos_web::cli_jobs::{SharedScope, VaultScope, needs_servers, operate_shared, operate_vault};
use contextos_web::job_operation::OperatorRequest;
use contextos_web::package_store;
use contextos_web::shared_state::SharedStateStore;
use contextos_web::shared_validation::SharedValidationHost;
use contextos_web::{McpClientSet, load_web_config};

use crate::one_shot_sessions::connect;
use contextos_workflows::{Clock, CronScheduleError, EmptySuspensionReason, JobSchedule, SuspensionReason};
use thiserror::Error;

/// The `jobs` subcommands.
#[derive(Clone, Debug, Subcommand)]
pub(crate) enum JobsAction {
    /// Runs a saved job synchronously from the command line and prints one run summary on standard output: how the
    /// run ended, its run number and record, and the outputs its script declared. A script's own `print` and `debug`
    /// go to the log, never to standard output. No browser, no `/settings/` round-trip, no separate `contextos-web`
    /// server process required to already be running. Connects its own transient `McpClientSet` against the same
    /// `web.toml`, entirely separate from any already-running server process on the same vault, and claims the
    /// identical lock a scheduled tick or the web UI's own "Run now" observes. Exit status: 0 completed (with or
    /// without warnings), 1 failed, 2 command line not understood, 3 refused before the script started, 75 refused
    /// because the job is already running, 124 timed out, 130 cancelled.
    Run {
        /// The job: `<vault> <package> <workflow> <job>`, or with `--shared` `<package> <workflow> <job>`.
        #[arg(required = true, num_args = 3..=4, value_name = "VAULT PACKAGE WORKFLOW JOB")]
        target: Vec<String>,
        /// Runs a job for a workflow installed in the host package store. Such a job has no vault, holds a
        /// host-level claim for the length of its run, and leaves a run record on the host.
        #[arg(long)]
        shared: bool,
        /// Runs against mocked tool responses instead of real MCP calls:
        /// every `call_mcp` is checked against the named (or, with
        /// exactly one file present, the sole) scenario under this
        /// workflow's own `tests/` directory, or with `--shared` the
        /// scenarios its package ships, and the script's real call
        /// arguments are still validated against each target tool's
        /// real, live-fetched schema. No real MCP call, mutating or
        /// otherwise, ever reaches the network in this mode.
        #[arg(long)]
        dry_run: bool,
        /// Names a specific scenario under `tests/` to use with
        /// `--dry-run`; required when more than one scenario file
        /// exists there, an error without `--dry-run`.
        #[arg(long, requires = "dry_run")]
        scenario: Option<String>,
        /// Skips writing a run record under `memory/jobs/...` and the
        /// operation-log entry - independent of `--dry-run`, freely
        /// combinable with it. A vault run only: a shared run always leaves a record on the host, so combining it with
        /// `--shared` is a usage error.
        #[arg(long, conflicts_with = "shared")]
        no_record: bool,
        /// One `name=value` input that replaces the job's own value for this run only, repeated once per value. A
        /// value is text for a `string` or `vault` input and JSON for every other kind. A run with overrides never
        /// registers a job or settles its first-run trial.
        #[arg(long = "input")]
        inputs: Vec<String>,
        /// `text`, or `json` for the run summary as one document on standard output.
        #[arg(long, value_enum, default_value_t)]
        format: OutputFormat,
    },
    /// Dry-runs a saved job against a scenario's mocked tool responses, as `run --dry-run` does. A dry run that
    /// completes registers a job that was validated for the version it ran, which is what lets an operator schedule
    /// it, and records that in the job's state. It connects to the configured servers to check the script's calls
    /// against their tool schemas, but no call it makes reaches a server for real.
    DryRun {
        #[command(flatten)]
        job: JobSelector,
        /// Names the scenario to use; required when there is more than one.
        #[arg(long)]
        scenario: Option<String>,
        /// One `name=value` input that replaces the job's own value for this run only, repeated once per value. A
        /// value is text for a `string` or `vault` input and JSON for every other kind. A run with overrides never
        /// registers a job or settles its first-run trial.
        #[arg(long = "input")]
        inputs: Vec<String>,
        /// `text`, or `json` for the run summary as one document on standard output.
        #[arg(long, value_enum, default_value_t)]
        format: OutputFormat,
    },
    /// Makes a successor of a job, so its workflow moves to another release: a copy of the job's file that names the
    /// release and the job it succeeds. `contextos-web jobs successor <vault> <package> <workflow> <job> <new-job>`, or
    /// with `--shared` `<package> <workflow> <job> <new-job>`. Enabling the successor retires its predecessor. With
    /// `--all` there is no job: a successor is made for every job of the workflow in `enabled` or `suspended` on the
    /// current release. A job that was never enabled has its `version` changed in place instead.
    Successor(crate::successor_commands::SuccessorArguments),
    #[command(flatten)]
    Operate(OperatorAction),
    #[command(flatten)]
    Author(AuthoringAction),
}

/// Which job a command is about.
#[derive(Clone, Debug, Args)]
pub(crate) struct JobSelector {
    /// The job: `<vault> <package> <workflow> <job>`, or with `--shared` `<package> <workflow> <job>`.
    #[arg(required = true, num_args = 3..=4, value_name = "VAULT PACKAGE WORKFLOW JOB")]
    pub(crate) target: Vec<String>,
    /// The job is a shared job, held in the host package store and the host state file, and not in a vault.
    #[arg(long)]
    pub(crate) shared: bool,
}

/// The commands that ask the shared operation to do something to one job. Each is made, or refused with the state it
/// found.
#[derive(Clone, Debug, Subcommand)]
pub(crate) enum OperatorAction {
    /// Validates the job now, whether or not its job file says it is ready, and records the result. A draft that passes
    /// becomes `validated`, and a failure is listed with each failure's code and file and exits unsuccessfully. The
    /// result is stored in the job's state. The script is compiled and never run, and no tool is called.
    Validate {
        #[command(flatten)]
        job: JobSelector,
    },
    /// Sets the job's cron schedule. A `registered` job becomes `enabled`, for the version it has now, which must be
    /// the one that was validated and dry-run. The schedule of an `enabled` or `suspended` job is replaced. `--clear`
    /// takes an `enabled` job back to `registered`.
    Schedule {
        /// The job, then the schedule: `<vault> <package> <workflow> <job> <expression>`, or with `--shared`
        /// `<package> <workflow> <job> <expression>`, where the expression is a five-field cron expression, in UTC, such
        /// as `0 3 * * *`. With `--clear` there is no expression.
        #[arg(required = true, num_args = 3..=5, value_name = "VAULT PACKAGE WORKFLOW JOB EXPRESSION")]
        words: Vec<String>,
        /// The job is a shared job, held in the host package store and the host state file, and not in a vault.
        #[arg(long)]
        shared: bool,
        /// Clears the schedule instead of setting one.
        #[arg(long)]
        clear: bool,
        /// `text`, or `json` for one document that carries the message and, for an enabling, the predecessor retired
        /// and how the current and previous releases moved.
        #[arg(long, value_enum, default_value_t)]
        format: OutputFormat,
    },
    /// Stops an `enabled` job on its schedule, and says why. A run already in progress finishes.
    Suspend {
        #[command(flatten)]
        job: JobSelector,
        /// Why the job is being suspended. It is stored and shown on the job's page.
        #[arg(long)]
        reason: String,
    },
    /// Resumes a `suspended` job. The job is validated first for the version it has now, and is resumed only when that
    /// passes. A version that is not the one that was enabled is put on trial for its first live run.
    Resume {
        #[command(flatten)]
        job: JobSelector,
    },
    /// Takes an `enabled` or `suspended` job out of use, which clears its schedule.
    Retire {
        #[command(flatten)]
        job: JobSelector,
    },
    /// Brings a `retired` job back as a `draft`, to be validated and dry-run afresh.
    Reactivate {
        #[command(flatten)]
        job: JobSelector,
    },
    /// Deletes a `retired` job's stored state. A shared job's file is removed with it; a vault job's files are left as
    /// they are.
    Delete {
        #[command(flatten)]
        job: JobSelector,
    },
}

/// One command, parsed into the job and the request it makes.
pub(crate) struct Invocation {
    pub(crate) job: JobSelector,
    pub(crate) request: OperatorRequest,
    /// How the result is printed.
    pub(crate) format: OutputFormat,
}

/// An argument the shared operation cannot be given.
#[derive(Debug, Error)]
pub(crate) enum InvocationError {
    #[error("{0}")]
    Schedule(#[from] CronScheduleError),
    #[error("a suspension needs a reason, and --reason was empty")]
    Reason(#[from] EmptySuspensionReason),
    #[error("a schedule command takes exactly one cron expression after the job, or --clear and none")]
    Expression,
    #[error("--clear clears the schedule, so no expression is given with it")]
    ClearWithExpression,
}

impl TryFrom<OperatorAction> for Invocation {
    type Error = InvocationError;

    fn try_from(action: OperatorAction) -> Result<Self, Self::Error> {
        let mut format = OutputFormat::Text;
        let (job, request) = match action {
            OperatorAction::Validate { job } => (job, OperatorRequest::Validate),
            OperatorAction::Schedule {
                mut words,
                shared,
                clear,
                format: asked,
            } => {
                format = asked;
                // The words name the job, and a schedule follows them unless it is being cleared; how many name the job
                // depends on whether it is shared, which a positional list cannot know, so the expression is split off here.
                let named = if shared { 3 } else { 4 };
                let expression = (words.len() > named).then(|| words.split_off(named));
                let request = match (clear, expression.as_deref()) {
                    (false, Some([expression])) => {
                        OperatorRequest::Schedule(JobSchedule::try_from(expression.as_str())?)
                    }
                    (true, None) => OperatorRequest::ClearSchedule,
                    (false, _) => return Err(InvocationError::Expression),
                    (true, Some(_)) => return Err(InvocationError::ClearWithExpression),
                };
                (JobSelector { target: words, shared }, request)
            }
            OperatorAction::Suspend { job, reason } => (
                job,
                OperatorRequest::Suspend(SuspensionReason::try_from(reason.as_str())?),
            ),
            OperatorAction::Resume { job } => (job, OperatorRequest::Resume),
            OperatorAction::Retire { job } => (job, OperatorRequest::Retire),
            OperatorAction::Reactivate { job } => (job, OperatorRequest::Reactivate),
            OperatorAction::Delete { job } => (job, OperatorRequest::Delete),
        };
        Ok(Self { job, request, format })
    }
}

/// Runs one lifecycle command and prints what came of it. A refusal, a failed validation, and any other failure come
/// back as the error the process exits with, whose message begins with its `workflow/` code.
pub(crate) async fn run_operator_command(
    config_path: PathBuf,
    action: OperatorAction,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let Invocation { job, request, format } = Invocation::try_from(action)?;
    let target = JobTarget::try_from((job.target.as_slice(), job.shared)).unwrap_or_else(|error| error.exit());
    let servers = if job.shared && !needs_servers(&request) {
        // No server is needed, so a missing or invalid `web.toml` is not an error, but a configured log still applies.
        crate::tracing_setup::initialise_command_tracing(&config_path)?;
        None
    } else {
        let config = load_web_config(&config_path)?;
        crate::initialise_tracing(&config.server)?;
        let clients = connect(&config, &config_path).await?;
        let names: Vec<String> = config.mcp_servers.iter().map(|entry| entry.name().to_owned()).collect();
        Some((clients, names))
    };
    // A command's stamps are the operator's local time: in the contextos server's zone when one is connected, else
    // this host's.
    let now = match &servers {
        Some((clients, _names)) => contextos_web::vault_time::VaultTime::warmed(Arc::clone(clients))
            .await
            .now(),
        None => contextos_web::vault_time::VaultTime::warmed(Arc::new(McpClientSet::connect(&[]).await?))
            .await
            .now(),
    };

    let operated = match target {
        JobTarget::Shared(key) => {
            let (clients, known_servers) = match servers {
                Some(connected) => connected,
                None => (Arc::new(McpClientSet::connect(&[]).await?), Vec::new()),
            };
            let store_root = package_store::default_store_root()?;
            let jobs_root = package_store::default_jobs_root()?;
            let state = SharedStateStore::new(package_store::default_state_root()?);
            let claims_root = package_store::default_claims_root()?;
            let primary_server = known_servers.first().cloned().unwrap_or_default();
            let validation = SharedValidationHost {
                store_root: &store_root,
                clients: &clients,
                primary_server: &primary_server,
                known_servers: &known_servers,
            };
            operate_shared(
                &SharedScope {
                    state: &state,
                    jobs_root: &jobs_root,
                    validation: &validation,
                    claims_root: &claims_root,
                },
                &key,
                request,
                now,
            )
            .await?
        }
        JobTarget::Vault { vault, key } => {
            let (clients, known_servers) = servers.ok_or("a vault job needs the configured MCP servers")?;
            let primary_server = known_servers.first().cloned().unwrap_or_default();
            let claims_root = package_store::default_claims_root().ok();
            operate_vault(
                &VaultScope {
                    clients: &clients,
                    primary_server: &primary_server,
                    known_servers: &known_servers,
                    vault: &vault,
                    claims_root: claims_root.as_deref(),
                },
                &key.package,
                &key.workflow,
                &key.job,
                request,
                now,
            )
            .await?
        }
    };
    let text = match format {
        OutputFormat::Text => operated.text,
        OutputFormat::Json => serde_json::to_string(&operated)?,
    };
    write_result(&text)?;
    Ok(())
}

/// Writes an operator command's result to standard output, which is where that result belongs.
#[allow(
    clippy::disallowed_methods,
    reason = "the operator command's result is its output on standard output, and this is the one place it is written"
)]
fn write_result(text: &str) -> std::io::Result<()> {
    writeln!(std::io::stdout().lock(), "{text}")
}

#[cfg(test)]
#[path = "job_commands_test.rs"]
mod tests;
