#![forbid(unsafe_code)]
// Standard output carries a command's result and standard error carries the log, so no module prints with a macro. The
// few functions that write a result to a stream do so through `std::io::stdout`, each with its own reasoned allow.
#![deny(clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro)]

mod job_authoring_commands;
mod job_commands;
mod job_target;
mod one_shot_sessions;
mod packages_commands;
mod run_commands;
mod shutdown;
mod source_install_commands;
mod successor_commands;
mod tracing_setup;
mod workflow_releases_commands;

use std::env;
use std::error::Error;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use contextos_service::cli::{ServiceAction, ServiceRequest};
use contextos_web::vault_source_read::SourceKind;
use contextos_web::{TerminalWebInterviewer, WebInterviewEnvironment, load_web_config, run_web_config_interview};
use job_commands::JobsAction;
use tokio::net::TcpListener;
use tracing_setup::initialise_tracing;

#[derive(Debug, Parser)]
#[command(
    name = "contextos-web",
    display_name = "ContextOS Web",
    version,
    about = "ContextOS web UI"
)]
struct Cli {
    /// Load configuration from this TOML file. With no `--config`, falls
    /// back to `CONTEXTOS_WEB_CONFIG`, then to
    /// `$HOME/.config/contextos/web.toml` (the same `.config/contextos/`
    /// directory `contextos config` writes its own `config.toml` into), so
    /// an operator who has already run `contextos config` finds
    /// `contextos-web config` alongside it rather than in the current
    /// working directory. Also the path a `service install` embeds into the
    /// generated service definition's own `--config` argument.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Clone, Debug, Subcommand)]
enum Command {
    /// Interactively create or update the `web.toml` configuration file:
    /// the bind address, logging, UI appearance, and the `[[mcp_server]]`
    /// entries this instance proxies to. Loads an existing file's current
    /// values as editable defaults; writes nothing until every question is
    /// answered.
    Config,
    /// Install, remove, or report on `contextos-web` running as an
    /// auto-starting, per-user background service (`systemd --user` on
    /// Linux, a `launchd` `LaunchAgent` on macOS, a Startup App on Windows),
    /// and start, stop, or restart it, for example after it stopped
    /// unexpectedly. The service itself always runs as the invoking user with
    /// no elevated privilege, and needs none to install, except a Windows
    /// Scheduled Task (`install --scheduled-task`), which Task Scheduler only
    /// lets an Administrator terminal register or remove.
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Install, list, inspect, or remove a versioned workflow package in a vault's package store, or with `--shared`
    /// in this host's, which is usable before any vault exists or `web.toml` is configured at all. See
    /// `contextos_web::package_store` for the store's own on-disk layout and concurrency contract.
    Packages {
        #[command(subcommand)]
        action: PackagesAction,
    },
    /// Saved-job commands distinct from any direct workflow-invocation
    /// commands a later task may add: `run` executes a saved job
    /// synchronously from the command line.
    Jobs {
        #[command(subcommand)]
        action: JobsAction,
    },
    /// Direct workflow invocation: `run` executes one workflow once, with no
    /// saved job, taking its input values from the command line. It takes no
    /// claim and writes no run record.
    Workflows {
        #[command(subcommand)]
        action: WorkflowsAction,
    },
    /// Libraries kept as source in a vault: `install` makes a release of one in the vault's package store.
    Libraries {
        #[command(subcommand)]
        action: LibrariesAction,
    },
}

#[derive(Clone, Debug, Subcommand)]
enum LibrariesAction {
    /// Installs a library from `registry/libraries/<library>/` in a vault as a release in the vault's package store.
    /// The source is read through the vault's primary server and checked as validation checks code, and nothing is
    /// installed when a check fails. The version is worked out from the highest the store has recorded for the library
    /// and what changed in its exports; content an installed release already holds installs nothing. With `--version`
    /// it restores that exact release when it was removed, damaged, or incomplete.
    Install(source_install_commands::InstallArguments),
}

#[derive(Clone, Debug, Subcommand)]
enum WorkflowsAction {
    /// Installs a workflow package from `registry/workflows/<package>/` in a vault as a release in the vault's package
    /// store. The source is read through the vault's primary server and checked as validation checks a job's code, and
    /// nothing is installed when a check fails. The version is worked out from the highest the store has recorded for the
    /// package and what changed in its workflows' interfaces; content a current or future release already holds installs
    /// nothing. A library the package names without a requirement is installed from `registry/libraries/` first, and the
    /// release requires `^` the version it was tested on. A new release is normally the future release. With `--version`
    /// it restores that exact release when it was removed, damaged, or incomplete.
    Install(source_install_commands::InstallArguments),
    /// Reports the releases of one workflow: `contextos-web workflows releases <vault> <package> <workflow>`, or with
    /// `--shared` `<package> <workflow>` for a package in this host's package store. Each release is shown as current,
    /// previous, future, or retired, with when it was installed, whether it can be read, the library releases it is
    /// locked to, and the jobs on it by stage. `--format json` gives the same facts as one document, with the version of
    /// each release, which an agent reads.
    Releases(workflow_releases_commands::ReleasesArguments),
    /// Runs one workflow once, with no job, no claim, and no run record, and never on a schedule.
    /// `contextos-web workflows run <vault> <package> <workflow-slug>` runs a workflow of a vault's package store, scoped
    /// to that vault. With `--shared`, the words are `<package> <workflow-slug>`, a package in this host's package store.
    /// `--release` chooses the release: `current` (the default), `future`, `previous`, or an exact version, which must be
    /// the workflow's current, previous, or a future release. A release with a library lock runs with it; an unlocked
    /// release resolves its libraries over the installed library releases for this run only. For a vault workflow,
    /// `--source` runs the source in `registry/` instead, without installing it, for development testing. Prints one run
    /// summary on standard output, and a script's own `print` and `debug` go to the log.
    /// Exit status: 0 completed (with or without warnings), 1 failed, 2 command line not understood, 3 refused before
    /// the script started, 75 refused because the job is already running, 124 timed out, 130 cancelled.
    Run {
        /// The workflow: `<vault> <package> <workflow>`, or with `--shared` `<package> <workflow>`.
        #[arg(required = true, num_args = 2..=3, value_name = "VAULT PACKAGE WORKFLOW")]
        target: Vec<String>,
        /// Runs a workflow of a package installed in the host package store.
        #[arg(long)]
        shared: bool,
        /// The release to run: `current`, `future`, `previous`, or an exact version such as `1.2.0`.
        #[arg(
            long,
            value_name = "RELEASE",
            default_value = "current",
            conflicts_with = "source",
            value_parser = |text: &str| contextos_workflows::ReleaseChoice::try_from(text)
        )]
        release: contextos_workflows::ReleaseChoice,
        /// Runs the workflow's source in the vault's `registry/` without installing it. A vault workflow only.
        #[arg(long, conflicts_with = "shared")]
        source: bool,
        /// One `name=value` input, repeated once per value. A value is text for a `string` or `vault` input and JSON
        /// for every other kind.
        #[arg(long = "input")]
        inputs: Vec<String>,
        /// Runs the workflow against a scenario's mocked tool responses: a vault-registered workflow's under its
        /// `tests/` folder, and with `--shared` one its package ships under `{workflow-slug}/tests/`.
        #[arg(long)]
        dry_run: bool,
        /// Names the scenario to use with `--dry-run`; required when there is more than one.
        #[arg(long, requires = "dry_run")]
        scenario: Option<String>,
        /// `text`, or `json` for the run summary as one document on standard output.
        #[arg(long, value_enum, default_value_t)]
        format: job_authoring_commands::OutputFormat,
    },
}

