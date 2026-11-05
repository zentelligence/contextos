//! The run commands: `jobs run`, `jobs dry-run`, and `workflows run`.
//!
//! Each runs once, ends with one run summary ([`contextos_web::run_summary`]) and a graded exit status, and nothing else
//! is written to standard output or standard error by these commands. Script output, progress, and the failure itself
//! go through `tracing`, whose subscriber writes to standard error or to `[server] log_file`. The one direct write is
//! [`write_summary`], called once at the end. `main.rs` denies `print_stdout`, `print_stderr`, and `dbg_macro` for the whole
//! binary, and `clippy.toml` disallows `std::io::stdout` and `std::io::stderr`, so a stray write is a lint failure and
//! [`write_summary`] is allowed the one narrowly.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use crate::one_shot_sessions::connect_until_shutdown;
use contextos_web::cli_run::{CliRunOptions, OneOffVaultRun, run_job_reported, run_workflow_once_reported};
use contextos_web::cli_shared::{
    OneOffRequest, SharedHost, SharedRunOptions, run_shared_job_reported, run_shared_once_reported,
};
use contextos_web::package_store;
use contextos_web::run_report::{RunControls, RunReport};
use contextos_web::run_summary::{
    ExitStatus, Refusal, RunCommand, RunEnd, RunInvocation, RunScope, RunSummary, Timings, codes,
};
use contextos_web::shared_state::SharedStateStore;
use contextos_web::{McpConnectError, WebConfig, WebMcpCaller, load_web_config};
use contextos_workflows::{Clock, RunOutcome, RunTrigger, WorkflowFailureKind};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use crate::job_authoring_commands::OutputFormat;
use crate::job_target::{JobTarget, WorkflowTarget};
use crate::shutdown::ShutdownSignal;
use crate::tracing_setup::{ConfigInvalid, initialise_tracing};

/// The parsed arguments of `contextos-web jobs run` and `jobs dry-run`.
pub(crate) struct JobRunArguments {
    /// Which command was typed, for the summary: `jobs run --dry-run` is still `jobs run`.
    pub(crate) command: RunCommand,
    pub(crate) target: JobTarget,
    pub(crate) dry_run: bool,
    pub(crate) scenario: Option<String>,
    pub(crate) no_record: bool,
    /// One `name=value` argument per input whose value replaces the job's own for this run.
    pub(crate) inputs: Vec<String>,
    pub(crate) format: OutputFormat,
}

/// The parsed arguments of `contextos-web workflows run`.
pub(crate) struct WorkflowRunArguments {
    pub(crate) target: WorkflowTarget,
    pub(crate) code: contextos_web::cli_run::OneOffCode,
    pub(crate) inputs: Vec<String>,
    pub(crate) dry_run: bool,
    pub(crate) scenario: Option<String>,
    pub(crate) format: OutputFormat,
}

/// What the command line knows of a run before anything has happened, and when it began, so a summary can be made
/// whatever becomes of the run, even when nothing could be loaded.
struct RunContext {
    command: RunCommand,
    scope: RunScope,
    dry_run: bool,
    scenario: Option<String>,
    began: Instant,
    /// The host's clock, in the zone the host writes times in. Known once the servers are connected; a refusal before
    /// that has no zone to use and is timed in UTC.
    clock: Option<Arc<dyn Clock>>,
}

impl RunContext {
    fn new(command: RunCommand, scope: RunScope, dry_run: bool, scenario: Option<String>) -> Self {
        Self {
            command,
            scope,
            dry_run,
            scenario,
            began: Instant::now(),
            clock: None,
        }
    }

    /// Times what follows by the host's clock, so a refusal is in the same zone as a run that finished.
    fn use_clock(&mut self, clock: Arc<dyn Clock>) {
        self.clock = Some(clock);
    }

    /// The invocation as it stands now, with the time spent so far, which stands for a refusal or any report that has no
    /// times of its own.
    fn invocation(&self) -> RunInvocation {
        let elapsed = self.began.elapsed();
        let ended = self
            .clock
            .as_ref()
            .map_or_else(OffsetDateTime::now_utc, |clock| clock.now());
        let started = time::Duration::try_from(elapsed).map_or(ended, |elapsed| ended - elapsed);
        RunInvocation {
            command: self.command,
            scope: self.scope.clone(),
            dry_run: self.dry_run,
            scenario: self.scenario.clone(),
            overridden_inputs: Vec::new(),
            timings: Timings {
                started: Some(started),
                ended: Some(ended),
                duration_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            },
        }
    }

