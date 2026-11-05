//! The `contextos-web workflows install` and `contextos-web libraries install` commands: arguments in, text or one JSON
//! document out.
//!
//! The commands only connect to the vault's primary server and render what [`contextos_web::vault_source_install`]
//! returned. What an install means, and every check it makes, is decided there. The one thing a command writes to
//! standard output is its result, and a refusal is part of that result; it then ends the process with
//! [`Refusal`](crate::job_authoring_commands::Refusal), which `main` turns into exit status 1.

use std::error::Error;
use std::io::Write as _;
use std::path::Path;

use clap::Args;
use contextos_web::load_web_config;
use contextos_web::source_install_error::SourceInstallError;
use contextos_web::source_install_report::{InstalledJson, InstalledText, RefusalJson, RefusalText};
use contextos_web::vault_source_install::{SourceInstall, SourceInstalled};
use contextos_web::vault_source_read::{SourceKind, SourceTarget};

use crate::one_shot_sessions::connect;
use contextos_workflows::PackageName;

use crate::job_authoring_commands::{OutputFormat, Refusal};

/// What an install from source is asked to do.
#[derive(Clone, Debug, Args)]
pub(crate) struct InstallArguments {
    /// The vault whose source is installed and whose package store receives it.
    pub(crate) vault: String,
    /// The package, or for `libraries install` the library: the name of its folder under `registry/`.
    pub(crate) name: String,
    /// Restores this exact release instead of installing a new one. It is allowed only when the ledger records the release
    /// with the digest the source has now, and the release is removed, damaged, or incomplete; a release present and
    /// verified is reported unchanged.
    #[arg(long = "version", value_parser = |text: &str| semver::Version::parse(text))]
    pub(crate) at_version: Option<semver::Version>,
    /// `text`, or `json` for one document on standard output.
    #[arg(long, value_enum, default_value_t)]
    pub(crate) format: OutputFormat,
}

/// Runs the install, writes its result, and ends with [`Refusal`] when it was refused.
pub(crate) async fn run_install_command(
    config_path: &Path,
    kind: SourceKind,
    arguments: InstallArguments,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let format = arguments.format;
    let result = match crate::tracing_setup::initialise_command_tracing(config_path) {
        // Boxed so the large session and server futures stay out of the command's own state.
        Ok(()) => Box::pin(carry_out(config_path, kind, &arguments)).await,
        Err(error) => Err(SourceInstallError::ConfigInvalid { reason: error.reason }),
    };
    match result {
        Ok(installed) => {
            write_result(&render_installed(&installed, format))?;
            Ok(())
        }
        Err(error) => {
            // Debug only: the refusal is the command's result, which the caller already has.
            tracing::debug!(code = error.code(), "an install from source was refused");
            write_result(&render_refusal(&error, format))?;
            Err(Refusal.into())
        }
    }
}

async fn carry_out(
    config_path: &Path,
    kind: SourceKind,
    arguments: &InstallArguments,
) -> Result<SourceInstalled, SourceInstallError> {
    let name = PackageName::try_from(arguments.name.as_str()).map_err(|error| {
        SourceInstallError::refusing(
            contextos_workflows::ValidationCode::ArtefactInvalid,
            &arguments.name,
            error.to_string(),
        )
    })?;
    let config = load_web_config(config_path).map_err(|error| SourceInstallError::ConfigInvalid {
        reason: crate::tracing_setup::ConfigInvalid::from(&error).reason,
    })?;
    let clients = connect(&config, config_path)
        .await
        .map_err(|_| SourceInstallError::NotConnected {
            reason: "the vault's server could not be reached",
        })?;
    let client = config
        .mcp_servers
        .first()
        .and_then(|primary| clients.get(primary.name()))
        .ok_or(SourceInstallError::NotConnected {
            reason: "no server is configured to reach the vault through",
        })?;
    SourceInstall {
        client: &client,
        vault: &arguments.vault,
    }
    .install(&SourceTarget { kind, name }, arguments.at_version.as_ref())
    .await
}

fn render_installed(installed: &SourceInstalled, format: OutputFormat) -> String {
    match format {
        OutputFormat::Json => InstalledJson(installed).to_string(),
        OutputFormat::Text => InstalledText(installed).to_string(),
    }
}

fn render_refusal(error: &SourceInstallError, format: OutputFormat) -> String {
    match format {
        OutputFormat::Json => RefusalJson(error).to_string(),
        OutputFormat::Text => RefusalText(error).to_string(),
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "the one place an install command writes its result to standard output"
)]
fn write_result(text: &str) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(text.as_bytes())?;
    if !text.ends_with('\n') {
        stdout.write_all(b"\n")?;
    }
    stdout.flush()
}
