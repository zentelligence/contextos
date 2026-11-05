//! Wires `config.toml`'s `[server]` logging fields (`log_file`,
//! `log_max_size_mb`, `log_rotate_daily`, `log_retention_days`,
//! `log_retention_files`) into `contextos-logging`'s rotating file
//! destination for `tracing`. Previously `log_file` was accepted and
//! writable through `contextos config` but never actually used: every log
//! line went to stderr regardless of its value.
//!
//! The rotation/retention mechanism itself ([`RotatingLog`],
//! [`LogRotationPolicy`], [`Clock`]) lives in `contextos-logging`, shared
//! with `contextos-web`'s identical `[server]`-logging schema so the same
//! security- and data-integrity-sensitive logic is never duplicated across
//! binaries. This module owns only the `ServerConfig`-specific conversion
//! and open call.

use std::io;

use contextos_logging::{LogRotationPolicy, RotatingLog, SystemClock};

use crate::config::ServerConfig;

impl From<&ServerConfig> for LogRotationPolicy {
    fn from(config: &ServerConfig) -> Self {
        Self {
            max_size_bytes: config.log_max_size_mb.map(|megabytes| megabytes * 1024 * 1024),
            rotate_daily: config.log_rotate_daily,
            retention_days: config.log_retention_days,
            retention_files: config.log_retention_files,
        }
    }
}

/// Opens `config.log_file` as a [`RotatingLog`], or `None` when `log_file`
/// is empty (logging goes to stderr, `main.rs`'s existing behaviour,
/// untouched). `ServerConfig::validate_logging` already rejects a
/// rotation/retention setting paired with an empty `log_file` before this
/// is ever called, so a `Some` policy here always has somewhere to apply.
///
/// # Errors
///
/// Returns the underlying [`io::Error`] when `log_file` cannot be opened
/// (for example, its parent directory does not exist): a startup error,
/// not a lazily-discovered one.
pub fn open(config: &ServerConfig) -> io::Result<Option<RotatingLog<SystemClock>>> {
    contextos_logging::open(&config.log_file, LogRotationPolicy::from(config))
}

#[cfg(test)]
#[path = "logging_test.rs"]
mod tests;
