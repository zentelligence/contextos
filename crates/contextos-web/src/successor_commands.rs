//! The `contextos-web jobs successor` command: arguments in, text or one JSON document out.
//!
//! The command only parses its arguments, connects to the vault's primary server or reads the host's folders, and
//! renders what [`contextos_web::successor_stores`] returned. Which successors are made, and every refusal, is decided
//! there and below it. The one thing the command writes to standard output is its result, and a refusal is part of that
//! result; it then ends the process with [`Refusal`], which `main` turns into exit status 1.

use std::error::Error;
use std::io::Write as _;
use std::path::Path;

use clap::{Args, CommandFactory};
use contextos_web::load_web_config;
use contextos_web::package_store;
use contextos_web::shared_job_authoring::AuthoringError;
use contextos_web::shared_state::SharedStateStore;
use contextos_web::successor_plan::{SuccessorRefused, SuccessorScope};
use contextos_web::successor_report::{SuccessorJson, SuccessorReport, SuccessorText};
use contextos_web::successor_service::SuccessorRequest;
use contextos_web::successor_stores::{SharedSuccessors, VaultSuccessors};
use contextos_workflows::SuccessorTarget;
use thiserror::Error;

use crate::job_authoring_commands::{OutputFormat, Refusal};
use crate::one_shot_sessions::connect;

#[cfg(test)]
#[path = "successor_commands_test.rs"]
mod tests;

/// The arguments of `jobs successor`.
#[derive(Clone, Debug, Args)]
pub(crate) struct SuccessorArguments {
    /// What to make a successor of, then what to call it: `<vault> <package> <workflow> <job> <new-job>`, or with
    /// `--shared` `<package> <workflow> <job> <new-job>`. With `--all` there is no job: `<vault> <package> <workflow>`,
    /// or with `--shared` `<package> <workflow>`.
    #[arg(required = true, num_args = 2..=5, value_name = "VAULT PACKAGE WORKFLOW JOB NEW-JOB")]
    pub(crate) words: Vec<String>,
    /// The workflow is in a package installed in this host's package store, with its jobs on the host, and not in a vault.
    #[arg(long)]
    pub(crate) shared: bool,
    /// Makes a successor for every job of the workflow in `enabled` or `suspended` on the current release, each named
    /// for its predecessor with `-v` and the release's version, such as `nightly-v2-0-0`. When any key is taken or any
    /// job would be refused, nothing is written and every refusal is reported.
    #[arg(long)]
    pub(crate) all: bool,
    /// The release the successors name: `future`, the newest future release (the default); `previous`, which makes
    /// return successors for a job on the current release; or an exact version such as `2.1.0`.
    #[arg(
        long,
        default_value = "future",
        value_parser = |value: &str| SuccessorTarget::try_from(value)
    )]
    pub(crate) to: SuccessorTarget,
    /// `text`, or `json` for one document on standard output.
    #[arg(long, value_enum, default_value_t)]
    pub(crate) format: OutputFormat,
}

/// Where the successors are made.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Where {
    /// The host's package store and jobs.
    Shared,
    /// A vault, through its primary server.
    Vault(String),
}

/// What the words of a command asked for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Asked {
    pub(crate) place: Where,
    pub(crate) request: SuccessorRequest,
}

/// The words after `jobs successor` do not name what to make a successor of.
#[derive(Debug, Error, Eq, PartialEq)]
#[error("{expected} is named by {what}, but {found} words were given")]
pub(crate) struct WordsError {
    expected: &'static str,
    what: &'static str,
    found: usize,
}

impl WordsError {
    /// Reports the mistake as the usage error it is and ends the process with the status a command line that was not
    /// understood has.
    pub(crate) fn exit(&self) -> ! {
        crate::Cli::command()
            .error(clap::error::ErrorKind::WrongNumberOfValues, self)
            .exit()
    }
}

impl TryFrom<&SuccessorArguments> for Asked {
    type Error = WordsError;

