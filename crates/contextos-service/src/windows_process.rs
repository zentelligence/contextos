//! What both Windows mechanisms (the Startup App and the Scheduled Task) share: the command they run at logon, the
//! tools they run, reading their registration, and finding, ending, and launching the server through its process
//! record ([`crate::process_record`]).
//!
//! Neither mechanism tracks the server's process itself, and matching processes by name would also catch a
//! command of the same program the operator is running, so the record is how the server is found. A process is the server
//! only when it runs the executable the server recorded and started before the record was written: a process id
//! Windows has since given to another process, the same program or not, started later. Any other record is a crash.
//!
//! Everything read back from Windows comes through PowerShell with UTF-8 output ([`powershell`]): `reg` and `schtasks`
//! write the console's code page, which would garble a path such as `C:\Users\José`. Every tool runs by full path from
//! `ServiceSpec::system_dir`, so no same-named program earlier on the search path runs in its place.

use std::path::Path;
use std::time::Duration;

use super::{CommandRunner, RunState, ServiceError, ServiceSpec};
use crate::process_record::{self, ProcessRecord};

/// How long [`end_server`] waits for a killed server to exit (`EXIT_CHECKS` times `EXIT_POLL`, five seconds), so a
/// restart's new server can take its port.
/// Between checks; none under test, where the scripted process answers at once.
const EXIT_POLL: Duration = if cfg!(test) {
    Duration::ZERO
} else {
    Duration::from_millis(200)
};
/// Checks before giving up.
const EXIT_CHECKS: u32 = 25;

/// The full path of the Windows tool `name` under `ServiceSpec::system_dir` (`reg.exe`, `schtasks.exe`,
/// `taskkill.exe`, `conhost.exe`, `WindowsPowerShell\v1.0\powershell.exe`). Joined with `\` whichever host builds
/// it, since it is only ever a Windows path.
pub(crate) fn system_tool(spec: &ServiceSpec, name: &str) -> String {
    let dir = spec.system_dir.display().to_string();
    format!("{}\\{name}", dir.trim_end_matches(['\\', '/']))
}

/// The command run at logon: the server under a headless console host, so it opens no console window for the
/// operator to keep open (its log lines go to the configuration's `log_file`). `conhost.exe --headless` needs Windows 10 1809 or later.
pub(crate) fn logon_command(spec: &ServiceSpec) -> String {
    let mut command = format!(
        "{} --headless {} --config {}",
        quote_windows_arg(&system_tool(spec, "conhost.exe")),
        quote_windows_arg(&spec.binary_path.display().to_string()),
        quote_windows_arg(&spec.config_path.display().to_string()),
    );
    for argument in &spec.run_args {
        command.push(' ');
        command.push_str(&quote_windows_arg(argument));
    }
    command
}

/// Refuses `command` when it is longer than `limit` UTF-16 code units, the unit Windows counts in, before a
/// mechanism that would refuse it less helpfully (or, for a Startup App, silently skip it at logon) is asked to.
pub(crate) fn within_limit(spec: &ServiceSpec, command: &str, limit: usize) -> Result<(), ServiceError> {
    let length = command.encode_utf16().count();
    if length > limit {
        return Err(ServiceError::CommandTooLong {
            program: spec.identity.program,
            config_file: spec.identity.config_file,
            length,
            limit,
        });
    }
    Ok(())
}