    /// The summary of what the run came to.
    fn summary(&self, outcome: Result<RunReport, Refusal>) -> RunSummary {
        match outcome {
            Ok(report) => RunSummary::finished(self.invocation(), report),
            Err(refusal) => RunSummary::refused(self.invocation(), refusal),
        }
    }
}

/// Runs `contextos-web jobs run` or `jobs dry-run`: loads `web.toml`, connects its own transient `McpClientSet`, runs one
/// saved job synchronously, and prints its summary. The exit status says how it ended.
///
/// `config_path` is `None` when no path could be resolved from `--config`, the environment, or the home folder, which
/// is a refusal like any other unusable configuration.
pub(crate) async fn run_job_command(
    config_path: Option<PathBuf>,
    arguments: JobRunArguments,
) -> std::process::ExitCode {
    let scope = match &arguments.target {
        JobTarget::Shared(key) => RunScope::Shared {
            package: key.package.clone(),
            workflow: key.workflow.clone(),
            job: Some(key.job.clone()),
            release: None,
        },
        JobTarget::Vault { vault, key } => RunScope::Vault {
            vault: vault.clone(),
            package: key.package.clone(),
            workflow: key.workflow.clone(),
            job: Some(key.job.clone()),
            release: None,
        },
    };
    let mut context = RunContext::new(arguments.command, scope, arguments.dry_run, arguments.scenario.clone());
    let format = arguments.format;
    let outcome = perform_job(config_path.as_deref(), arguments, &mut context).await;
    let summary = context.summary(outcome);
    // The clock holds the connected servers; `main` closes their sessions once the summary is written.
    drop(context);
    write_summary(&summary, format)
}

/// Runs `contextos-web workflows run`: one workflow once, with no saved job, no claim, and no run record.
pub(crate) async fn run_workflow_command(
    config_path: Option<PathBuf>,
    arguments: WorkflowRunArguments,
) -> std::process::ExitCode {
    let scope = match &arguments.target {
        WorkflowTarget::Shared { package, workflow } => RunScope::Shared {
            package: package.clone(),
            workflow: workflow.clone(),
            job: None,
            release: None,
        },
        WorkflowTarget::Vault {
            vault,
            package,
            workflow,
        } => RunScope::Vault {
            vault: vault.clone(),
            package: package.clone(),
            workflow: workflow.clone(),
            job: None,
            release: None,
        },
    };
    let mut context = RunContext::new(
        RunCommand::WorkflowsRun,
        scope,
        arguments.dry_run,
        arguments.scenario.clone(),
    );
    let format = arguments.format;
    let outcome = perform_workflow(config_path.as_deref(), arguments, &mut context).await;
    let summary = context.summary(outcome);
    drop(context);
    write_summary(&summary, format)
}

/// What a run command works with once `web.toml` is loaded and its servers are connected.
struct Session {
    config: WebConfig,
    clients: Arc<contextos_web::McpClientSet>,
    primary_server: String,
    /// The host's clock, in the zone the host writes times in.
    clock: Arc<contextos_web::vault_time::VaultTime>,
    /// Cancelled when the process is asked to end.
    cancel: CancellationToken,
}

