//! The `contextos-web jobs create`, `show`, `replace`, and `list` commands: arguments in, text or one JSON document
//! out.
//!
//! The commands only parse their arguments and render what [`contextos_web::shared_job_authoring`] returned. What each
//! one means, and every check it makes, is decided there.
//!
//! The one thing a command writes to standard output is its result, through [`write_result`], and a refusal is part of
//! that result: the `{"error":{...}}` document for `--format json`, a line of text otherwise. A refusal then ends the
//! process with [`Refusal`], which `main` turns into exit status 1 without printing a second report. Diagnostics go to
//! `tracing`, never to a stream directly.

use std::error::Error;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand, ValueEnum};
use contextos_core::ContentHash;
use contextos_web::package_store;
use contextos_web::shared_claim::SharedJobKey;

use crate::job_target::JobTarget;
use contextos_web::load_web_config;
use contextos_web::shared_job_authoring::{
    Authored, AuthoringError, AuthoringWarning, JobListing, ListingProblem, SharedJobAuthoring, ShownJob,
    read_job_input,
};
use contextos_web::shared_state::SharedStateStore;
use contextos_web::vault_job_authoring::VaultJobAuthoring;

use crate::one_shot_sessions::connect;
use serde::Serialize;
use thiserror::Error;

/// How a command prints its result.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum OutputFormat {
    /// Human-readable lines.
    #[default]
    Text,
    /// One JSON document on standard output.
    Json,
}

/// Where a job is kept, which is also how a result names it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Store {
    /// The host's jobs folder, for a workflow installed in the host package store.
    Shared,
    /// A vault, written through its primary server.
    Vault(String),
}

impl Store {
    /// The word a result names the store with: `shared`, or the vault's name.
    fn word(&self) -> &str {
        match self {
            Self::Shared => "shared",
            Self::Vault(name) => name,
        }
    }
}

/// The job a command is about.
#[derive(Clone, Debug, Args)]
pub(crate) struct AuthoredJob {
    /// The job: `<vault> <package> <workflow> <job>`, or with `--shared` `<package> <workflow> <job>`. The job is its
    /// file's stem.
    #[arg(required = true, num_args = 3..=4, value_name = "VAULT PACKAGE WORKFLOW JOB")]
    pub(crate) target: Vec<String>,
    /// The job is a shared job, held in the host's jobs folder, and not in a vault.
    #[arg(long)]
    pub(crate) shared: bool,
}

impl AuthoredJob {
    /// The job the words name, or the usage error that ends the process when they do not.
    fn target(&self) -> JobTarget {
        JobTarget::try_from((self.target.as_slice(), self.shared)).unwrap_or_else(|error| error.exit())
    }

    fn store(&self) -> Store {
        match self.target() {
            JobTarget::Shared(_) => Store::Shared,
            JobTarget::Vault { vault, .. } => Store::Vault(vault),
        }
    }

    /// The key of the job, whichever store holds it.
    fn key(&self) -> SharedJobKey {
        match self.target() {
            JobTarget::Shared(key) => key,
            JobTarget::Vault { key, .. } => SharedJobKey {
                package: key.package,
                workflow: key.workflow,
                job: key.job,
            },
        }
    }
}

/// The commands that author and inspect a job's file.
#[derive(Clone, Debug, Subcommand)]
pub(crate) enum AuthoringAction {
    /// Creates a job's file from TOML, which is stored exactly as given and never overwrites an existing file.
    Create {
        #[command(flatten)]
        job: AuthoredJob,
        /// The TOML to store: a file path, or `-` for standard input.
        #[arg(long)]
        file: PathBuf,
        /// `text`, or `json` for one document on standard output.
        #[arg(long, value_enum, default_value_t)]
        format: OutputFormat,
    },
    /// Prints a job's file and its content hash.
    Show {
        #[command(flatten)]
        job: AuthoredJob,
        /// `text`, or `json` for one document on standard output.
        #[arg(long, value_enum, default_value_t)]
        format: OutputFormat,
    },
    /// Replaces a job's file, when it still has the hash `show` printed.
    Replace {
        #[command(flatten)]
        job: AuthoredJob,
        /// The new TOML: a file path, or `-` for standard input.
        #[arg(long)]
        file: PathBuf,
        /// The SHA-256 hash of the file as the caller last read it, 64 hexadecimal characters.
        #[arg(long, value_parser = |value: &str| ContentHash::try_from(value))]
        expected_hash: ContentHash,
        /// `text`, or `json` for one document on standard output.
        #[arg(long, value_enum, default_value_t)]
        format: OutputFormat,
    },
    /// Lists every job file and stored job state of a vault, or with `--shared` of this host.
    List {
        /// The vault to list.
        #[arg(conflicts_with = "shared", required_unless_present = "shared")]
        vault: Option<String>,
        /// Lists shared jobs, which are held in the host's jobs folder.
        #[arg(long)]
        shared: bool,
        /// `text`, or `json` for one document on standard output.
        #[arg(long, value_enum, default_value_t)]
        format: OutputFormat,
    },
}

