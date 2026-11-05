#![forbid(unsafe_code)]

use std::env;
use std::error::Error;
use std::io::Write;
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use contextos_host::HostFolders;
use contextos_mcp::{
    CliHttp, Config, ConfigEnvironment, ConfigLoadInput, ContextOsServer, DeregisterOutcome, DoctorReport,
    HostPathResolution, IndexReport, InterviewEnvironment, LogLevel, ModelReport, RegisteredServer, RegistrationStatus,
    ServerConfig, ServiceCommand, SystemProcessDetector, TerminalInterviewer, Transport, claim_process_record,
    default_claude_desktop_config_path, default_model_cache_dir, deregister, download_default_model,
    load_config_document, register, run_interview, run_service_command, status as host_status, write_config_document,
};
use contextos_service::cli::ServiceAction;
use rmcp::{ServiceExt, service::ServerInitializeError, transport::stdio};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "contextos",
    version = concat!("v", env!("CARGO_PKG_VERSION")),
    about = "ContextOS MCP server"
)]
struct Cli {
    /// Load configuration from this TOML file.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Add an allowed vault directory. May repeat.
    #[arg(long = "vault", global = true)]
    vaults: Vec<PathBuf>,

    /// Override the configured runtime log level.
    #[arg(long, value_enum, global = true)]
    log_level: Option<CliLogLevel>,

    /// Enable the HTTP transport for this run, optionally overriding the
    /// configured bind address (for example `--http 127.0.0.1:9000`). With
    /// no address, the configured or default bind is used unchanged.
    #[arg(long, global = true, num_args = 0..=1, default_missing_value = "", value_name = "ADDR")]
    http: Option<String>,

    /// Serve no stdio for this run, whatever `[server] transports` says: with `--http`, this process serves HTTP
    /// alone and keeps running with no client on its standard input, as a background service does.
    #[arg(long, global = true)]
    no_stdio: bool,

    /// Register the ephemeris tools (`ephemeris_moon_phase`,
    /// `ephemeris_solar_events`, and the rest) for this run, overriding
    /// `[server] astro` in config. Enable only; there is no `--no-astro` to
    /// force it off from an already-enabling config.
    #[arg(long, global = true)]
    astro: bool,

    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Clone, Debug, Subcommand)]
