#![forbid(unsafe_code)]

//! The stable launcher installed on `PATH` as both `contextos` and `contextos-web` (the same
//! compiled binary, copied under each name at install time). Resolves the active release set and
//! starts the matching real executable; never activates a release or deletes one.

use std::ffi::OsString;
use std::process::ExitCode;

use updater_launcher::{ProcessEnvironment, launch, resolve_installation_root};

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

    match launch(&root, &command_name, &args) {
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

fn exit_code_for(status: std::process::ExitStatus) -> ExitCode {
    match status.code() {
        Some(code) => match u8::try_from(code) {
            Ok(code) => ExitCode::from(code),
            Err(_) => ExitCode::FAILURE,
        },
        None => ExitCode::FAILURE,
    }
}