impl AuthoringAction {
    /// Where the job or jobs the command is about are kept.
    fn store(&self) -> Store {
        match self {
            Self::Create { job, .. } | Self::Show { job, .. } | Self::Replace { job, .. } => job.store(),
            Self::List { vault, shared, .. } => match vault {
                Some(vault) if !shared => Store::Vault(vault.clone()),
                _ => Store::Shared,
            },
        }
    }

    const fn format(&self) -> OutputFormat {
        match self {
            Self::Create { format, .. }
            | Self::Show { format, .. }
            | Self::Replace { format, .. }
            | Self::List { format, .. } => *format,
        }
    }
}

/// What an authoring command came to, ready to print.
#[derive(Debug)]
pub(crate) enum AuthoringReport {
    /// A create or replace that was carried out, or found to be already done.
    Authored(Store, Authored),
    /// A job file as it is stored.
    Shown(Store, ShownJob),
    /// Every job in the store.
    Listed(Store, Vec<JobListing>),
    /// A request that was refused.
    Refused(AuthoringError),
}

/// What a command's blocking task came to: its report, or the refusal it ended in, or the task itself failing.
impl From<Result<Result<AuthoringReport, AuthoringError>, tokio::task::JoinError>> for AuthoringReport {
    fn from(joined: Result<Result<AuthoringReport, AuthoringError>, tokio::task::JoinError>) -> Self {
        match joined {
            Ok(Ok(report)) => report,
            Ok(Err(error)) => Self::Refused(error),
            Err(_) => Self::Refused(AuthoringError::JobStoreUnavailable {
                action: "the authoring task did not complete",
                kind: None,
            }),
        }
    }
}

/// Marks a command that was refused and has already reported that in its result, so the process exits with status 1
/// and prints nothing more.
#[derive(Debug, Error)]
#[error("the command was refused, and the refusal is in its result")]
pub(crate) struct Refusal;

#[derive(Serialize)]
struct WarningView {
    code: &'static str,
    message: String,
}

impl From<&AuthoringWarning> for WarningView {
    fn from(warning: &AuthoringWarning) -> Self {
        let message = match warning {
            AuthoringWarning::PackageUnavailable { package, workflow } => format!(
                "package {package:?} or its workflow {workflow:?} is not installed, so the job cannot be validated until it is"
            ),
            AuthoringWarning::PackageUnchecked { package, workflow } => format!(
                "package {package:?} and its workflow {workflow:?} could not be checked, because the package store could not be read"
            ),
            AuthoringWarning::RetentionFailed(warning) => warning.message().to_owned(),
        };
        Self {
            code: warning.code(),
            message,
        }
    }
}

#[derive(Serialize)]
struct AuthoredView {
    outcome: &'static str,
    store: String,
    package: String,
    workflow: String,
    job: String,
    hash: String,
    warnings: Vec<WarningView>,
}

impl From<(&Store, &Authored)> for AuthoredView {
    fn from((store, authored): (&Store, &Authored)) -> Self {
        Self {
            outcome: authored.outcome.as_str(),
            store: store.word().to_owned(),
            package: authored.key.package.clone(),
            workflow: authored.key.workflow.clone(),
            job: authored.key.job.clone(),
            hash: <&str>::from(&authored.hash).to_owned(),
            warnings: authored.warnings.iter().map(WarningView::from).collect(),
        }
    }
}

#[derive(Serialize)]
struct ShownView {
    store: String,
    package: String,
    workflow: String,
    job: String,
    hash: String,
    content: String,
}

impl From<(&Store, &ShownJob)> for ShownView {
    fn from((store, shown): (&Store, &ShownJob)) -> Self {
        Self {
            store: store.word().to_owned(),
            package: shown.key.package.clone(),
            workflow: shown.key.workflow.clone(),
            job: shown.key.job.clone(),
            hash: <&str>::from(&shown.hash).to_owned(),
            content: shown.content.clone(),
        }
    }
}

#[derive(Serialize)]
struct ProblemView {
    code: String,
    message: String,
}

