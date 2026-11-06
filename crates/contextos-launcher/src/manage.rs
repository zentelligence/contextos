//! `contextos-launcher` under its own name: installing, updating, rolling back, and removing the suite.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::uninstall::run_uninstall;
use crate::update_cli::{CliResult, run_check, run_cleanup, run_diagnostics, run_install, run_rollback, run_update};

#[derive(Debug, Parser)]
#[command(
    name = "contextos-launcher",
    display_name = "ContextOS Launcher",
    version,
    about = "Installs, updates, and removes ContextOS"
)]
struct Cli {
    #[command(subcommand)]
    command: Action,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// Install the current release and put the commands on your `PATH`.
    Install {
        /// Install this exact release, for CI and automation. It must be the channel's current release.
        #[arg(long)]
        version: Option<String>,
    },
    /// Install the newest release. Check, clean up, or diagnose with a subcommand.
    Update {
        #[command(subcommand)]
        action: Option<UpdateAction>,
        /// Install without an interactive confirmation prompt.
        #[arg(long, global = true)]
        yes: bool,
    },
    /// Return to the previously active release.
    Rollback,
    #[command(about = "Remove ContextOS from this computer, keeping your vaults and settings")]
    Uninstall {
        /// Remove without an interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
enum UpdateAction {
    /// Report whether a newer release is available, without installing it.
    Check,
    /// Delete obsolete releases, best-effort.
    Cleanup,
    /// Print a structured summary of the installation.
    Diagnostics,
}

/// Carries out the command line's action, printing what happened.
pub(crate) fn run() -> ExitCode {
    match perform(Cli::parse().command) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn perform(action: Action) -> CliResult<String> {
    match action {
        Action::Install { version } => run_install(version),
        Action::Update { action, yes } => match action {
            None => run_update(yes),
            Some(UpdateAction::Check) => run_check(),
            Some(UpdateAction::Cleanup) => run_cleanup(),
            Some(UpdateAction::Diagnostics) => run_diagnostics(),
        },
        Action::Rollback => run_rollback(),
        Action::Uninstall { yes } => run_uninstall(yes),
    }
}
