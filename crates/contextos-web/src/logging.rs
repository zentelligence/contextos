//! Wires `web.toml`'s `[server]` logging fields (`log_file`,
//! `log_max_size_mb`, `log_rotate_daily`, `log_retention_days`,
//! `log_retention_files`) into `contextos-logging`'s rotating file
//! destination for `tracing`, closing the gap where `log_file` was accepted
//! and displayed on `/settings/` but never used: every log line went to
//! stderr regardless of its value.
//!
//! The rotation/retention mechanism itself ([`RotatingLog`],
//! [`LogRotationPolicy`], [`Clock`]) lives in `contextos-logging`, shared
//! with `contextos-mcp`'s identical `[server]`-logging schema so the same
//! security- and data-integrity-sensitive logic is never duplicated across
//! binaries. This module owns only the `WebServerConfig`-specific
//! conversion and open call.

use std::io;

pub use contextos_logging::{Clock, LogRotationPolicy, RotatingLog, SystemClock};

use crate::config::WebServerConfig;

impl From<&WebServerConfig> for LogRotationPolicy {
    fn from(config: &WebServerConfig) -> Self {
        Self {
            max_size_bytes: config.log_max_size_mb.map(|megabytes| megabytes * 1024 * 1024),
            rotate_daily: config.log_rotate_daily,
            retention_days: config.log_retention_days,
            retention_files: config.log_retention_files,
        }
    }
}

/// Opens `config.server.log_file` as a [`RotatingLog`], or `None` when
/// `log_file` is empty (logging goes to stderr, `main.rs`'s existing
/// behaviour, untouched). `config.server.validate_logging` already rejects
/// a rotation/retention setting paired with an empty `log_file` before this
/// is ever called, so a `Some` policy here always has somewhere to apply.
///
/// # Errors
///
/// Returns the underlying [`io::Error`] when `log_file` cannot be opened
/// (for example, its parent directory does not exist): a startup error,
/// not a lazily-discovered one, matching `mcp_client`'s own connect-time
/// failure discipline.
pub fn open(config: &WebServerConfig) -> io::Result<Option<RotatingLog<SystemClock>>> {
    contextos_logging::open(&config.log_file, LogRotationPolicy::from(config))
}

#[cfg(test)]
#[path = "logging_test.rs"]
mod tests;