#[derive(Clone, Debug, Subcommand)]
enum PackagesAction {
    /// Verifies and installs a package from a local bundle (a `.tar.gz` archive) or a local folder holding the package's
    /// files as a bundle would: `contextos-web packages install <vault> <bundle>` into that vault's package store, or
    /// with `--shared` `<bundle>` into this host's. A folder is only read: its files are copied into the store and
    /// verified against its manifest's digest. Releases nothing keeps are removed afterwards.
    Install(packages_commands::InstallArguments),
    /// Lists a package store: `contextos-web packages list <vault>`, or with `--shared` this host's. Shows the ledger,
    /// each release with where it came from, whether it can be read, whether each workflow in it is current, previous,
    /// future, or retired, the libraries it requires and is locked to, and every reason a rule keeps it, including the
    /// workflow releases that hold a library release. Then what was removed, and what unfinished operations left behind.
    List(packages_commands::ListArguments),
    /// Reports one release in full: `contextos-web packages inspect <vault> <package> <version>`, or with `--shared`
    /// `<package> <version>`. With `--rebuild-ledger` the words name the store alone, and a missing ledger is rebuilt
    /// from the releases present.
    Inspect(packages_commands::InspectArguments),
    /// Removes one release: `contextos-web packages remove <vault> <package> <version>`, or with `--shared`
    /// `<package> <version>`. Refused with `packages/in-use`, naming the jobs and releases, while a rule keeps the
    /// release; `--force` removes it anyway. Releases nothing keeps are removed afterwards.
    Remove(packages_commands::RemoveArguments),
}