/// Installs the signal handlers, loads `web.toml`, installs the logging it describes, and connects its servers.
///
/// The handlers come first, so a signal that arrives while the configuration is being read or the servers are being
/// connected ends the command as cancelled and is never lost to the default handler. A handler that could not be installed
/// is kept, and logged once logging is installed, which is after the configuration is read. A configuration that cannot
/// be used, or none that could be found, is a refusal, and the default logging is installed first so the refusal is
/// logged, since the configured destination is the thing that failed.
async fn open_session(config_path: Option<&Path>) -> Result<Result<Session, RunReport>, Refusal> {
    let ShutdownWatch { cancel, failure } = cancel_on_shutdown_signal();
    let configured = configure(config_path);
    // Logging is installed by now, whether from the configuration or, for a refusal, by default.
    if let Some(error) = failure {
        tracing::warn!(%error, "signal handlers could not be installed, so the run cannot be interrupted cleanly");
    }
    let (config, config_path) = configured?;
    let clients = match connect_until_shutdown(&config, config_path, &cancel).await {
        Ok(clients) => clients,
        Err(McpConnectError::Shutdown) => {
            return Ok(Err(RunReport::ended_before_start(RunOutcome::Cancelled)));
        }
        Err(error) => {
            tracing::debug!(%error, "the configured MCP servers could not be connected");
            return Err(Refusal {
                code: WorkflowFailureKind::ServerUnavailable.code().to_owned(),
                message: "the configured MCP servers could not be connected; see the log, and check `web.toml`"
                    .to_owned(),
                suspension: None,
            });
        }
    };
    let primary_server = config
        .mcp_servers
        .first()
        .map(|entry| entry.name().to_owned())
        .unwrap_or_default();
    let clock = contextos_web::vault_time::VaultTime::warmed(Arc::clone(&clients)).await;
    Ok(Ok(Session {
        config,
        clients,
        primary_server,
        clock,
        cancel,
    }))
}

/// Loads `web.toml` and installs the logging it describes.
fn configure(config_path: Option<&Path>) -> Result<(WebConfig, &Path), Refusal> {
    let Some(config_path) = config_path else {
        return Err(config_refusal(&ConfigInvalid {
            reason: "no configuration file path could be determined; pass --config or set CONTEXTOS_WEB_CONFIG"
                .to_owned(),
        }));
    };
    let config = load_web_config(config_path).map_err(|error| config_refusal(&ConfigInvalid::from(&error)))?;
    initialise_tracing(&config.server).map_err(|error| config_refusal(&ConfigInvalid::from(error)))?;
    Ok((config, config_path))
}

/// A refusal for a configuration that cannot be used. The default logging is installed first, because a subscriber
/// that could not be made from the configuration must not leave the refusal unlogged.
fn config_refusal(error: &ConfigInvalid) -> Refusal {
    // Installing is skipped silently when one already exists, which is the case when only the log file failed to open.
    let _ = initialise_tracing(&contextos_web::WebServerConfig::default());
    Refusal::new(codes::CONFIG_INVALID, error)
}

/// The app-data folder could not be resolved, so there is nowhere a host-level claim, state, or record could live.
fn app_data_refusal(error: &package_store::PackageStoreError) -> Refusal {
    tracing::debug!(%error, "the app-data folder could not be resolved");
    Refusal {
        code: codes::STATE_UNAVAILABLE.to_owned(),
        message: "the host's app-data folder could not be resolved".to_owned(),
        suspension: None,
    }
}

/// The roots of the host's own storage, which no vault holds.
struct HostRoots {
    store: PathBuf,
    jobs: PathBuf,
    claims: PathBuf,
    runs: PathBuf,
    state: SharedStateStore,
}

impl HostRoots {
    fn resolve() -> Result<Self, Refusal> {
        let roots = || -> Result<Self, package_store::PackageStoreError> {
            Ok(Self {
                store: package_store::default_store_root()?,
                jobs: package_store::default_jobs_root()?,
                claims: package_store::default_claims_root()?,
                runs: package_store::default_runs_root()?,
                state: SharedStateStore::new(package_store::default_state_root()?),
            })
        };
        roots().map_err(|error| app_data_refusal(&error))
    }
}

/// A shared host: everything a shared run needs, borrowed from the session and the roots.
fn shared_host<'a>(session: &'a Session, roots: &'a HostRoots) -> SharedHost<'a> {
    SharedHost {
        clients: Arc::clone(&session.clients),
        primary_server: &session.primary_server,
        store_root: &roots.store,
        jobs_root: &roots.jobs,
        claims_root: &roots.claims,
        runs_root: &roots.runs,
        state: &roots.state,
        run_retention: session.config.workflows.run_retention(),
        clock: Arc::clone(&session.clock) as Arc<dyn Clock>,
        caller: Arc::new(WebMcpCaller::new(Arc::clone(&session.clients))),
        owner: session.config.owner.as_ref().map(|owner| owner.name.as_str()),
        log_level: session.config.workflows.log_level,
        cancel: session.cancel.clone(),
    }
}