enum CliCommand {
    /// Validate configuration, vault indexes, and local Git recovery state.
    Doctor {
        /// Resolve every currently auto-fixable finding (a stale or missing
        /// managed index, MCP-made changes left uncommitted, or a Git
        /// repository that is absent or has no commits yet) instead of only
        /// reporting it. Mirrors the `doctor_resolve` MCP tool.
        #[arg(long)]
        resolve: bool,
        /// With --resolve, report what would be resolved without writing
        /// anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Rebuild every enabled vault search index (text and link graph).
    Index,
    /// Manage the shared local embedding model cache. Vault-independent:
    /// no `--config` or `--vault` is required.
    Model {
        #[command(subcommand)]
        action: ModelAction,
    },
    /// Edit `config.toml` directly, or with no subcommand at all, run the
    /// interactive guided-setup interview. Vault-independent: works against
    /// a not-yet-valid or not-yet-existing configuration file, so it runs
    /// before the strict `Config` load the other subcommands require. When
    /// `config.toml` already has vault(s) configured, the interview loads
    /// them first: a single existing vault is offered back for edit with
    /// its current settings prefilled as defaults, and more than one
    /// existing vault instead asks what to focus on (general server
    /// settings, all vaults, or one named vault to edit or remove) before
    /// optionally adding more.
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },
    /// Suite-level self-update operations: check, install, clean up, or diagnose. With no
    /// subcommand, installs the newest available release set. Vault-independent.
    Update {
        #[command(subcommand)]
        action: Option<UpdateAction>,
        /// Install without an interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Roll back to the previously active release set. Vault-independent.
    Rollback,
    /// Install a specific, pinned release set, for CI and automation. Vault-independent.
    Install {
        #[arg(long)]
        version: String,
    },
    /// Install, remove, or report on one per-user background `contextos` that serves MCP over HTTP alone
    /// (`systemd --user` on Linux, a `launchd` `LaunchAgent` on macOS, a Startup App on Windows), and start, stop, or
    /// restart it. It runs `contextos --config <path> --http [ADDR] --no-stdio`: give `--http ADDR` to install to
    /// serve on that address, or none to serve on the configured bind. Vault-independent: the configuration it loads
    /// is checked, not used, by these commands.
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
}

#[derive(Clone, Debug, Subcommand)]
enum UpdateAction {
    /// Report whether a newer release set is available, without installing it.
    Check,
    /// Delete obsolete release sets, best-effort.
    Cleanup,
    /// Print a structured summary of the installation.
    Diagnostics,
}

#[derive(Clone, Debug, Subcommand)]
enum ConfigAction {
    /// Manage this configuration's `[[vault]]` entries.
    Vault {
        #[command(subcommand)]
        action: ConfigVaultAction,
    },
    /// Register, deregister, or report this server's entry in an MCP
    /// host's own configuration file.
    Mcp {
        #[command(subcommand)]
        action: ConfigMcpAction,
    },
}

#[derive(Clone, Debug, Subcommand)]
enum ConfigMcpAction {
    /// Add or update this server's entry in the host's `mcpServers` key.
    Register {
        #[arg(long, value_enum)]
        host: HostChoice,
        /// Skip discovery and edit this file directly. Required when
        /// discovery reports more than one candidate, or none.
        #[arg(long)]
        config_path: Option<PathBuf>,
        /// Proceed even though the host is detected running, logging a
        /// warning instead of refusing.
        #[arg(long)]
        force: bool,
    },
    /// Remove this server's entry from the host's `mcpServers` key.
    Deregister {
        #[arg(long, value_enum)]
        host: HostChoice,
        #[arg(long)]
        config_path: Option<PathBuf>,
        #[arg(long)]
        force: bool,
    },
    /// Report whether this server is currently registered, without writing.
    Status {
        #[arg(long, value_enum)]
        host: HostChoice,
        #[arg(long)]
        config_path: Option<PathBuf>,
    },
}

/// Supported MCP hosts for `contextos config mcp`. Only Claude Desktop today;
/// another host is a new variant.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum HostChoice {
    #[value(name = "claude-desktop")]
    ClaudeDesktop,
}

#[derive(Clone, Debug, Subcommand)]
enum ConfigVaultAction {
    /// Add a vault.
    Add {
        /// Explicit vault name, addressed as `name://relative-path`.
        name: String,
        /// Absolute path to an existing vault root directory.
        path: PathBuf,
        /// Mark this vault filesystem-only: the tools still read and write
        /// its files, but managed indexes, the oplog, Git recovery, and
        /// search are disabled.
        #[arg(long)]
        unmanaged: bool,
    },
    /// Remove a configured vault by name.
    Remove {
        /// The vault's configured or default-derived name.
        name: String,
    },
    /// List configured vaults.
    List,
}