/// Runs `script` in Windows PowerShell with its output in UTF-8, returning what it printed.
///
/// Windows PowerShell exits 1 after any error in a `-Command` script, even one the script silenced on purpose (a
/// lookup with `-ErrorAction SilentlyContinue` for a registration that does not exist yet, as on every clean
/// install), and then prints nothing. So the script runs with every unsilenced error stopping it: one that reaches the
/// wrapper prints its message to standard error and exits 1, and a script that finishes exits 0.
pub(crate) fn powershell(runner: &dyn CommandRunner, spec: &ServiceSpec, script: &str) -> Result<String, ServiceError> {
    let program = system_tool(spec, "WindowsPowerShell\\v1.0\\powershell.exe");
    let script = format!(
        "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; $ErrorActionPreference = 'Stop'; try {{ {script} }} \
         catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit 1 }}; exit 0"
    );
    let args = ["-NoProfile", "-NonInteractive", "-Command", script.as_str()];
    let to_owned = || args.iter().map(|arg| (*arg).to_owned()).collect();
    let output = runner.run(&program, &args).map_err(|source| ServiceError::Spawn {
        program: program.clone(),
        args: to_owned(),
        source,
    })?;
    if output.success {
        Ok(output.stdout)
    } else {
        Err(ServiceError::CommandFailed {
            program,
            args: to_owned(),
            code: output.code,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

/// What `script` printed, trimmed, or `None` when it printed nothing: how a registration is read back.
pub(crate) fn printed(
    runner: &dyn CommandRunner,
    spec: &ServiceSpec,
    script: &str,
) -> Result<Option<String>, ServiceError> {
    let output = powershell(runner, spec, script)?;
    let output = output.trim();
    Ok((!output.is_empty()).then(|| output.to_owned()))
}

/// Whether this service's server is running, per its process record: no record is stopped; a record whose process is
/// the server ([`is_server`]) is running; any other record is a crash.
pub(crate) fn server_state(runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<RunState, ServiceError> {
    let Some(record) = process_record::read(&spec.process_record, spec.identity.program)? else {
        return Ok(RunState::Stopped);
    };
    Ok(if is_server(runner, spec, &record)? {
        RunState::Running
    } else {
        RunState::Crashed
    })
}

/// Ends this service's server when it is running, waits for it to exit, and clears its record either way. A crashed
/// server's recorded id is never killed: Windows may since have given it to another process.
pub(crate) fn end_server(runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
    let Some(record) = process_record::read(&spec.process_record, spec.identity.program)? else {
        return Ok(());
    };
    if is_server(runner, spec, &record)? {
        let program = system_tool(spec, "taskkill.exe");
        let pid = record.pid.to_string();
        let args = ["/PID", pid.as_str(), "/F"];
        let to_owned = || args.iter().map(|arg| (*arg).to_owned()).collect();
        let killed = runner.run(&program, &args).map_err(|source| ServiceError::Spawn {
            program: program.clone(),
            args: to_owned(),
            source,
        })?;
        // `taskkill` also fails for a server that exited first, as one the task scheduler was ending may have.
        if !killed.success && is_server(runner, spec, &record)? {
            return Err(ServiceError::CommandFailed {
                program,
                args: to_owned(),
                code: killed.code,
                stdout: killed.stdout,
                stderr: killed.stderr,
            });
        }
        wait_for_exit(runner, spec, &record)?;
    }
    clear_record(&spec.process_record)
}

/// A registered logon command read back into the program it runs and that program's arguments, so the server is
/// started exactly as it was registered, whatever arguments the command starting it was given.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RegisteredCommand {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
}

/// Splits a command line the way `CommandLineToArgvW` does, the inverse of [`quote_windows_arg`]: the program runs to
/// the next space, or to its closing quote when quoted; in each argument after it, `2n` backslashes before a quote are
/// `n` backslashes and the quote opens or closes a quoted run, `2n + 1` are `n` backslashes and a literal quote, and
/// backslashes before anything else are literal. A quote left open is refused rather than run to the end of the line.
impl TryFrom<&str> for RegisteredCommand {
    type Error = ServiceError;

    fn try_from(registration: &str) -> Result<Self, Self::Error> {
        let unreadable = || ServiceError::UnreadableRegistration {
            registration: registration.trim().to_owned(),
        };
        let line = registration.trim();
        let (program, rest) = match line.strip_prefix('"') {
            Some(quoted) => quoted.split_once('"').ok_or_else(unreadable)?,
            None => line.split_once(char::is_whitespace).unwrap_or((line, "")),
        };
        if program.is_empty() {
            return Err(unreadable());
        }
        let mut args = Vec::new();
        let mut current = String::new();
        let mut in_argument = false;
        let mut quoted = false;
        let mut backslashes = 0usize;
        for character in rest.chars() {
            match character {
                '\\' => {
                    backslashes += 1;
                    in_argument = true;
                }
                '"' => {
                    current.extend(std::iter::repeat_n('\\', backslashes / 2));
                    if backslashes % 2 == 1 {
                        current.push('"');
                    } else {
                        quoted = !quoted;
                    }
                    backslashes = 0;
                    in_argument = true;
                }
                space if space.is_whitespace() && !quoted => {
                    current.extend(std::iter::repeat_n('\\', backslashes));
                    backslashes = 0;
                    if in_argument {
                        args.push(std::mem::take(&mut current));
                        in_argument = false;
                    }
                }
                other => {
                    current.extend(std::iter::repeat_n('\\', backslashes));
                    backslashes = 0;
                    current.push(other);
                    in_argument = true;
                }
            }
        }
        if quoted {
            return Err(unreadable());
        }
        current.extend(std::iter::repeat_n('\\', backslashes));
        if in_argument {
            args.push(current);
        }
        Ok(Self {
            program: program.to_owned(),
            args,
        })
    }
}

/// Starts `command`, the server's registered logon command, detached, clearing a record a crash left first so the new
/// server writes its own.
pub(crate) fn launch(
    runner: &dyn CommandRunner,
    spec: &ServiceSpec,
    command: &RegisteredCommand,
) -> Result<(), ServiceError> {
    clear_record(&spec.process_record)?;
    let args: Vec<&str> = command.args.iter().map(String::as_str).collect();
    runner
        .spawn_detached(&command.program, &args)
        .map_err(|source| ServiceError::Spawn {
            program: command.program.clone(),
            args: command.args.clone(),
            source,
        })
}

/// The PowerShell that prints process `pid`'s executable and start time (milliseconds since the Unix epoch), one per
/// line, and nothing for a process that does not exist or that this user may not inspect (which, since the server
/// runs as this user, is not the server).
fn process_script(pid: u32) -> String {
    format!(
        "try {{ $p = Get-Process -Id {pid} -ErrorAction Stop; $p.Path; \
         [DateTimeOffset]::new($p.StartTime.ToUniversalTime()).ToUnixTimeMilliseconds() }} catch {{ }}"
    )
}

/// Whether `record`'s process is the server it describes: alive, running the executable the server recorded, and
/// started no later than the record was written.
fn is_server(runner: &dyn CommandRunner, spec: &ServiceSpec, record: &ProcessRecord) -> Result<bool, ServiceError> {
    let output = powershell(runner, spec, &process_script(record.pid))?;
    let mut lines = output.lines().map(str::trim).filter(|line| !line.is_empty());
    let (Some(path), Some(started)) = (lines.next(), lines.next()) else {
        return Ok(false);
    };
    let recorded = record.exe.display().to_string();
    let same_executable = path.to_lowercase() == recorded.to_lowercase();
    let started_first = started
        .parse::<u64>()
        .is_ok_and(|started_ms| started_ms <= record.written_at_ms);
    Ok(same_executable && started_first)
}

/// Waits for `record`'s server to exit after it was killed.
fn wait_for_exit(runner: &dyn CommandRunner, spec: &ServiceSpec, record: &ProcessRecord) -> Result<(), ServiceError> {
    for _ in 0..EXIT_CHECKS {
        if !is_server(runner, spec, record)? {
            return Ok(());
        }
        std::thread::sleep(EXIT_POLL);
    }
    Err(ServiceError::ServerDidNotExit {
        program: spec.identity.program,
        pid: record.pid,
    })
}

/// Whether `registration` (the registered logon command, as Windows reports it) runs this service's server for this
/// configuration file. Paths compare without regard to case, as Windows compares them.
pub(crate) fn registered_for(spec: &ServiceSpec, registration: &str) -> Result<(), ServiceError> {
    let config = quote_windows_arg(&spec.config_path.display().to_string());
    if registration.to_lowercase().contains(&config.to_lowercase()) {
        Ok(())
    } else {
        Err(ServiceError::RegisteredForAnotherConfig {
            program: spec.identity.program,
            config_file: spec.identity.config_file,
            config: spec.config_path.clone(),
            registration: registration.trim().to_owned(),
        })
    }
}

fn clear_record(path: &Path) -> Result<(), ServiceError> {
    process_record::remove(path).map_err(|source| ServiceError::ClearRecord {
        path: path.to_path_buf(),
        source,
    })
}

/// Quotes `value` as one Windows command-line argument, following the backslash and quote rules `CommandLineToArgvW`
/// (and every C runtime built against it) uses to split a command line back into its arguments: a `"` is escaped as
/// `\"` (with every backslash immediately before it doubled first), and a run of backslashes immediately before the
/// closing quote is doubled so it reads as literal backslashes rather than escaping that quote. A bare `"..."` would be
/// wrong for a path ending in `\` (a directory with its trailing separator kept): `"...\"` reads as an escaped quote,
/// the argument never closes, and the rest of the logon command is swallowed into it.
fn quote_windows_arg(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    let mut trailing_backslashes = 0usize;
    for character in value.chars() {
        match character {
            '\\' => trailing_backslashes += 1,
            '"' => {
                for _ in 0..=trailing_backslashes * 2 {
                    quoted.push('\\');
                }
                quoted.push('"');
                trailing_backslashes = 0;
            }
            other => {
                for _ in 0..trailing_backslashes {
                    quoted.push('\\');
                }
                quoted.push(other);
                trailing_backslashes = 0;
            }
        }
    }
    for _ in 0..trailing_backslashes * 2 {
        quoted.push('\\');
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
#[path = "windows_process_test.rs"]
pub(crate) mod tests;