/// How a shared run that returned an error ended: a refusal, or, when its task was lost after it had begun, a report of
/// a failed run.
fn shared_end(result: Result<RunReport, contextos_web::cli_shared::CliSharedError>) -> Result<RunReport, Refusal> {
    result.or_else(|error| match RunEnd::from(&error) {
        RunEnd::Ran(report) => Ok(*report),
        RunEnd::Refused(refusal) => Err(refusal),
    })
}

async fn perform_job(
    config_path: Option<&Path>,
    arguments: JobRunArguments,
    context: &mut RunContext,
) -> Result<RunReport, Refusal> {
    let session = match open_session(config_path).await? {
        Ok(session) => session,
        Err(cancelled) => return Ok(cancelled),
    };
    context.use_clock(Arc::clone(&session.clock) as Arc<dyn Clock>);
    let JobRunArguments {
        target,
        dry_run,
        scenario,
        no_record,
        inputs,
        ..
    } = arguments;
    let (vault, key) = match target {
        JobTarget::Shared(key) => {
            let roots = HostRoots::resolve()?;
            let host = shared_host(&session, &roots);
            return shared_end(
                run_shared_job_reported(
                    host,
                    key,
                    SharedRunOptions {
                        trigger: RunTrigger::Manual,
                        dry_run_scenario: dry_run.then_some(scenario),
                    },
                    inputs,
                )
                .await,
            );
        }
        JobTarget::Vault { vault, key } => (vault, key),
    };
    let claims_root = package_store::default_claims_root().map_err(|error| app_data_refusal(&error))?;
    let host = contextos_web::run_settings::HostContext {
        attention: contextos_web::job_attention::JobAttentionMemory::default(),
        log_level: session.config.workflows.log_level,
        claims_root: Some(claims_root),
        clock: Arc::clone(&session.clock),
    };
    run_job_reported(
        session.clients,
        &host,
        &vault,
        &key,
        CliRunOptions {
            dry_run_scenario: dry_run.then_some(scenario),
            record: !no_record,
        },
        RunControls {
            overrides: inputs,
            cancel: session.cancel,
        },
    )
    .await
    .map_err(|error| Refusal::from(&error))
}

async fn perform_workflow(
    config_path: Option<&Path>,
    arguments: WorkflowRunArguments,
    context: &mut RunContext,
) -> Result<RunReport, Refusal> {
    let session = match open_session(config_path).await? {
        Ok(session) => session,
        Err(cancelled) => return Ok(cancelled),
    };
    context.use_clock(Arc::clone(&session.clock) as Arc<dyn Clock>);
    let WorkflowRunArguments {
        target,
        code,
        inputs,
        dry_run,
        scenario,
        ..
    } = arguments;
    let (vault, package, workflow_slug) = match target {
        WorkflowTarget::Shared { package, workflow } => {
            // A shared workflow has no source on the host, and the command line refuses `--source` with `--shared`.
            let contextos_web::cli_run::OneOffCode::Release(release) = code else {
                return Err(Refusal::new(
                    WorkflowFailureKind::JobNotRunnable.code(),
                    &"a shared workflow has no source to run; name a release",
                ));
            };
            let roots = HostRoots::resolve()?;
            let host = shared_host(&session, &roots);
            return shared_end(
                run_shared_once_reported(
                    host,
                    OneOffRequest {
                        package,
                        workflow,
                        release,
                        arguments: inputs,
                        dry_run_scenario: dry_run.then_some(scenario),
                    },
                )
                .await,
            );
        }
        WorkflowTarget::Vault {
            vault,
            package,
            workflow,
        } => (vault, package, workflow),
    };
    run_workflow_once_reported(
        session.clients,
        &session.primary_server,
        OneOffVaultRun {
            code,
            vault,
            package,
            workflow_slug,
            arguments: inputs,
            dry_run_scenario: dry_run.then_some(scenario),
            log_level: session.config.workflows.log_level,
        },
        session.cancel,
    )
    .await
    .map_err(|error| Refusal::from(&error))
}