#[derive(Clone, Copy, Debug, Subcommand)]
enum ModelAction {
    /// Report the default local embedding model's cache status.
    List,
    /// Download the default local embedding model into the shared cache.
    Download,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CliLogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl From<CliLogLevel> for LogLevel {
    fn from(value: CliLogLevel) -> Self {
        match value {
            CliLogLevel::Error => Self::Error,
            CliLogLevel::Warn => Self::Warn,
            CliLogLevel::Info => Self::Info,
            CliLogLevel::Debug => Self::Debug,
            CliLogLevel::Trace => Self::Trace,
        }
    }
}

/// Runs the command, and reports a failure as its own message rather than its `Debug` form, which is what a bare
/// `Result` from `main` prints and which gives an operator no remedy.
#[tokio::main]
#[allow(
    clippy::disallowed_methods,
    reason = "a command that failed before it had a logger says so on standard error, the last resort"
)]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "contextos stopped");
            // Ignored on purpose: with stderr closed there is nowhere left to report that it is.
            let _ = writeln!(std::io::stderr().lock(), "Error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut cli = Cli::parse();
    // Taken once and re-bound after each independent-of-`Config` branch
    // below returns, rather than matched on `cli.command` directly:
    // `CliCommand` carries owned `String`/`PathBuf` fields (the `Config`
    // variant's vault name and path), so it is no longer `Copy`, unlike
    // before this subcommand existed.
    let command = cli.command.take();
    let command = match command {
        Some(CliCommand::Model { action }) => return run_model_command(action).await,
        Some(CliCommand::Config { action }) => {
            let config_path = resolve_config_toml_path(cli.config.clone())?;
            return dispatch_config_command(action, config_path).await;
        }
        Some(CliCommand::Service { action }) => {
            return dispatch_service_command(action, resolve_config_toml_path(cli.config.clone())?, cli.http).await;
        }
        Some(command @ (CliCommand::Update { .. } | CliCommand::Rollback | CliCommand::Install { .. })) => {
            return dispatch_update_command(command).await;
        }
        other => other,
    };
    let mut config = Config::try_from(ConfigLoadInput {
        cli_config_path: cli.config,
        default_config_path: default_config_path(),
        cli_vaults: cli.vaults,
        cli_log_level: cli.log_level.map(LogLevel::from),
        cli_http: cli.http.map(CliHttp::from),
        cli_no_stdio: cli.no_stdio,
        environment: ConfigEnvironment {
            config_path: env::var_os("CONTEXTOS_MCP_CONFIG").map(PathBuf::from),
            token: env::var("CONTEXTOS_MCP_TOKEN").ok(),
            log_level: env::var("CONTEXTOS_MCP_LOG_LEVEL").ok(),
            home: HostFolders::detect().map(|folders| folders.home().to_path_buf()),
        },
    })?;
    initialise_tracing(&config.server)?;
    // Emitted before any fallible construction below (config validation
    // already succeeded by this point, but `ContextOsServer::try_from` and
    // the CLI subcommand handlers have not run yet): the one log line
    // guaranteed to exist no matter what fails afterwards, so a process
    // that dies during construction still leaves behind confirmation it
    // was this build that ran, and which PID, rather than leaving an
    // operator unable to tell a startup failure apart from the binary
    // never having started at all.
    tracing::info!(
        name = "contextos",
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        vaults = config.vaults.len(),
        "contextos starting"
    );
    if cli.astro {
        config.server.astro = true;
    }
    let command = match command {
        Some(CliCommand::Doctor { resolve, dry_run }) => {
            return run_doctor_command(config, resolve, dry_run).await;
        }
        other => other,
    };
    if matches!(command, Some(CliCommand::Index)) {
        return run_index_command(config).await;
    }

    serve(config).await
}

/// Serves the configured transports until stdio closes or the process is asked to stop, then flushes what the
/// substrates hold.
async fn serve(config: Config) -> Result<(), Box<dyn Error + Send + Sync>> {
    let run_stdio = config.server.transports.contains(&Transport::Stdio);
    let run_http = config.server.transports.contains(&Transport::Http);
    let http_config = config.server.http.clone();
    // Listening comes before the server is built, so a process that cannot serve HTTP, most often because another
    // already holds the address, stops before it opens a single vault store.
    let http_listener = if run_http {
        let token = http_config.resolve_token(|name| env::var(name).ok())?;
        Some(contextos_mcp::listen(&http_config.bind, &token).await?)
    } else {
        None
    };
    let process_record = claim_record_when_serving_http(http_listener.is_some(), config.provenance.path.clone()).await;
    let server = tokio::task::spawn_blocking(move || ContextOsServer::try_from(config)).await??;

    // One `CancellationToken` signals both transports to stop; dropping the
    // stdio `RunningService` on the losing branch of the `select!` below
    // closes stdio, and cancelling this token stops the HTTP listener from
    // accepting new connections while in-flight requests finish.
    let shutdown = CancellationToken::new();
    let semantic_drain_tasks = server.spawn_semantic_drain(&shutdown);
    server.warm_file_name_indexes();
    let events_task = server.spawn_events_maintenance(&shutdown);

    let http_task = match http_listener {
        Some(listener) => Some(spawn_http(listener, &server, &http_config, &shutdown)?),
        None => None,
    };

    let stdio_running = if run_stdio {
        tracing::info!(transport = "stdio", "starting ContextOS MCP server");
        Some(server.clone().serve(stdio()).await)
    } else {
        None
    };

    match stdio_running {
        Some(Ok(running)) => {
            tokio::select! {
                result = running.waiting() => { let _reason = result?; },
                result = shutdown_signal() => { result?; },
            }
        }
        // A client that closes stdio before it initialises the session has ended the conversation, exactly as one
        // that closes it later does, so the server stops the same way rather than reporting a failure.
        Some(Err(ServerInitializeError::ConnectionClosed(_))) => {
            tracing::info!(
                transport = "stdio",
                "the client closed stdio before initialising the session"
            );
        }
        Some(Err(error)) => return Err(error.into()),
        None => shutdown_signal().await?,
    }
    shutdown.cancel();
    if let Some(task) = http_task {
        task.await??;
    }
    for task in semantic_drain_tasks {
        task.await?;
    }
    events_task.await?;

    tokio::task::spawn_blocking(move || server.flush_substrates()).await??;
    if let Some(guard) = process_record {
        let _ = tokio::task::spawn_blocking(move || drop(guard)).await;
    }
    Ok(())
}

