//! The names `contextos-web` registers as a background service. They are what an installed service is found by, so
//! they never change: a rename would orphan every service already installed under the old name.

use contextos_service::ServiceIdentity;

/// `contextos-web`'s service identity.
pub const SERVICE_IDENTITY: ServiceIdentity = ServiceIdentity {
    program: "contextos-web",
    description: "ContextOS web UI",
    unit_name: "contextos-web.service",
    launchd_label: "au.zentelligence.contextos-web",
    windows_name: "ContextOS Web",
    config_file: "web.toml",
};

#[cfg(test)]
#[path = "service_identity_test.rs"]
mod tests;