/// Runs the command, and reports a failure as its own message rather than its `Debug` form, which is what a bare
/// `Result` from `main` prints and which names no code and gives no remedy. A run command ends with its own graded
/// status and prints its summary itself.
#[tokio::main]
#[allow(
    clippy::disallowed_methods,
    reason = "a command that failed before it had a logger says so on standard error, the last resort"
)]
async fn main() -> std::process::ExitCode {
    let outcome = Box::pin(run()).await;
    // Every session a command opened ends before the runtime stops, so none is cancelled in the middle of closing.
    one_shot_sessions::close_all().await;
    match outcome {
        Ok(code) => code,
        // A refused authoring command has already said so in its result.
        Err(error) if error.is::<job_authoring_commands::Refusal>() => std::process::ExitCode::FAILURE,
        Err(error) => {
            // Ignored on purpose: with stderr closed there is nowhere left to report that it is.
            let _ = writeln!(std::io::stderr().lock(), "Error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<std::process::ExitCode, Box<dyn Error + Send + Sync>> {
    let cli = Cli::parse();
    let config_path = resolve_web_config_path(
        cli.config,
        env::var_os("CONTEXTOS_WEB_CONFIG").map(PathBuf::from),
        default_web_config_path(),
    );

    if let Some(command) = cli.command {
        return run_subcommand(command, config_path).await;
    }

    let config_path = config_path?;
    let config = load_web_config(&config_path)?;
    initialise_tracing(&config.server)?;

    tracing::info!(
        name = "contextos-web",
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        mcp_servers = config.mcp_servers.len(),
        "contextos-web starting"
    );

    // Every configured MCP server is attempted (by `server/discover`, or an `initialize` handshake for a server
    // without protocol revision 2026-07-28) before any HTTP request is served, so a page never meets a session that is
    // still opening. A server that cannot be connected does not stop start-up: it is served as not connected, with its
    // cause, in `/settings/`, and connected again from there without a restart.
    let mut shutdown_signal = shutdown::ShutdownSignal::new()?;
    let shutdown_requested = tokio_util::sync::CancellationToken::new();
    let signal_token = shutdown_requested.clone();
    let signal_task = tokio::spawn(async move {
        shutdown_signal.wait().await;
        signal_token.cancel();
    });
    let clients = match contextos_web::app::start_until_shutdown(&config, &config_path, &shutdown_requested).await {
        Ok(clients) => clients,
        Err(contextos_web::McpConnectError::Shutdown) => return Ok(std::process::ExitCode::SUCCESS),
        Err(error) => return Err(error.into()),
    };

    // The headless job scheduler: a background task in
    // this same process, sharing the identical `McpClientSet` every HTTP
    // route already uses, never a separate process or client.
    // Resolved in the background, never before the server starts listening: a contextos server slow to answer must not
    // delay startup or shutdown. Until it answers, the scheduler reads this host's zone.
    let scheduler_clock = contextos_web::vault_time::VaultTime::unwarmed(Arc::clone(&clients));
    tokio::spawn({
        let clock = Arc::clone(&scheduler_clock);
        async move { clock.warm().await }
    });
    let scheduler = tokio::spawn(contextos_web::scheduler::run_forever(
        Arc::clone(&clients),
        contextos_web::run_settings::HostContext {
            attention: contextos_web::job_attention::JobAttentionMemory::default(),
            log_level: config.workflows.log_level,
            claims_root: contextos_web::package_store::default_claims_root().ok(),
            clock: scheduler_clock,
        },
        shared_scheduler_host(&config),
    ));

    let listener = match TcpListener::bind(&config.server.bind).await {
        Ok(listener) => listener,
        Err(error) => {
            scheduler.abort();
            let _ = scheduler.await;
            clients.shutdown().await;
            return Err(error.into());
        }
    };
    let bound = listener.local_addr()?;
    // Lets `contextos-web service` find this server to stop, restart, or report on; a server that cannot write it
    // still serves, and only those commands lose sight of it.
    let process_record = claim_process_record(contextos_service::process_record::path_for(&config_path)).await;
    let router = contextos_web::build_router_with_shutdown(
        Arc::clone(&clients),
        config.server.static_dir.as_deref(),
        &config_path,
        &config.workflows,
        shutdown_requested.clone(),
    );
    tracing::info!(bind = %bound, "contextos-web listening");
    let result = shutdown::serve(listener, router, clients, scheduler, shutdown_requested).await;
    signal_task.abort();
    if let Some(guard) = process_record {
        let _ = tokio::task::spawn_blocking(move || drop(guard)).await;
    }
    result?;
    Ok(std::process::ExitCode::SUCCESS)
}

/// Writes this server's process record ([`contextos_service::process_record`]) at `path`, or logs why not.
async fn claim_process_record(path: PathBuf) -> Option<contextos_service::process_record::ProcessRecordGuard> {
    match tokio::task::spawn_blocking(move || {
        contextos_service::process_record::ProcessRecordGuard::claim(
            path,
            contextos_web::service_identity::SERVICE_IDENTITY.program,
        )
    })
    .await
    {
        Ok(Ok(guard)) => Some(guard),
        // Only Windows' service commands read the record, so only there is a missing one worth a warning.
        Ok(Err(error)) if cfg!(windows) => {
            tracing::warn!(%error, "no process record written; `contextos-web service` cannot see this server");
            None
        }
        Ok(Err(error)) => {
            tracing::debug!(%error, "the process record could not be written");
            None
        }
        Err(error) => {
            tracing::warn!(%error, "writing the process record failed; `contextos-web service` cannot see this server");
            None
        }
    }
}

/// Runs one subcommand to completion; only a bare `contextos-web` starts the server.
///
/// `config_path` is the outcome of resolving it. A run command turns a path that could not be resolved into its own
/// `workflow/config-invalid` refusal; every other command fails with the resolution error.
async fn run_subcommand(
    command: Command,
    config_path: Result<PathBuf, Box<dyn Error + Send + Sync>>,
) -> Result<std::process::ExitCode, Box<dyn Error + Send + Sync>> {
    let succeeded = |result: Result<(), Box<dyn Error + Send + Sync>>| result.map(|()| std::process::ExitCode::SUCCESS);
    match command {
        Command::Config => succeeded(run_config_command(config_path?).await),
        Command::Service { action } => succeeded(run_service_command(action, config_path?).await),
        Command::Packages { action } => succeeded(run_packages(config_path, action).await),
        Command::Jobs {
            action:
                JobsAction::Run {
                    target,
                    shared,
                    dry_run,
                    scenario,
                    no_record,
                    inputs,
                    format,
                },
        } => Ok(run_commands::run_job_command(
            config_path.ok(),
            run_commands::JobRunArguments {
                command: contextos_web::run_summary::RunCommand::JobsRun,
                target: job_target::JobTarget::try_from((target.as_slice(), shared))
                    .unwrap_or_else(|error| error.exit()),
                dry_run,
                scenario,
                no_record,
                inputs,
                format,
            },
        )
        .await),
        Command::Jobs {
            action:
                JobsAction::DryRun {
                    job,
                    scenario,
                    inputs,
                    format,
                },
        } => Ok(run_commands::run_job_command(
            config_path.ok(),
            run_commands::JobRunArguments {
                command: contextos_web::run_summary::RunCommand::JobsDryRun,
                target: job_target::JobTarget::try_from((job.target.as_slice(), job.shared))
                    .unwrap_or_else(|error| error.exit()),
                dry_run: true,
                scenario,
                no_record: false,
                inputs,
                format,
            },
        )
        .await),
        Command::Jobs {
            action: JobsAction::Operate(action),
        } => succeeded(job_commands::run_operator_command(config_path?, action).await),
        Command::Jobs {
            action: JobsAction::Successor(arguments),
        } => succeeded(successor_commands::run_successor_command(&config_path?, arguments).await),
        Command::Jobs {
            action: JobsAction::Author(action),
        } => succeeded(job_authoring_commands::run_authoring_command(&config_path?, action).await),
        Command::Workflows {
            action: WorkflowsAction::Install(arguments),
        } => install_from_source(config_path, SourceKind::Workflows, arguments).await,
        Command::Workflows {
            action: WorkflowsAction::Releases(arguments),
        } => succeeded(workflow_releases_commands::run_releases_command(&config_path?, arguments).await),
        Command::Libraries {
            action: LibrariesAction::Install(arguments),
        } => install_from_source(config_path, SourceKind::Library, arguments).await,
        Command::Workflows {
            action:
                WorkflowsAction::Run {
                    target,
                    shared,
                    release,
                    source,
                    inputs,
                    dry_run,
                    scenario,
                    format,
                },
        } => Ok(run_commands::run_workflow_command(
            config_path.ok(),
            run_commands::WorkflowRunArguments {
                target: job_target::WorkflowTarget::try_from((target.as_slice(), shared))
                    .unwrap_or_else(|error| error.exit()),
                code: contextos_web::cli_run::OneOffCode::from((source, release)),
                inputs,
                dry_run,
                scenario,
                format,
            },
        )
        .await),
    }
}

/// Runs `contextos-web workflows install` or `libraries install`: a vault's source made a release in its package store.
async fn install_from_source(
    config_path: Result<PathBuf, Box<dyn Error + Send + Sync>>,
    kind: SourceKind,
    arguments: source_install_commands::InstallArguments,
) -> Result<std::process::ExitCode, Box<dyn Error + Send + Sync>> {
    source_install_commands::run_install_command(&config_path?, kind, arguments).await?;
    Ok(std::process::ExitCode::SUCCESS)
}

/// Runs `contextos-web config`: the interactive `web.toml` setup wizard.
/// Real terminal I/O runs on a blocking thread (blocking work never runs on
/// an async executor thread), matching `run_service_command`'s own pattern
/// and `contextos-mcp`'s `run_config_interview`.
#[allow(
    clippy::disallowed_methods,
    reason = "the wizard's closing report is this command's result on standard output"
)]
async fn run_config_command(config_path: PathBuf) -> Result<(), Box<dyn Error + Send + Sync>> {
    let report = tokio::task::spawn_blocking(move || {
        let mut interviewer = TerminalWebInterviewer;
        run_web_config_interview(
            &mut interviewer,
            &WebInterviewEnvironment {
                config_path,
                default_log_file: contextos_web::config_interview::default_log_file(),
            },
        )
    })
    .await??;
    write!(std::io::stdout().lock(), "{report}")?;
    Ok(())
}

/// Runs `contextos-web service ...` ([`contextos_service::cli::run`]) on a blocking thread: it runs platform
/// commands and reads files, which never happens on an async executor thread.
#[allow(
    clippy::disallowed_methods,
    reason = "the service report is this command's result on standard output"
)]
async fn run_service_command(action: ServiceAction, config_path: PathBuf) -> Result<(), Box<dyn Error + Send + Sync>> {
    let report = tokio::task::spawn_blocking(move || {
        contextos_service::cli::run(
            action,
            &ServiceRequest {
                identity: contextos_web::service_identity::SERVICE_IDENTITY,
                config_path,
                run_args: Vec::new(),
            },
        )
    })
    .await??;
    writeln!(std::io::stdout().lock(), "{report}")?;
    Ok(())
}