/// Only the process serving HTTP claims a record: the port admits one such process at a time, so it is the one
/// `contextos service` must find. Stdio-only processes, of which there may be many, never write it.
async fn claim_record_when_serving_http(
    serving_http: bool,
    config_path: Option<PathBuf>,
) -> Option<contextos_service::process_record::ProcessRecordGuard> {
    match config_path {
        Some(path) if serving_http => claim_process_record_blocking(path).await,
        _ => None,
    }
}

/// Writes this process's record beside the configuration it loaded (`contextos_service::process_record`), or logs why
/// not. A server that cannot write it still serves, and only `contextos service` loses sight of it.
async fn claim_process_record_blocking(
    config_path: PathBuf,
) -> Option<contextos_service::process_record::ProcessRecordGuard> {
    let outcome = tokio::task::spawn_blocking(move || {
        // Named as `contextos service` names it, which makes the path absolute first.
        let absolute = std::path::absolute(&config_path)?;
        claim_process_record(&absolute)
    })
    .await;
    match outcome {
        Ok(Ok(guard)) => Some(guard),
        // Only Windows' service commands read the record, so only there is a missing one worth a warning.
        Ok(Err(error)) if cfg!(windows) => {
            tracing::warn!(%error, "no process record written; `contextos service` cannot see this server");
            None
        }
        Ok(Err(error)) => {
            tracing::debug!(%error, "the process record could not be written");
            None
        }
        Err(error) => {
            tracing::warn!(%error, "writing the process record failed; `contextos service` cannot see this server");
            None
        }
    }
}

/// Serves HTTP on `listener` until `shutdown` is cancelled, then lets in-flight requests finish.
fn spawn_http(
    listener: tokio::net::TcpListener,
    server: &ContextOsServer,
    http_config: &contextos_mcp::HttpConfig,
    shutdown: &CancellationToken,
) -> Result<tokio::task::JoinHandle<std::io::Result<()>>, Box<dyn Error + Send + Sync>> {
    let bound = listener.local_addr()?;
    let router = contextos_mcp::build_router(server.clone(), http_config)?;
    tracing::info!(transport = "http", bind = %bound, "starting ContextOS MCP HTTP server");
    let shutdown = shutdown.clone();
    Ok(tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { shutdown.cancelled().await })
            .await
    }))
}

/// Runs `contextos index`: rebuilds every enabled vault search index and prints the report.
async fn run_index_command(config: Config) -> Result<(), Box<dyn Error + Send + Sync>> {
    let report = tokio::task::spawn_blocking(move || IndexReport::try_from(&config)).await??;
    let failed = report.has_failures();
    std::io::stdout().lock().write_all(report.to_string().as_bytes())?;
    if failed {
        return Err(std::io::Error::other("index rebuild found checks requiring action").into());
    }
    Ok(())
}