impl From<&ListingProblem> for ProblemView {
    fn from(problem: &ListingProblem) -> Self {
        Self {
            code: problem.code.clone(),
            message: problem.message.clone(),
        }
    }
}

#[derive(Serialize)]
struct JobView {
    package: String,
    workflow: String,
    job: String,
    file: Option<String>,
    name: Option<String>,
    state: &'static str,
    schedule: Option<String>,
    ready: Option<bool>,
    version: Option<String>,
    release_kind: Option<&'static str>,
    predecessor: Option<String>,
    successor: Option<String>,
    hash: Option<String>,
    problems: Vec<ProblemView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    suspension: Option<SuspensionView>,
}

/// Why a listed job is suspended, as a script reads it.
#[derive(Serialize)]
struct SuspensionView {
    code: String,
    reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    by: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<String>,
}

impl From<&contextos_workflows::SuspensionRecord> for SuspensionView {
    fn from(record: &contextos_workflows::SuspensionRecord) -> Self {
        Self {
            code: record.code.clone(),
            reason: record.reason.clone(),
            by: record.by.map(contextos_workflows::Actor::as_str),
            at: record.at.map(contextos_web::job_status::stamp),
            run: record.run,
            failure: record.failure.clone(),
        }
    }
}

impl From<&JobListing> for JobView {
    fn from(row: &JobListing) -> Self {
        Self {
            package: row.key.package.clone(),
            workflow: row.key.workflow.clone(),
            job: row.key.job.clone(),
            file: row.file.clone(),
            name: row.name.clone(),
            state: row.lifecycle.as_str(),
            schedule: row.schedule.clone(),
            ready: row.ready,
            version: row.version.as_ref().map(ToString::to_string),
            release_kind: row.release_kind.map(contextos_workflows::ReleaseKind::as_str),
            predecessor: row.predecessor.clone(),
            successor: row.successor.clone(),
            hash: row.hash.as_ref().map(|hash| <&str>::from(hash).to_owned()),
            problems: row.problems.iter().map(ProblemView::from).collect(),
            suspension: row.suspension.as_ref().map(SuspensionView::from),
        }
    }
}

#[derive(Serialize)]
struct ListView {
    store: String,
    jobs: Vec<JobView>,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cause: Option<&'static str>,
}

#[derive(Serialize)]
struct ErrorView {
    error: ErrorBody,
}

impl From<&AuthoringError> for ErrorView {
    fn from(error: &AuthoringError) -> Self {
        let text = error.to_string();
        // Every message begins with its code, which is its own field here.
        let message = text
            .strip_prefix(error.code())
            .and_then(|rest| rest.strip_prefix(": "))
            .map_or(text.clone(), str::to_owned);
        Self {
            error: ErrorBody {
                code: error.code(),
                message,
                cause: error.cause(),
            },
        }
    }
}

/// The text or the one JSON document that shows `report`.
pub(crate) fn render(report: &AuthoringReport, format: OutputFormat) -> String {
    match format {
        OutputFormat::Json => render_json(report),
        OutputFormat::Text => render_text(report),
    }
}

fn render_json(report: &AuthoringReport) -> String {
    let document = match report {
        AuthoringReport::Authored(store, authored) => serde_json::to_string(&AuthoredView::from((store, authored))),
        AuthoringReport::Shown(store, shown) => serde_json::to_string(&ShownView::from((store, shown))),
        AuthoringReport::Listed(store, rows) => serde_json::to_string(&ListView {
            store: store.word().to_owned(),
            jobs: rows.iter().map(JobView::from).collect(),
        }),
        AuthoringReport::Refused(error) => serde_json::to_string(&ErrorView::from(error)),
    };
    // Every view is plain strings and numbers, which always serialise; the fallback keeps the result a document.
    document.unwrap_or_else(|_| {
        r#"{"error":{"code":"workflow/job-store-unavailable","message":"the result could not be rendered"}}"#.to_owned()
    })
}

fn render_text(report: &AuthoringReport) -> String {
    match report {
        AuthoringReport::Authored(store, authored) => {
            let view = AuthoredView::from((store, authored));
            let mut text = format!(
                "{} {}\nhash {}",
                view.outcome,
                job_name(store, &authored.key),
                view.hash
            );
            for warning in &view.warnings {
                let _ = write!(text, "\nwarning {}: {}", warning.code, warning.message);
            }
            text
        }
        AuthoringReport::Shown(_, shown) => format!("hash {}\n{}", <&str>::from(&shown.hash), shown.content),
        AuthoringReport::Listed(store, rows) => render_list(store, rows),
        AuthoringReport::Refused(error) => format!("refused {error}"),
    }
}