/// Runs one `contextos-web packages` subcommand. Only a vault's store needs `web.toml`, so its path is passed on as it was
/// resolved, and a command on the host's store never fails for want of one.
async fn run_packages(
    config_path: Result<PathBuf, Box<dyn Error + Send + Sync>>,
    action: PackagesAction,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let request = match action {
        PackagesAction::Install(arguments) => {
            match packages_commands::Installation::try_from(arguments).unwrap_or_else(|error| error.exit()) {
                packages_commands::Installation::Shared(bundle_path) => return run_packages_install(bundle_path).await,
                packages_commands::Installation::Vault(request) => Ok(request),
            }
        }
        PackagesAction::List(arguments) => packages_commands::Request::try_from(arguments),
        PackagesAction::Inspect(arguments) => packages_commands::Request::try_from(arguments),
        PackagesAction::Remove(arguments) => packages_commands::Request::try_from(arguments),
    };
    packages_commands::run_packages_command(config_path, request.unwrap_or_else(|error| error.exit())).await
}

/// Runs `contextos-web packages install`: installs a workflow package into this host's package store, independent of
/// any vault or `web.toml`, so no configuration is loaded at all for it. The store's own filesystem I/O runs on a
/// blocking thread (blocking work never runs on an async executor thread), matching `run_service_command`'s identical
/// pattern.
#[allow(
    clippy::disallowed_methods,
    reason = "the package report is this command's result on standard output"
)]
async fn run_packages_install(bundle_path: PathBuf) -> Result<(), Box<dyn Error + Send + Sync>> {
    tokio::task::spawn_blocking(move || -> Result<(), Box<dyn Error + Send + Sync>> {
        let store_root = contextos_web::package_store::default_store_root()?;
        let jobs = saved_jobs()?;
        let installed = if bundle_path.is_dir() {
            contextos_web::package_store::install_folder(&store_root, &jobs, &bundle_path)
        } else {
            let bundle_bytes = std::fs::read(&bundle_path)?;
            contextos_web::package_store::install(&store_root, &jobs, &bundle_bytes)
        }
        .map_err(packages_commands::refuse_host_install)?;
        write!(
            std::io::stdout().lock(),
            "{}",
            contextos_web::package_report::InstalledReport(&installed)
        )?;
        Ok(())
    })
    .await??;
    Ok(())
}