/// Runs `contextos service ...` on a blocking thread: it runs platform commands and reads files, which never happens
/// on an async executor thread.
async fn dispatch_service_command(
    action: ServiceAction,
    config_path: PathBuf,
    http: Option<String>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let command = ServiceCommand {
        config_path,
        http: http.map(CliHttp::from),
    };
    let report = tokio::task::spawn_blocking(move || run_service_command(action, &command)).await??;
    writeln!(std::io::stdout().lock(), "{report}")?;
    Ok(())
}

/// Runs a `contextos update`/`rollback`/`install` subcommand's blocking body (network retrieval,
/// signature verification, extraction, and filesystem writes) off the async runtime thread, and
/// prints its human-readable result.
async fn run_update_transaction(
    body: impl FnOnce() -> Result<String, Box<dyn Error + Send + Sync>> + Send + 'static,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let output = tokio::task::spawn_blocking(body).await??;
    println!("{output}");
    Ok(())
}

/// Dispatches `contextos update[/check/cleanup/diagnostics]`, `rollback`, and
/// `install --version`, all of which are vault-independent and never reach `Config::try_from`.
async fn dispatch_update_command(command: CliCommand) -> Result<(), Box<dyn Error + Send + Sync>> {
    match command {
        CliCommand::Update { action, yes } => {
            run_update_transaction(move || match action {
                None => contextos_mcp::run_update(yes),
                Some(UpdateAction::Check) => contextos_mcp::run_check(),
                Some(UpdateAction::Cleanup) => contextos_mcp::run_cleanup(),
                Some(UpdateAction::Diagnostics) => contextos_mcp::run_diagnostics(),
            })
            .await
        }
        CliCommand::Rollback => run_update_transaction(contextos_mcp::run_rollback).await,
        CliCommand::Install { version } => run_update_transaction(move || contextos_mcp::run_install(version)).await,
        _ => Err("dispatch_update_command received a command it does not handle".into()),
    }
}

/// Runs `contextos doctor`, or `contextos doctor --resolve`, which resolves through the same dispatch as the
/// `doctor_resolve` MCP tool (`contextos_mcp::resolve_for_cli`), or with `--dry-run` previews through the same
/// selection as that tool's dry run (`contextos_mcp::preview_resolution_for_cli`), so CLI and MCP stay behaviourally
/// identical for the auto-fixable set.
async fn run_doctor_command(config: Config, resolve: bool, dry_run: bool) -> Result<(), Box<dyn Error + Send + Sync>> {
    if resolve {
        let (outcomes, search_unavailable) = if dry_run {
            // A dry run must write nothing, and composing a server opens derived state, so the preview is computed
            // from the configuration alone.
            let config_for_preview = config.clone();
            let outcomes =
                tokio::task::spawn_blocking(move || contextos_mcp::preview_resolution_for_cli(&config_for_preview))
                    .await??;
            (outcomes, Vec::new())
        } else {
            let report = contextos_mcp::resolve_for_cli(config.clone()).await?;
            (report.outcomes, report.search_unavailable)
        };
        let mut stdout = std::io::stdout().lock();
        for outcome in &outcomes {
            let verb = match (dry_run, outcome.resolved) {
                (true, _) => "Would resolve",
                (false, true) => "Resolved",
                (false, false) => "Not resolved",
            };
            writeln!(
                stdout,
                "{verb}: {} ({}): {}",
                outcome.subject,
                outcome.vault_path.as_deref().unwrap_or("no single vault"),
                outcome.message
            )?;
        }
        for skipped in &search_unavailable {
            writeln!(
                stdout,
                "Warning: search could not be opened for {} ({}), so its indexes do not yet reflect what was resolved: \
                 {}. Correct the cause, then run `contextos index`.",
                skipped.vault_name, skipped.vault_path, skipped.reason
            )?;
        }
        let report = tokio::task::spawn_blocking(move || {
            if dry_run {
                DoctorReport::without_derived_state(&config)
            } else {
                DoctorReport::try_from(&config)
            }
        })
        .await??;
        let failed = report.has_failures();
        stdout.write_all(report.to_string().as_bytes())?;
        if failed {
            return Err(std::io::Error::other("doctor found checks requiring action").into());
        }
        return Ok(());
    }
    let report = tokio::task::spawn_blocking(move || DoctorReport::try_from(&config)).await??;
    let failed = report.has_failures();
    std::io::stdout().lock().write_all(report.to_string().as_bytes())?;
    if failed {
        return Err(std::io::Error::other("doctor found checks requiring action").into());
    }
    Ok(())
}

