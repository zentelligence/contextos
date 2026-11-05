//! macOS backend: a `launchd` per-user `LaunchAgent`, so no elevation is
//! needed and the service runs and stops with the user's own login
//! session (`gui/<uid>` domain, never `system`).

use std::fmt::Write as _;
use std::path::PathBuf;

use super::{
    CommandRunner, RunState, ServiceBackend, ServiceError, ServiceSpec, ServiceStatus, UninstallOutcome, run_checked,
};
use crate::write_atomically;

/// Installs a binary as a `launchd` `LaunchAgent`.
#[derive(Clone, Copy, Debug, Default)]
pub struct LaunchdBackend;

fn plist_path(spec: &ServiceSpec) -> PathBuf {
    spec.home_dir
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", spec.identity.launchd_label))
}

fn xml_escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn plist_contents(spec: &ServiceSpec) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t<string>{label}</string>\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         \t\t<string>{binary}</string>\n\
         \t\t<string>--config</string>\n\
         \t\t<string>{config}</string>\n\
         {run_args}\
         \t</array>\n\
         \t<key>RunAtLoad</key>\n\
         \t<true/>\n\
         \t<key>KeepAlive</key>\n\
         \t<true/>\n\
         </dict>\n\
         </plist>\n",
        binary = xml_escape(&spec.binary_path.display().to_string()),
        config = xml_escape(&spec.config_path.display().to_string()),
        label = xml_escape(spec.identity.launchd_label),
        run_args = spec.run_args.iter().fold(String::new(), |mut lines, argument| {
            let _ = writeln!(lines, "\t\t<string>{}</string>", xml_escape(argument));
            lines
        }),
    )
}

/// The current user's numeric id, via `id -u` rather than an unsafe
/// `libc::getuid()` call (`#![forbid(unsafe_code)]`), so it stays
/// assertable against the injected [`CommandRunner`] in tests.
fn current_uid(runner: &dyn CommandRunner) -> Result<String, ServiceError> {
    let args: [&str; 1] = ["-u"];
    let output = runner.run("id", &args).map_err(|source| ServiceError::Spawn {
        program: "id".to_owned(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        source,
    })?;
    if !output.success {
        return Err(ServiceError::CommandFailed {
            program: "id".to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            code: output.code,
            stdout: output.stdout,
            stderr: output.stderr,
        });
    }
    let uid = output.stdout.trim();
    if uid.is_empty() || !uid.chars().all(|c| c.is_ascii_digit()) {
        return Err(ServiceError::UnexpectedOutput {
            program: "id".to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            detail: format!("expected a numeric user id, found {:?}", output.stdout),
        });
    }
    Ok(uid.to_owned())
}

impl ServiceBackend for LaunchdBackend {
    fn install(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        let path = plist_path(spec);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ServiceError::Write {
                path: path.clone(),
                source,
            })?;
        }
        write_atomically(&path, plist_contents(spec).as_bytes()).map_err(|source| ServiceError::Write {
            path: path.clone(),
            source,
        })?;

        let uid = current_uid(runner)?;
        let domain = format!("gui/{uid}");
        let target = format!("{domain}/{}", spec.identity.launchd_label);
        let path_str = path.display().to_string();
        // Best-effort: bootout a previous load so re-install is idempotent.
        // `launchctl` fails this when nothing is loaded yet, which is the
        // common case on a first install; that failure is not itself an
        // install failure.
        let _ = runner.run("launchctl", &["bootout", &target]);
        run_checked(runner, "launchctl", &["bootstrap", &domain, &path_str])
    }

    fn uninstall(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<UninstallOutcome, ServiceError> {
        let path = plist_path(spec);
        if !path.exists() {
            return Ok(UninstallOutcome::NotInstalled);
        }

        let uid = current_uid(runner)?;
        let target = format!("gui/{uid}/{}", spec.identity.launchd_label);
        // Best-effort, matching install: bootout can fail if the agent is
        // not currently loaded even though its plist is still on disk (for
        // example after a manual unload), which must not block removing
        // the file below.
        let _ = runner.run("launchctl", &["bootout", &target]);

        std::fs::remove_file(&path).map_err(|source| ServiceError::Remove {
            path: path.clone(),
            source,
        })?;
        Ok(UninstallOutcome::Removed)
    }

    fn status(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<ServiceStatus, ServiceError> {
        let path = plist_path(spec);
        if !path.exists() {
            return Ok(ServiceStatus::NotInstalled);
        }

        let uid = current_uid(runner)?;
        let target = format!("gui/{uid}/{}", spec.identity.launchd_label);
        let output = runner
            .run("launchctl", &["print", &target])
            .map_err(|source| ServiceError::Spawn {
                program: "launchctl".to_owned(),
                args: vec!["print".to_owned(), target],
                source,
            })?;
        // `print` fails for an agent that is not loaded (after `stop`); a loaded one names its state. A loaded agent
        // that is not running has ended without being asked to, and `KeepAlive` will start it again, so it counts as
        // crashed: `stop` must unload it, and `start` kicks it now.
        Ok(ServiceStatus::Installed(if !output.success {
            RunState::Stopped
        } else if output.stdout.contains("state = running") {
            RunState::Running
        } else {
            RunState::Crashed
        }))
    }

    fn start(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        kick(runner, spec, &["kickstart"])
    }

    /// Unloads the agent: with `KeepAlive`, launchd would start a server that was only killed straight back up.
    fn stop(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        let uid = current_uid(runner)?;
        run_checked(
            runner,
            "launchctl",
            &["bootout", &format!("gui/{uid}/{}", spec.identity.launchd_label)],
        )
    }

    fn restart(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        kick(runner, spec, &["kickstart", "-k"])
    }
}

/// Loads the agent when it is not loaded (`bootstrap`, which starts it through `RunAtLoad`), else runs
/// `launchctl {kickstart} gui/{uid}/<label>`: `kickstart` starts it, `kickstart -k` restarts it.
fn kick(runner: &dyn CommandRunner, spec: &ServiceSpec, kickstart: &[&str]) -> Result<(), ServiceError> {
    let uid = current_uid(runner)?;
    let target = format!("gui/{uid}/{}", spec.identity.launchd_label);
    let loaded = runner
        .run("launchctl", &["print", &target])
        .is_ok_and(|output| output.success);
    if loaded {
        let mut args = kickstart.to_vec();
        args.push(&target);
        run_checked(runner, "launchctl", &args)
    } else {
        let path = plist_path(spec).display().to_string();
        run_checked(runner, "launchctl", &["bootstrap", &format!("gui/{uid}"), &path])
    }
}

#[cfg(test)]
#[path = "macos_test.rs"]
mod tests;