/// The saved shared jobs and the host state that records their library locks, which an install or a removal reports the
/// version changes of.
fn saved_jobs() -> Result<contextos_web::package_store::SavedJobs, contextos_web::package_store::PackageStoreError> {
    Ok(contextos_web::package_store::SavedJobs {
        jobs_root: contextos_web::package_store::default_jobs_root()?,
        state: contextos_web::shared_state::SharedStateStore::new(contextos_web::package_store::default_state_root()?),
    })
}

/// What the scheduler needs to run shared jobs, from the platform app-data directory. Without a resolvable app-data
/// directory there is nowhere shared jobs could live, so none is scheduled and the reason is logged.
fn shared_scheduler_host(
    config: &contextos_web::WebConfig,
) -> Option<contextos_web::shared_scheduler::SharedSchedulerHost> {
    use contextos_web::package_store as store;
    let roots = (|| -> Result<_, store::PackageStoreError> {
        Ok(contextos_web::shared_scheduler::SharedSchedulerHost {
            store_root: store::default_store_root()?,
            jobs_root: store::default_jobs_root()?,
            claims_root: store::default_claims_root()?,
            runs_root: store::default_runs_root()?,
            state: contextos_web::shared_state::SharedStateStore::new(store::default_state_root()?),
            run_retention: config.workflows.run_retention(),
            owner: config.owner.as_ref().map(|owner| owner.name.clone()),
            log_level: config.workflows.log_level,
        })
    })();
    match roots {
        Ok(host) => Some(host),
        Err(error) => {
            tracing::warn!(%error, "shared jobs are not scheduled: the app-data directory could not be resolved");
            None
        }
    }
}

