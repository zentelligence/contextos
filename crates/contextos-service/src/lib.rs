//! Installs, removes, and reports on a `ContextOS` binary (`contextos-web`, `contextos`) running as an
//! auto-starting, per-user background service.
//!
//! One [`ServiceBackend`] per host platform (Linux via `systemd --user`,
//! macOS via a `launchd` `LaunchAgent`, Windows via a per-user Startup App,
//! or a Scheduled Task for an operator who chooses one); [`backend_for_os`]
//! selects the one matching the running process's own OS. Every backend
//! also starts, stops, and restarts its service ([`control`]), which is how
//! an operator recovers a server that stopped unexpectedly. Every backend is always compiled in on every host,
//! deliberately: none of this module's logic calls a real platform binary
//! except through the injected [`CommandRunner`], so the two backends for
//! platforms this process is not running on stay unit-testable (exact
//! argument construction, asserted against a fake runner) without
//! `#[cfg(target_os = ...)]` gating anything here. Only [`SystemCommandRunner`]
//! itself is genuinely platform-specific, and it is exercised end to end
//! only on its own host; the other two platforms' real-host behaviour is a
//! residual acceptance gap, matching this repository's standing
//! Windows/macOS acceptance discipline (never claim a platform's gate from
//! Linux-only evidence).
//!
//! All three run the service itself as the invoking user, none with any
//! elevated or `LocalSystem`-equivalent privilege: a deliberate scope choice
//! that avoids a Windows-only `windows-service` dependency and a true SCM
//! service's own elevated *running* account. Registering the Windows
//! Startup App needs no elevation either; only the optional Scheduled Task
//! (`service::windows`) needs an elevated (Administrator) terminal to
//! install or uninstall, because Windows Task Scheduler requires it to
//! create or delete any task, even one that (like this one) carries no
//! elevated run level once installed.

pub mod cli;
pub mod control;
mod identity;
mod linux;
mod macos;
pub mod process_record;
mod startup_app;
#[cfg(test)]
mod test_identities;
mod windows;
mod windows_host;
mod windows_process;

use std::io;
use std::path::PathBuf;
use std::process::Command;

use thiserror::Error;

pub use control::{ControlAction, ControlOutcome, RunState, control};
pub use identity::ServiceIdentity;
pub use linux::SystemdUserBackend;
pub use macos::LaunchdBackend;
pub use startup_app::StartupAppBackend;
pub use windows::ScheduledTaskBackend;
pub use windows_host::{InstallChoice, WindowsBackend, WindowsMechanism};

/// Everything a backend needs to install a binary as a service.
/// Directories are passed in, never resolved internally, so tests never
/// touch the operator's real home or configuration directory
/// (`testing.md`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceSpec {
    /// Who the service is: the names it registers and prints.
    pub identity: ServiceIdentity,
    /// Absolute path to the binary to run.
    pub binary_path: PathBuf,
    /// Absolute path to the configuration file this instance should load, passed as `--config`.
    pub config_path: PathBuf,
    /// The arguments the service runs with after `--config <path>`.
    pub run_args: Vec<String>,
    /// The user's home directory (the macOS backend's `LaunchAgents`
    /// directory is anchored here).
    pub home_dir: PathBuf,
    /// The user's configuration directory (the Linux backend's
    /// `systemd/user` unit directory is anchored here). Unused by the
    /// macOS and Windows backends.
    pub config_dir: PathBuf,
    /// The running server's process record ([`crate::process_record`]),
    /// which the Windows backends read to stop and report on the server.
    pub process_record: PathBuf,
    /// Windows' system folder (`%SystemRoot%\System32`), where the Windows
    /// backends find every tool they run (`conhost.exe`, `reg.exe`,
    /// `schtasks.exe`, `taskkill.exe`, PowerShell), so no same-named program
    /// earlier on the search path runs in its place. Unused elsewhere.
    pub system_dir: PathBuf,
}

/// The service's current installation and run state, as reported by
/// [`ServiceBackend::status`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceStatus {
    Installed(RunState),
    NotInstalled,
}

/// The outcome of [`ServiceBackend::uninstall`]: whether a service was
/// actually present to remove.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UninstallOutcome {
    Removed,
    NotInstalled,
}

