//! The identities the backend tests register: one for each binary that uses this crate.

use crate::ServiceIdentity;

/// The identity `contextos-web` registers, as the backend tests use it.
pub(crate) const WEB: ServiceIdentity = ServiceIdentity {
    program: "contextos-web",
    description: "ContextOS web UI",
    unit_name: "contextos-web.service",
    launchd_label: "au.zentelligence.contextos-web",
    windows_name: "ContextOS Web",
    config_file: "web.toml",
};

/// The identity `contextos` registers, as the backend tests use it.
pub(crate) const MCP: ServiceIdentity = ServiceIdentity {
    program: "contextos",
    description: "ContextOS MCP server",
    unit_name: "contextos.service",
    launchd_label: "au.zentelligence.contextos",
    windows_name: "ContextOS MCP",
    config_file: "config.toml",
};