/// Runs a `contextos model` subcommand. Vault-independent: fetches or
/// reports on the shared local embedding model cache without loading any
/// `Config`, so it works with no `--config` or `--vault` supplied.
async fn run_model_command(action: ModelAction) -> Result<(), Box<dyn Error + Send + Sync>> {
    let report = tokio::task::spawn_blocking(move || {
        let cache_dir = default_model_cache_dir()?;
        match action {
            ModelAction::List => Ok(ModelReport::list(&cache_dir)),
            ModelAction::Download => ModelReport::download(&cache_dir),
        }
    })
    .await??;
    std::io::stdout().lock().write_all(report.to_string().as_bytes())?;
    Ok(())
}

/// Dispatches a `contextos config` invocation: a subcommand (`vault`/`mcp`)
/// straight to [`run_config_command`], or no subcommand at all to the
/// interactive guided-setup interview.
async fn dispatch_config_command(
    action: Option<ConfigAction>,
    config_path: PathBuf,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    match action {
        Some(action) => run_config_command(action, config_path).await,
        None => run_config_interview(config_path).await,
    }
}

/// Runs a `contextos config` subcommand: structure-preserving edits to
/// `config.toml` via `ConfigDocument` (`config_writer.rs`). Vault-
/// independent in the same sense `Model` is: it works against a not-yet-
/// valid or not-yet-existing configuration file, so it never goes through
/// `Config::try_from(ConfigLoadInput)`'s strict load.
async fn run_config_command(action: ConfigAction, config_path: PathBuf) -> Result<(), Box<dyn Error + Send + Sync>> {
    match action {
        ConfigAction::Vault { action } => run_config_vault_command(action, config_path).await,
        ConfigAction::Mcp { action } => run_config_mcp_command(action, config_path).await,
    }
}