/// The token a signal cancels, and why no signal can, when the handlers could not be installed.
struct ShutdownWatch {
    cancel: CancellationToken,
    /// Why the handlers could not be installed. Logged once logging is installed, which is after this is made.
    failure: Option<std::io::Error>,
}

/// A token that is cancelled when the process is asked to end (Ctrl-C or a termination signal, and on Windows a console
/// close), so a command-line run stops, records `cancelled`, and releases its claim instead of being killed mid-call.
///
/// Installed before the configuration is read, so a signal is never lost to the default handler while it is read. When
/// the handlers cannot be installed the token is never cancelled, the run goes ahead without them, and the cause comes
/// back for the caller to log once logging exists.
fn cancel_on_shutdown_signal() -> ShutdownWatch {
    let cancel = CancellationToken::new();
    let mut signal = match ShutdownSignal::new() {
        Ok(signal) => signal,
        Err(failure) => {
            return ShutdownWatch {
                cancel,
                failure: Some(failure),
            };
        }
    };
    let token = cancel.clone();
    tokio::spawn(async move {
        signal.wait().await;
        token.cancel();
        // A second signal does not wait for the run to stop. The claim is dropped with the process and the record is
        // left `running`, which the next run of the job marks `interrupted`. No summary is written.
        signal.wait().await;
        tracing::warn!("a second signal arrived, so the run was abandoned without finishing its record");
        std::process::exit(130);
    });
    ShutdownWatch { cancel, failure: None }
}

/// Prints `summary` as the command's one result, and returns the exit status it states.
///
/// This is the only function on the run path that writes to standard output. It renders the whole document into one
/// buffer and writes it once, so a failure part way never leaves half a document. A run that ends with a non-zero
/// status is also logged as one `error` event carrying its outcome, code, and exit status, which is where a person
/// reading the log finds it, since the command writes nothing of its own to standard error. The failure message can quote
/// the job's data, so it goes to `debug` only.
#[allow(
    clippy::disallowed_methods,
    reason = "the one place a run command writes its summary, its only result, to standard output"
)]
pub(crate) fn write_summary(summary: &RunSummary, format: OutputFormat) -> std::process::ExitCode {
    let status = u8::from(ExitStatus::from(summary));
    if status != 0 {
        tracing::error!(
            outcome = ?summary.outcome,
            code = summary.code.as_deref().unwrap_or_default(),
            exit_code = status,
            "the run did not complete"
        );
        tracing::debug!(
            message = summary.message.as_deref().unwrap_or_default(),
            "why the run did not complete"
        );
    }
    let mut document = match format {
        OutputFormat::Json => serde_json::to_string(summary).unwrap_or_else(|_| fallback_document(summary)),
        OutputFormat::Text => summary.to_string(),
    };
    if !document.ends_with('\n') {
        document.push('\n');
    }
    let mut stdout = std::io::stdout().lock();
    // Ignored on purpose: with standard output closed there is nowhere left to say so, and the exit status stands.
    let _ = stdout.write_all(document.as_bytes()).and_then(|()| stdout.flush());
    std::process::ExitCode::from(status)
}

/// A summary that could not be serialised, which no field of one can cause, still ends as a document: every field of the
/// summary is present, null where it knows nothing, so a caller reading any of them finds it.
fn fallback_document(summary: &RunSummary) -> String {
    let status = u8::from(ExitStatus::from(summary));
    let document = serde_json::json!({
        "summary_version": contextos_web::run_summary::SUMMARY_VERSION,
        "command": serde_json::Value::Null,
        "scope": serde_json::Value::Null,
        "dry_run": summary.dry_run,
        "scenario": serde_json::Value::Null,
        "outcome": summary.outcome.word(),
        "exit_code": status,
        "code": "workflow/internal-error",
        "message": "the summary could not be rendered",
        "owner": serde_json::Value::Null,
        "overridden_inputs": [],
        "run": serde_json::Value::Null,
        "registration": serde_json::Value::Null,
        "outputs": serde_json::Value::Null,
        "libraries": [],
        "warnings": { "kept": [], "omitted": 0 },
        "host_warnings": [],
        "timings": serde_json::Value::Null,
    });
    document.to_string()
}

#[cfg(test)]
#[path = "run_commands_test.rs"]
mod tests;
