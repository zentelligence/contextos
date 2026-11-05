//! Who a service is: everything that names one binary's background service rather than another's, supplied once by
//! each binary so the backends, their messages, and their remedies never hard-code a program.

/// The names one binary's service registers and prints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServiceIdentity {
    /// The program's executable name as the operator types it, used in every message and remedy
    /// (`contextos-web service install`).
    pub program: &'static str,
    /// What the service is, as the systemd unit's description reads it.
    pub description: &'static str,
    /// The systemd user unit's file name, extension included.
    pub unit_name: &'static str,
    /// The launchd `LaunchAgent` label, which also names its property list.
    pub launchd_label: &'static str,
    /// The name of the Windows Startup App's `Run` value and of the Scheduled Task. Only one of the two is ever
    /// registered.
    pub windows_name: &'static str,
    /// The configuration file's name, as messages call it (`web.toml`).
    pub config_file: &'static str,
}

#[cfg(test)]
#[path = "identity_test.rs"]
mod tests;