/// Resolves the `web.toml` path for this invocation: the explicit
/// `--config` flag when given, otherwise `environment_config_path`
/// (`CONTEXTOS_WEB_CONFIG`, read by the caller so this precedence merge
/// stays testable without touching real process environment, matching
/// `contextos-mcp::Config::try_from(ConfigLoadInput)`'s identical
/// discipline), otherwise `default_config_path`
/// ([`default_web_config_path`]). Mirrors `contextos config`'s own
/// `resolve_config_toml_path` precedence (`contextos-mcp/src/main.rs`) so
/// both binaries agree on where an operator's configuration lives without
/// either one needing to be told explicitly.
fn resolve_web_config_path(
    cli_config_path: Option<PathBuf>,
    environment_config_path: Option<PathBuf>,
    default_config_path: Option<PathBuf>,
) -> Result<PathBuf, Box<dyn Error + Send + Sync>> {
    cli_config_path
        .or(environment_config_path)
        .or(default_config_path)
        .ok_or_else(|| {
            std::io::Error::other(
                "no configuration file path could be determined; pass --config or set \
                 CONTEXTOS_WEB_CONFIG",
            )
            .into()
        })
}

/// Fallback `web.toml` path used when neither `--config` nor
/// `CONTEXTOS_WEB_CONFIG` is set: `$HOME/.config/contextos/web.toml`, the
/// same `.config/contextos/` directory `contextos config` writes its own
/// `config.toml` into (`contextos-mcp::default_config_path`). Before this
/// fallback existed, `contextos-web config` and a bare `contextos-web` run
/// both defaulted to a `web.toml` resolved against the current working
/// directory, so the file landed wherever the operator happened to invoke
/// the binary from rather than a single well-known location.
fn default_web_config_path() -> Option<PathBuf> {
    let folders = contextos_host::HostFolders::detect()?;
    Some(folders.home().join(".config").join("contextos").join("web.toml"))
}

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;
