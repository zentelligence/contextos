//! The `contextos-web workflows releases` command: arguments in, text or one JSON document out.
//!
//! The command only connects to the vault's primary server, or reads the host's folders, and renders what
//! [`contextos_web::workflow_releases_stores`] returned. Which kind each release is, and what is reported about it, is
//! decided there. The one thing the command writes to standard output is its result, and a refusal is part of that
//! result; it then ends the process with [`Refusal`](crate::job_authoring_commands::Refusal), which `main` turns into
//! exit status 1.

use std::error::Error;
use std::io::Write as _;
use std::path::Path;

use clap::Args;
use contextos_web::load_web_config;
use contextos_web::package_store;
use contextos_web::shared_state::SharedStateStore;
use contextos_web::workflow_releases::WorkflowReleases;
use contextos_web::workflow_releases_error::WorkflowReleasesError;
use contextos_web::workflow_releases_report::{ReleasesJson, ReleasesRefusalJson, ReleasesRefusalText, ReleasesText};
use contextos_web::workflow_releases_stores::{SharedReleases, VaultReleases};

use crate::one_shot_sessions::connect;

use crate::job_authoring_commands::{OutputFormat, Refusal};
use crate::job_target::WorkflowTarget;

/// Which workflow's releases are asked for.
#[derive(Clone, Debug, Args)]
pub(crate) struct ReleasesArguments {
    /// The workflow: `<vault> <package> <workflow>`, or with `--shared` `<package> <workflow>`.
    #[arg(required = true, num_args = 2..=3, value_name = "VAULT PACKAGE WORKFLOW")]
    pub(crate) target: Vec<String>,
    /// Reports the releases in this host's package store instead of a vault's.
    #[arg(long)]
    pub(crate) shared: bool,
    /// `text`, or `json` for one document on standard output.
    #[arg(long, value_enum, default_value_t)]
    pub(crate) format: OutputFormat,
}

/// Reports the releases, writes the result, and ends with [`Refusal`] when it could not be made.
pub(crate) async fn run_releases_command(
    config_path: &Path,
    arguments: ReleasesArguments,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let target =
        WorkflowTarget::try_from((arguments.target.as_slice(), arguments.shared)).unwrap_or_else(|error| error.exit());
    let format = arguments.format;
    let result = match crate::tracing_setup::initialise_command_tracing(config_path) {
        // Boxed so the large session and server futures stay out of the command's own state.
        Ok(()) => Box::pin(carry_out(config_path, &target)).await,
        Err(error) => Err(WorkflowReleasesError::ConfigInvalid { reason: error.reason }),
    };
    match result {
        Ok(report) => {
            write_result(&render_report(&report, format))?;
            Ok(())
        }
        Err(error) => {
            // Debug only: the refusal is the command's result, which the caller already has.
            tracing::debug!(code = error.code(), "a report of a workflow's releases was refused");
            write_result(&render_refusal(&error, format))?;
            Err(Refusal.into())
        }
    }
}

async fn carry_out(config_path: &Path, target: &WorkflowTarget) -> Result<WorkflowReleases, WorkflowReleasesError> {
    match target {
        WorkflowTarget::Shared { package, workflow } => {
            let host = |_| WorkflowReleasesError::Host {
                reason: "the host's data folder could not be determined",
            };
            SharedReleases {
                store_root: package_store::default_store_root().map_err(host)?,
                jobs_root: package_store::default_jobs_root().map_err(host)?,
                state: SharedStateStore::new(package_store::default_state_root().map_err(host)?),
            }
            .report(package, workflow)
            .await
        }
        WorkflowTarget::Vault {
            vault,
            package,
            workflow,
        } => {
            let config = load_web_config(config_path).map_err(|error| WorkflowReleasesError::ConfigInvalid {
                reason: crate::tracing_setup::ConfigInvalid::from(&error).reason,
            })?;
            let clients = connect(&config, config_path)
                .await
                .map_err(|_| WorkflowReleasesError::NotConnected {
                    reason: "the vault's server could not be reached",
                })?;
            let known_servers: Vec<String> = config.mcp_servers.iter().map(|entry| entry.name().to_owned()).collect();
            let client = known_servers.first().and_then(|primary| clients.get(primary)).ok_or(
                WorkflowReleasesError::NotConnected {
                    reason: "no server is configured to reach the vault through",
                },
            )?;
            VaultReleases {
                client: &client,
                vault,
                known_servers: &known_servers,
            }
            .report(package, workflow)
            .await
        }
    }
}

fn render_report(report: &WorkflowReleases, format: OutputFormat) -> String {
    match format {
        OutputFormat::Json => ReleasesJson(report).to_string(),
        OutputFormat::Text => ReleasesText(report).to_string(),
    }
}

fn render_refusal(error: &WorkflowReleasesError, format: OutputFormat) -> String {
    match format {
        OutputFormat::Json => ReleasesRefusalJson(error).to_string(),
        OutputFormat::Text => ReleasesRefusalText(error).to_string(),
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "the one place the releases command writes its result to standard output"
)]
fn write_result(text: &str) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(text.as_bytes())?;
    if !text.ends_with('\n') {
        stdout.write_all(b"\n")?;
    }
    stdout.flush()
}
