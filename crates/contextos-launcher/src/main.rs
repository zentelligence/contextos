#![forbid(unsafe_code)]

//! The stable launcher installed on `PATH` as both `contextos` and `contextos-web` (the same
//! compiled binary, copied under each name at install time). Resolves the active release set and
//! starts the matching real executable; never activates a release or deletes one.
//!
//! The started program is told this launcher's path, so that a service or MCP host registration it
//! creates records the launcher, not a versioned folder a later update leaves behind.

use std::ffi::OsString;
use std::process::{Command, ExitCode};

use contextos_host::LAUNCHER_VARIABLE;
use updater_launcher::{ProcessEnvironment, resolve_command, resolve_installation_root};

fn main() -> ExitCode {
    let command_name = requested_command_name();
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();

    let root = match resolve_installation_root(&ProcessEnvironment, "CONTEXTOS", "contextos") {
        Ok(root) => root,
        Err(error) => {
            eprintln!("{command_name}: {error}");
            return ExitCode::FAILURE;
        }
    };

    let resolved = match resolve_command(&root, &command_name) {
        Ok(resolved) => resolved,
        Err(error) => {
            eprintln!("{command_name}: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut command = Command::new(&resolved.executable_path);
    command.args(args);
    if let Ok(launcher) = std::env::current_exe() {
        command.env(LAUNCHER_VARIABLE, launcher);
    }
    run(command, &command_name)
}

/// Becomes the program. A service manager stops a service by signalling the process it started, so that process
/// must be the program itself, which shuts down cleanly, and not a launcher that would die and leave it to be killed.
#[cfg(unix)]
fn run(mut command: Command, command_name: &str) -> ExitCode {
    use std::os::unix::process::CommandExt;

    let error = command.exec();
    eprintln!("{command_name}: {error}");
    ExitCode::FAILURE
}

/// Windows cannot replace a running process, so the launcher waits and passes on the exit status. Stopping a service
/// there ends the program directly, through the process record the program keeps.
#[cfg(not(unix))]
fn run(mut command: Command, command_name: &str) -> ExitCode {
    match command.status() {
        Ok(status) => exit_code_for(status),
        Err(error) => {
            eprintln!("{command_name}: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The public command this invocation should resolve to, taken from this executable's own file
/// name (`contextos` or `contextos-web`, however it was installed on `PATH`).
fn requested_command_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.file_stem().map(|stem| stem.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "contextos".to_string())
}

#[cfg(not(unix))]
fn exit_code_for(status: std::process::ExitStatus) -> ExitCode {
    match status.code() {
        Some(code) => match u8::try_from(code) {
            Ok(code) => ExitCode::from(code),
            Err(_) => ExitCode::FAILURE,
        },
        None => ExitCode::FAILURE,
    }
}
