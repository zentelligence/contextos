//! The `tracing` subscriber the `contextos-web` binary installs from `web.toml`'s `[server]` logging fields.

use std::path::Path;

use tracing_subscriber::EnvFilter;

/// Installs the `tracing` subscriber that `[server]`'s logging fields describe.
///
/// `log_level` always applies. `log_file`, with its `log_max_size_mb`, `log_rotate_daily`, `log_retention_days`, and
/// `log_retention_files` companions, is opened as a rotating destination when set, and the subscriber writes to stderr
/// when it is empty.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when `log_file` is set but cannot be opened (for example, its parent
/// directory does not exist), so a bad destination is a start-up error and never found out lazily.
#[allow(
    clippy::disallowed_methods,
    reason = "standard error is the log sink when no log file is configured"
)]
pub(crate) fn initialise_tracing(server: &contextos_web::WebServerConfig) -> std::io::Result<()> {
    let filter = match server.log_level {
        contextos_web::WebLogLevel::Error => "error",
        contextos_web::WebLogLevel::Warn => "warn",
        contextos_web::WebLogLevel::Info => "info",
        contextos_web::WebLogLevel::Debug => "debug",
        contextos_web::WebLogLevel::Trace => "trace",
    };
    let writer = match contextos_web::logging::open(server)? {
        Some(rotating) => tracing_subscriber::fmt::writer::BoxMakeWriter::new(std::sync::Mutex::new(rotating)),
        None => tracing_subscriber::fmt::writer::BoxMakeWriter::new(std::io::stderr),
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .with_writer(writer)
        .with_ansi(false)
        // A log write that fails, as every one does once whatever read standard error has gone, is dropped: reporting
        // it would mean `eprintln!` to that same closed stream, which panics and takes the server down with it.
        .log_internal_errors(false)
        .try_init();
    Ok(())
}

/// Why a command that needs no server could not start: the web configuration cannot be used. Holds the class of the
/// failure only, never a path or a value from the file.
#[derive(Debug, thiserror::Error)]
#[error(
    "workflow/config-invalid: the web configuration cannot be used ({reason}), so nothing was done; fix or remove it"
)]
pub(crate) struct ConfigInvalid {
    /// What kind of failure it was.
    pub(crate) reason: String,
}

impl From<&contextos_web::WebConfigError> for ConfigInvalid {
    fn from(error: &contextos_web::WebConfigError) -> Self {
        let reason = match error {
            contextos_web::WebConfigError::Read { source, .. } => format!("it could not be read: {}", source.kind()),
            contextos_web::WebConfigError::Toml { .. } => "it is not valid TOML or has an unknown setting".to_owned(),
            _ => "a setting is not valid".to_owned(),
        };
        Self { reason }
    }
}

impl From<std::io::Error> for ConfigInvalid {
    fn from(error: std::io::Error) -> Self {
        Self {
            reason: format!("the log file could not be opened: {}", error.kind()),
        }
    }
}

/// Installs the subscriber for a command that does not need the web configuration to do its work.
///
/// When the file at `config_path` loads, its `[server]` logging fields apply exactly as they do for every other jobs
/// command. When it is absent, the default logging applies, which writes to stderr, so the command still works on a
/// host that has no `web.toml` and its result stays the only thing on standard output. A file that is there and cannot
/// be used is never ignored, because its logging settings would be, and neither is a log file that cannot be opened.
///
/// # Errors
///
/// Returns [`ConfigInvalid`] when the file exists and cannot be read, parsed, or validated, or when the configured
/// `log_file` cannot be opened.
pub(crate) fn initialise_command_tracing(config_path: &Path) -> Result<(), ConfigInvalid> {
    match contextos_web::load_web_config(config_path) {
        Ok(config) => Ok(initialise_tracing(&config.server)?),
        Err(contextos_web::WebConfigError::Read { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            Ok(initialise_tracing(&contextos_web::WebServerConfig::default())?)
        }
        Err(error) => Err(ConfigInvalid::from(&error)),
    }
}
