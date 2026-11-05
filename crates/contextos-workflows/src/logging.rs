//! Script logging and the reasons a run is degraded.
//!
//! A script logs through [`ScriptLogSink`], so the host decides where messages go and a test can capture them
//! without a global subscriber. Which messages are kept is decided by a [`LogLevel`] minimum, resolved once when the
//! run starts by [`effective_log_level`].

use std::fmt;

use serde::{Deserialize, Deserializer};

/// The most reasons a run keeps. Further reasons are only counted.
pub const MAX_REASONS: usize = 20;
/// The longest a kept reason, or an emitted message, may be, in bytes.
pub const MAX_REASON_BYTES: usize = 512;
/// The longest a message written to the log may be, in bytes.
pub const MAX_MESSAGE_BYTES: usize = 2048;
/// Ends any text that was shortened.
pub const CUT_MARKER: &str = " [cut]";

/// How serious a message is. The order is the order of severity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// A level name that is not one of `debug`, `info`, `warn`, or `error`.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{value:?} is not a log level: expected debug, info, warn, or error")]
pub struct LogLevelError {
    pub value: String,
}

impl TryFrom<&str> for LogLevel {
    type Error = LogLevelError;

    fn try_from(value: &str) -> Result<Self, LogLevelError> {
        match value {
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            other => Err(LogLevelError {
                value: other.to_owned(),
            }),
        }
    }
}

impl<'de> Deserialize<'de> for LogLevel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::try_from(name.as_str()).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        })
    }
}

impl LogLevel {
    /// Whether a message at this level is kept when the minimum is `minimum`.
    #[must_use]
    pub fn is_kept_at(self, minimum: Self) -> bool {
        self >= minimum
    }
}

/// The minimum level a run keeps: the job's, else the workflow's, else the host's, else `info`.
#[must_use]
pub fn effective_log_level(host: Option<LogLevel>, workflow: Option<LogLevel>, job: Option<LogLevel>) -> LogLevel {
    job.or(workflow).or(host).unwrap_or(LogLevel::Info)
}

/// Where a script's kept messages go.
pub trait ScriptLogSink: Send + Sync {
    /// Writes one message that passed the run's minimum level, already cut to [`MAX_MESSAGE_BYTES`].
    fn emit(&self, level: LogLevel, message: &str);
}

/// Identifies a run in the messages written for it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RunLabel {
    pub workflow: String,
    pub job: Option<String>,
    pub run_id: Option<u64>,
}

/// Writes a run's messages through `tracing`, with the run's identity as fields.
#[derive(Clone, Debug, Default)]
pub struct TracingLogSink {
    label: RunLabel,
}

impl From<RunLabel> for TracingLogSink {
    fn from(label: RunLabel) -> Self {
        Self { label }
    }
}

impl ScriptLogSink for TracingLogSink {
    fn emit(&self, level: LogLevel, message: &str) {
        let RunLabel { workflow, job, run_id } = &self.label;
        let job = job.as_deref().unwrap_or_default();
        let run_id = run_id.unwrap_or_default();
        match level {
            LogLevel::Debug => tracing::debug!(target: "workflow_script", %workflow, %job, run_id, "{message}"),
            LogLevel::Info => tracing::info!(target: "workflow_script", %workflow, %job, run_id, "{message}"),
            LogLevel::Warn => tracing::warn!(target: "workflow_script", %workflow, %job, run_id, "{message}"),
            LogLevel::Error => tracing::error!(target: "workflow_script", %workflow, %job, run_id, "{message}"),
        }
    }
}

/// Why a run that completed is degraded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DegradedReasons {
    /// The first [`MAX_REASONS`] reasons, each cut to [`MAX_REASON_BYTES`].
    pub kept: Vec<String>,
    /// How many further reasons were counted and not kept.
    pub omitted: usize,
}

/// The reasons a run has counted so far.
#[derive(Debug, Default)]
pub(crate) struct RunReport {
    kept: Vec<String>,
    omitted: usize,
}

impl RunReport {
    pub(crate) fn note(&mut self, reason: &str) {
        if self.kept.len() < MAX_REASONS {
            self.kept.push(cut_to_bytes(reason, MAX_REASON_BYTES));
        } else {
            self.omitted += 1;
        }
    }

    /// The reasons, or `None` when the run counted none and so is not degraded.
    pub(crate) fn into_reasons(self) -> Option<DegradedReasons> {
        (!self.kept.is_empty()).then_some(DegradedReasons {
            kept: self.kept,
            omitted: self.omitted,
        })
    }
}

/// `text` if it fits in `limit` bytes, otherwise its first `limit` bytes, shortened to a character boundary, then
/// [`CUT_MARKER`].
#[must_use]
pub fn cut_to_bytes(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{CUT_MARKER}", &text[..end])
}

#[cfg(test)]
#[path = "logging_test.rs"]
mod tests;