/// How a result names a job in text: `shared job acme/sync/nightly`, or `job va/sync/weekly` for a vault's.
fn job_name(store: &Store, key: &SharedJobKey) -> String {
    match store {
        Store::Shared => format!("shared job {}/{}/{}", key.package, key.workflow, key.job),
        Store::Vault(vault) => format!("job {vault}/{}/{}/{}", key.package, key.workflow, key.job),
    }
}

/// One aligned line per job, with each problem indented beneath it.
fn render_list(store: &Store, rows: &[JobListing]) -> String {
    if rows.is_empty() {
        return match store {
            Store::Shared => "No shared jobs.".to_owned(),
            Store::Vault(vault) => format!("No jobs in {vault}."),
        };
    }
    let cells: Vec<[String; 9]> = rows
        .iter()
        .map(|row| {
            let view = JobView::from(row);
            let release = match (&view.version, view.release_kind) {
                (Some(version), Some(kind)) => format!("{version} {kind}"),
                (Some(version), None) => version.clone(),
                _ => "-".to_owned(),
            };
            let links = match (&view.predecessor, &view.successor) {
                (None, None) => "-".to_owned(),
                (before, after) => format!(
                    "{}{}",
                    before.as_ref().map(|key| format!("after {key}")).unwrap_or_default(),
                    after
                        .as_ref()
                        .map(|key| format!("{}before {key}", if before.is_some() { ", " } else { "" }))
                        .unwrap_or_default()
                ),
            };
            [
                format!("{}/{}/{}", view.package, view.workflow, view.job),
                view.file.map_or_else(|| "-".to_owned(), |stem| format!("{stem}.toml")),
                view.state.to_owned(),
                view.schedule.unwrap_or_else(|| "-".to_owned()),
                view.ready
                    .map_or("-", |ready| if ready { "ready" } else { "not-ready" })
                    .to_owned(),
                release,
                links,
                view.hash.unwrap_or_else(|| "-".to_owned()),
                view.name.unwrap_or_else(|| "-".to_owned()),
            ]
        })
        .collect();
    let mut widths = [0_usize; 9];
    for line in &cells {
        for (width, cell) in widths.iter_mut().zip(line) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let mut text = String::new();
    for (line, row) in cells.iter().zip(rows) {
        if !text.is_empty() {
            text.push('\n');
        }
        let aligned: Vec<String> = line
            .iter()
            .zip(widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect();
        text.push_str(aligned.join("  ").trim_end());
        for problem in &row.problems {
            let _ = write!(text, "\n    problem {}: {}", problem.code, problem.message);
        }
        if let Some(record) = &row.suspension {
            let _ = write!(text, "\n    suspended {record}");
        }
    }
    text
}

/// The one place an authoring command writes to standard output: its result, ended by a newline.
#[allow(
    clippy::disallowed_methods,
    reason = "the one place an authoring command writes its result to standard output"
)]
fn write_result(text: &str) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(text.as_bytes())?;
    if !text.ends_with('\n') {
        stdout.write_all(b"\n")?;
    }
    stdout.flush()
}

/// Carries out `action` against this host's jobs folder, package store, and state. Blocking.
fn carry_out(action: AuthoringAction) -> Result<AuthoringReport, AuthoringError> {
    let unavailable = || AuthoringError::JobStoreUnavailable {
        action: "the host's data folder could not be determined",
        kind: None,
    };
    let authoring = SharedJobAuthoring {
        jobs_root: package_store::default_jobs_root().map_err(|_| unavailable())?,
        store_root: package_store::default_store_root().map_err(|_| unavailable())?,
        state: SharedStateStore::new(package_store::default_state_root().map_err(|_| unavailable())?),
    };
    match action {
        AuthoringAction::Create { job, file, .. } => {
            let bytes = read_job_input(&file)?;
            authoring
                .create(&job.key(), &bytes)
                .map(|authored| AuthoringReport::Authored(Store::Shared, authored))
        }
        AuthoringAction::Show { job, .. } => authoring
            .show(&job.key())
            .map(|shown| AuthoringReport::Shown(Store::Shared, shown)),
        AuthoringAction::Replace {
            job,
            file,
            expected_hash,
            ..
        } => {
            let bytes = read_job_input(&file)?;
            authoring
                .replace(&job.key(), &bytes, &expected_hash)
                .map(|authored| AuthoringReport::Authored(Store::Shared, authored))
        }
        AuthoringAction::List { .. } => authoring
            .list()
            .map(|rows| AuthoringReport::Listed(Store::Shared, rows)),
    }
}