    fn try_from(arguments: &SuccessorArguments) -> Result<Self, Self::Error> {
        let found = arguments.words.len();
        let wrong = |expected, what| WordsError { expected, what, found };
        let (place, package, workflow, scope) = match (arguments.shared, arguments.all, arguments.words.as_slice()) {
            (false, false, [vault, package, workflow, job, new_job]) => (
                Where::Vault(vault.clone()),
                package,
                workflow,
                SuccessorScope::One {
                    job: job.clone(),
                    new_job: new_job.clone(),
                },
            ),
            (true, false, [package, workflow, job, new_job]) => (
                Where::Shared,
                package,
                workflow,
                SuccessorScope::One {
                    job: job.clone(),
                    new_job: new_job.clone(),
                },
            ),
            (false, true, [vault, package, workflow]) => {
                (Where::Vault(vault.clone()), package, workflow, SuccessorScope::All)
            }
            (true, true, [package, workflow]) => (Where::Shared, package, workflow, SuccessorScope::All),
            (false, false, _) => {
                return Err(wrong(
                    "a successor in a vault",
                    "<vault> <package> <workflow> <job> <new-job>",
                ));
            }
            (true, false, _) => {
                return Err(wrong("a shared successor", "<package> <workflow> <job> <new-job>"));
            }
            (false, true, _) => {
                return Err(wrong(
                    "every successor in a vault's workflow",
                    "<vault> <package> <workflow>",
                ));
            }
            (true, true, _) => return Err(wrong("every shared successor", "<package> <workflow>")),
        };
        Ok(Self {
            place,
            request: SuccessorRequest {
                package: package.clone(),
                workflow: workflow.clone(),
                scope,
                to: arguments.to.clone(),
            },
        })
    }
}

/// Makes the successors the arguments ask for, prints the result, and ends with [`Refusal`] when any was refused.
pub(crate) async fn run_successor_command(
    config_path: &Path,
    arguments: SuccessorArguments,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let asked = Asked::try_from(&arguments).unwrap_or_else(|error| error.exit());
    let format = arguments.format;
    let report = match crate::tracing_setup::initialise_command_tracing(config_path) {
        // Boxed so the large session and server futures stay out of the command's own state.
        Ok(()) => Box::pin(carry_out(config_path, &asked)).await,
        Err(error) => refused_report(&asked, AuthoringError::ConfigInvalid { reason: error.reason }),
    };
    if report.is_refused() {
        // Debug only: the refusal is the command's result, which the caller already has.
        tracing::debug!(refusals = report.refused.len(), "a request for successors was refused");
    }
    write_result(&render(&report, format))?;
    if report.is_refused() {
        Err(Refusal.into())
    } else {
        Ok(())
    }
}

fn refused_report(asked: &Asked, error: AuthoringError) -> SuccessorReport {
    SuccessorReport {
        store: match &asked.place {
            Where::Shared => "shared".to_owned(),
            Where::Vault(vault) => vault.clone(),
        },
        package: asked.request.package.clone(),
        workflow: asked.request.workflow.clone(),
        to: None,
        release: None,
        made: Vec::new(),
        refused: vec![SuccessorRefused { job: None, error }],
    }
}

async fn carry_out(config_path: &Path, asked: &Asked) -> SuccessorReport {
    match &asked.place {
        Where::Shared => {
            let unavailable = || {
                refused_report(
                    asked,
                    AuthoringError::JobStoreUnavailable {
                        action: "the host's data folder could not be determined",
                        kind: None,
                    },
                )
            };
            let (Ok(jobs_root), Ok(store_root), Ok(state_root)) = (
                package_store::default_jobs_root(),
                package_store::default_store_root(),
                package_store::default_state_root(),
            ) else {
                return unavailable();
            };
            SharedSuccessors {
                jobs_root,
                store_root,
                state: SharedStateStore::new(state_root),
            }
            .make(&asked.request)
            .await
        }
        Where::Vault(vault) => {
            let config = match load_web_config(config_path) {
                Ok(config) => config,
                Err(error) => {
                    return refused_report(
                        asked,
                        AuthoringError::ConfigInvalid {
                            reason: crate::tracing_setup::ConfigInvalid::from(&error).reason,
                        },
                    );
                }
            };
            let Ok(clients) = connect(&config, config_path).await else {
                return refused_report(
                    asked,
                    AuthoringError::JobStoreUnavailable {
                        action: "the vault's server could not be reached",
                        kind: None,
                    },
                );
            };
            let known_servers: Vec<String> = config.mcp_servers.iter().map(|entry| entry.name().to_owned()).collect();
            let Some(client) = known_servers.first().and_then(|primary| clients.get(primary)) else {
                return refused_report(
                    asked,
                    AuthoringError::JobStoreUnavailable {
                        action: "no server is configured to reach the vault through",
                        kind: None,
                    },
                );
            };
            VaultSuccessors {
                client: &client,
                vault,
                known_servers: &known_servers,
            }
            .make(&asked.request)
            .await
        }
    }
}

fn render(report: &SuccessorReport, format: OutputFormat) -> String {
    match format {
        OutputFormat::Json => SuccessorJson(report).to_string(),
        OutputFormat::Text => SuccessorText(report).to_string(),
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "the one place the successor command writes its result to standard output"
)]
fn write_result(text: &str) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(text.as_bytes())?;
    if !text.ends_with('\n') {
        stdout.write_all(b"\n")?;
    }
    stdout.flush()
}