/// One platform's mechanism for registering a binary as an
/// auto-starting, per-user background service.
pub trait ServiceBackend {
    /// Installs the service, overwriting any existing definition, and
    /// starts it immediately. Idempotent: installing over an
    /// already-installed, already-running service succeeds and leaves it
    /// running.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError`] when the service definition cannot be
    /// written, or a required platform command cannot be spawned or exits
    /// non-zero.
    fn install(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError>;

    /// Stops and removes the service. Reports
    /// [`UninstallOutcome::NotInstalled`], not an error, when no service was
    /// registered.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError`] when a required platform command cannot be
    /// spawned or exits non-zero, or when the service definition exists but
    /// cannot be removed.
    fn uninstall(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<UninstallOutcome, ServiceError>;

    /// Reports current installation and run state without changing
    /// anything.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError`] when a required platform command cannot be
    /// spawned.
    fn status(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<ServiceStatus, ServiceError>;

    /// Starts the installed service's server. [`control`] calls it only when the server is not running.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError`] when a required platform command cannot be spawned or exits non-zero.
    fn start(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError>;

    /// Stops the installed service's server until it is next started or the user next logs in, clearing anything a
    /// crash left behind. [`control`] calls it only when the server is running or crashed.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError`] when a required platform command cannot be spawned or exits non-zero.
    fn stop(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError>;

    /// Stops the server if it is running, then starts it.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError`] when stopping or starting fails.
    fn restart(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        self.stop(runner, spec)?;
        self.start(runner, spec)
    }
}

/// Selects the [`ServiceBackend`] matching `os` (an `std::env::consts::OS`
/// value), taking the value as a parameter rather than reading it directly
/// so the "unrecognised platform" branch is reachable by a test without
/// needing to run this binary on one.
///
/// # Errors
///
/// Returns [`ServiceError::UnsupportedPlatform`] when `os` names a platform
/// with no [`ServiceBackend`] implementation.
pub fn backend_for_os(
    identity: ServiceIdentity,
    os: &str,
    windows: WindowsMechanism,
) -> Result<Box<dyn ServiceBackend>, ServiceError> {
    match os {
        "linux" => Ok(Box::new(SystemdUserBackend)),
        "macos" => Ok(Box::new(LaunchdBackend)),
        "windows" => Ok(Box::new(WindowsBackend::from(windows))),
        other => Err(ServiceError::UnsupportedPlatform {
            program: identity.program,
            os: other.to_owned(),
        }),
    }
}

/// Selects the [`ServiceBackend`] for the platform this binary is actually
/// running on; `windows` is the mechanism a Windows install registers.
///
/// # Errors
///
/// Returns [`ServiceError::UnsupportedPlatform`] when the running platform
/// (`std::env::consts::OS`) has no [`ServiceBackend`] implementation.
pub fn current_platform_backend(
    identity: ServiceIdentity,
    windows: WindowsMechanism,
) -> Result<Box<dyn ServiceBackend>, ServiceError> {
    backend_for_os(identity, std::env::consts::OS, windows)
}

/// The result of running one external command: exit success and captured,
/// lossily-decoded output. Used for diagnostics and, in one backend's
/// `status` implementation, a best-effort running/idle read; a backend's
/// own success/failure test for an install or uninstall step is always the
/// command's exit status, never a match on this output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandOutput {
    pub success: bool,
    /// The exit code, or `None` for a process ended by a signal.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl From<std::process::Output> for CommandOutput {
    fn from(output: std::process::Output) -> Self {
        Self {
            success: output.status.success(),
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

/// Runs one external command, abstracted so every backend's exact
/// invocation is assertable against a fake without a real
/// `systemctl`/`launchctl`/`schtasks` binary on the test host
/// (`testing.md`: "inject ... external providers").
pub trait CommandRunner {
    /// # Errors
    ///
    /// Returns the underlying [`io::Error`] when `program` cannot be
    /// spawned at all; a non-zero exit is reported through
    /// [`CommandOutput::success`], not this `Result`.
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, io::Error>;

    /// Starts `program` and returns without waiting for it, leaving it running after this process exits: how the
    /// Windows Startup App starts its server outside a logon.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`io::Error`] when `program` cannot be spawned.
    fn spawn_detached(&self, program: &str, args: &[&str]) -> Result<(), io::Error>;
}

/// The real [`CommandRunner`], backed by [`std::process::Command`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, io::Error> {
        Command::new(program).args(args).output().map(CommandOutput::from)
    }

    fn spawn_detached(&self, program: &str, args: &[&str]) -> Result<(), io::Error> {
        // Dropping the child handle neither waits for it nor ends it; the server outlives this command.
        let detached = |command: &mut Command| {
            command
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map(drop)
        };
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
            // Out of this terminal's console, its Ctrl+C group, and (where the job allows) its job object, so closing
            // an SSH session or IDE terminal that kills its job does not take the server with it. A job that forbids
            // breaking away refuses the spawn outright, so it is retried without.
            let flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
            detached(Command::new(program).creation_flags(flags | CREATE_BREAKAWAY_FROM_JOB)).or_else(|error| {
                tracing::warn!(
                    %error,
                    "could not start the server outside this terminal's job; it may stop when the terminal closes"
                );
                detached(Command::new(program).creation_flags(flags))
            })
        }
        #[cfg(not(windows))]
        {
            detached(&mut Command::new(program))
        }
    }
}

/// Runs `program args` and maps a spawn failure or a non-zero exit onto
/// [`ServiceError`]. Shared by every backend's own "this step must
/// succeed" commands (`daemon-reload`, `enable --now`, `bootstrap`, and so
/// on); a backend step that treats a non-zero exit as informative rather
/// than fatal (systemd's `is-active`, launchd's best-effort `bootout`) does
/// not use this helper.
pub(crate) fn run_checked(runner: &dyn CommandRunner, program: &str, args: &[&str]) -> Result<(), ServiceError> {
    let output = runner.run(program, args).map_err(|source| ServiceError::Spawn {
        program: program.to_owned(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        source,
    })?;
    if output.success {
        Ok(())
    } else {
        Err(ServiceError::CommandFailed {
            program: program.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            code: output.code,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

/// How a failed command ended, for its error message.
fn exit_status(code: Option<i32>) -> String {
    code.map_or_else(
        || "no exit code (it was ended by a signal)".to_owned(),
        |code| format!("exit code {code}"),
    )
}

/// What a failed command printed to explain itself: its error output, or failing that its standard output, which
/// some Windows tools write their errors to. A command that printed neither is said to have printed nothing, rather
/// than leaving the message to end in an empty explanation.
fn what_it_printed(stdout: &str, stderr: &str) -> String {
    [stderr.trim(), stdout.trim()]
        .into_iter()
        .find(|text| !text.is_empty())
        .map_or_else(|| "it printed nothing to explain why".to_owned(), str::to_owned)
}

/// Writes `contents` to `path` through the one atomic writer for files outside every vault, in `contextos-fs`.
pub(crate) fn write_atomically(path: &std::path::Path, contents: &[u8]) -> io::Result<()> {
    contextos_fs::host_file::replace_file_atomically(path, contents).map_err(io::Error::from)
}

/// Typed failures installing, removing, or reporting on a binary's background service. Each variant that tells the
/// operator what to run names the program and configuration file of the [`ServiceIdentity`] that raised it.
#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("{program} service management is not implemented for platform {os:?}")]
    UnsupportedPlatform { program: &'static str, os: String },
    #[error("`--scheduled-task` only applies on Windows, not {os:?}; run `{program} service install` without it")]
    ScheduledTaskOffWindows { program: &'static str, os: String },
    #[error(transparent)]
    ProcessRecord(#[from] process_record::ProcessRecordError),
    #[error(
        "the {program} service is registered to run `{registration}`, not with {config}; run the command again \
         with `--config` naming the registered {config_file}, or run `{program} service install` to register this one",
        config = config.display()
    )]
    RegisteredForAnotherConfig {
        program: &'static str,
        config_file: &'static str,
        config: PathBuf,
        registration: String,
    },
    #[error(
        "the {program} service is already registered to run `{registration}`; uninstall it first with \
         `{program} service uninstall --config` naming that {config_file}, then install again"
    )]
    InstalledForAnotherConfig {
        program: &'static str,
        config_file: &'static str,
        registration: String,
    },
    #[error(
        "the {program} server (process {pid}) was told to stop but is still running; end it in Task Manager, then \
         run the command again"
    )]
    ServerDidNotExit { program: &'static str, pid: u32 },
    #[error("could not clear the server's process record at {}: {source}", path.display())]
    ClearRecord {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not write the service definition at {}: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not remove the service definition at {}: {source}", path.display())]
    Remove {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not run `{program} {}`: {source}", args.join(" "))]
    Spawn {
        program: String,
        args: Vec<String>,
        #[source]
        source: io::Error,
    },
    #[error("`{program} {}` failed with {}: {}", args.join(" "), exit_status(*code), what_it_printed(stdout, stderr))]
    CommandFailed {
        program: String,
        args: Vec<String>,
        code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    #[error(
        "`{program} {}` failed: {stderr}\nWindows Task Scheduler requires an administrator to register or remove \
         a task, even one (like this one) that runs with no elevated privileges once installed. Run the \
         command again from a terminal opened as Administrator (right-click the terminal, \"Run as \
         administrator\").",
        args.join(" ")
    )]
    ElevationRequired {
        program: String,
        args: Vec<String>,
        stderr: String,
    },
    #[error(
        "the command Windows would run at logon would be {length} characters, longer than the {limit} it accepts. \
         Install {program}, or keep its {config_file}, at a shorter path, then run `{program} service install` again."
    )]
    CommandTooLong {
        program: &'static str,
        config_file: &'static str,
        length: usize,
        limit: usize,
    },
    #[error("the {program} service is not installed; run `{program} service install`")]
    NotRegistered { program: &'static str },
    #[error(
        "the registered logon command `{registration}` cannot be read back into a program and its arguments; \
         uninstall the service and install it again"
    )]
    UnreadableRegistration { registration: String },
    #[error("unexpected output from `{program} {}`: {detail}", args.join(" "))]
    UnexpectedOutput {
        program: String,
        args: Vec<String>,
        detail: String,
    },
}

/// Test-only [`CommandRunner`] that records every invocation and replays a
/// scripted queue of responses, so each backend's exact command sequence is
/// assertable without a real `systemctl`/`launchctl`/`schtasks` binary.
/// Shared by `linux_test`, `macos_test`, and `windows_test` rather than each
/// keeping its own copy.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct FakeCommandRunner {
    calls: std::cell::RefCell<Vec<(String, Vec<String>)>>,
    spawned: std::cell::RefCell<Vec<(String, Vec<String>)>>,
    spawn_fails: std::cell::Cell<bool>,
    responses: std::cell::RefCell<std::collections::VecDeque<Result<CommandOutput, io::Error>>>,
}

#[cfg(test)]
impl FakeCommandRunner {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Queues a successful response for the next call.
    pub(crate) fn push_success(&self, stdout: &str) {
        self.responses.borrow_mut().push_back(Ok(CommandOutput {
            success: true,
            code: Some(0),
            stdout: stdout.to_owned(),
            stderr: String::new(),
        }));
    }

    /// Queues a non-zero-exit response for the next call.
    pub(crate) fn push_failure(&self, stderr: &str) {
        self.responses.borrow_mut().push_back(Ok(CommandOutput {
            success: false,
            code: Some(1),
            stdout: String::new(),
            stderr: stderr.to_owned(),
        }));
    }

    /// Queues a response with this exit status and standard output, for a command whose output matters whatever its
    /// exit status (`systemctl is-active` prints its state and exits non-zero unless active).
    pub(crate) fn push_output(&self, success: bool, stdout: &str) {
        self.responses.borrow_mut().push_back(Ok(CommandOutput {
            success,
            code: Some(i32::from(!success)),
            stdout: stdout.to_owned(),
            stderr: String::new(),
        }));
    }

    /// Queues a spawn failure (the program itself could not be started) for
    /// the next call.
    pub(crate) fn push_spawn_error(&self) {
        self.responses
            .borrow_mut()
            .push_back(Err(io::Error::other("simulated spawn failure")));
    }

    /// Every call made so far, in order, as `(program, args)`.
    pub(crate) fn calls(&self) -> Vec<(String, Vec<String>)> {
        self.calls.borrow().clone()
    }

    /// Makes every later [`CommandRunner::spawn_detached`] fail as a program that cannot be started would.
    pub(crate) fn fail_spawns(&self) {
        self.spawn_fails.set(true);
    }

    /// Every detached process started so far, in order, as `(program, args)`.
    pub(crate) fn spawned(&self) -> Vec<(String, Vec<String>)> {
        self.spawned.borrow().clone()
    }
}

#[cfg(test)]
impl CommandRunner for FakeCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, io::Error> {
        self.calls
            .borrow_mut()
            .push((program.to_owned(), args.iter().map(|arg| (*arg).to_owned()).collect()));
        self.responses.borrow_mut().pop_front().unwrap_or(Ok(CommandOutput {
            success: true,
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        }))
    }

    fn spawn_detached(&self, program: &str, args: &[&str]) -> Result<(), io::Error> {
        if self.spawn_fails.get() {
            return Err(io::Error::other("simulated spawn failure"));
        }
        self.spawned
            .borrow_mut()
            .push((program.to_owned(), args.iter().map(|arg| (*arg).to_owned()).collect()));
        Ok(())
    }
}

#[cfg(test)]
#[path = "lib_test.rs"]
mod tests;