/// Runs `contextos config` with no subcommand: the interactive guided-setup
/// interview. Real terminal I/O and real network/host-detection
/// dependencies, unlike `run_interview` itself, which is exercised in tests
/// against a scripted `Interviewer` and fake dependencies instead.
async fn run_config_interview(config_path: PathBuf) -> Result<(), Box<dyn Error + Send + Sync>> {
    let command = std::env::current_exe()?
        .to_str()
        .ok_or_else(|| std::io::Error::other("this binary's own path is not valid UTF-8"))?
        .to_owned();
    let report = tokio::task::spawn_blocking(move || -> Result<_, Box<dyn Error + Send + Sync>> {
        let model_cache_dir = default_model_cache_dir()?;
        let detector = SystemProcessDetector;
        let wait_tick_calls = std::sync::atomic::AtomicU32::new(0);
        let wait_tick = move || {
            let call = wait_tick_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if call.is_multiple_of(5) {
                tracing::info!("still waiting for Claude Desktop to close");
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        };
        let environment = InterviewEnvironment {
            config_path,
            model_cache_dir,
            download_model: &download_default_model,
            resolve_host_path: &default_claude_desktop_config_path,
            process_detector: &detector,
            wait_tick: &wait_tick,
            server_command: command,
            default_log_file: contextos_mcp::default_log_file(),
        };
        let mut interviewer = TerminalInterviewer;
        Ok(run_interview(&mut interviewer, &environment)?)
    })
    .await??;
    std::io::stdout().lock().write_all(report.to_string().as_bytes())?;
    Ok(())
}

/// Resolves the host config-file path for a `contextos config mcp`
/// subcommand: the explicit `--config-path` override when given, otherwise
/// platform discovery, rejecting `Ambiguous`/`NotFound` with a message
/// telling the operator to pass `--config-path` explicitly.
fn resolve_host_config_path(
    host: HostChoice,
    config_path: Option<PathBuf>,
) -> Result<PathBuf, Box<dyn Error + Send + Sync>> {
    if let Some(path) = config_path {
        return Ok(path);
    }
    let HostChoice::ClaudeDesktop = host;
    match default_claude_desktop_config_path()? {
        HostPathResolution::Found(path) => Ok(path),
        HostPathResolution::NotFound { reason } => Err(std::io::Error::other(format!(
            "could not locate Claude Desktop's configuration file ({reason}); pass \
             --config-path explicitly"
        ))
        .into()),
        HostPathResolution::Ambiguous { candidates } => Err(std::io::Error::other(format!(
            "more than one Claude Desktop configuration file candidate was found ({}); pass \
             --config-path explicitly to pick one",
            candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .into()),
    }
}

async fn run_config_mcp_command(
    action: ConfigMcpAction,
    server_config_path: PathBuf,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut stdout = std::io::stdout().lock();
    match action {
        ConfigMcpAction::Register {
            host,
            config_path,
            force,
        } => {
            let host_path = resolve_host_config_path(host, config_path)?;
            let command = std::env::current_exe()?
                .to_str()
                .ok_or_else(|| std::io::Error::other("this binary's own path is not valid UTF-8"))?
                .to_owned();
            let server_config_path_str = server_config_path
                .to_str()
                .ok_or_else(|| std::io::Error::other("configuration path is not valid UTF-8"))?
                .to_owned();
            let entry = RegisteredServer {
                command,
                args: vec!["--config".to_owned(), server_config_path_str],
            };
            let detector = SystemProcessDetector;
            let host_path_for_task = host_path.clone();
            tokio::task::spawn_blocking(move || register(&host_path_for_task, &entry, &detector, force)).await??;
            if force {
                tracing::warn!(
                    host_path = %host_path.display(),
                    "registered contextos with --force while the host may still be running"
                );
            }
            writeln!(stdout, "Registered contextos with {}.", host_path.display())?;
        }
        ConfigMcpAction::Deregister {
            host,
            config_path,
            force,
        } => {
            let host_path = resolve_host_config_path(host, config_path)?;
            let detector = SystemProcessDetector;
            let host_path_for_task = host_path.clone();
            let outcome =
                tokio::task::spawn_blocking(move || deregister(&host_path_for_task, &detector, force)).await??;
            if force {
                tracing::warn!(
                    host_path = %host_path.display(),
                    "deregistered contextos with --force while the host may still be running"
                );
            }
            match outcome {
                DeregisterOutcome::Removed => {
                    writeln!(stdout, "Deregistered contextos from {}.", host_path.display())?;
                }
                DeregisterOutcome::NotRegistered => {
                    writeln!(stdout, "contextos was not registered with {}.", host_path.display())?;
                }
            }
        }
        ConfigMcpAction::Status { host, config_path } => {
            let host_path = resolve_host_config_path(host, config_path)?;
            let host_path_for_task = host_path.clone();
            let status = tokio::task::spawn_blocking(move || host_status(&host_path_for_task)).await??;
            match status {
                RegistrationStatus::Registered(entry) => {
                    writeln!(
                        stdout,
                        "contextos is registered with {}: {} {}",
                        host_path.display(),
                        entry.command,
                        entry.args.join(" ")
                    )?;
                }
                RegistrationStatus::NotRegistered => {
                    writeln!(stdout, "contextos is not registered with {}.", host_path.display())?;
                }
            }
        }
    }
    Ok(())
}

async fn run_config_vault_command(
    action: ConfigVaultAction,
    config_path: PathBuf,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut stdout = std::io::stdout().lock();
    match action {
        ConfigVaultAction::List => {
            let read_path = config_path.clone();
            let vaults = tokio::task::spawn_blocking(move || {
                Ok::<_, Box<dyn Error + Send + Sync>>(load_config_document(&read_path)?.vaults())
            })
            .await??;
            if vaults.is_empty() {
                writeln!(stdout, "No vaults configured in {}.", config_path.display())?;
            } else {
                writeln!(stdout, "Vaults configured in {}:", config_path.display())?;
                for vault in vaults {
                    let managed = if vault.managed { "managed" } else { "unmanaged" };
                    writeln!(stdout, "  {} -> {} ({managed})", vault.name, vault.path.display())?;
                }
            }
        }
        ConfigVaultAction::Add { name, path, unmanaged } => {
            let write_path = config_path.clone();
            let (display_name, display_path) = (name.clone(), path.clone());
            tokio::task::spawn_blocking(move || -> Result<(), Box<dyn Error + Send + Sync>> {
                let mut document = load_config_document(&write_path)?;
                document.add_vault(&name, &path, !unmanaged)?;
                write_config_document(&write_path, &document)?;
                Ok(())
            })
            .await??;
            writeln!(
                stdout,
                "Added vault {display_name:?} ({}) to {}.",
                display_path.display(),
                config_path.display()
            )?;
        }
        ConfigVaultAction::Remove { name } => {
            let write_path = config_path.clone();
            let display_name = name.clone();
            tokio::task::spawn_blocking(move || -> Result<(), Box<dyn Error + Send + Sync>> {
                let mut document = load_config_document(&write_path)?;
                document.remove_vault(&name)?;
                write_config_document(&write_path, &document)?;
                Ok(())
            })
            .await??;
            writeln!(stdout, "Removed vault {display_name:?} from {}.", config_path.display())?;
        }
    }
    Ok(())
}

/// Resolves the `config.toml` path for a `contextos config` subcommand,
/// matching `ConfigLoadInput`'s own precedence (`--config`, then
/// `CONTEXTOS_MCP_CONFIG`, then the default path) but standalone: this
/// subcommand edits the file directly and must run before
/// `Config::try_from` would reject a not-yet-valid or not-yet-existing one.
fn resolve_config_toml_path(cli_config_path: Option<PathBuf>) -> Result<PathBuf, Box<dyn Error + Send + Sync>> {
    cli_config_path
        .or_else(|| env::var_os("CONTEXTOS_MCP_CONFIG").map(PathBuf::from))
        .or_else(default_config_path)
        .ok_or_else(|| {
            std::io::Error::other(
                "no configuration file path could be determined; pass --config or set \
                 CONTEXTOS_MCP_CONFIG",
            )
            .into()
        })
}

#[cfg(unix)]
async fn shutdown_signal() -> Result<(), std::io::Error> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = terminate.recv() => Ok(()),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> Result<(), std::io::Error> {
    tokio::signal::ctrl_c().await
}

/// Fallback configuration file path used when neither `--config` nor
/// `CONTEXTOS_MCP_CONFIG` is set: `$HOME/.config/contextos/config.toml` on
/// every platform, so an operator can `contextos index` (or any other
/// subcommand) without ever passing a path, by simply placing their config
/// at this one well-known, documented location.
fn default_config_path() -> Option<PathBuf> {
    let folders = HostFolders::detect()?;
    Some(folders.home().join(".config").join("contextos").join("config.toml"))
}

/// Builds the actual `tracing` subscriber from `[server]`'s logging fields:
/// `log_level` always applied, `log_file` (with its `log_max_size_mb`,
/// `log_rotate_daily`, `log_retention_days`, `log_retention_files`
/// companions) opened as a real rotating destination when set, stderr (the
/// prior, still-default behaviour) when it is empty. Mirrors
/// `contextos-web`'s own `initialise_tracing`/`logging::open` pairing.
fn initialise_tracing(server: &ServerConfig) -> Result<(), Box<dyn Error + Send + Sync>> {
    let filter = match server.log_level {
        LogLevel::Error => "error",
        LogLevel::Warn => "warn",
        LogLevel::Info => "info",
        LogLevel::Debug => "debug",
        LogLevel::Trace => "trace",
    };
    let writer = match contextos_mcp::open_log(server)? {
        Some(rotating) => tracing_subscriber::fmt::writer::BoxMakeWriter::new(std::sync::Mutex::new(rotating)),
        None => tracing_subscriber::fmt::writer::BoxMakeWriter::new(std::io::stderr),
    };
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .with_writer(writer)
        .with_ansi(false)
        // A log write that fails, as every one does once whatever read standard error has gone, is dropped: reporting
        // it would mean `eprintln!` to that same closed stream, which panics and takes the server down with it.
        .log_internal_errors(false)
        .try_init()?;
    Ok(())
}

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;