/// Carries out `action` against `vault`, through the primary server `config_path` configures.
async fn carry_out_in_vault(
    config_path: &Path,
    vault: &str,
    action: AuthoringAction,
) -> Result<AuthoringReport, AuthoringError> {
    // Reading a file or standard input blocks, so it is done before anything else and on a thread that may.
    let input = match &action {
        AuthoringAction::Create { file, .. } | AuthoringAction::Replace { file, .. } => {
            let file = file.clone();
            Some(
                tokio::task::spawn_blocking(move || read_job_input(&file))
                    .await
                    .map_err(|_| AuthoringError::JobStoreUnavailable {
                        action: "the job input could not be read",
                        kind: None,
                    })??,
            )
        }
        AuthoringAction::Show { .. } | AuthoringAction::List { .. } => None,
    };
    let config = load_web_config(config_path).map_err(|error| AuthoringError::ConfigInvalid {
        reason: crate::tracing_setup::ConfigInvalid::from(&error).reason,
    })?;
    let clients = connect(&config, config_path)
        .await
        .map_err(|_| AuthoringError::JobStoreUnavailable {
            action: "the vault's server could not be reached",
            kind: None,
        })?;
    let known_servers: Vec<String> = config.mcp_servers.iter().map(|entry| entry.name().to_owned()).collect();
    let client =
        known_servers
            .first()
            .and_then(|primary| clients.get(primary))
            .ok_or(AuthoringError::JobStoreUnavailable {
                action: "no server is configured to reach the vault through",
                kind: None,
            })?;
    let authoring = VaultJobAuthoring {
        client: &client,
        vault,
        known_servers: &known_servers,
    };
    let store = Store::Vault(vault.to_owned());
    let input = input.unwrap_or_default();
    match action {
        AuthoringAction::Create { job, .. } => authoring
            .create(&job.key().package, &job.key().workflow, &job.key().job, &input)
            .await
            .map(|authored| AuthoringReport::Authored(store, authored)),
        AuthoringAction::Show { job, .. } => authoring
            .show(&job.key().package, &job.key().workflow, &job.key().job)
            .await
            .map(|shown| AuthoringReport::Shown(store, shown)),
        AuthoringAction::Replace { job, expected_hash, .. } => authoring
            .replace(
                &job.key().package,
                &job.key().workflow,
                &job.key().job,
                &input,
                &expected_hash,
            )
            .await
            .map(|authored| AuthoringReport::Authored(store, authored)),
        AuthoringAction::List { .. } => authoring.list().await.map(|rows| AuthoringReport::Listed(store, rows)),
    }
}

/// Runs `task` on a blocking thread and reports what came of it. A refusal, and the task itself failing to complete (a
/// panic, or the runtime shutting down), are both a result, so the command still prints one document.
async fn report_of(task: impl FnOnce() -> Result<AuthoringReport, AuthoringError> + Send + 'static) -> AuthoringReport {
    AuthoringReport::from(tokio::task::spawn_blocking(task).await)
}

/// Runs one authoring command and prints its result, with its diagnostics going to the log `config_path` configures. A
/// refusal is printed as the result, and then returned as [`Refusal`], so the process exits unsuccessfully without a
/// second report.
pub(crate) async fn run_authoring_command(
    config_path: &Path,
    action: AuthoringAction,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    // Diagnostics go where the operator's other jobs commands send theirs: the configured log, or stderr by default.
    let format = action.format();
    let store = action.store();
    let report = match crate::tracing_setup::initialise_command_tracing(config_path) {
        Ok(()) => match store {
            // Reading a file or standard input, taking the host lock, and writing the job file all block.
            Store::Shared => report_of(move || carry_out(action)).await,
            // Boxed so the large session and server futures stay out of the command's own state.
            Store::Vault(vault) => match Box::pin(carry_out_in_vault(config_path, &vault, action)).await {
                Ok(report) => report,
                Err(error) => AuthoringReport::Refused(error),
            },
        },
        // Nothing is run, and the refusal is still the one document the caller asked for.
        Err(error) => AuthoringReport::Refused(AuthoringError::ConfigInvalid { reason: error.reason }),
    };
    if let AuthoringReport::Refused(error) = &report {
        // Debug only: the refusal is the command's result, which the caller already has.
        tracing::debug!(code = error.code(), "a job authoring command was refused");
    }
    let refused = matches!(report, AuthoringReport::Refused(_));
    write_result(&render(&report, format))?;
    if refused { Err(Refusal.into()) } else { Ok(()) }
}

#[cfg(test)]
#[path = "job_authoring_commands_test.rs"]
mod tests;
